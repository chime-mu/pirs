//! `read` tool (port of pi's `read.ts`).

use std::io::Read as _;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use base64::Engine as _;
use pi_agent::{AgentTool, ToolResult, UpdateFn};
use pi_ai::Content;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::path_utils::resolve_read_path;
use super::truncate::{format_size, truncate_head, TruncatedBy, TruncationOptions, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES};
use super::{arg_str, arg_usize};

const IMAGE_TYPE_SNIFF_BYTES: usize = 4100;
const PNG_SIGNATURE: [u8; 8] = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];

pub struct ReadTool {
    cwd: PathBuf,
}

impl ReadTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
}

fn starts_with_ascii(buf: &[u8], offset: usize, text: &str) -> bool {
    buf.len() >= offset + text.len() && &buf[offset..offset + text.len()] == text.as_bytes()
}

fn read_u16_le(buf: &[u8], offset: usize) -> u32 {
    let b = |i: usize| u32::from(buf.get(i).copied().unwrap_or(0));
    b(offset) | (b(offset + 1) << 8)
}

fn read_u32_le(buf: &[u8], offset: usize) -> u64 {
    let b = |i: usize| u64::from(buf.get(i).copied().unwrap_or(0));
    b(offset) | (b(offset + 1) << 8) | (b(offset + 2) << 16) | (b(offset + 3) << 24)
}

fn read_u32_be(buf: &[u8], offset: usize) -> u64 {
    let b = |i: usize| u64::from(buf.get(i).copied().unwrap_or(0));
    (b(offset) << 24) | (b(offset + 1) << 16) | (b(offset + 2) << 8) | b(offset + 3)
}

fn is_png(buf: &[u8]) -> bool {
    buf.len() >= 16 && read_u32_be(buf, PNG_SIGNATURE.len()) == 13 && starts_with_ascii(buf, 12, "IHDR")
}

fn is_animated_png(buf: &[u8]) -> bool {
    let mut offset = PNG_SIGNATURE.len();
    while offset + 8 <= buf.len() {
        let chunk_length = read_u32_be(buf, offset) as usize;
        let chunk_type_offset = offset + 4;
        if starts_with_ascii(buf, chunk_type_offset, "acTL") {
            return true;
        }
        if starts_with_ascii(buf, chunk_type_offset, "IDAT") {
            return false;
        }
        let next = offset.saturating_add(8).saturating_add(chunk_length).saturating_add(4);
        if next <= offset || next > buf.len() {
            return false;
        }
        offset = next;
    }
    false
}

fn is_bmp(buf: &[u8]) -> bool {
    if buf.len() < 26 {
        return false;
    }
    let declared_file_size = read_u32_le(buf, 2);
    let pixel_data_offset = read_u32_le(buf, 10);
    let dib_header_size = read_u32_le(buf, 14);
    if declared_file_size != 0 && declared_file_size < 26 {
        return false;
    }
    if pixel_data_offset < 14 + dib_header_size {
        return false;
    }
    if declared_file_size != 0 && pixel_data_offset >= declared_file_size {
        return false;
    }
    let (color_planes, bits_per_pixel) = if dib_header_size == 12 {
        (read_u16_le(buf, 22), read_u16_le(buf, 24))
    } else if (40..=124).contains(&dib_header_size) {
        if buf.len() < 30 {
            return false;
        }
        (read_u16_le(buf, 26), read_u16_le(buf, 28))
    } else {
        return false;
    };
    color_planes == 1 && [1, 4, 8, 16, 24, 32].contains(&bits_per_pixel)
}

/// Detect a supported image MIME type from the leading bytes of a file.
pub fn detect_supported_image_mime_type(buf: &[u8]) -> Option<&'static str> {
    if buf.starts_with(&[0xff, 0xd8, 0xff]) {
        return if buf.get(3) == Some(&0xf7) { None } else { Some("image/jpeg") };
    }
    if buf.starts_with(&PNG_SIGNATURE) {
        return if is_png(buf) && !is_animated_png(buf) { Some("image/png") } else { None };
    }
    if starts_with_ascii(buf, 0, "GIF") {
        return Some("image/gif");
    }
    if starts_with_ascii(buf, 0, "RIFF") && starts_with_ascii(buf, 8, "WEBP") {
        return Some("image/webp");
    }
    if starts_with_ascii(buf, 0, "BM") && is_bmp(buf) {
        return Some("image/bmp");
    }
    None
}

fn detect_image_mime_type_from_file(path: &std::path::Path) -> std::io::Result<Option<&'static str>> {
    let mut file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; IMAGE_TYPE_SNIFF_BYTES];
    let mut read = 0usize;
    while read < buf.len() {
        let n = file.read(&mut buf[read..])?;
        if n == 0 {
            break;
        }
        read += n;
    }
    Ok(detect_supported_image_mime_type(&buf[..read]))
}

