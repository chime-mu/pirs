//! `find` tool (port of pi's `find.ts`, walking in-process instead of via fd).

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use globset::{GlobBuilder, GlobMatcher};
use pi_agent::{AgentTool, ToolResult, UpdateFn};
use pi_ai::Content;
use serde_json::{json, Map, Value};
use tokio_util::sync::CancellationToken;

use super::grep::build_walker;
use super::path_utils::{relative_posix, resolve_to_cwd};
use super::truncate::{format_size, truncate_head, TruncationOptions, DEFAULT_MAX_BYTES};
use super::{arg_str, arg_usize};

const DEFAULT_LIMIT: usize = 1000;

pub struct FindTool {
    cwd: PathBuf,
}

impl FindTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
}

/// How fd's `--glob` interprets the pattern: against the basename, unless the
/// pattern contains a `/`, in which case it is matched against the full path
/// (with an implicit leading `**/`).
struct PatternMatcher {
    matcher: GlobMatcher,
    full_path: bool,
}

impl PatternMatcher {
    fn new(pattern: &str) -> Result<Self> {
        let full_path = pattern.contains('/');
        let mut effective = pattern.to_string();
        if full_path && !pattern.starts_with('/') && !pattern.starts_with("**/") && pattern != "**" {
            effective = format!("**/{pattern}");
        }
        // fd uses smart case: a pattern without uppercase letters matches case-insensitively.
        let case_insensitive = !pattern.chars().any(char::is_uppercase);
        let glob = GlobBuilder::new(&effective)
            .literal_separator(true)
            .case_insensitive(case_insensitive)
            .build()
            .map_err(|e| anyhow!("Invalid glob pattern '{pattern}': {e}"))?;
        Ok(Self { matcher: glob.compile_matcher(), full_path })
    }

    fn matches(&self, entry: &ignore::DirEntry) -> bool {
        if self.full_path {
            self.matcher.is_match(entry.path())
        } else {
            self.matcher.is_match(entry.file_name())
        }
    }
}

fn run_find(pattern: &str, search_path: &Path, limit: usize, cancel: &CancellationToken) -> Result<ToolResult> {
    if !search_path.exists() {
        return Err(anyhow!("Path not found: {}", search_path.display()));
    }
    let matcher = PatternMatcher::new(pattern)?;
    let mut results: Vec<String> = Vec::new();
    for entry in build_walker(search_path, None)? {
        if cancel.is_cancelled() {
            return Err(anyhow!("Operation aborted"));
        }
        if results.len() >= limit {
            break;
        }
        let Ok(entry) = entry else { continue };
        if entry.depth() == 0 || !matcher.matches(&entry) {
            continue;
        }
        let Some(mut rel) = relative_posix(entry.path(), search_path) else { continue };
        if entry.file_type().is_some_and(|t| t.is_dir()) {
            rel.push('/');
        }
        results.push(rel);
    }

    if results.is_empty() {
        return Ok(ToolResult::text("No files found matching pattern"));
    }

    let result_limit_reached = results.len() >= limit;
    let raw_output = results.join("\n");
    let truncation = truncate_head(&raw_output, TruncationOptions::bytes_only());
    let mut output = truncation.content.clone();
    let mut details = Map::new();
    let mut notices: Vec<String> = Vec::new();
    if result_limit_reached {
        notices.push(format!("{limit} results limit reached. Use limit={} for more, or refine pattern", limit * 2));
        details.insert("resultLimitReached".into(), json!(limit));
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
impl AgentTool for FindTool {
    fn name(&self) -> String {
        "find".into()
    }

    fn description(&self) -> String {
        format!(
            "Search for files by glob pattern. Returns matching file paths relative to the search directory. Respects .gitignore. Output is truncated to {DEFAULT_LIMIT} results or {}KB (whichever is hit first).",
            DEFAULT_MAX_BYTES / 1024
        )
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern to match files, e.g. '*.ts', '**/*.json', or 'src/**/*.spec.ts'" },
                "path": { "type": "string", "description": "Directory to search in (default: current directory)" },
                "limit": { "type": "number", "description": "Maximum number of results (default: 1000)" }
            },
            "required": ["pattern"]
        })
    }

    fn prompt_snippet(&self) -> Option<String> {
        Some("Find files by glob pattern (respects .gitignore)".into())
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
        let search_path = resolve_to_cwd(search_dir, &self.cwd);
        let limit = arg_usize(&args, "limit").unwrap_or(DEFAULT_LIMIT).max(1);
        let cancel_for_task = cancel.clone();
        tokio::task::spawn_blocking(move || run_find(&pattern, &search_path, limit, &cancel_for_task))
            .await
            .map_err(|e| anyhow!("find task failed: {e}"))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support::{first_text, noop_update};

    fn setup() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src/nested")).expect("mkdir");
        std::fs::create_dir_all(dir.path().join("build")).expect("mkdir");
        std::fs::write(dir.path().join("src/a.ts"), "").expect("write");
        std::fs::write(dir.path().join("src/nested/b.spec.ts"), "").expect("write");
        std::fs::write(dir.path().join("build/out.ts"), "").expect("write");
        std::fs::write(dir.path().join("README.md"), "").expect("write");
        std::fs::write(dir.path().join(".hidden.ts"), "").expect("write");
        std::fs::write(dir.path().join(".gitignore"), "build/\n").expect("write");
        dir
    }

    async fn run(dir: &tempfile::TempDir, args: Value) -> Result<ToolResult> {
        FindTool::new(dir.path().to_path_buf()).execute("id", args, CancellationToken::new(), noop_update()).await
    }

    #[tokio::test]
    async fn basename_glob_respects_gitignore_and_includes_hidden() {
        let dir = setup();
        let r = run(&dir, json!({"pattern": "*.ts"})).await.expect("ok");
        assert_eq!(first_text(&r), ".hidden.ts\nsrc/a.ts\nsrc/nested/b.spec.ts");
    }

    #[tokio::test]
    async fn path_glob_and_directories() {
        let dir = setup();
        let r = run(&dir, json!({"pattern": "src/**/*.spec.ts"})).await.expect("ok");
        assert_eq!(first_text(&r), "src/nested/b.spec.ts");
        let r = run(&dir, json!({"pattern": "nested"})).await.expect("ok");
        assert_eq!(first_text(&r), "src/nested/");
        let r = run(&dir, json!({"pattern": "*.ts", "path": "src"})).await.expect("ok");
        assert_eq!(first_text(&r), "a.ts\nnested/b.spec.ts");
    }

    #[tokio::test]
    async fn limit_notice_and_not_found() {
        let dir = setup();
        let r = run(&dir, json!({"pattern": "*.ts", "limit": 2})).await.expect("ok");
        let text = first_text(&r);
        assert!(text.ends_with("[2 results limit reached. Use limit=4 for more, or refine pattern]"), "{text}");
        assert_eq!(r.details.expect("details")["resultLimitReached"], 2);
        let r = run(&dir, json!({"pattern": "*.zzz"})).await.expect("ok");
        assert_eq!(first_text(&r), "No files found matching pattern");
        let err = run(&dir, json!({"pattern": "*", "path": "missing"})).await.expect_err("missing");
        assert!(err.to_string().starts_with("Path not found: "));
    }
}
