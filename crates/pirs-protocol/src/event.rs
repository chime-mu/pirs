//! Events (server → observers), sent as JSON-RPC notifications.
//!
//! Every event carries `loop`. Every event except a streaming delta carries a
//! monotonic per-loop `seq`: the index of its entry in the loop's session log,
//! which *is* the event stream. `subscribe { since }` replays entries after
//! that `seq`; deltas have no `seq` and are never replayed (D-06).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::envelope::{join_method_params, split_method_params, RpcNotification};
use crate::{JsonRpcVersion, LoopState, Message, Role, ServerPath};

/// `loop.status`: the loop changed state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LoopStatusEvent {
    /// The loop.
    #[serde(rename = "loop")]
    pub loop_id: String,
    /// Log sequence number.
    pub seq: u64,
    /// The new state.
    pub state: LoopState,
    /// Unix milliseconds when the state changed.
    pub since: u64,
    /// One line about why, when there is something to say (`aborted`,
    /// `error: …`, `closed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A streamed fragment of an assistant message, tagged by `type`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Delta {
    /// More text for the text block at `index`.
    Text {
        /// Index of the content block within the message being streamed.
        index: usize,
        /// The text to append.
        text: String,
    },
    /// More reasoning for the thinking block at `index`.
    Thinking {
        /// Index of the content block within the message being streamed.
        index: usize,
        /// The reasoning text to append.
        thinking: String,
    },
}

/// The body of a `loop.message`: either a streamed delta or a complete message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum LoopMessageBody {
    /// A fragment of the assistant message being streamed. Carries no `seq`.
    Delta {
        /// The fragment.
        delta: Delta,
    },
    /// A complete message appended to the conversation: the user's prompt as
    /// the model saw it, the finished assistant message (including its tool
    /// calls), or a tool result.
    Message {
        /// The message.
        message: Box<Message>,
    },
}

/// `loop.message`: conversation traffic — streaming assistant text and
/// thinking as deltas, then each complete message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LoopMessageEvent {
    /// The loop.
    #[serde(rename = "loop")]
    pub loop_id: String,
    /// Log sequence number; absent on deltas, which are not logged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// The role of the message this belongs to.
    pub role: Role,
    /// `delta` or `message`.
    #[serde(flatten)]
    pub body: LoopMessageBody,
}

/// `loop.turn_end`: the model stopped once (a turn ends at every stop reason;
/// a `toolUse` stop is followed by tool results and another turn).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LoopTurnEndEvent {
    /// The loop.
    #[serde(rename = "loop")]
    pub loop_id: String,
    /// Log sequence number.
    pub seq: u64,
    /// The messages this turn appended, in order.
    pub messages: Vec<Message>,
}

/// `loop.run_end`: the loop went idle after a prompt — no more tool calls to
/// run and no queued prompt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LoopRunEndEvent {
    /// The loop.
    #[serde(rename = "loop")]
    pub loop_id: String,
    /// Log sequence number.
    pub seq: u64,
    /// Every message the run appended, across all its turns.
    pub messages: Vec<Message>,
}

/// `ui.status`: a status key changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UiStatusEvent {
    /// The loop.
    #[serde(rename = "loop")]
    pub loop_id: String,
    /// Log sequence number.
    pub seq: u64,
    /// The status key.
    pub key: String,
    /// The new text; empty clears the key.
    pub text: String,
}

/// `ui.widget`: a widget's lines changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UiWidgetEvent {
    /// The loop.
    #[serde(rename = "loop")]
    pub loop_id: String,
    /// Log sequence number.
    pub seq: u64,
    /// The widget key.
    pub key: String,
    /// Lines to draw; empty removes the widget.
    pub lines: Vec<String>,
}

/// Severity of a `ui.notify`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NotifyLevel {
    /// Informational.
    Info,
    /// Something to look at, e.g. a handler timed out and was skipped.
    Warning,
    /// Something failed.
    Error,
}

/// `ui.notify`: a one-off message for the user. Print mode shows it on
/// stderr; the TUI shows it as a notice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UiNotifyEvent {
    /// The loop.
    #[serde(rename = "loop")]
    pub loop_id: String,
    /// Log sequence number.
    pub seq: u64,
    /// Severity.
    pub level: NotifyLevel,
    /// The message.
    pub text: String,
}

