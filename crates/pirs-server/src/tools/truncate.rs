//! Shared truncation utilities for tool outputs (port of pi's `truncate.ts`).
//!
//! Truncation is based on two independent limits - whichever is hit first wins:
//! - Line limit (default: 2000 lines)
//! - Byte limit (default: 50KB)
//!
//! Never returns partial lines (except the bash tail truncation edge case).

use serde::Serialize;

pub const DEFAULT_MAX_LINES: usize = 2000;
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024; // 50KB
pub const GREP_MAX_LINE_LENGTH: usize = 500; // Max chars per grep match line

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TruncatedBy {
    Lines,
    Bytes,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TruncationResult {
    /// The truncated content
    pub content: String,
    /// Whether truncation occurred
    pub truncated: bool,
    /// Which limit was hit, or `None` if not truncated
    pub truncated_by: Option<TruncatedBy>,
    /// Total number of lines in the original content
    pub total_lines: usize,
    /// Total number of bytes in the original content
    pub total_bytes: usize,
    /// Number of complete lines in the truncated output
    pub output_lines: usize,
    /// Number of bytes in the truncated output
    pub output_bytes: usize,
    /// Whether the last line was partially truncated (only for tail truncation edge case)
    pub last_line_partial: bool,
    /// Whether the first line exceeded the byte limit (for head truncation)
    pub first_line_exceeds_limit: bool,
    /// The max lines limit that was applied
    pub max_lines: usize,
    /// The max bytes limit that was applied
    pub max_bytes: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct TruncationOptions {
    pub max_lines: usize,
    pub max_bytes: usize,
}

impl Default for TruncationOptions {
    fn default() -> Self {
        Self { max_lines: DEFAULT_MAX_LINES, max_bytes: DEFAULT_MAX_BYTES }
    }
}

impl TruncationOptions {
    /// Byte limit only (used where an item count already caps the rows).
    pub fn bytes_only() -> Self {
        Self { max_lines: usize::MAX, max_bytes: DEFAULT_MAX_BYTES }
    }
}

fn split_lines_for_counting(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}

/// Format bytes as human-readable size.
pub fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes}B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn untruncated(content: &str, total_lines: usize, total_bytes: usize, opts: TruncationOptions) -> TruncationResult {
    TruncationResult {
        content: content.to_string(),
        truncated: false,
        truncated_by: None,
        total_lines,
        total_bytes,
        output_lines: total_lines,
        output_bytes: total_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines: opts.max_lines,
        max_bytes: opts.max_bytes,
    }
}

/// Truncate content from the head (keep first N lines/bytes).
/// Suitable for file reads where you want to see the beginning.
///
/// Never returns partial lines. If the first line exceeds the byte limit,
/// returns empty content with `first_line_exceeds_limit = true`.
pub fn truncate_head(content: &str, opts: TruncationOptions) -> TruncationResult {
    let TruncationOptions { max_lines, max_bytes } = opts;
    let total_bytes = content.len();
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return untruncated(content, total_lines, total_bytes, opts);
    }

    let first_line_bytes = lines.first().map_or(0, |l| l.len());
    if first_line_bytes > max_bytes {
        return TruncationResult {
            content: String::new(),
            truncated: true,
            truncated_by: Some(TruncatedBy::Bytes),
            total_lines,
            total_bytes,
            output_lines: 0,
            output_bytes: 0,
            last_line_partial: false,
            first_line_exceeds_limit: true,
            max_lines,
            max_bytes,
        };
    }

    let mut output: Vec<&str> = Vec::new();
    let mut output_bytes = 0usize;
    let mut truncated_by = TruncatedBy::Lines;

    for (i, line) in lines.iter().enumerate().take(max_lines) {
        let line_bytes = line.len() + usize::from(i > 0);
        if output_bytes + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            break;
        }
        output.push(line);
        output_bytes += line_bytes;
    }

    if output.len() >= max_lines && output_bytes <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    let content = output.join("\n");
    let output_bytes = content.len();
    TruncationResult {
        output_lines: output.len(),
        content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_bytes,
        last_line_partial: false,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// Truncate content from the tail (keep last N lines/bytes).
/// Suitable for bash output where you want to see the end (errors, final results).
///
/// May return a partial first line if the last line of the original content exceeds the byte limit.
pub fn truncate_tail(content: &str, opts: TruncationOptions) -> TruncationResult {
    let TruncationOptions { max_lines, max_bytes } = opts;
    let total_bytes = content.len();
    let lines = split_lines_for_counting(content);
    let total_lines = lines.len();

    if total_lines <= max_lines && total_bytes <= max_bytes {
        return untruncated(content, total_lines, total_bytes, opts);
    }

    let mut output: std::collections::VecDeque<&str> = std::collections::VecDeque::new();
    let mut output_bytes = 0usize;
    let mut truncated_by = TruncatedBy::Lines;
    let mut last_line_partial = false;

    for line in lines.iter().rev() {
        if output.len() >= max_lines {
            break;
        }
        let line_bytes = line.len() + usize::from(!output.is_empty());
        if output_bytes + line_bytes > max_bytes {
            truncated_by = TruncatedBy::Bytes;
            // Edge case: nothing collected yet and this line alone exceeds the
            // limit - keep the end of the line (partial).
            if output.is_empty() {
                let truncated_line = truncate_str_to_bytes_from_end(line, max_bytes);
                output_bytes = truncated_line.len();
                output.push_front(truncated_line);
                last_line_partial = true;
            }
            break;
        }
        output.push_front(line);
        output_bytes += line_bytes;
    }

    if output.len() >= max_lines && output_bytes <= max_bytes {
        truncated_by = TruncatedBy::Lines;
    }

    let content = output.iter().copied().collect::<Vec<&str>>().join("\n");
    let output_bytes = content.len();
    TruncationResult {
        output_lines: output.len(),
        content,
        truncated: true,
        truncated_by: Some(truncated_by),
        total_lines,
        total_bytes,
        output_bytes,
        last_line_partial,
        first_line_exceeds_limit: false,
        max_lines,
        max_bytes,
    }
}

/// Truncate a string to fit within a byte limit (from the end), on a char boundary.
fn truncate_str_to_bytes_from_end(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut start = s.len() - max_bytes;
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

/// Truncate a single line to `max_chars` characters, adding a `[truncated]` suffix.
/// Used for grep match lines. Returns the text and whether it was truncated.
pub fn truncate_line(line: &str, max_chars: usize) -> (String, bool) {
    if line.chars().count() <= max_chars {
        return (line.to_string(), false);
    }
    let head: String = line.chars().take(max_chars).collect();
    (format!("{head}... [truncated]"), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_no_truncation() {
        let r = truncate_head("a\nb\n", TruncationOptions::default());
        assert!(!r.truncated);
        assert_eq!(r.total_lines, 2);
        assert_eq!(r.content, "a\nb\n");
    }

    #[test]
    fn head_line_limit() {
        let content = (1..=10).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        let r = truncate_head(&content, TruncationOptions { max_lines: 3, max_bytes: 1000 });
        assert!(r.truncated);
        assert_eq!(r.truncated_by, Some(TruncatedBy::Lines));
        assert_eq!(r.content, "1\n2\n3");
        assert_eq!(r.output_lines, 3);
        assert_eq!(r.total_lines, 10);
    }

    #[test]
    fn head_byte_limit_and_first_line_exceeds() {
        let r = truncate_head("aaaa\nbbbb\ncccc", TruncationOptions { max_lines: 100, max_bytes: 9 });
        assert_eq!(r.truncated_by, Some(TruncatedBy::Bytes));
        assert_eq!(r.content, "aaaa\nbbbb");
        let r = truncate_head("aaaaaaaaaa\nb", TruncationOptions { max_lines: 100, max_bytes: 5 });
        assert!(r.first_line_exceeds_limit);
        assert_eq!(r.content, "");
    }

    #[test]
    fn tail_keeps_last_lines() {
        let content = (1..=10).map(|i| i.to_string()).collect::<Vec<_>>().join("\n");
        let r = truncate_tail(&content, TruncationOptions { max_lines: 2, max_bytes: 1000 });
        assert_eq!(r.content, "9\n10");
        assert_eq!(r.truncated_by, Some(TruncatedBy::Lines));
    }

    #[test]
    fn tail_partial_last_line() {
        let r = truncate_tail("short\nthis-is-a-long-line", TruncationOptions { max_lines: 10, max_bytes: 4 });
        assert!(r.last_line_partial);
        assert_eq!(r.content, "line");
        assert_eq!(r.truncated_by, Some(TruncatedBy::Bytes));
    }

    #[test]
    fn line_truncation_and_size_format() {
        let (text, was) = truncate_line("abcdef", 3);
        assert!(was);
        assert_eq!(text, "abc... [truncated]");
        assert_eq!(truncate_line("ab", 3), ("ab".to_string(), false));
        assert_eq!(format_size(512), "512B");
        assert_eq!(format_size(51200), "50.0KB");
        assert_eq!(format_size(3 * 1024 * 1024), "3.0MB");
    }

    #[test]
    fn serializes_camel_case() {
        let r = truncate_head("x", TruncationOptions::default());
        let v = serde_json::to_value(&r).expect("serialize");
        assert_eq!(v["truncatedBy"], serde_json::Value::Null);
        assert_eq!(v["totalLines"], 1);
        assert_eq!(v["firstLineExceedsLimit"], false);
        let r = truncate_head("a\nb\nc", TruncationOptions { max_lines: 1, max_bytes: 100 });
        assert_eq!(serde_json::to_value(&r).expect("serialize")["truncatedBy"], "lines");
    }
}
