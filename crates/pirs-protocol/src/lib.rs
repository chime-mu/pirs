//! Wire types for the pirs protocol.
//!
//! This crate is the formal interface between a pirs loop server and everything
//! that talks to it: the print client, the TUI, extension executables and other
//! servers. It holds every type that travels on the wire and nothing else:
//!
//! - the JSON-RPC 2.0 [`Envelope`] and its [`error codes`](code);
//! - [`Request`] (client → server) and the result type of each request;
//! - [`Event`] (server → observers, notifications);
//! - handler [`Slot`]s (server → handlers, requests with a reply) with their
//!   payloads and replies — the same JSON a *called* process reads from stdin
//!   and writes to stdout;
//! - the protocol's own conversation [`Message`] type, which serialises to the
//!   same JSON as the session log;
//! - [`ServerPath`], the opaque path label, and [`Frame`], the JSON-line framing.
//!
//! The crate depends on `serde`, `serde_json`, `schemars` and `thiserror` only.
//! It performs no I/O, knows nothing about sockets or processes, and never
//! interprets a path. `docs/protocol.schema.json` is generated from
//! [`ProtocolSchema`] and checked by a test: anything not in that schema does
//! not exist on the wire.
//!
//! Field names on protocol messages are `snake_case`. The conversation
//! [`Message`] and its content blocks are `camelCase`, because they are the
//! session-log format and must stay byte-compatible with it.

#![deny(unreachable_pub)]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod envelope;
mod event;
mod frame;
mod message;
mod path;
mod request;
mod schema;
mod slot;

pub use envelope::{
    code, Envelope, Id, JsonRpcVersion, RpcError, RpcNotification, RpcRequest, RpcResponse,
};
pub use event::{
    ChangedBy, Delta, Event, FsChangedEvent, LoopMessageBody, LoopMessageEvent, LoopRunEndEvent,
    LoopStatusEvent, LoopTurnEndEvent, NotifyLevel, UiNotifyEvent, UiStatusEvent, UiWidgetEvent,
};
pub use frame::{Frame, FrameError};
pub use message::{
    AssistantMessage, Content, Cost, Message, Role, StopReason, SystemMessage, ToolResultMessage,
    Usage, UserContent, UserMessage,
};
pub use path::{Ref, ServerPath};
pub use request::{
    CommandInfo, ConversationInfo, DslCheckParams, DslCheckResult, DslConflict, Empty, FsEntry,
    FsEntryKind, FsListParams, FsListResult, FsReadParams, FsReadResult, HelloParams, HelloResult,
    LoopAbortParams, LoopAttachParams, LoopAttachResult, LoopCloseParams, LoopCreateParams,
    LoopInfo, LoopListParams, LoopListResult, LoopModelParams, LoopPromptParams, LoopReloadParams,
    LoopReloadResult, LoopSelector, LoopState, LoopToolsParams, LoopWaitParams, LoopWaitResult,
    Manifest, ModelSpec, PromptWhen, RegisterParams, Request, Response, SubscribeParams,
    ThinkingLevel, ToolInfo, UiNotifyParams, UiStatusParams, UiWidgetParams, UnregisterParams,
    UnsubscribeParams,
};
pub use schema::ProtocolSchema;
pub use slot::{
    InputPayload, InputReply, OnEvent, OnPayload, OnReloadPayload, OnStartPayload,
    OnToolResultPayload, PromptPayload, PromptReply, Slot, SlotError, SlotReply, SlotRequest,
    ToolCallPayload, ToolContent, ToolReply, ToolResultPayload, ToolResultReply, True,
};

/// The protocol version this crate speaks, as `major.minor`.
///
/// Sent in both directions of [`hello`](Request::Hello). Two peers are
/// compatible when their majors agree (see [`compatible`]); a server refuses a
/// `hello` with a different major with [`code::VERSION_REFUSED`]. A minor bump
/// only adds optional fields, methods or events; a major bump may change or
/// remove anything, including the framing.
pub const PROTOCOL_VERSION: &str = "0.1";

/// Whether two protocol versions can talk to each other.
///
/// Versions are `major.minor` strings; they are compatible when the majors are
/// equal. A string without a `.` is its own major. Either side may then be the
/// newer one: a peer ignores fields and methods it does not know.
///
/// ```
/// use pirs_protocol::compatible;
/// assert!(compatible("0.1", "0.9"));
/// assert!(!compatible("0.1", "1.0"));
/// ```
pub fn compatible(a: &str, b: &str) -> bool {
    fn major(v: &str) -> &str {
        v.split('.').next().unwrap_or(v).trim()
    }
    let (a, b) = (major(a), major(b));
    !a.is_empty() && a == b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn majors_decide_compatibility() {
        assert!(compatible("0.1", "0.9"));
        assert!(compatible("0.1", "0.1"));
        assert!(compatible("1", "1.4"));
        assert!(!compatible("0.1", "1.0"));
        assert!(!compatible("", ""));
        assert!(compatible(PROTOCOL_VERSION, "0.0"));
    }
}
