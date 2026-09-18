//! `write` tool (port of pi's `write.ts`).

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pi_agent::{AgentTool, ToolResult, UpdateFn};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use super::arg_str;
use super::path_utils::resolve_to_cwd;

pub struct WriteTool {
    cwd: PathBuf,
}

impl WriteTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
}

#[async_trait]
impl AgentTool for WriteTool {
    fn name(&self) -> String {
        "write".into()
    }

    fn description(&self) -> String {
        "Write content to a file. Creates the file if it doesn't exist, overwrites if it does. Automatically creates parent directories.".into()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to write (relative or absolute)" },
                "content": { "type": "string", "description": "Content to write to the file" }
            },
            "required": ["path", "content"]
        })
    }

    fn prompt_snippet(&self) -> Option<String> {
        Some("Create or overwrite files".into())
    }

    fn prompt_guidelines(&self) -> Vec<String> {
        vec!["Use write only for new files or complete rewrites.".into()]
    }

    async fn execute(
        &self,
        _tool_call_id: &str,
        args: Value,
        cancel: CancellationToken,
        _on_update: UpdateFn,
    ) -> Result<ToolResult> {
        let path = arg_str(&args, "path")?.to_string();
        let content = arg_str(&args, "content")?.to_string();
        if cancel.is_cancelled() {
            return Err(anyhow!("Operation aborted"));
        }
        let absolute_path = resolve_to_cwd(&path, &self.cwd);
        if let Some(dir) = absolute_path.parent() {
            tokio::fs::create_dir_all(dir)
                .await
                .map_err(|e| anyhow!("Could not create directory {}: {e}", dir.display()))?;
        }
        if cancel.is_cancelled() {
            return Err(anyhow!("Operation aborted"));
        }
        tokio::fs::write(&absolute_path, content.as_bytes())
            .await
            .map_err(|e| anyhow!("Could not write file {}: {e}", absolute_path.display()))?;
        Ok(ToolResult::text(format!("Successfully wrote to {path}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support::{first_text, noop_update};

    #[tokio::test]
    async fn creates_parent_dirs_and_overwrites() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tool = WriteTool::new(dir.path().to_path_buf());
        let r = tool
            .execute("id", json!({"path": "a/b/c.txt", "content": "hello"}), CancellationToken::new(), noop_update())
            .await
            .expect("ok");
        assert_eq!(first_text(&r), "Successfully wrote to a/b/c.txt");
        assert_eq!(std::fs::read_to_string(dir.path().join("a/b/c.txt")).expect("read"), "hello");

        tool.execute("id", json!({"path": "a/b/c.txt", "content": "bye"}), CancellationToken::new(), noop_update())
            .await
            .expect("ok");
        assert_eq!(std::fs::read_to_string(dir.path().join("a/b/c.txt")).expect("read"), "bye");
    }

    #[tokio::test]
    async fn missing_content_is_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tool = WriteTool::new(dir.path().to_path_buf());
        let err = tool.execute("id", json!({"path": "x"}), CancellationToken::new(), noop_update()).await.expect_err("err");
        assert!(err.to_string().contains("content"));
    }
}