/// What wrote the file an `fs.changed` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangedBy {
    /// One of the loop's tools wrote it during a tool call.
    Tool,
    /// The write was noticed at the end of the turn.
    Turn,
}

/// `fs.changed`: one of the loop's own tools wrote a file. There is no
/// watcher; files changed by anything else are not reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FsChangedEvent {
    /// The loop.
    #[serde(rename = "loop")]
    pub loop_id: String,
    /// Log sequence number.
    pub seq: u64,
    /// The file, as the server names it; readable with `fs.read`.
    pub path: ServerPath,
    /// What wrote it.
    pub by: ChangedBy,
}

/// Every event, tagged by `method` with its `params`. Serialises as the two
/// members of an [`RpcNotification`]; [`Event::into_rpc`] adds the envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "method", content = "params")]
pub enum Event {
    /// State change.
    #[serde(rename = "loop.status")]
    LoopStatus(LoopStatusEvent),
    /// Streamed delta or complete message.
    #[serde(rename = "loop.message")]
    LoopMessage(LoopMessageEvent),
    /// A turn ended.
    #[serde(rename = "loop.turn_end")]
    LoopTurnEnd(LoopTurnEndEvent),
    /// A run ended; the loop is idle.
    #[serde(rename = "loop.run_end")]
    LoopRunEnd(LoopRunEndEvent),
    /// Status key changed.
    #[serde(rename = "ui.status")]
    UiStatus(UiStatusEvent),
    /// Widget changed.
    #[serde(rename = "ui.widget")]
    UiWidget(UiWidgetEvent),
    /// Notice for the user.
    #[serde(rename = "ui.notify")]
    UiNotify(UiNotifyEvent),
    /// A file was written by the loop's tools.
    #[serde(rename = "fs.changed")]
    FsChanged(FsChangedEvent),
}

impl Event {
    /// Every event name, in table order.
    pub const METHODS: [&'static str; 8] = [
        "loop.status",
        "loop.message",
        "loop.turn_end",
        "loop.run_end",
        "ui.status",
        "ui.widget",
        "ui.notify",
        "fs.changed",
    ];

    /// The event name this event travels under.
    pub fn method(&self) -> &'static str {
        match self {
            Event::LoopStatus(_) => "loop.status",
            Event::LoopMessage(_) => "loop.message",
            Event::LoopTurnEnd(_) => "loop.turn_end",
            Event::LoopRunEnd(_) => "loop.run_end",
            Event::UiStatus(_) => "ui.status",
            Event::UiWidget(_) => "ui.widget",
            Event::UiNotify(_) => "ui.notify",
            Event::FsChanged(_) => "fs.changed",
        }
    }

    /// The loop this event belongs to.
    pub fn loop_id(&self) -> &str {
        match self {
            Event::LoopStatus(e) => &e.loop_id,
            Event::LoopMessage(e) => &e.loop_id,
            Event::LoopTurnEnd(e) => &e.loop_id,
            Event::LoopRunEnd(e) => &e.loop_id,
            Event::UiStatus(e) => &e.loop_id,
            Event::UiWidget(e) => &e.loop_id,
            Event::UiNotify(e) => &e.loop_id,
            Event::FsChanged(e) => &e.loop_id,
        }
    }

    /// The event's log sequence number; `None` for a streaming delta.
    pub fn seq(&self) -> Option<u64> {
        match self {
            Event::LoopStatus(e) => Some(e.seq),
            Event::LoopMessage(e) => e.seq,
            Event::LoopTurnEnd(e) => Some(e.seq),
            Event::LoopRunEnd(e) => Some(e.seq),
            Event::UiStatus(e) => Some(e.seq),
            Event::UiWidget(e) => Some(e.seq),
            Event::UiNotify(e) => Some(e.seq),
            Event::FsChanged(e) => Some(e.seq),
        }
    }

    /// Wrap in a JSON-RPC notification envelope.
    pub fn into_rpc(self) -> RpcNotification {
        let (method, params) =
            split_method_params(serde_json::to_value(&self).expect("events serialise"));
        RpcNotification {
            jsonrpc: JsonRpcVersion,
            method,
            params,
        }
    }

    /// Recover the typed event from a notification envelope.
    pub fn from_rpc(rpc: &RpcNotification) -> Result<Self, serde_json::Error> {
        serde_json::from_value(join_method_params(&rpc.method, rpc.params.clone()))
    }
}
