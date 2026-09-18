//! Core message, content, model, and streaming types.
//!
//! The JSON shapes mirror pi's `@earendil-works/pi-ai` types so that session
//! files written by pi can be read by pirs and vice versa.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Content blocks
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Content {
    #[serde(rename_all = "camelCase")]
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        text_signature: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Image { data: String, mime_type: String },
    #[serde(rename_all = "camelCase")]
    Thinking {
        thinking: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        thinking_signature: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        redacted: bool,
    },
    #[serde(rename_all = "camelCase")]
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
}

impl Content {
    pub fn text(text: impl Into<String>) -> Self {
        Content::Text { text: text.into(), text_signature: None }
    }
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Content::Text { text, .. } => Some(text),
            _ => None,
        }
    }
    pub fn is_tool_call(&self) -> bool {
        matches!(self, Content::ToolCall { .. })
    }
}

/// `string | Content[]` as used by user and custom messages.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum UserContent {
    Text(String),
    Blocks(Vec<Content>),
}

impl UserContent {
    pub fn plain_text(&self) -> String {
        match self {
            UserContent::Text(s) => s.clone(),
            UserContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| b.as_text())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
    pub fn blocks(&self) -> Vec<Content> {
        match self {
            UserContent::Text(s) => vec![Content::text(s.clone())],
            UserContent::Blocks(b) => b.clone(),
        }
    }
}

impl From<String> for UserContent {
    fn from(s: String) -> Self {
        UserContent::Text(s)
    }
}
impl From<&str> for UserContent {
    fn from(s: &str) -> Self {
        UserContent::Text(s.to_string())
    }
}

// ---------------------------------------------------------------------------
// Usage / cost
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Cost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub total: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    pub total_tokens: u64,
    pub cost: Cost,
}

impl Usage {
    pub fn compute_cost(&mut self, cost: &ModelCost) {
        let per = |tokens: u64, rate: f64| tokens as f64 * rate / 1_000_000.0;
        self.cost.input = per(self.input, cost.input);
        self.cost.output = per(self.output, cost.output);
        self.cost.cache_read = per(self.cache_read, cost.cache_read);
        self.cost.cache_write = per(self.cache_write, cost.cache_write);
        self.cost.total = self.cost.input + self.cost.output + self.cost.cache_read + self.cost.cache_write;
        self.total_tokens = self.input + self.output + self.cache_read + self.cache_write;
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum StopReason {
    Pending,
    Stop,
    Length,
    ToolUse,
    Error,
    Aborted,
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SystemMessage {
    pub content: UserContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sections: Option<HashMap<String, Option<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_added: Option<Vec<Tool>>,
    #[serde(default)]
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UserMessage {
    pub content: UserContent,
    #[serde(default)]
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    pub content: Vec<Content>,
    pub api: String,
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(default)]
    pub usage: Usage,
    pub stop_reason: StopReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_stop_reason: Option<String>,
    #[serde(default)]
    pub timestamp: u64,
}

impl AssistantMessage {
    pub fn new(model: &Model) -> Self {
        AssistantMessage {
            content: Vec::new(),
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            response_id: None,
            usage: Usage::default(),
            stop_reason: StopReason::Pending,
            error_message: None,
            raw_stop_reason: None,
            timestamp: now_ms(),
        }
    }

    pub fn error(model: &Model, message: impl Into<String>, aborted: bool) -> Self {
        let mut m = Self::new(model);
        m.stop_reason = if aborted { StopReason::Aborted } else { StopReason::Error };
        m.error_message = Some(message.into());
        m
    }

    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|c| c.as_text())
            .collect::<Vec<_>>()
            .join("")
    }

    pub fn tool_calls(&self) -> Vec<ToolCallRef> {
        self.content
            .iter()
            .filter_map(|c| match c {
                Content::ToolCall { id, name, arguments, .. } => Some(ToolCallRef {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                }),
                _ => None,
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCallRef {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultMessage {
    pub tool_call_id: String,
    pub tool_name: String,
    pub content: Vec<Content>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    pub is_error: bool,
    #[serde(default)]
    pub timestamp: u64,
}

/// The messages an LLM provider understands.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "role", rename_all = "camelCase")]
pub enum Message {
    System(SystemMessage),
    User(UserMessage),
    Assistant(AssistantMessage),
    ToolResult(ToolResultMessage),
}

impl Message {
    pub fn user(text: impl Into<UserContent>) -> Self {
        Message::User(UserMessage { content: text.into(), timestamp: now_ms() })
    }
    pub fn role(&self) -> &'static str {
        match self {
            Message::System(_) => "system",
            Message::User(_) => "user",
            Message::Assistant(_) => "assistant",
            Message::ToolResult(_) => "toolResult",
        }
    }
}

// ---------------------------------------------------------------------------
// Tools, models, context
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tool {
    pub name: String,
    pub description: String,
    /// JSON schema (object) for the tool arguments.
    pub parameters: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ModelCost {
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default)]
    pub cache_read: f64,
    #[serde(default)]
    pub cache_write: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub name: String,
    /// `anthropic-messages` | `openai-completions` | `faux`
    pub api: String,
    pub provider: String,
    pub base_url: String,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default = "default_input")]
    pub input: Vec<String>,
    #[serde(default)]
    pub cost: ModelCost,
    #[serde(default = "default_context_window")]
    pub context_window: u64,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
}

fn default_input() -> Vec<String> {
    vec!["text".into()]
}
fn default_context_window() -> u64 {
    128_000
}
fn default_max_tokens() -> u64 {
    8192
}

impl Model {
    pub fn supports_images(&self) -> bool {
        self.input.iter().any(|i| i == "image")
    }
    pub fn key(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    #[default]
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ThinkingLevel {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "off" => Self::Off,
            "minimal" => Self::Minimal,
            "low" => Self::Low,
            "medium" => Self::Medium,
            "high" => Self::High,
            "xhigh" => Self::Xhigh,
            "max" => Self::Max,
            _ => return None,
        })
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
    /// Token budget used by budget-based thinking APIs (Anthropic).
    pub fn budget_tokens(&self) -> Option<u64> {
        match self {
            Self::Off => None,
            Self::Minimal => Some(1024),
            Self::Low => Some(4096),
            Self::Medium => Some(10_240),
            Self::High => Some(32_768),
            Self::Xhigh | Self::Max => Some(65_536),
        }
    }
    /// Effort string used by OpenAI-style `reasoning_effort`.
    pub fn effort(&self) -> Option<&'static str> {
        match self {
            Self::Off => None,
            Self::Minimal => Some("minimal"),
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
            Self::Xhigh | Self::Max => Some("high"),
        }
    }
}

/// Request context: system prompt, transcript, and tool declarations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub tools: Vec<Tool>,
}

