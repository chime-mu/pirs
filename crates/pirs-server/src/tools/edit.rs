//! `edit` tool (port of pi's `edit.ts`).

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pi_agent::{AgentTool, ToolResult, UpdateFn};
use pi_ai::Content;
use serde_json::{json, Map, Value};
use tokio_util::sync::CancellationToken;

use super::arg_str;
use super::edit_diff::{
    apply_edits_to_normalized_content, detect_line_ending, generate_diff_string, generate_unified_patch, normalize_to_lf,
    restore_line_endings, split_bom, Edit,
};
use super::path_utils::resolve_to_cwd;

pub struct EditTool {
    cwd: PathBuf,
}

impl EditTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
}

fn is_single_edit_input(value: &Value) -> bool {
    value.get("oldText").is_some_and(Value::is_string) && value.get("newText").is_some_and(Value::is_string)
}

/// Normalize the argument shapes models actually send:
/// - `edits` as a JSON string instead of an array
/// - `edits` as a single edit object instead of a one-element array
/// - legacy top-level `oldText`/`newText`
pub fn prepare_edit_arguments(input: Value) -> Value {
    let mut args: Map<String, Value> = match input {
        Value::Object(map) => map,
        other => return other,
    };

    match args.get("edits") {
        Some(Value::String(s)) => {
            if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                if parsed.is_array() {
                    args.insert("edits".into(), parsed);
                } else if is_single_edit_input(&parsed) {
                    args.insert("edits".into(), Value::Array(vec![parsed]));
                }
            }
        }
        Some(edits) if is_single_edit_input(edits) => {
            let single = edits.clone();
            args.insert("edits".into(), Value::Array(vec![single]));
        }
        _ => {}
    }

    let legacy_old = args.get("oldText").and_then(Value::as_str).map(str::to_string);
    let legacy_new = args.get("newText").and_then(Value::as_str).map(str::to_string);
    let (Some(old_text), Some(new_text)) = (legacy_old, legacy_new) else {
        return Value::Object(args);
    };

    let mut edits = match args.remove("edits") {
        Some(Value::Array(list)) => list,
        _ => Vec::new(),
    };
    edits.push(json!({ "oldText": old_text, "newText": new_text }));
    args.remove("oldText");
    args.remove("newText");
    args.insert("edits".into(), Value::Array(edits));
    Value::Object(args)
}

fn validate_edit_input(args: &Value) -> Result<(String, Vec<Edit>)> {
    let path = arg_str(args, "path")?.to_string();
    let edits = match args.get("edits") {
        Some(Value::Array(list)) if !list.is_empty() => list,
        _ => return Err(anyhow!("Edit tool input is invalid. edits must contain at least one replacement.")),
    };
    let mut parsed = Vec::with_capacity(edits.len());
    for (i, edit) in edits.iter().enumerate() {
        let old_text = edit.get("oldText").and_then(Value::as_str);
        let new_text = edit.get("newText").and_then(Value::as_str);
        match (old_text, new_text) {
            (Some(o), Some(n)) => parsed.push(Edit { old_text: o.to_string(), new_text: n.to_string() }),
            _ => return Err(anyhow!("Edit tool input is invalid. edits[{i}] must have string oldText and newText.")),
        }
    }
    Ok((path, parsed))
}

fn access_error_code(err: &std::io::Error) -> String {
    match err.kind() {
        std::io::ErrorKind::NotFound => "Error code: ENOENT".into(),
        std::io::ErrorKind::PermissionDenied => "Error code: EACCES".into(),
        _ => err.to_string(),
    }
}

#[async_trait]
impl AgentTool for EditTool {
    fn name(&self) -> String {
        "edit".into()
    }

