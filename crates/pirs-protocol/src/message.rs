//! The protocol's own conversation message type (D-08).
//!
//! These types serialise to exactly the JSON of the session log
//! (`docs/session-format.md`): `role`-tagged messages, `type`-tagged content
//! blocks, `camelCase` fields. The server converts its internal message type to
//! this one at the edge; no conversion code lives here, so this crate never
//! depends on the provider layer.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::request::ToolInfo;

/// One block of message content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Content {
    /// Plain text. In an assistant message this is what the model said; in a
    /// user or tool-result message, what it reads.
    #[serde(rename_all = "camelCase")]
    Text {
        /// The text.
        text: String,
        /// Provider signature over the text, when the provider requires it to
        /// be sent back verbatim.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text_signature: Option<String>,
    },
    /// An image, base64-encoded.
    #[serde(rename_all = "camelCase")]
    Image {
        /// Base64 image data.
        data: String,
        /// MIME type, e.g. `image/png`.
        mime_type: String,
    },
    /// Model reasoning, when the model exposes it.
    #[serde(rename_all = "camelCase")]
    Thinking {
        /// The reasoning text (empty when `redacted`).
        thinking: String,
        /// Provider signature over the reasoning.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thinking_signature: Option<String>,
        /// True when the provider withheld the reasoning text.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        redacted: bool,
    },
    /// The model asking for a tool to run.
    #[serde(rename_all = "camelCase")]
    ToolCall {
        /// Provider-assigned call id; the tool result answers it by
        /// `toolCallId`.
        id: String,
        /// The tool's name in the loop's manifest.
        name: String,
        /// The arguments, a JSON object matching the tool's `parameters`.
        arguments: Value,
        /// Provider signature over the model's thinking that led to this call.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
}

impl Content {
    /// A text block without a signature.
    pub fn text(text: impl Into<String>) -> Self {
        Content::Text {
            text: text.into(),
            text_signature: None,
        }
    }

    /// The text of a text block.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Content::Text { text, .. } => Some(text),
            _ => None,
        }
    }
}

/// `string | Content[]`: the content of system and user messages, which the
/// session format allows as a bare string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum UserContent {
    /// A bare string, equivalent to one text block.
    Text(String),
    /// A list of blocks (text and images).
    Blocks(Vec<Content>),
}

impl UserContent {
    /// The text of the content, blocks joined with newlines.
    pub fn plain_text(&self) -> String {
        match self {
            UserContent::Text(s) => s.clone(),
            UserContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(Content::as_text)
                .collect::<Vec<_>>()
                .join("\n"),
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
        UserContent::Text(s.to_owned())
    }
}

/// Cost of a message in the provider's currency, by token class.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Cost {
    /// Cost of input tokens.
    pub input: f64,
    /// Cost of output tokens.
    pub output: f64,
    /// Cost of tokens read from the prompt cache.
    pub cache_read: f64,
    /// Cost of tokens written to the prompt cache.
    pub cache_write: f64,
    /// Sum of the above.
    pub total: f64,
}

/// Token usage of a message.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    /// Input tokens.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Tokens read from the prompt cache.
    pub cache_read: u64,
    /// Tokens written to the prompt cache.
    pub cache_write: u64,
    /// Reasoning tokens, when the provider reports them separately.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    /// Sum of input, output, cache read and cache write.
    pub total_tokens: u64,
    /// Cost breakdown.
    pub cost: Cost,
}

/// Why the model stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum StopReason {
    /// Still streaming; never appears in a complete message.
    Pending,
    /// The model finished its turn.
    Stop,
    /// The output token limit was reached.
    Length,
    /// The model called one or more tools; the loop runs them and continues.
    ToolUse,
    /// The provider returned an error; see `errorMessage`.
    Error,
    /// The turn was aborted (`loop.abort`).
    Aborted,
}

/// The system prompt, as recorded in the session log.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SystemMessage {
    /// The prompt text (empty when `sections` carries it).
    pub content: UserContent,
    /// Named prompt sections; `null` removes a section set by an earlier
    /// system message. Replaying system messages in order yields the current
    /// prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sections: Option<BTreeMap<String, Option<String>>>,
    /// Tool declarations added to the loadout by this message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_added: Option<Vec<ToolInfo>>,
    /// Unix milliseconds.
    #[serde(default)]
    pub timestamp: u64,
}

/// What the user (or a rewriting `input` handler) said.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UserMessage {
    /// Text, or text and image blocks.
    pub content: UserContent,
    /// Unix milliseconds.
    #[serde(default)]
    pub timestamp: u64,
}

/// What the model said, with the provider bookkeeping the session format keeps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    /// Text, thinking and tool-call blocks, in order.
    pub content: Vec<Content>,
    /// The provider API used, e.g. `anthropic-messages`, `openai-completions`, `faux`.
    pub api: String,
    /// The provider name, e.g. `anthropic`.
    pub provider: String,
    /// The model id, e.g. `claude-sonnet-4-5`.
    pub model: String,
    /// The provider's response id, when it gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    /// Token usage and cost.
    #[serde(default)]
    pub usage: Usage,
    /// Why the model stopped.
    pub stop_reason: StopReason,
    /// Error text when `stopReason` is `error` or `aborted`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// The provider's own stop reason string, unmapped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_stop_reason: Option<String>,
    /// Unix milliseconds.
    #[serde(default)]
    pub timestamp: u64,
}

impl AssistantMessage {
    /// All text blocks joined.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(Content::as_text)
            .collect::<Vec<_>>()
            .join("")
    }
}

/// The result of a tool call, as the model reads it. When a `tool_result`
/// handler rewrote it, this is the rewritten one; the session log holds the
/// original beside it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultMessage {
    /// The `id` of the tool-call block being answered.
    pub tool_call_id: String,
    /// The tool's name.
    pub tool_name: String,
    /// Text and image blocks the model reads.
    pub content: Vec<Content>,
    /// Tool-specific metadata not sent to the model (shown by UIs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    /// LLM usage the tool itself incurred (e.g. a `loop` tool).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Whether the tool failed; the content is then the error text.
    pub is_error: bool,
    /// Unix milliseconds.
    #[serde(default)]
    pub timestamp: u64,
}

/// A conversation message, tagged by `role`. This is the element type of
/// `loop.turn_end { messages }`, `loop.run_end { messages }` and
/// `loop.message { message }`, and the `message` member of a session-log entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "role", rename_all = "camelCase")]
pub enum Message {
    /// `role: "system"`.
    System(SystemMessage),
    /// `role: "user"`.
    User(UserMessage),
    /// `role: "assistant"`.
    Assistant(AssistantMessage),
    /// `role: "toolResult"`.
    ToolResult(ToolResultMessage),
}

impl Message {
    /// The message's role.
    pub fn role(&self) -> Role {
        match self {
            Message::System(_) => Role::System,
            Message::User(_) => Role::User,
            Message::Assistant(_) => Role::Assistant,
            Message::ToolResult(_) => Role::ToolResult,
        }
    }
}

/// A message role, as the `role` tag spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    /// The system prompt.
    System,
    /// The user.
    User,
    /// The model.
    Assistant,
    /// A tool answering a tool call.
    ToolResult,
}
