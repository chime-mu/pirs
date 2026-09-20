//! Handler slots (server → handlers): requests the server sends and waits for.
//!
//! Five slot forms exist: `input`, `prompt`, `tool_result`, `tool.<name>` and
//! `on.<event>`. A *connected* handler registers one with `register` and
//! receives each firing as a JSON-RPC message whose `method` is the slot and
//! whose `params` is the payload; it answers with a response whose `result` is
//! the reply. `on.<event>` is fire and forget, so it arrives as a
//! notification, with no `id` and no reply. A *called* process (a DSL `run =`) receives exactly the same
//! payload as one JSON line on stdin and writes the reply as one JSON line on
//! stdout, then exits (D-23). The shapes are identical, so a handler moves
//! between the two bindings without being rewritten.
//!
//! A handler that does not reply within its timeout, or that exits non-zero,
//! counts as "no opinion": the slot's default outcome applies and a `ui.notify`
//! warning is emitted.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::str::FromStr;

use crate::envelope::{Envelope, RpcNotification, RpcRequest};
use crate::{Content, Id, JsonRpcVersion, LoopRunEndEvent, LoopTurnEndEvent, ServerPath};

/// The events an `on.<event>` slot can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OnEvent {
    /// `on.start`: the loop was created (or a policy entry is new after a
    /// reload). Payload: [`OnStartPayload`]. Fire and forget, so a handler
    /// started here may connect and register (D-16).
    Start,
    /// `on.turn_end`: the model stopped once. Payload: [`LoopTurnEndEvent`].
    TurnEnd,
    /// `on.run_end`: the loop went idle. Payload: [`LoopRunEndEvent`].
    RunEnd,
    /// `on.tool_result`: a tool produced a result (after any `tool_result`
    /// rewrite). Payload: [`OnToolResultPayload`].
    ToolResult,
    /// `on.reload`: the loop's policy files were re-read. Payload:
    /// [`OnReloadPayload`].
    Reload,
}

impl OnEvent {
    /// Every event name, in table order.
    pub const NAMES: [&'static str; 5] = ["start", "turn_end", "run_end", "tool_result", "reload"];

    /// The name as it appears after `on.`.
    pub fn as_str(self) -> &'static str {
        match self {
            OnEvent::Start => "start",
            OnEvent::TurnEnd => "turn_end",
            OnEvent::RunEnd => "run_end",
            OnEvent::ToolResult => "tool_result",
            OnEvent::Reload => "reload",
        }
    }
}

impl FromStr for OnEvent {
    type Err = SlotError;
    fn from_str(s: &str) -> Result<Self, SlotError> {
        Ok(match s {
            "start" => OnEvent::Start,
            "turn_end" => OnEvent::TurnEnd,
            "run_end" => OnEvent::RunEnd,
            "tool_result" => OnEvent::ToolResult,
            "reload" => OnEvent::Reload,
            _ => return Err(SlotError::UnknownEvent(s.to_owned())),
        })
    }
}

impl fmt::Display for OnEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a string is not a slot.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SlotError {
    /// Not one of `input`, `prompt`, `tool_result`, `tool.<name>`, `on.<event>`.
    #[error("unknown slot {0:?}: expected input, prompt, tool_result, tool.<name> or on.<event>")]
    UnknownSlot(String),
    /// `on.<event>` with an event that is not `start`, `turn_end`, `run_end`,
    /// `tool_result` or `reload`.
    #[error("unknown on-event {0:?}: expected start, turn_end, run_end, tool_result or reload")]
    UnknownEvent(String),
    /// `tool.` with an empty or whitespace-containing name.
    #[error("invalid tool name {0:?}")]
    InvalidToolName(String),
}