    fn description(&self) -> String {
        "Edit a single file using exact text replacement. Every edits[].oldText must match a unique, non-overlapping region of the original file. If two changes affect the same block or nearby lines, merge them into one edit instead of emitting overlapping edits. Do not include large unchanged regions just to connect distant changes.".into()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to edit (relative or absolute)" },
                "edits": {
                    "type": "array",
                    "description": "One or more targeted replacements. Each edit is matched against the original file, not incrementally. Do not include overlapping or nested edits. If two changes touch the same block or nearby lines, merge them into one edit instead.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "oldText": {
                                "type": "string",
                                "description": "Exact text for one targeted replacement. It must be unique in the original file and must not overlap with any other edits[].oldText in the same call."
                            },
                            "newText": { "type": "string", "description": "Replacement text for this targeted edit." }
                        },
                        "required": ["oldText", "newText"]
                    }
                }
            },
            "required": ["path", "edits"]
        })
    }

    fn prompt_snippet(&self) -> Option<String> {
        Some("Make precise file edits with exact text replacement, including multiple disjoint edits in one call".into())
    }

    fn prompt_guidelines(&self) -> Vec<String> {
        vec![
            "Use edit for precise changes (edits[].oldText must match exactly)".into(),
            "When changing multiple separate locations in one file, use one edit call with multiple entries in edits[] instead of multiple edit calls".into(),
            "Each edits[].oldText is matched against the original file, not after earlier edits are applied. Do not emit overlapping or nested edits. Merge nearby changes into one edit.".into(),
            "Keep edits[].oldText as small as possible while still being unique in the file. Do not pad with large unchanged regions.".into(),
        ]
    }

    fn prepare_arguments(&self, args: Value) -> Value {
        prepare_edit_arguments(args)
    }

    async fn execute(
        &self,
        _tool_call_id: &str,
        args: Value,
        cancel: CancellationToken,
        _on_update: UpdateFn,
    ) -> Result<ToolResult> {
        let (path, edits) = validate_edit_input(&args)?;
        if cancel.is_cancelled() {
            return Err(anyhow!("Operation aborted"));
        }
        let absolute_path = resolve_to_cwd(&path, &self.cwd);

        let bytes = match tokio::fs::read(&absolute_path).await {
            Ok(bytes) => bytes,
            Err(e) => return Err(anyhow!("Could not edit file: {path}. {}.", access_error_code(&e))),
        };
        if cancel.is_cancelled() {
            return Err(anyhow!("Operation aborted"));
        }
        let raw_content = String::from_utf8_lossy(&bytes);

        // Strip BOM before matching. The model will not include an invisible BOM in oldText.
        let (bom, content) = split_bom(&raw_content);
        let original_ending = detect_line_ending(content);
        let normalized_content = normalize_to_lf(content);
        let applied = apply_edits_to_normalized_content(&normalized_content, &edits, &path)?;

        let final_content = format!("{bom}{}", restore_line_endings(&applied.new_content, original_ending));
        tokio::fs::write(&absolute_path, final_content.as_bytes())
            .await
            .map_err(|e| anyhow!("Could not edit file: {path}. {e}."))?;

        let diff_result = generate_diff_string(&applied.base_content, &applied.new_content, 4);
        let patch = generate_unified_patch(&path, &applied.base_content, &applied.new_content, 4);
        Ok(ToolResult {
            content: vec![Content::text(format!("Successfully replaced {} block(s) in {path}.", edits.len()))],
            details: Some(json!({
                // The loop turns `details.path` into an `fs.changed` event.
                "path": absolute_path.to_string_lossy(),
                "diff": diff_result.diff,
                "patch": patch,
                "firstChangedLine": diff_result.first_changed_line,
            })),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support::{first_text, noop_update};

    fn tool(dir: &tempfile::TempDir) -> EditTool {
        EditTool::new(dir.path().to_path_buf())
    }

    async fn run(dir: &tempfile::TempDir, args: Value) -> Result<ToolResult> {
        let t = tool(dir);
        let args = t.prepare_arguments(args);
        t.execute("id", args, CancellationToken::new(), noop_update()).await
    }

    #[tokio::test]
    async fn multi_edit_applies_against_original() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("f.txt"), "one\ntwo\nthree\n").expect("write");
        let r = run(&dir, json!({"path": "f.txt", "edits": [{"oldText": "three", "newText": "3"}, {"oldText": "one", "newText": "1"}]}))
            .await
            .expect("ok");
        assert_eq!(first_text(&r), "Successfully replaced 2 block(s) in f.txt.");
        assert_eq!(std::fs::read_to_string(dir.path().join("f.txt")).expect("read"), "1\ntwo\n3\n");
        let details = r.details.expect("details");
        assert_eq!(details["path"], dir.path().join("f.txt").to_string_lossy().as_ref());
        assert_eq!(details["firstChangedLine"], 1);
        assert!(details["diff"].as_str().expect("diff").contains("-1 one"));
        assert!(details["patch"].as_str().expect("patch").starts_with("--- f.txt\n+++ f.txt\n"));
    }

    #[tokio::test]
    async fn non_unique_old_text_errors() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("f.txt"), "dup\ndup\n").expect("write");
        let err = run(&dir, json!({"path": "f.txt", "edits": [{"oldText": "dup", "newText": "x"}]})).await.expect_err("dup");
        assert!(err.to_string().starts_with("Found 2 occurrences of the text in f.txt."), "{err}");
        assert_eq!(std::fs::read_to_string(dir.path().join("f.txt")).expect("read"), "dup\ndup\n");
    }

    #[tokio::test]
    async fn overlapping_edits_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("f.txt"), "hello world\n").expect("write");
        let err = run(
            &dir,
            json!({"path": "f.txt", "edits": [{"oldText": "hello wo", "newText": "a"}, {"oldText": "world", "newText": "b"}]}),
        )
        .await
        .expect_err("overlap");
        assert!(err.to_string().contains("overlap in f.txt"), "{err}");
    }

    #[tokio::test]
    async fn preserves_crlf_and_bom() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("f.txt"), "\u{FEFF}a\r\nb\r\nc\r\n").expect("write");
        run(&dir, json!({"path": "f.txt", "edits": [{"oldText": "b\n", "newText": "B\nB2\n"}]})).await.expect("ok");
        assert_eq!(std::fs::read_to_string(dir.path().join("f.txt")).expect("read"), "\u{FEFF}a\r\nB\r\nB2\r\nc\r\n");
    }

    #[tokio::test]
    async fn legacy_single_edit_and_string_edits() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("f.txt"), "foo bar\n").expect("write");
        let r = run(&dir, json!({"path": "f.txt", "oldText": "foo", "newText": "FOO"})).await.expect("legacy");
        assert_eq!(first_text(&r), "Successfully replaced 1 block(s) in f.txt.");
        assert_eq!(std::fs::read_to_string(dir.path().join("f.txt")).expect("read"), "FOO bar\n");

        let edits_string = serde_json::to_string(&json!([{"oldText": "bar", "newText": "BAR"}])).expect("json");
        run(&dir, json!({"path": "f.txt", "edits": edits_string})).await.expect("string edits");
        assert_eq!(std::fs::read_to_string(dir.path().join("f.txt")).expect("read"), "FOO BAR\n");

        run(&dir, json!({"path": "f.txt", "edits": {"oldText": "FOO", "newText": "x"}})).await.expect("object edits");
        assert_eq!(std::fs::read_to_string(dir.path().join("f.txt")).expect("read"), "x BAR\n");
    }

    #[test]
    fn prepare_arguments_shapes() {
        let v = prepare_edit_arguments(json!({"path": "p", "edits": [{"oldText": "a", "newText": "b"}], "oldText": "c", "newText": "d"}));
        assert_eq!(v["edits"].as_array().expect("array").len(), 2);
        assert!(v.get("oldText").is_none());
        assert_eq!(prepare_edit_arguments(json!("str")), json!("str"));
        let v = prepare_edit_arguments(json!({"path": "p", "edits": "not json"}));
        assert_eq!(v["edits"], "not json");
    }

    #[tokio::test]
    async fn missing_file_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = run(&dir, json!({"path": "nope.txt", "edits": [{"oldText": "a", "newText": "b"}]})).await.expect_err("missing");
        assert_eq!(err.to_string(), "Could not edit file: nope.txt. Error code: ENOENT.");
        let err = run(&dir, json!({"path": "nope.txt", "edits": []})).await.expect_err("empty");
        assert_eq!(err.to_string(), "Edit tool input is invalid. edits must contain at least one replacement.");
    }
}