fn io_error_message(err: &std::io::Error, action: &str, path: &std::path::Path) -> String {
    let (code, text) = match err.kind() {
        std::io::ErrorKind::NotFound => ("ENOENT", "no such file or directory".to_string()),
        std::io::ErrorKind::PermissionDenied => ("EACCES", "permission denied".to_string()),
        _ => ("EIO", err.to_string()),
    };
    format!("{code}: {text}, {action} '{}'", path.display())
}

#[async_trait]
impl AgentTool for ReadTool {
    fn name(&self) -> String {
        "read".into()
    }

    fn description(&self) -> String {
        format!(
            "Read the contents of a file. Supports text files and images (jpg, png, gif, webp, bmp). Images are sent as attachments. For text files, output is truncated to {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.",
            DEFAULT_MAX_BYTES / 1024
        )
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to read (relative or absolute)" },
                "offset": { "type": "number", "description": "Line number to start reading from (1-indexed)" },
                "limit": { "type": "number", "description": "Maximum number of lines to read" }
            },
            "required": ["path"]
        })
    }

    fn prompt_snippet(&self) -> Option<String> {
        Some("Read file contents".into())
    }

    fn prompt_guidelines(&self) -> Vec<String> {
        vec!["Use read to examine files instead of cat or sed.".into()]
    }

    async fn execute(
        &self,
        _tool_call_id: &str,
        args: Value,
        cancel: CancellationToken,
        _on_update: UpdateFn,
    ) -> Result<ToolResult> {
        if cancel.is_cancelled() {
            return Err(anyhow!("Operation aborted"));
        }
        let path = arg_str(&args, "path")?.to_string();
        let offset = arg_usize(&args, "offset");
        let limit = arg_usize(&args, "limit");
        let cwd = self.cwd.clone();

        let result = tokio::task::spawn_blocking(move || read_file_blocking(&path, offset, limit, &cwd))
            .await
            .map_err(|e| anyhow!("read task failed: {e}"))?;
        if cancel.is_cancelled() {
            return Err(anyhow!("Operation aborted"));
        }
        result
    }
}