/// A handler slot. Serialises as its string form: `input`, `prompt`,
/// `tool_result`, `tool.<name>` or `on.<event>`. That string is also the
/// `method` of the request the server sends to the handler.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Slot {
    /// Runs on every prompt before the model sees it. Payload:
    /// [`InputPayload`]; reply: [`InputReply`]. Default outcome: the text
    /// unchanged.
    Input,
    /// Runs when the system prompt is assembled. Payload: [`PromptPayload`];
    /// reply: [`PromptReply`]. Default outcome: the prompt unchanged.
    Prompt,
    /// Runs on every tool result before the model reads it. Payload:
    /// [`ToolResultPayload`]; reply: [`ToolResultReply`]. Default outcome:
    /// the result unchanged. Both versions go to the session log.
    ToolResult,
    /// Implements the tool `<name>` for the model's tool calls. Payload:
    /// [`ToolCallPayload`]; reply: [`ToolReply`]. Default outcome on timeout:
    /// an error result.
    Tool(String),
    /// Observes a loop event. Payload: that event's; no reply.
    On(OnEvent),
}

impl Slot {
    /// The tool name of a `tool.<name>` slot.
    pub fn tool_name(&self) -> Option<&str> {
        match self {
            Slot::Tool(name) => Some(name),
            _ => None,
        }
    }
}

impl FromStr for Slot {
    type Err = SlotError;
    fn from_str(s: &str) -> Result<Self, SlotError> {
        match s {
            "input" => Ok(Slot::Input),
            "prompt" => Ok(Slot::Prompt),
            "tool_result" => Ok(Slot::ToolResult),
            _ => {
                if let Some(name) = s.strip_prefix("tool.") {
                    if name.is_empty() || name.chars().any(char::is_whitespace) {
                        return Err(SlotError::InvalidToolName(name.to_owned()));
                    }
                    Ok(Slot::Tool(name.to_owned()))
                } else if let Some(event) = s.strip_prefix("on.") {
                    Ok(Slot::On(event.parse()?))
                } else {
                    Err(SlotError::UnknownSlot(s.to_owned()))
                }
            }
        }
    }
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Slot::Input => f.write_str("input"),
            Slot::Prompt => f.write_str("prompt"),
            Slot::ToolResult => f.write_str("tool_result"),
            Slot::Tool(name) => write!(f, "tool.{name}"),
            Slot::On(event) => write!(f, "on.{event}"),
        }
    }
}

impl Serialize for Slot {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Slot {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Slot {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Slot".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "pattern": "^(input|prompt|tool_result|tool\\.\\S+|on\\.(start|turn_end|run_end|tool_result|reload))$",
            "description": "A handler slot: `input`, `prompt`, `tool_result`, `tool.<name>` or `on.<event>` where event is start, turn_end, run_end, tool_result or reload. Also the `method` of the request the server sends the handler."
        })
    }
}

/// The JSON literal `true`. Deserialising `false` fails.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct True;

impl Serialize for True {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bool(true)
    }
}

impl<'de> Deserialize<'de> for True {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if bool::deserialize(deserializer)? {
            Ok(True)
        } else {
            Err(serde::de::Error::custom("expected true"))
        }
    }
}

impl JsonSchema for True {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "True".into()
    }
    fn inline_schema() -> bool {
        true
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "boolean", "const": true })
    }
}

/// `input` payload: a prompt on its way to the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InputPayload {
    /// The text the user (or a client) sent, after any earlier `input`
    /// handler's rewrite.
    pub text: String,
}

/// `input` reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum InputReply {
    /// The text to hand on (unchanged, rewritten, or expanded from a slash
    /// command).
    Text {
        /// The text the model will see.
        text: String,
    },
    /// The input was consumed; nothing reaches the model and no turn starts.
    Handled {
        /// Always `true`.
        handled: True,
    },
}

/// `prompt` payload: the system prompt as assembled so far.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PromptPayload {
    /// The full system prompt text.
    pub system_prompt: String,
}

/// `prompt` reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum PromptReply {
    /// Append this text to the prompt.
    Append {
        /// Text appended after the current prompt.
        append: String,
    },
    /// Replace the whole prompt.
    Replace {
        /// The new prompt.
        replace: String,
    },
}

/// Tool output content: a bare string (one text block) or a list of text and
/// image blocks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ToolContent {
    /// A string, equivalent to one text block.
    Text(String),
    /// Text and image blocks.
    Blocks(Vec<Content>),
}