#[derive(Clone, Default)]
pub struct StreamOptions {
    pub api_key: Option<String>,
    pub reasoning: ThinkingLevel,
    pub max_tokens: Option<u64>,
    pub temperature: Option<f64>,
    pub headers: HashMap<String, String>,
    pub cancel: Option<tokio_util::sync::CancellationToken>,
    /// Hook to inspect or replace the provider payload before sending.
    pub on_payload: Option<PayloadHook>,
    /// Hook receiving the response status and headers before the body is consumed.
    pub on_response: Option<ResponseHook>,
}

pub type PayloadHook = std::sync::Arc<
    dyn Fn(Value) -> futures::future::BoxFuture<'static, Option<Value>> + Send + Sync,
>;
pub type ResponseHook = std::sync::Arc<
    dyn Fn(u16, HashMap<String, String>) -> futures::future::BoxFuture<'static, ()> + Send + Sync,
>;

// ---------------------------------------------------------------------------
// Streaming events
// ---------------------------------------------------------------------------

/// Event protocol for an assistant response stream. `partial` is the
/// response-so-far; `Done` / `Error` carry the final message.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantMessageEvent {
    Start { partial: AssistantMessage },
    #[serde(rename_all = "camelCase")]
    TextStart { content_index: usize, partial: AssistantMessage },
    #[serde(rename_all = "camelCase")]
    TextDelta { content_index: usize, delta: String, partial: AssistantMessage },
    #[serde(rename_all = "camelCase")]
    TextEnd { content_index: usize, content: String, partial: AssistantMessage },
    #[serde(rename_all = "camelCase")]
    ThinkingStart { content_index: usize, partial: AssistantMessage },
    #[serde(rename_all = "camelCase")]
    ThinkingDelta { content_index: usize, delta: String, partial: AssistantMessage },
    #[serde(rename_all = "camelCase")]
    ThinkingEnd { content_index: usize, content: String, partial: AssistantMessage },
    #[serde(rename = "toolcall_start", rename_all = "camelCase")]
    ToolCallStart { content_index: usize, partial: AssistantMessage },
    #[serde(rename = "toolcall_delta", rename_all = "camelCase")]
    ToolCallDelta { content_index: usize, delta: String, partial: AssistantMessage },
    #[serde(rename = "toolcall_end", rename_all = "camelCase")]
    ToolCallEnd { content_index: usize, tool_call: ToolCallRef, partial: AssistantMessage },
    Done { reason: StopReason, message: AssistantMessage },
    Error { reason: StopReason, error: AssistantMessage },
}

impl AssistantMessageEvent {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Done { .. } | Self::Error { .. })
    }
    pub fn final_message(&self) -> Option<&AssistantMessage> {
        match self {
            Self::Done { message, .. } => Some(message),
            Self::Error { error, .. } => Some(error),
            _ => None,
        }
    }
}

pub type EventStream = futures::stream::BoxStream<'static, AssistantMessageEvent>;
