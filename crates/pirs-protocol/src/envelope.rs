//! The JSON-RPC 2.0 envelope: requests, responses, errors and notifications.
//!
//! Every line on the wire is one [`Envelope`]. The typed [`Request`](crate::Request),
//! [`Event`](crate::Event) and [`SlotRequest`](crate::SlotRequest) enums describe the
//! `method`/`params` pair inside it; the envelope itself carries them as a plain
//! JSON value so a hub can route a message before it knows its type.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Error codes carried in [`RpcError::code`].
///
/// The four negative-32xxx codes are JSON-RPC's own. The `-320xx` range is
/// pirs's; each is documented on its constant.
pub mod code {
    /// The line was not valid JSON, or not an object.
    pub const PARSE_ERROR: i64 = -32700;
    /// The object was JSON but not a valid JSON-RPC 2.0 request (missing
    /// `method`, wrong `jsonrpc`, malformed `id`).
    pub const INVALID_REQUEST: i64 = -32600;
    /// The `method` is not one this protocol version defines.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// `params` did not deserialise as the method's parameter type; `data`
    /// carries the deserialisation error text.
    pub const INVALID_PARAMS: i64 = -32602;
    /// The server failed while handling an otherwise valid request. The
    /// message says what happened; nothing about the loop is implied.
    pub const INTERNAL_ERROR: i64 = -32603;

    /// `hello` named a protocol version whose major differs from the server's.
    /// `data` is `{ "server": "<version>" }`. The connection is closed after
    /// this reply.
    pub const VERSION_REFUSED: i64 = -32000;
    /// The `loop` id in the request names no loop on this server (it never
    /// existed or has been closed).
    pub const UNKNOWN_LOOP: i64 = -32001;
    /// The `slot` in `register`/`unregister` is not one of the five slot
    /// forms, or `unregister` names a slot this connection never registered.
    pub const UNKNOWN_SLOT: i64 = -32002;
    /// A handler did not reply within its registered timeout. Sent to nobody
    /// as an error reply — the server treats the slot as "no opinion" — but
    /// used as the `code` of the `ui.notify` warning's details and in the
    /// session log.
    pub const HANDLER_TIMEOUT: i64 = -32003;
    /// A handler replied with an error response, or a called process exited
    /// non-zero or wrote something that was not the slot's reply type. `data`
    /// carries the handler's error or its stderr.
    pub const HANDLER_ERROR: i64 = -32004;
    /// `fs.list`/`fs.read` named a path that does not exist or cannot be read,
    /// or `loop.create { session }` / `loop.attach` named an unknown
    /// conversation.
    pub const NOT_FOUND: i64 = -32005;
    /// The request cannot be served right now (for example a reload while the
    /// loop is working); retry later.
    pub const BUSY: i64 = -32006;
}

/// The literal string `"2.0"` in the `jsonrpc` field of every message.
///
/// Serialises as `"2.0"`; any other value fails to deserialise.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JsonRpcVersion;

impl Serialize for JsonRpcVersion {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str("2.0")
    }
}

impl<'de> Deserialize<'de> for JsonRpcVersion {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        if s == "2.0" {
            Ok(JsonRpcVersion)
        } else {
            Err(serde::de::Error::custom(format!(
                "jsonrpc must be \"2.0\", got {s:?}"
            )))
        }
    }
}

impl JsonSchema for JsonRpcVersion {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "JsonRpcVersion".into()
    }
    fn inline_schema() -> bool {
        true
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "const": "2.0",
            "description": "The literal string \"2.0\" in the `jsonrpc` field of every message."
        })
    }
}

/// A JSON-RPC request id: an integer or a string, chosen by the sender and
/// echoed in the reply. Clients pick ids for their requests; the server picks
/// ids for the slot requests it sends to handlers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Id {
    /// An integer id.
    Number(i64),
    /// A string id.
    String(String),
}

impl From<i64> for Id {
    fn from(n: i64) -> Self {
        Id::Number(n)
    }
}

impl From<String> for Id {
    fn from(s: String) -> Self {
        Id::String(s)
    }
}

impl From<&str> for Id {
    fn from(s: &str) -> Self {
        Id::String(s.to_owned())
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Id::Number(n) => write!(f, "{n}"),
            Id::String(s) => f.write_str(s),
        }
    }
}

/// A request: `{ jsonrpc, id, method, params }`. Expects exactly one
/// [`RpcResponse`] with the same `id`.
///
/// Client → server requests have a [`Request`](crate::Request) method; server →
/// handler requests have a [`Slot`](crate::Slot) as their method.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RpcRequest {
    /// Always `"2.0"`.
    pub jsonrpc: JsonRpcVersion,
    /// Echoed in the response.
    pub id: Id,
    /// The request or slot name.
    pub method: String,
    /// The method's parameters, always a JSON object in this protocol.
    #[serde(default)]
    pub params: Value,
}