impl From<String> for ToolContent {
    fn from(s: String) -> Self {
        ToolContent::Text(s)
    }
}

impl From<&str> for ToolContent {
    fn from(s: &str) -> Self {
        ToolContent::Text(s.to_owned())
    }
}

/// What a tool produced: the `tool.<name>` reply, and the `result` a
/// `tool_result` handler sees and returns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ToolReply {
    /// The tool succeeded.
    Ok {
        /// What the model reads.
        content: ToolContent,
        /// Metadata for UIs, not sent to the model.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
    },
    /// The tool failed; the model reads `error` as an error result.
    Error {
        /// The error text.
        error: String,
    },
}

/// `tool_result` payload: a result on its way to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolResultPayload {
    /// The tool's name.
    pub tool: String,
    /// The arguments the model called it with.
    pub args: Value,
    /// What the tool produced.
    pub result: ToolReply,
}

/// `tool_result` reply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolResultReply {
    /// The result the model reads instead. The original stays in the session
    /// log beside it.
    pub result: ToolReply,
}

/// `tool.<name>` payload: the model called the tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolCallPayload {
    /// The arguments, matching the tool's `parameters` schema.
    pub args: Value,
    /// The tool-call id, for correlating with the session log.
    pub id: String,
}

/// `on.start` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OnStartPayload {
    /// The loop that started.
    #[serde(rename = "loop")]
    pub loop_id: String,
    /// Its working directory.
    pub cwd: ServerPath,
}

/// `on.tool_result` payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OnToolResultPayload {
    /// The loop.
    #[serde(rename = "loop")]
    pub loop_id: String,
    /// The tool's name.
    pub tool: String,
    /// The arguments the model called it with.
    pub args: Value,
    /// The result as the model reads it (after any `tool_result` rewrite).
    pub result: ToolReply,
}

/// `on.reload` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OnReloadPayload {
    /// The loop.
    #[serde(rename = "loop")]
    pub loop_id: String,
    /// The policy files now loaded, in load order.
    pub files: Vec<ServerPath>,
}

/// The payload of an `on.<event>` request; the variant is the event.
#[derive(Debug, Clone, PartialEq)]
pub enum OnPayload {
    /// `on.start`.
    Start(OnStartPayload),
    /// `on.turn_end`.
    TurnEnd(LoopTurnEndEvent),
    /// `on.run_end`.
    RunEnd(LoopRunEndEvent),
    /// `on.tool_result`.
    ToolResult(OnToolResultPayload),
    /// `on.reload`.
    Reload(OnReloadPayload),
}

impl OnPayload {
    /// Which event this is.
    pub fn event(&self) -> OnEvent {
        match self {
            OnPayload::Start(_) => OnEvent::Start,
            OnPayload::TurnEnd(_) => OnEvent::TurnEnd,
            OnPayload::RunEnd(_) => OnEvent::RunEnd,
            OnPayload::ToolResult(_) => OnEvent::ToolResult,
            OnPayload::Reload(_) => OnEvent::Reload,
        }
    }

    fn to_value(&self) -> Value {
        fn v<T: Serialize>(t: &T) -> Value {
            serde_json::to_value(t).expect("slot payloads serialise")
        }
        match self {
            OnPayload::Start(p) => v(p),
            OnPayload::TurnEnd(p) => v(p),
            OnPayload::RunEnd(p) => v(p),
            OnPayload::ToolResult(p) => v(p),
            OnPayload::Reload(p) => v(p),
        }
    }

    fn from_value(event: OnEvent, params: Value) -> Result<Self, serde_json::Error> {
        use serde_json::from_value as f;
        Ok(match event {
            OnEvent::Start => OnPayload::Start(f(params)?),
            OnEvent::TurnEnd => OnPayload::TurnEnd(f(params)?),
            OnEvent::RunEnd => OnPayload::RunEnd(f(params)?),
            OnEvent::ToolResult => OnPayload::ToolResult(f(params)?),
            OnEvent::Reload => OnPayload::Reload(f(params)?),
        })
    }
}

