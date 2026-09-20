//! `grep` tool (port of pi's `grep.ts`, searching in-process instead of via ripgrep).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use ignore::overrides::OverrideBuilder;
use ignore::WalkBuilder;
use pi_agent::{AgentTool, ToolResult, UpdateFn};
use pi_ai::Content;
use regex::{Regex, RegexBuilder};
use serde_json::{json, Map, Value};
use tokio_util::sync::CancellationToken;

use super::path_utils::{relative_posix, resolve_to_cwd};
use super::truncate::{format_size, truncate_head, truncate_line, TruncationOptions, DEFAULT_MAX_BYTES, GREP_MAX_LINE_LENGTH};
use super::{arg_bool, arg_str, arg_usize};

const DEFAULT_LIMIT: usize = 100;

pub(crate) struct GrepTool {
    cwd: PathBuf,
}

impl GrepTool {
    pub(crate) fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
}

struct GrepMatch {
    file_path: PathBuf,
    line_number: usize,
    line_text: String,
}

struct GrepRequest {
    pattern: String,
    search_path: PathBuf,
    glob: Option<String>,
    ignore_case: bool,
    literal: bool,
    context: usize,
    limit: usize,
}

fn build_regex(pattern: &str, literal: bool, ignore_case: bool) -> Result<Regex> {
    let source = if literal { regex::escape(pattern) } else { pattern.to_string() };
    RegexBuilder::new(&source)
        .case_insensitive(ignore_case)
        .build()
        .map_err(|e| anyhow!("regex parse error: {e}"))
}

/// Build the file walker used by grep and find: includes hidden files,
/// honours .gitignore (even outside git repos), skips `.git`, deterministic order.
pub(crate) fn build_walker(root: &Path, glob: Option<&str>) -> Result<ignore::Walk> {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .require_git(false)
        .sort_by_file_name(|a, b| a.cmp(b))
        .filter_entry(|entry| entry.file_name() != ".git");
    if let Some(glob) = glob {
        let mut overrides = OverrideBuilder::new(root);
        overrides.add(glob).map_err(|e| anyhow!("Invalid glob '{glob}': {e}"))?;
        builder.overrides(overrides.build().map_err(|e| anyhow!("Invalid glob '{glob}': {e}"))?);
    }
    Ok(builder.build())
}

fn normalize_lines(content: &str) -> Vec<String> {
    content.replace("\r\n", "\n").replace('\r', "\n").split('\n').map(str::to_string).collect()
}

fn search_file(path: &Path, regex: &Regex, matches: &mut Vec<GrepMatch>, limit: usize) -> bool {
    let Ok(bytes) = std::fs::read(path) else { return false };
    // Skip binary files (ripgrep's NUL heuristic).
    if bytes.contains(&0) {
        return false;
    }
    let content = String::from_utf8_lossy(&bytes);
    for (idx, line) in content.split('\n').enumerate() {
        if matches.len() >= limit {
            return true;
        }
        let line = line.strip_suffix('\r').unwrap_or(line);
        if regex.is_match(line) {
            matches.push(GrepMatch { file_path: path.to_path_buf(), line_number: idx + 1, line_text: line.to_string() });
        }
    }
    matches.len() >= limit
}