fn read_file_blocking(path: &str, offset: Option<usize>, limit: Option<usize>, cwd: &std::path::Path) -> Result<ToolResult> {
    let absolute_path = resolve_read_path(path, cwd);
    // Check the file exists and is readable.
    let metadata = std::fs::metadata(&absolute_path).map_err(|e| anyhow!(io_error_message(&e, "access", &absolute_path)))?;
    if metadata.is_dir() {
        return Err(anyhow!("EISDIR: illegal operation on a directory, read '{}'", absolute_path.display()));
    }
    let mime_type = detect_image_mime_type_from_file(&absolute_path)
        .map_err(|e| anyhow!(io_error_message(&e, "open", &absolute_path)))?;

    if let Some(mime_type) = mime_type {
        let bytes = std::fs::read(&absolute_path).map_err(|e| anyhow!(io_error_message(&e, "open", &absolute_path)))?;
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        return Ok(ToolResult {
            content: vec![
                Content::text(format!("Read image file [{mime_type}]")),
                Content::Image { data, mime_type: mime_type.to_string() },
            ],
            ..Default::default()
        });
    }

    let bytes = std::fs::read(&absolute_path).map_err(|e| anyhow!(io_error_message(&e, "open", &absolute_path)))?;
    let text_content = String::from_utf8_lossy(&bytes);
    let all_lines: Vec<&str> = text_content.split('\n').collect();
    let total_file_lines = all_lines.len();

    // Convert from 1-indexed input to 0-indexed array access.
    let start_line = offset.map_or(0, |o| o.saturating_sub(1));
    let start_line_display = start_line + 1;
    if start_line >= all_lines.len() {
        return Err(anyhow!(
            "Offset {} is beyond end of file ({} lines total)",
            offset.unwrap_or(0),
            all_lines.len()
        ));
    }

    // If the user gave a limit, honor it first. Otherwise truncate_head decides.
    let (selected_content, user_limited_lines) = match limit {
        Some(limit) => {
            let end_line = start_line.saturating_add(limit).min(all_lines.len());
            (all_lines[start_line..end_line].join("\n"), Some(end_line - start_line))
        }
        None => (all_lines[start_line..].join("\n"), None),
    };

    let truncation = truncate_head(&selected_content, TruncationOptions::default());
    let mut details = None;
    let output_text = if truncation.first_line_exceeds_limit {
        let first_line_size = format_size(all_lines[start_line].len());
        details = Some(json!({ "truncation": truncation }));
        format!(
            "[Line {start_line_display} is {first_line_size}, exceeds {} limit. Use bash: sed -n '{start_line_display}p' {path} | head -c {DEFAULT_MAX_BYTES}]",
            format_size(DEFAULT_MAX_BYTES)
        )
    } else if truncation.truncated {
        let end_line_display = start_line_display + truncation.output_lines.saturating_sub(1);
        let next_offset = end_line_display + 1;
        let mut text = truncation.content.clone();
        if truncation.truncated_by == Some(TruncatedBy::Lines) {
            text.push_str(&format!(
                "\n\n[Showing lines {start_line_display}-{end_line_display} of {total_file_lines}. Use offset={next_offset} to continue.]"
            ));
        } else {
            text.push_str(&format!(
                "\n\n[Showing lines {start_line_display}-{end_line_display} of {total_file_lines} ({} limit). Use offset={next_offset} to continue.]",
                format_size(DEFAULT_MAX_BYTES)
            ));
        }
        details = Some(json!({ "truncation": truncation }));
        text
    } else if let Some(user_limited) = user_limited_lines.filter(|n| start_line + n < all_lines.len()) {
        let remaining = all_lines.len() - (start_line + user_limited);
        let next_offset = start_line + user_limited + 1;
        format!("{}\n\n[{remaining} more lines in file. Use offset={next_offset} to continue.]", truncation.content)
    } else {
        truncation.content
    };

    Ok(ToolResult { content: vec![Content::text(output_text)], details, ..Default::default() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support::{first_text, noop_update};

    fn tool(dir: &tempfile::TempDir) -> ReadTool {
        ReadTool::new(dir.path().to_path_buf())
    }

    #[tokio::test]
    async fn reads_whole_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\nthree").expect("write");
        let r = tool(&dir).execute("id", json!({"path": "a.txt"}), CancellationToken::new(), noop_update()).await.expect("ok");
        assert_eq!(first_text(&r), "one\ntwo\nthree");
        assert!(r.details.is_none());
    }

    #[tokio::test]
    async fn offset_and_limit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let content = (1..=10).map(|i| format!("line{i}")).collect::<Vec<_>>().join("\n");
        std::fs::write(dir.path().join("a.txt"), content).expect("write");
        let r = tool(&dir)
            .execute("id", json!({"path": "a.txt", "offset": 3, "limit": 2}), CancellationToken::new(), noop_update())
            .await
            .expect("ok");
        assert_eq!(first_text(&r), "line3\nline4\n\n[6 more lines in file. Use offset=5 to continue.]");

        let err = tool(&dir)
            .execute("id", json!({"path": "a.txt", "offset": 50}), CancellationToken::new(), noop_update())
            .await
            .expect_err("beyond end");
        assert_eq!(err.to_string(), "Offset 50 is beyond end of file (10 lines total)");
    }

    #[tokio::test]
    async fn truncates_long_files_with_notice() {
        let dir = tempfile::tempdir().expect("tempdir");
        let content = (1..=2500).map(|i| format!("l{i}")).collect::<Vec<_>>().join("\n");
        std::fs::write(dir.path().join("big.txt"), content).expect("write");
        let r = tool(&dir).execute("id", json!({"path": "big.txt"}), CancellationToken::new(), noop_update()).await.expect("ok");
        let text = first_text(&r);
        assert!(text.ends_with("[Showing lines 1-2000 of 2500. Use offset=2001 to continue.]"), "{text}");
        let details = r.details.expect("details");
        assert_eq!(details["truncation"]["truncatedBy"], "lines");
        assert_eq!(details["truncation"]["totalLines"], 2500);
    }

    #[tokio::test]
    async fn byte_truncation_notice() {
        let dir = tempfile::tempdir().expect("tempdir");
        let line = "x".repeat(1000);
        let content = std::iter::repeat_n(line.as_str(), 100).collect::<Vec<_>>().join("\n");
        std::fs::write(dir.path().join("wide.txt"), content).expect("write");
        let r = tool(&dir).execute("id", json!({"path": "wide.txt"}), CancellationToken::new(), noop_update()).await.expect("ok");
        let text = first_text(&r);
        assert!(text.contains("(50.0KB limit). Use offset=52 to continue.]"), "{text}");
    }

    #[tokio::test]
    async fn missing_file_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = tool(&dir).execute("id", json!({"path": "nope.txt"}), CancellationToken::new(), noop_update()).await.expect_err("missing");
        assert!(err.to_string().starts_with("ENOENT: no such file or directory"), "{err}");
    }

    #[tokio::test]
    async fn reads_images_as_attachments() {
        let dir = tempfile::tempdir().expect("tempdir");
        let gif = b"GIF89a\x01\x00\x01\x00\x00\x00\x00;";
        std::fs::write(dir.path().join("pic.gif"), gif).expect("write");
        let r = tool(&dir).execute("id", json!({"path": "pic.gif"}), CancellationToken::new(), noop_update()).await.expect("ok");
        assert_eq!(first_text(&r), "Read image file [image/gif]");
        match &r.content[1] {
            Content::Image { data, mime_type } => {
                assert_eq!(mime_type, "image/gif");
                assert_eq!(base64::engine::general_purpose::STANDARD.decode(data).expect("b64"), gif);
            }
            other => panic!("expected image, got {other:?}"),
        }
    }

    #[test]
    fn sniffs_image_types() {
        assert_eq!(detect_supported_image_mime_type(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
        assert_eq!(detect_supported_image_mime_type(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(detect_supported_image_mime_type(b"plain text"), None);
        let mut png = PNG_SIGNATURE.to_vec();
        png.extend_from_slice(&[0, 0, 0, 13]);
        png.extend_from_slice(b"IHDR");
        assert_eq!(detect_supported_image_mime_type(&png), Some("image/png"));
    }
}