/// A request from the server to a handler: a slot and its payload.
///
/// Serialises as `{ "method": "<slot>", "params": { … } }`, the two members of
/// an [`RpcRequest`]; [`SlotRequest::into_rpc`] adds the envelope — a request
/// for the slots that expect a reply, a notification for `on.<event>`. A
/// called process receives only `params` on stdin, the same either way.
#[derive(Debug, Clone, PartialEq)]
pub enum SlotRequest {
    /// `input`.
    Input(InputPayload),
    /// `prompt`.
    Prompt(PromptPayload),
    /// `tool_result`.
    ToolResult(ToolResultPayload),
    /// `tool.<name>`.
    Tool {
        /// The tool's name.
        name: String,
        /// The call.
        payload: ToolCallPayload,
    },
    /// `on.<event>`.
    On(OnPayload),
}

/// The reply a handler sends for a [`SlotRequest`]. Not a wire type by itself:
/// a response carries only the untagged `result`, and the request it answers
/// determines the variant ([`SlotRequest::parse_reply`]).
#[derive(Debug, Clone, PartialEq)]
pub enum SlotReply {
    /// Reply to `input`.
    Input(InputReply),
    /// Reply to `prompt`.
    Prompt(PromptReply),
    /// Reply to `tool_result`.
    ToolResult(ToolResultReply),
    /// Reply to `tool.<name>`.
    Tool(ToolReply),
    /// `on.<event>` takes no reply.
    None,
}

impl SlotReply {
    /// The reply as the JSON value a response carries in `result`; `None` for
    /// `on.<event>`, which is not answered.
    pub fn to_value(&self) -> Option<Value> {
        fn v<T: Serialize>(t: &T) -> Value {
            serde_json::to_value(t).expect("slot replies serialise")
        }
        match self {
            SlotReply::Input(r) => Some(v(r)),
            SlotReply::Prompt(r) => Some(v(r)),
            SlotReply::ToolResult(r) => Some(v(r)),
            SlotReply::Tool(r) => Some(v(r)),
            SlotReply::None => None,
        }
    }
}

impl SlotRequest {
    /// The slot this request is for.
    pub fn slot(&self) -> Slot {
        match self {
            SlotRequest::Input(_) => Slot::Input,
            SlotRequest::Prompt(_) => Slot::Prompt,
            SlotRequest::ToolResult(_) => Slot::ToolResult,
            SlotRequest::Tool { name, .. } => Slot::Tool(name.clone()),
            SlotRequest::On(p) => Slot::On(p.event()),
        }
    }

    /// The payload as a JSON value: what a called process reads on stdin.
    pub fn params(&self) -> Value {
        fn v<T: Serialize>(t: &T) -> Value {
            serde_json::to_value(t).expect("slot payloads serialise")
        }
        match self {
            SlotRequest::Input(p) => v(p),
            SlotRequest::Prompt(p) => v(p),
            SlotRequest::ToolResult(p) => v(p),
            SlotRequest::Tool { payload, .. } => v(payload),
            SlotRequest::On(p) => p.to_value(),
        }
    }

    /// Build from a slot and its payload value.
    pub fn from_parts(slot: Slot, params: Value) -> Result<Self, serde_json::Error> {
        use serde_json::from_value as f;
        Ok(match slot {
            Slot::Input => SlotRequest::Input(f(params)?),
            Slot::Prompt => SlotRequest::Prompt(f(params)?),
            Slot::ToolResult => SlotRequest::ToolResult(f(params)?),
            Slot::Tool(name) => SlotRequest::Tool {
                name,
                payload: f(params)?,
            },
            Slot::On(event) => SlotRequest::On(OnPayload::from_value(event, params)?),
        })
    }

