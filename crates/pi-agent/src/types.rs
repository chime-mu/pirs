//! Agent-level types: message union, tools, events, hooks.

use async_trait::async_trait;
use pi_ai::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Extended messages (mirrors pi-coding-agent's message types)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CustomMessage {
    pub custom_type: String,
    pub content: UserContent,
    pub display: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default)]
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BashExecutionMessage {
    pub command: String,
    pub output: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub cancelled: bool,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_output_path: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exclude_from_context: bool,
    #[serde(default)]
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BranchSummaryMessage {
    pub summary: String,
    #[serde(default)]
    pub from_id: Option<String>,
    #[serde(default)]
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSummaryMessage {
    pub summary: String,
    #[serde(default)]
    pub tokens_before: u64,
    #[serde(default)]
    pub timestamp: u64,
}

/// Union of LLM messages and app-level messages, tagged by `role`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "role", rename_all = "camelCase")]
pub enum AgentMessage {
    System(SystemMessage),
    User(UserMessage),
    Assistant(AssistantMessage),
    ToolResult(ToolResultMessage),
    Custom(CustomMessage),
    BashExecution(BashExecutionMessage),
    BranchSummary(BranchSummaryMessage),
    CompactionSummary(CompactionSummaryMessage),
}

impl AgentMessage {
    pub fn role(&self) -> &'static str {
        match self {
            Self::System(_) => "system",
            Self::User(_) => "user",
            Self::Assistant(_) => "assistant",
            Self::ToolResult(_) => "toolResult",
            Self::Custom(_) => "custom",
            Self::BashExecution(_) => "bashExecution",
            Self::BranchSummary(_) => "branchSummary",
            Self::CompactionSummary(_) => "compactionSummary",
        }
    }
    pub fn user(text: impl Into<UserContent>) -> Self {
        Self::User(UserMessage { content: text.into(), timestamp: now_ms() })
    }
    pub fn timestamp(&self) -> u64 {
        match self {
            Self::System(m) => m.timestamp,
            Self::User(m) => m.timestamp,
            Self::Assistant(m) => m.timestamp,
            Self::ToolResult(m) => m.timestamp,
            Self::Custom(m) => m.timestamp,
            Self::BashExecution(m) => m.timestamp,
            Self::BranchSummary(m) => m.timestamp,
            Self::CompactionSummary(m) => m.timestamp,
        }
    }
    pub fn as_llm(&self) -> Option<Message> {
        match self {
            Self::System(m) => Some(Message::System(m.clone())),
            Self::User(m) => Some(Message::User(m.clone())),
            Self::Assistant(m) => Some(Message::Assistant(m.clone())),
            Self::ToolResult(m) => Some(Message::ToolResult(m.clone())),
            _ => None,
        }
    }
}

impl From<Message> for AgentMessage {
    fn from(m: Message) -> Self {
        match m {
            Message::System(m) => Self::System(m),
            Message::User(m) => Self::User(m),
            Message::Assistant(m) => Self::Assistant(m),
            Message::ToolResult(m) => Self::ToolResult(m),
        }
    }
}

/// Default conversion used when the app does not customize it.
pub fn default_convert_to_llm(messages: &[AgentMessage]) -> Vec<Message> {
    messages
        .iter()
        .filter_map(|m| match m {
            AgentMessage::Custom(c) => Some(Message::User(UserMessage { content: c.content.clone(), timestamp: c.timestamp })),
            AgentMessage::BashExecution(b) if !b.exclude_from_context => {
                let mut text = format!("$ {}\n{}", b.command, b.output);
                if let Some(code) = b.exit_code {
                    if code != 0 {
                        text.push_str(&format!("\n(exit code {code})"));
                    }
                }
                Some(Message::User(UserMessage { content: UserContent::Text(text), timestamp: b.timestamp }))
            }
            AgentMessage::BashExecution(_) => None,
            AgentMessage::BranchSummary(b) => Some(Message::User(UserMessage {
                content: UserContent::Text(format!("<branch_summary>\n{}\n</branch_summary>", b.summary)),
                timestamp: b.timestamp,
            })),
            AgentMessage::CompactionSummary(c) => Some(Message::User(UserMessage {
                content: UserContent::Text(format!("<compaction_summary>\n{}\n</compaction_summary>", c.summary)),
                timestamp: c.timestamp,
            })),
            other => other.as_llm(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ToolExecutionMode {
    Sequential,
    #[default]
    Parallel,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    #[serde(default)]
    pub content: Vec<Content>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub terminate: bool,
}

impl ToolResult {
    pub fn text(text: impl Into<String>) -> Self {
        ToolResult { content: vec![Content::text(text)], ..Default::default() }
    }
    pub fn error(text: impl Into<String>) -> Self {
        ToolResult { content: vec![Content::text(text)], details: Some(Value::Object(Default::default())), ..Default::default() }
    }
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

pub type UpdateFn = Arc<dyn Fn(ToolResult) + Send + Sync>;

/// A tool the model can call.
#[async_trait]
pub trait AgentTool: Send + Sync {
    fn name(&self) -> String;
    fn label(&self) -> String {
        self.name()
    }
    fn description(&self) -> String;
    /// JSON schema (object) for the arguments.
    fn parameters(&self) -> Value;
    fn execution_mode(&self) -> ToolExecutionMode {
        ToolExecutionMode::Parallel
    }
    fn prompt_snippet(&self) -> Option<String> {
        None
    }
    fn prompt_guidelines(&self) -> Vec<String> {
        Vec::new()
    }
    /// Compatibility shim applied to raw arguments before validation.
    fn prepare_arguments(&self, args: Value) -> Value {
        args
    }
    async fn execute(
        &self,
        tool_call_id: &str,
        args: Value,
        cancel: CancellationToken,
        on_update: UpdateFn,
    ) -> anyhow::Result<ToolResult>;

    fn declaration(&self) -> Tool {
        Tool { name: self.name(), description: self.description(), parameters: self.parameters() }
    }
}

pub type ToolRef = Arc<dyn AgentTool>;

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    AgentStart,
    AgentEnd { messages: Vec<AgentMessage> },
    TurnStart,
    #[serde(rename_all = "camelCase")]
    TurnEnd { message: AgentMessage, tool_results: Vec<ToolResultMessage> },
    MessageStart { message: AgentMessage },
    #[serde(rename_all = "camelCase")]
    MessageUpdate { message: AgentMessage, assistant_message_event: Box<AssistantMessageEvent> },
    MessageEnd { message: AgentMessage },
    #[serde(rename_all = "camelCase")]
    ToolExecutionStart { tool_call_id: String, tool_name: String, args: Value },
    #[serde(rename_all = "camelCase")]
    ToolExecutionUpdate { tool_call_id: String, tool_name: String, args: Value, partial_result: ToolResult },
    #[serde(rename_all = "camelCase")]
    ToolExecutionEnd { tool_call_id: String, tool_name: String, result: ToolResult, is_error: bool },
}

pub type EventSink = Arc<dyn Fn(AgentEvent) -> futures::future::BoxFuture<'static, ()> + Send + Sync>;

// ---------------------------------------------------------------------------
// Hooks
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct BeforeToolCallResult {
    pub block: bool,
    pub reason: Option<String>,
    pub terminate: bool,
    /// Replacement arguments (hooks may rewrite tool input).
    pub args: Option<Value>,
}

#[derive(Debug, Clone, Default)]
pub struct AfterToolCallResult {
    pub content: Option<Vec<Content>>,
    pub details: Option<Value>,
    pub is_error: Option<bool>,
    pub usage: Option<Usage>,
    pub terminate: Option<bool>,
}

pub struct BeforeToolCallContext<'a> {
    pub assistant_message: &'a AssistantMessage,
    pub tool_call: &'a ToolCallRef,
    pub args: &'a Value,
}

pub struct AfterToolCallContext<'a> {
    pub assistant_message: &'a AssistantMessage,
    pub tool_call: &'a ToolCallRef,
    pub args: &'a Value,
    pub result: &'a ToolResult,
    pub is_error: bool,
}

/// Replacement message handed back from `message_end` handlers.
#[async_trait]
pub trait AgentHooks: Send + Sync {
    async fn convert_to_llm(&self, messages: &[AgentMessage]) -> Vec<Message> {
        default_convert_to_llm(messages)
    }
    async fn transform_context(&self, messages: Vec<AgentMessage>, _cancel: &CancellationToken) -> Vec<AgentMessage> {
        messages
    }
    async fn before_tool_call(&self, _ctx: BeforeToolCallContext<'_>, _cancel: &CancellationToken) -> Option<BeforeToolCallResult> {
        None
    }
    async fn after_tool_call(&self, _ctx: AfterToolCallContext<'_>, _cancel: &CancellationToken) -> Option<AfterToolCallResult> {
        None
    }
    async fn get_steering_messages(&self) -> Vec<AgentMessage> {
        Vec::new()
    }
    async fn get_follow_up_messages(&self) -> Vec<AgentMessage> {
        Vec::new()
    }
    async fn get_api_key(&self, _provider: &str) -> Option<String> {
        None
    }
    /// Extra per-request stream options (headers, payload hooks).
    async fn stream_options(&self) -> StreamOptions {
        StreamOptions::default()
    }
    async fn should_stop_after_turn(&self, _message: &AssistantMessage) -> bool {
        false
    }
    /// Called before every LLM request; return `Some` to replace the tool set
    /// for the rest of the run (tools registered mid-run become callable).
    async fn refresh_tools(&self) -> Option<Vec<ToolRef>> {
        None
    }
    /// Called before every LLM request, after `refresh_tools`; return `Some`
    /// to replace the system prompt for the rest of the run (a policy file
    /// the agent itself wrote takes effect on its next request).
    async fn refresh_system_prompt(&self) -> Option<String> {
        None
    }
}

pub struct NoHooks;
#[async_trait]
impl AgentHooks for NoHooks {}

#[derive(Clone)]
pub struct AgentLoopConfig {
    pub model: Model,
    pub thinking_level: ThinkingLevel,
    pub tool_execution: ToolExecutionMode,
    pub max_tokens: Option<u64>,
    pub temperature: Option<f64>,
}