/// A response: `{ jsonrpc, id, result }` on success or `{ jsonrpc, id, error }`
/// on failure — never both.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RpcResponse {
    /// Always `"2.0"`.
    pub jsonrpc: JsonRpcVersion,
    /// The id of the request being answered.
    pub id: Id,
    /// The result, present on success. `{}` for requests without a result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// The error, present on failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl RpcResponse {
    /// A successful response carrying `result`.
    pub fn ok(id: Id, result: Value) -> Self {
        RpcResponse {
            jsonrpc: JsonRpcVersion,
            id,
            result: Some(result),
            error: None,
        }
    }

    /// A failed response carrying `error`.
    pub fn err(id: Id, error: RpcError) -> Self {
        RpcResponse {
            jsonrpc: JsonRpcVersion,
            id,
            result: None,
            error: Some(error),
        }
    }

    /// The outcome as a `Result`. A response with neither field is an
    /// [`INTERNAL_ERROR`](code::INTERNAL_ERROR); one with both is treated as
    /// the error.
    pub fn into_result(self) -> Result<Value, RpcError> {
        match (self.result, self.error) {
            (_, Some(e)) => Err(e),
            (Some(v), None) => Ok(v),
            (None, None) => Err(RpcError::new(
                code::INTERNAL_ERROR,
                "response carries neither result nor error",
            )),
        }
    }
}

/// The `error` member of a failed response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, thiserror::Error)]
#[error("{message} (code {code})")]
pub struct RpcError {
    /// One of the constants in [`code`].
    pub code: i64,
    /// A short human-readable message.
    pub message: String,
    /// Structured detail, documented per code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    /// An error with `code` and `message` and no `data`.
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        RpcError {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// Attach structured detail.
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

/// A notification: `{ jsonrpc, method, params }` with no `id`, so no reply is
/// possible. Every [`Event`](crate::Event) travels as one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RpcNotification {
    /// Always `"2.0"`.
    pub jsonrpc: JsonRpcVersion,
    /// The event name.
    pub method: String,
    /// The event payload, always a JSON object in this protocol.
    #[serde(default)]
    pub params: Value,
}

/// One line on the wire, in any direction.
///
/// Untagged: a message with `method` and `id` is a request, with `method`
/// and no `id` a notification, and with `id` and no `method` a response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Envelope {
    /// A request expecting a reply.
    Request(RpcRequest),
    /// A notification; no reply.
    Notification(RpcNotification),
    /// A reply to an earlier request.
    Response(RpcResponse),
}

impl Envelope {
    /// A request envelope with `id`, `method` and `params`.
    pub fn request(id: impl Into<Id>, method: impl Into<String>, params: Value) -> Self {
        Envelope::Request(RpcRequest {
            jsonrpc: JsonRpcVersion,
            id: id.into(),
            method: method.into(),
            params,
        })
    }

    /// A notification envelope with `method` and `params`.
    pub fn notification(method: impl Into<String>, params: Value) -> Self {
        Envelope::Notification(RpcNotification {
            jsonrpc: JsonRpcVersion,
            method: method.into(),
            params,
        })
    }

    /// The `id`, for requests and responses.
    pub fn id(&self) -> Option<&Id> {
        match self {
            Envelope::Request(r) => Some(&r.id),
            Envelope::Response(r) => Some(&r.id),
            Envelope::Notification(_) => None,
        }
    }

    /// The `method`, for requests and notifications.
    pub fn method(&self) -> Option<&str> {
        match self {
            Envelope::Request(r) => Some(&r.method),
            Envelope::Notification(n) => Some(&n.method),
            Envelope::Response(_) => None,
        }
    }
}

/// Split a `{ "method": ..., "params": ... }` value produced by a
/// method-tagged enum into its two members.
pub(crate) fn split_method_params(v: Value) -> (String, Value) {
    match v {
        Value::Object(mut map) => {
            let method = match map.remove("method") {
                Some(Value::String(s)) => s,
                _ => String::new(),
            };
            let params = map
                .remove("params")
                .unwrap_or(Value::Object(Default::default()));
            (method, params)
        }
        _ => (String::new(), Value::Object(Default::default())),
    }
}

/// Reassemble a method-tagged value from its two members.
pub(crate) fn join_method_params(method: &str, params: Value) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("method".into(), Value::String(method.to_owned()));
    map.insert("params".into(), params);
    Value::Object(map)
}