    /// Wrap in a JSON-RPC envelope: a request with `id` for the slots that
    /// expect a reply, a notification for `on.<event>`, which is fire and
    /// forget. `id` is ignored in the notification case. The `params` are the
    /// same either way (D-23).
    pub fn into_rpc(self, id: impl Into<Id>) -> Envelope {
        let method = self.slot().to_string();
        let expects_reply = self.expects_reply();
        let params = self.params();
        if expects_reply {
            Envelope::Request(RpcRequest {
                jsonrpc: JsonRpcVersion,
                id: id.into(),
                method,
                params,
            })
        } else {
            Envelope::Notification(RpcNotification {
                jsonrpc: JsonRpcVersion,
                method,
                params,
            })
        }
    }

    /// Recover the typed slot request from an envelope, request or
    /// notification alike. A method that is not a slot is an error naming it,
    /// and a response is not a slot request at all.
    pub fn from_rpc(env: &Envelope) -> Result<Self, serde_json::Error> {
        let (method, params) = match env {
            Envelope::Request(r) => (&r.method, &r.params),
            Envelope::Notification(n) => (&n.method, &n.params),
            Envelope::Response(_) => {
                return Err(serde::de::Error::custom("a response is not a slot request"))
            }
        };
        let slot: Slot = method.parse().map_err(serde::de::Error::custom)?;
        SlotRequest::from_parts(slot, params.clone())
    }

    /// Whether a reply is expected (`on.<event>` is fire and forget).
    pub fn expects_reply(&self) -> bool {
        !matches!(self, SlotRequest::On(_))
    }

    /// Parse a response's `result` (or a called process's stdout line) as this
    /// request's reply type. For `on.<event>` any value yields
    /// [`SlotReply::None`].
    pub fn parse_reply(&self, result: Value) -> Result<SlotReply, serde_json::Error> {
        use serde_json::from_value as f;
        Ok(match self {
            SlotRequest::Input(_) => SlotReply::Input(f(result)?),
            SlotRequest::Prompt(_) => SlotReply::Prompt(f(result)?),
            SlotRequest::ToolResult(_) => SlotReply::ToolResult(f(result)?),
            SlotRequest::Tool { .. } => SlotReply::Tool(f(result)?),
            SlotRequest::On(_) => SlotReply::None,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct SlotWire {
    method: Slot,
    params: Value,
}

impl Serialize for SlotRequest {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        SlotWire {
            method: self.slot(),
            params: self.params(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SlotRequest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = SlotWire::deserialize(deserializer)?;
        SlotRequest::from_parts(wire.method, wire.params).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_strings() {
        assert_eq!(
            "tool.fetch".parse::<Slot>().unwrap(),
            Slot::Tool("fetch".into())
        );
        assert_eq!(Slot::Tool("fetch".into()).to_string(), "tool.fetch");
        assert_eq!(
            "on.turn_end".parse::<Slot>().unwrap(),
            Slot::On(OnEvent::TurnEnd)
        );
        assert_eq!(Slot::On(OnEvent::TurnEnd).to_string(), "on.turn_end");
        for s in ["input", "prompt", "tool_result"] {
            assert_eq!(s.parse::<Slot>().unwrap().to_string(), s);
        }
        assert_eq!(
            "guard".parse::<Slot>(),
            Err(SlotError::UnknownSlot("guard".into()))
        );
        assert_eq!(
            "on.guard".parse::<Slot>(),
            Err(SlotError::UnknownEvent("guard".into()))
        );
        assert_eq!(
            "tool.".parse::<Slot>(),
            Err(SlotError::InvalidToolName(String::new()))
        );
        assert!("tool.a b".parse::<Slot>().is_err());
        assert_eq!(
            serde_json::to_string(&Slot::On(OnEvent::Reload)).unwrap(),
            "\"on.reload\""
        );
        assert!(serde_json::from_str::<Slot>("\"guard\"").is_err());
    }

    #[test]
    fn handled_must_be_true() {
        let r: InputReply = serde_json::from_str(r#"{"handled":true}"#).unwrap();
        assert_eq!(r, InputReply::Handled { handled: True });
        assert!(serde_json::from_str::<InputReply>(r#"{"handled":false}"#).is_err());
        let r: InputReply = serde_json::from_str(r#"{"text":"hi"}"#).unwrap();
        assert_eq!(r, InputReply::Text { text: "hi".into() });
    }
}