fn run_grep(req: &GrepRequest, cancel: &CancellationToken) -> Result<ToolResult> {
    let metadata = std::fs::metadata(&req.search_path).map_err(|_| anyhow!("Path not found: {}", req.search_path.display()))?;
    let is_directory = metadata.is_dir();
    let regex = build_regex(&req.pattern, req.literal, req.ignore_case)?;

    let format_path = |file_path: &Path| -> String {
        if is_directory {
            if let Some(rel) = relative_posix(file_path, &req.search_path).filter(|r| !r.is_empty() && !r.starts_with("..")) {
                return rel;
            }
        }
        file_path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
    };

    let mut matches: Vec<GrepMatch> = Vec::new();
    let mut match_limit_reached = false;
    if is_directory {
        for entry in build_walker(&req.search_path, req.glob.as_deref())? {
            if cancel.is_cancelled() {
                return Err(anyhow!("Operation aborted"));
            }
            let Ok(entry) = entry else { continue };
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            if search_file(entry.path(), &regex, &mut matches, req.limit) {
                match_limit_reached = true;
                break;
            }
        }
    } else if search_file(&req.search_path, &regex, &mut matches, req.limit) {
        match_limit_reached = true;
    }

    if matches.is_empty() {
        return Ok(ToolResult::text("No matches found"));
    }

    let mut file_cache: HashMap<PathBuf, Vec<String>> = HashMap::new();
    let mut lines_truncated = false;
    let mut output_lines: Vec<String> = Vec::new();
    for m in &matches {
        let relative_path = format_path(&m.file_path);
        if req.context == 0 {
            let (text, was_truncated) = truncate_line(&m.line_text, GREP_MAX_LINE_LENGTH);
            lines_truncated |= was_truncated;
            output_lines.push(format!("{relative_path}:{}: {text}", m.line_number));
            continue;
        }
        let lines = file_cache.entry(m.file_path.clone()).or_insert_with(|| {
            std::fs::read(&m.file_path).map(|b| normalize_lines(&String::from_utf8_lossy(&b))).unwrap_or_default()
        });
        if lines.is_empty() {
            output_lines.push(format!("{relative_path}:{}: (unable to read file)", m.line_number));
            continue;
        }
        let start = m.line_number.saturating_sub(req.context).max(1);
        let end = (m.line_number + req.context).min(lines.len());
        for current in start..=end {
            let line_text = lines.get(current - 1).map(String::as_str).unwrap_or("");
            let (text, was_truncated) = truncate_line(line_text, GREP_MAX_LINE_LENGTH);
            lines_truncated |= was_truncated;
            if current == m.line_number {
                output_lines.push(format!("{relative_path}:{current}: {text}"));
            } else {
                output_lines.push(format!("{relative_path}-{current}- {text}"));
            }
        }
    }

    let raw_output = output_lines.join("\n");
    // Byte truncation only: the match limit already caps the number of rows.
    let truncation = truncate_head(&raw_output, TruncationOptions::bytes_only());
    let mut output = truncation.content.clone();
    let mut details = Map::new();
    let mut notices: Vec<String> = Vec::new();
    if match_limit_reached {
        notices.push(format!("{} matches limit reached. Use limit={} for more, or refine pattern", req.limit, req.limit * 2));
        details.insert("matchLimitReached".into(), json!(req.limit));
    }
    if truncation.truncated {
        notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
        details.insert("truncation".into(), serde_json::to_value(&truncation)?);
    }
    if lines_truncated {
        notices.push(format!("Some lines truncated to {GREP_MAX_LINE_LENGTH} chars. Use read tool to see full lines"));
        details.insert("linesTruncated".into(), json!(true));
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
impl AgentTool for GrepTool {
    fn name(&self) -> String {
        "grep".into()
    }

    fn description(&self) -> String {
        format!(
            "Search file contents for a pattern. Returns matching lines with file paths and line numbers. Respects .gitignore. Output is truncated to {DEFAULT_LIMIT} matches or {}KB (whichever is hit first). Long lines are truncated to {GREP_MAX_LINE_LENGTH} chars.",
            DEFAULT_MAX_BYTES / 1024
        )
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Search pattern (regex or literal string)" },
                "path": { "type": "string", "description": "Directory or file to search (default: current directory)" },
                "glob": { "type": "string", "description": "Filter files by glob pattern, e.g. '*.ts' or '**/*.spec.ts'" },
                "ignoreCase": { "type": "boolean", "description": "Case-insensitive search (default: false)" },
                "literal": { "type": "boolean", "description": "Treat pattern as literal string instead of regex (default: false)" },
                "context": { "type": "number", "description": "Number of lines to show before and after each match (default: 0)" },
                "limit": { "type": "number", "description": "Maximum number of matches to return (default: 100)" }
            },
            "required": ["pattern"]
        })
    }

    fn prompt_snippet(&self) -> Option<String> {
        Some("Search file contents for patterns (respects .gitignore)".into())
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
        let pattern = arg_str(&args, "pattern")?.to_string();
        let search_dir = args.get("path").and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(".");
        let req = GrepRequest {
            pattern,
            search_path: resolve_to_cwd(search_dir, &self.cwd),
            glob: args.get("glob").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string),
            ignore_case: arg_bool(&args, "ignoreCase"),
            literal: arg_bool(&args, "literal"),
            context: arg_usize(&args, "context").unwrap_or(0),
            limit: arg_usize(&args, "limit").unwrap_or(DEFAULT_LIMIT).max(1),
        };
        let cancel_for_task = cancel.clone();
        tokio::task::spawn_blocking(move || run_grep(&req, &cancel_for_task))
            .await
            .map_err(|e| anyhow!("grep task failed: {e}"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support::{first_text, noop_update};

    fn setup() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        std::fs::write(dir.path().join("src/a.ts"), "alpha\nfoo.bar\nbeta\nfoo bar\ngamma\n").expect("write");
        std::fs::write(dir.path().join("b.md"), "Foo\nnothing\n").expect("write");
        std::fs::write(dir.path().join("ignored.log"), "foo.bar\n").expect("write");
        std::fs::write(dir.path().join(".gitignore"), "*.log\n").expect("write");
        std::fs::write(dir.path().join("bin.dat"), b"foo\0bar").expect("write");
        dir
    }

    async fn run(dir: &tempfile::TempDir, args: Value) -> Result<ToolResult> {
        GrepTool::new(dir.path().to_path_buf()).execute("id", args, CancellationToken::new(), noop_update()).await
    }

    #[tokio::test]
    async fn regex_vs_literal() {
        let dir = setup();
        let r = run(&dir, json!({"pattern": "foo.bar"})).await.expect("ok");
        assert_eq!(first_text(&r), "src/a.ts:2: foo.bar\nsrc/a.ts:4: foo bar");
        let r = run(&dir, json!({"pattern": "foo.bar", "literal": true})).await.expect("ok");
        assert_eq!(first_text(&r), "src/a.ts:2: foo.bar");
    }

    #[tokio::test]
    async fn ignore_case_glob_and_context() {
        let dir = setup();
        let r = run(&dir, json!({"pattern": "^foo$", "ignoreCase": true})).await.expect("ok");
        assert_eq!(first_text(&r), "b.md:1: Foo");
        let r = run(&dir, json!({"pattern": "foo", "glob": "*.md", "ignoreCase": true})).await.expect("ok");
        assert_eq!(first_text(&r), "b.md:1: Foo");
        let r = run(&dir, json!({"pattern": "beta", "context": 1})).await.expect("ok");
        assert_eq!(first_text(&r), "src/a.ts-2- foo.bar\nsrc/a.ts:3: beta\nsrc/a.ts-4- foo bar");
    }

    #[tokio::test]
    async fn respects_gitignore_and_skips_binary() {
        let dir = setup();
        let r = run(&dir, json!({"pattern": "foo"})).await.expect("ok");
        let text = first_text(&r);
        assert!(!text.contains("ignored.log"), "{text}");
        assert!(!text.contains("bin.dat"), "{text}");
    }

    #[tokio::test]
    async fn limit_notice_and_no_matches() {
        let dir = setup();
        let r = run(&dir, json!({"pattern": "foo", "limit": 1})).await.expect("ok");
        let text = first_text(&r);
        assert!(text.ends_with("[1 matches limit reached. Use limit=2 for more, or refine pattern]"), "{text}");
        assert_eq!(r.details.expect("details")["matchLimitReached"], 1);
        let r = run(&dir, json!({"pattern": "zzz"})).await.expect("ok");
        assert_eq!(first_text(&r), "No matches found");
    }

    #[tokio::test]
    async fn single_file_and_missing_path() {
        let dir = setup();
        let r = run(&dir, json!({"pattern": "alpha", "path": "src/a.ts"})).await.expect("ok");
        assert_eq!(first_text(&r), "a.ts:1: alpha");
        let err = run(&dir, json!({"pattern": "x", "path": "nope"})).await.expect_err("missing");
        assert!(err.to_string().starts_with("Path not found: "));
    }

    #[tokio::test]
    async fn long_lines_are_truncated() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("long.txt"), format!("{}\n", "y".repeat(600))).expect("write");
        let r = run(&dir, json!({"pattern": "y"})).await.expect("ok");
        let text = first_text(&r);
        assert!(text.contains("... [truncated]"));
        assert!(text.ends_with("[Some lines truncated to 500 chars. Use read tool to see full lines]"), "{text}");
    }
}
