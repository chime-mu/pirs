//! Data types shared between the host thread and the application.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    pub name: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub parameters: Value,
    #[serde(default)]
    pub prompt_snippet: Option<String>,
    #[serde(default)]
    pub prompt_guidelines: Option<Vec<String>>,
    #[serde(default)]
    pub execution_mode: Option<String>,
    #[serde(default)]
    pub has_render_call: bool,
    #[serde(default)]
    pub has_render_result: bool,
    #[serde(default)]
    pub extension_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CommandInfo {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub has_completions: bool,
    #[serde(default)]
    pub extension_path: String,
    /// `name` or `name:N` when several extensions register the same command.
    #[serde(default)]
    pub invocation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutInfo {
    pub shortcut: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FlagInfo {
    pub name: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub default: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LoadedExtension {
    pub path: String,
    #[serde(default)]
    pub tools: Vec<ToolInfo>,
    #[serde(default)]
    pub commands: Vec<CommandInfo>,
    #[serde(default)]
    pub shortcuts: Vec<ShortcutInfo>,
    #[serde(default)]
    pub flags: Vec<FlagInfo>,
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub message_renderers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionError {
    pub extension_path: String,
    pub event: String,
    pub error: String,
    #[serde(default)]
    pub stack: Option<String>,
}

/// Result of dispatching an event: the (possibly mutated) event and the
/// aggregated handler result according to pi's per-event semantics.
#[derive(Debug, Clone, Deserialize)]
pub struct DispatchOutcome {
    pub result: Value,
    pub event: Value,
}

impl DispatchOutcome {
    pub fn empty(event: Value) -> Self {
        DispatchOutcome { result: Value::Null, event }
    }
}

/// Snapshot of app state exposed to extensions as `ctx`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ContextInfo {
    pub cwd: String,
    /// "tui" | "rpc" | "json" | "print"
    pub mode: String,
    #[serde(rename = "hasUI")]
    pub has_ui: bool,
    #[serde(default)]
    pub model: Option<Value>,
    #[serde(default)]
    pub thinking_level: String,
    pub is_idle: bool,
    #[serde(default)]
    pub session_file: Option<String>,
    #[serde(default)]
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
    pub killed: bool,
}
