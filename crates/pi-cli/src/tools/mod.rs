//! Built-in tools (port of pi's `packages/coding-agent/src/core/tools`).
//!
//! Each tool lives in its own file and implements [`pi_agent::AgentTool`].
//! Shared helpers: [`truncate`] (output limits), [`path_utils`] (path
//! resolution), [`edit_diff`] (edit application and diff rendering).

// This module is the tool library surface consumed by the CLI wiring; until
// `main.rs` uses it, everything here would otherwise trip `dead_code`.
#![allow(dead_code)]

pub mod bash;
pub mod edit;
pub mod edit_diff;
pub mod find;
pub mod grep;
pub mod ls;
pub mod path_utils;
pub mod read;
pub mod truncate;
pub mod write;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pi_agent::ToolRef;

pub use bash::BashTool;
pub use edit::EditTool;
pub use find::FindTool;
pub use grep::GrepTool;
pub use ls::LsTool;
pub use read::ReadTool;
pub use write::WriteTool;

/// Options shared by all built-in tools.
#[derive(Debug, Clone)]
pub struct ToolOptions {
    /// Working directory relative paths are resolved against.
    pub cwd: PathBuf,
}

/// pi's default tool selection.
pub const DEFAULT_TOOL_NAMES: &[&str] = &["read", "bash", "edit", "write"];

/// Every built-in tool, in the order `builtin_tools` returns them.
pub const ALL_TOOL_NAMES: &[&str] = &["read", "bash", "edit", "write", "grep", "find", "ls"];

/// All built-in tools: read, bash, edit, write, grep, find, ls (in that order).
pub fn builtin_tools(cwd: &Path) -> Vec<ToolRef> {
    ALL_TOOL_NAMES.iter().filter_map(|name| tool_by_name(cwd, name)).collect()
}

/// Look up a single built-in tool by name.
pub fn tool_by_name(cwd: &Path, name: &str) -> Option<ToolRef> {
    let cwd = cwd.to_path_buf();
    let tool: ToolRef = match name {
        "read" => Arc::new(ReadTool::new(cwd)),
        "bash" => Arc::new(BashTool::new(cwd)),
        "edit" => Arc::new(EditTool::new(cwd)),
        "write" => Arc::new(WriteTool::new(cwd)),
        "grep" => Arc::new(GrepTool::new(cwd)),
        "find" => Arc::new(FindTool::new(cwd)),
        "ls" => Arc::new(LsTool::new(cwd)),
        _ => return None,
    };
    Some(tool)
}

/// Read a JSON number argument as a non-negative integer.
///
/// Models frequently send `3.0` or `"3"`; accept both. Negative values clamp to 0.
pub(crate) fn arg_usize(args: &serde_json::Value, key: &str) -> Option<usize> {
    let value = args.get(key)?;
    let number = match value {
        serde_json::Value::Number(n) => n.as_f64()?,
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    if !number.is_finite() {
        return None;
    }
    Some(number.max(0.0) as usize)
}

/// Read a string argument, returning an error naming the missing parameter.
pub(crate) fn arg_str<'a>(args: &'a serde_json::Value, key: &str) -> anyhow::Result<&'a str> {
    args.get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("Missing required parameter: {key}"))
}

pub(crate) fn arg_bool(args: &serde_json::Value, key: &str) -> bool {
    match args.get(key) {
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => s.eq_ignore_ascii_case("true"),
        _ => false,
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use pi_agent::{ToolResult, UpdateFn};
    use std::sync::{Arc, Mutex};

    /// An `on_update` callback that records every partial result.
    pub fn recording_update() -> (UpdateFn, Arc<Mutex<Vec<ToolResult>>>) {
        let store: Arc<Mutex<Vec<ToolResult>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = store.clone();
        let f: UpdateFn = Arc::new(move |r| {
            if let Ok(mut guard) = sink.lock() {
                guard.push(r);
            }
        });
        (f, store)
    }

    pub fn noop_update() -> UpdateFn {
        Arc::new(|_| {})
    }

    /// Text of the first content block.
    pub fn first_text(result: &ToolResult) -> String {
        match result.content.first() {
            Some(pi_ai::Content::Text { text, .. }) => text.clone(),
            _ => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_tools_are_in_pi_order() {
        let tools = builtin_tools(Path::new("/"));
        let names: Vec<String> = tools.iter().map(|t| t.name()).collect();
        assert_eq!(names, ALL_TOOL_NAMES);
        for tool in &tools {
            let schema = tool.parameters();
            assert_eq!(schema["type"], "object", "{} schema must be an object", tool.name());
            assert!(!tool.description().is_empty());
        }
    }

    #[test]
    fn tool_by_name_rejects_unknown() {
        assert!(tool_by_name(Path::new("/"), "nope").is_none());
        assert_eq!(tool_by_name(Path::new("/"), "grep").map(|t| t.name()), Some("grep".to_string()));
    }

    #[test]
    fn default_tools_are_subset_of_all() {
        for name in DEFAULT_TOOL_NAMES {
            assert!(ALL_TOOL_NAMES.contains(name));
        }
    }

    #[test]
    fn arg_usize_accepts_numbers_and_strings() {
        let args = serde_json::json!({"a": 3, "b": 4.0, "c": "5", "d": -2, "e": "x"});
        assert_eq!(arg_usize(&args, "a"), Some(3));
        assert_eq!(arg_usize(&args, "b"), Some(4));
        assert_eq!(arg_usize(&args, "c"), Some(5));
        assert_eq!(arg_usize(&args, "d"), Some(0));
        assert_eq!(arg_usize(&args, "e"), None);
        assert_eq!(arg_usize(&args, "missing"), None);
    }
}
