//! `ls` tool (port of pi's `ls.ts`).

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pi_agent::{AgentTool, ToolResult, UpdateFn};
use pi_ai::Content;
use serde_json::{json, Map, Value};
use tokio_util::sync::CancellationToken;

use super::arg_usize;
use super::path_utils::resolve_to_cwd;
use super::truncate::{format_size, truncate_head, TruncationOptions, DEFAULT_MAX_BYTES};

const DEFAULT_LIMIT: usize = 500;

pub struct LsTool {
    cwd: PathBuf,
}

impl LsTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
}

fn run_ls(dir_path: &Path, limit: usize) -> Result<ToolResult> {
    if !dir_path.exists() {
        return Err(anyhow!("Path not found: {}", dir_path.display()));
    }
    let metadata = std::fs::metadata(dir_path).map_err(|e| anyhow!("Cannot read directory: {e}"))?;
    if !metadata.is_dir() {
        return Err(anyhow!("Not a directory: {}", dir_path.display()));
    }
    let read_dir = std::fs::read_dir(dir_path).map_err(|e| anyhow!("Cannot read directory: {e}"))?;
    let mut entries: Vec<String> =
        read_dir.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string()).collect();

    // Sort alphabetically, case-insensitive.
    entries.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b)));

    let mut results: Vec<String> = Vec::new();
    let mut entry_limit_reached = false;
    for entry in entries {
        if results.len() >= limit {
            entry_limit_reached = true;
            break;
        }
        let Ok(entry_meta) = std::fs::metadata(dir_path.join(&entry)) else { continue };
        let suffix = if entry_meta.is_dir() { "/" } else { "" };
        results.push(format!("{entry}{suffix}"));
    }

    if results.is_empty() {
        return Ok(ToolResult::text("(empty directory)"));
    }

    let raw_output = results.join("\n");
    let truncation = truncate_head(&raw_output, TruncationOptions::bytes_only());
    let mut output = truncation.content.clone();
    let mut details = Map::new();
    let mut notices: Vec<String> = Vec::new();
    if entry_limit_reached {
        notices.push(format!("{limit} entries limit reached. Use limit={} for more", limit * 2));
        details.insert("entryLimitReached".into(), json!(limit));
    }
    if truncation.truncated {
        notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
        details.insert("truncation".into(), serde_json::to_value(&truncation)?);
    }
    if !notices.is_empty() {
        output.push_str(&format!("\n\n[{}]", notices.join(". ")));
    }
    Ok(ToolResult {
        content: vec![Content::text(output)],
        details: if details.is_empty() { None } else { Some(Value::Object(details)) },
        ..Default::default()
    })
}

#[async_trait]
impl AgentTool for LsTool {
    fn name(&self) -> String {
        "ls".into()
    }

    fn description(&self) -> String {
        format!(
            "List directory contents. Returns entries sorted alphabetically, with '/' suffix for directories. Includes dotfiles. Output is truncated to {DEFAULT_LIMIT} entries or {}KB (whichever is hit first).",
            DEFAULT_MAX_BYTES / 1024
        )
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory to list (default: current directory)" },
                "limit": { "type": "number", "description": "Maximum number of entries to return (default: 500)" }
            },
            "required": []
        })
    }

    fn prompt_snippet(&self) -> Option<String> {
        Some("List directory contents".into())
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
        let path = args.get("path").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(".");
        let dir_path = resolve_to_cwd(path, &self.cwd);
        let limit = arg_usize(&args, "limit").unwrap_or(DEFAULT_LIMIT);
        tokio::task::spawn_blocking(move || run_ls(&dir_path, limit))
            .await
            .map_err(|e| anyhow!("ls task failed: {e}"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support::{first_text, noop_update};

    async fn run(dir: &tempfile::TempDir, args: Value) -> Result<ToolResult> {
        LsTool::new(dir.path().to_path_buf()).execute("id", args, CancellationToken::new(), noop_update()).await
    }

    #[tokio::test]
    async fn sorts_case_insensitively_with_dir_suffix_and_dotfiles() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("Zeta")).expect("mkdir");
        std::fs::write(dir.path().join("alpha.txt"), "").expect("write");
        std::fs::write(dir.path().join("Beta.txt"), "").expect("write");
        std::fs::write(dir.path().join(".env"), "").expect("write");
        let r = run(&dir, json!({})).await.expect("ok");
        assert_eq!(first_text(&r), ".env\nalpha.txt\nBeta.txt\nZeta/");
    }

    #[tokio::test]
    async fn limit_empty_and_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let r = run(&dir, json!({})).await.expect("ok");
        assert_eq!(first_text(&r), "(empty directory)");

        for name in ["a", "b", "c"] {
            std::fs::write(dir.path().join(name), "").expect("write");
        }
        let r = run(&dir, json!({"limit": 2})).await.expect("ok");
        assert_eq!(first_text(&r), "a\nb\n\n[2 entries limit reached. Use limit=4 for more]");
        assert_eq!(r.details.expect("details")["entryLimitReached"], 2);

        let err = run(&dir, json!({"path": "a"})).await.expect_err("not dir");
        assert!(err.to_string().starts_with("Not a directory: "));
        let err = run(&dir, json!({"path": "missing"})).await.expect_err("missing");
        assert!(err.to_string().starts_with("Path not found: "));
    }
}
