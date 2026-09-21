//! Requests (client → server) and their results.
//!
//! [`Request`] is the closed list of methods a client may send. Each variant's
//! documentation names its result type; the result travels untagged in the
//! response's `result` member, so [`Request::parse_response`] is how a client
//! turns it back into a [`Response`].

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::envelope::{join_method_params, split_method_params, RpcRequest};
use crate::{Id, JsonRpcVersion, ServerPath, Slot};

/// An empty result `{}`; the response of every request that has nothing to
/// return.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Empty {}

/// How much the model should think. Mirrors the server's model layer; the
/// server maps it to each provider's budget or effort setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    /// No extended thinking.
    #[default]
    Off,
    /// The smallest budget the provider allows.
    Minimal,
    /// A small budget.
    Low,
    /// A medium budget.
    Medium,
    /// A large budget.
    High,
    /// A very large budget.
    Xhigh,
    /// The largest budget the provider allows.
    Max,
}

/// Which model a loop runs, and how hard it thinks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ModelSpec {
    /// The model as `provider/id` (e.g. `anthropic/claude-sonnet-4-5`,
    /// `faux/scripted`) or a bare id the server's registry resolves.
    pub model: String,
    /// Thinking level; absent means the server's default for the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingLevel>,
}

/// What a loop is doing. There is no third state: nothing but the model asks
/// the user anything, and it does so in text and then goes idle (D-37).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LoopState {
    /// A turn is running: the model is generating or a tool is executing.
    Working,
    /// Stopped; will do nothing until prompted.
    Idle,
}

/// A running loop, as `loop.create`, `loop.list` and `loop.attach` describe it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LoopInfo {
    /// Short random id, unique on this server for its lifetime.
    pub id: String,
    /// The name given at `loop.create`, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The working directory the loop's tools and policy files are relative to.
    pub cwd: ServerPath,
    /// The model the loop currently runs.
    pub model: ModelSpec,
    /// `working` or `idle`.
    pub state: LoopState,
    /// Unix milliseconds when `state` last changed.
    pub since: u64,
    /// The id of the conversation (session log) this loop appends to.
    pub conversation: String,
    /// The loop that created this one through a `[[tool]] loop = …` call;
    /// absent for a top-level loop. Closing the parent closes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

/// A conversation on disk, whether or not a loop currently runs on it (D-28).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConversationInfo {
    /// The conversation id; `loop.create { session }` continues it.
    pub id: String,
    /// The conversation's display name, if one was set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The working directory it was recorded in.
    pub cwd: ServerPath,
    /// The session log file.
    pub path: ServerPath,
    /// Unix milliseconds of the last entry.
    pub updated: u64,
}

/// A tool declaration: the shape the model is shown and the manifest lists.
/// Also the element type of a system message's `toolsAdded`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolInfo {
    /// The tool's name, as the model calls it.
    pub name: String,
    /// What the tool does, as the model reads it.
    pub description: String,
    /// JSON Schema (an object schema) for the tool's arguments.
    pub parameters: Value,
}

/// A slash command the loop's policy defines: an `input` match with a name
/// and a description (D-13), so a UI can list and complete it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CommandInfo {
    /// The name, without the leading `/`.
    pub name: String,
    /// One line of help.
    pub description: String,
}

/// The loop's merged manifest: everything its policy files and built-ins
/// contribute that a client needs to render or complete.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Manifest {
    /// Tools the model can call, built-in and policy-defined, after
    /// disabling and wrapping.
    pub tools: Vec<ToolInfo>,
    /// Slash commands.
    pub commands: Vec<CommandInfo>,
    /// Keys that `ui.status` events may carry, so a status line can be laid
    /// out before the first event.
    pub status_keys: Vec<String>,
    /// Keys that `ui.widget` events may carry.
    pub widget_keys: Vec<String>,
}

/// `hello` params: the first message on every connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HelloParams {
    /// The client's name and version, free text for logs (`pirs-tui 0.1.0`).
    pub client: String,
    /// The client's [`PROTOCOL_VERSION`](crate::PROTOCOL_VERSION).
    pub protocol_version: String,
}

/// `hello` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HelloResult {
    /// The server's name and version, free text.
    pub server: String,
    /// The server's protocol version. Same major as the client's, or the
    /// request failed with `VERSION_REFUSED` instead.
    pub protocol_version: String,
}

/// `loop.create` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LoopCreateParams {
    /// Working directory on the server.
    pub cwd: ServerPath,
    /// Model to run; absent means the server's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelSpec>,
    /// A name for the loop and its conversation, shown in lists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// A conversation id to continue (from `loop.list`'s `conversations`);
    /// absent starts a new conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// `loop.list` params.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LoopListParams {
    /// When present, `conversations` in the result lists the conversations
    /// recorded for this directory; when absent it is empty. Running loops
    /// are listed regardless.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<ServerPath>,
}

/// `loop.list` result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LoopListResult {
    /// Every running loop on this server.
    pub loops: Vec<LoopInfo>,
    /// Conversations on disk for the requested `cwd`, most recent first;
    /// empty when no `cwd` was given.
    pub conversations: Vec<ConversationInfo>,
}

/// `loop.attach` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LoopAttachParams {
    /// The loop to attach to.
    #[serde(rename = "loop")]
    pub loop_id: String,
}

/// `loop.attach` result: what a client needs to render the loop and to
/// `subscribe` from the right point.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LoopAttachResult {
    /// The loop.
    #[serde(rename = "loop")]
    pub info: LoopInfo,
    /// Its merged manifest.
    pub manifest: Manifest,
    /// The latest `seq` in the loop's log; `subscribe { since: seq }` from
    /// here misses nothing.
    pub seq: u64,
}

/// `loop.close` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LoopCloseParams {
    /// The loop to close. Its processes are killed; its conversation stays.
    #[serde(rename = "loop")]
    pub loop_id: String,
}

/// When a prompt is delivered relative to the running turn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PromptWhen {
    /// Steer: deliver at the next opportunity inside the current turn, or
    /// immediately when idle.
    #[default]
    Now,
    /// After the current turn ends (after the model's next stop).
    AfterTurn,
    /// Queue as the next input, after the run ends.
    NextInput,
}

/// `loop.prompt` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LoopPromptParams {
    /// The loop to prompt.
    #[serde(rename = "loop")]
    pub loop_id: String,
    /// The prompt text. It passes through the `input` slot before the model
    /// sees it.
    pub text: String,
    /// Delivery timing.
    #[serde(default)]
    pub when: PromptWhen,
}

/// `loop.abort` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LoopAbortParams {
    /// The loop whose current turn to abort (Esc). Idle loops are unaffected.
    #[serde(rename = "loop")]
    pub loop_id: String,
}

/// `loop.wait` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LoopWaitParams {
    /// The loop to wait for. The response arrives when it is idle.
    #[serde(rename = "loop")]
    pub loop_id: String,
}

/// `loop.wait` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LoopWaitResult {
    /// The loop's state when the wait ended; `idle` unless the loop was
    /// closed meanwhile.
    pub state: LoopState,
}

/// A loop id or `"*"` for every loop on the server, in `subscribe` and
/// `unsubscribe`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LoopSelector {
    /// `"*"`: every loop, including ones created later.
    All,
    /// One loop by id.
    Loop(String),
}

impl Serialize for LoopSelector {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            LoopSelector::All => serializer.serialize_str("*"),
            LoopSelector::Loop(id) => serializer.serialize_str(id),
        }
    }
}

impl<'de> Deserialize<'de> for LoopSelector {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(if s == "*" {
            LoopSelector::All
        } else {
            LoopSelector::Loop(s)
        })
    }
}

impl JsonSchema for LoopSelector {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "LoopSelector".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "A loop id, or \"*\" for every loop on the server including ones created later."
        })
    }
}

/// `subscribe` params: become an observer of a loop's events. Observers are
/// never waited on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SubscribeParams {
    /// Which loop(s).
    #[serde(rename = "loop")]
    pub loop_id: LoopSelector,
    /// Event names to receive (`loop.message`, `fs.changed`, …); absent means
    /// all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<String>>,
    /// Replay sequenced events with `seq` greater than this from the session
    /// log before live ones, across runs. Deltas are not replayed (D-06).
    /// `0` replays everything; absent replays nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<u64>,
}

/// `unsubscribe` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UnsubscribeParams {
    /// The same selector given to `subscribe`.
    #[serde(rename = "loop")]
    pub loop_id: LoopSelector,
}

/// `register` params: become a handler for a slot on a loop. The server then
/// sends this connection a [`SlotRequest`](crate::SlotRequest) each time the
/// slot fires and waits up to `timeout` for the reply. A registration ends
/// with `unregister`, with the loop, or when the connection drops.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RegisterParams {
    /// The loop.
    #[serde(rename = "loop")]
    pub loop_id: String,
    /// The slot: `input`, `prompt`, `tool_result`, `tool.<name>` or `on.<event>`.
    pub slot: Slot,
    /// How long the server waits for a reply, in milliseconds. Expiry counts
    /// as "no opinion" (the slot's default outcome) plus a `ui.notify`
    /// warning.
    pub timeout: u64,
}

/// `unregister` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UnregisterParams {
    /// The loop.
    #[serde(rename = "loop")]
    pub loop_id: String,
    /// The slot this connection registered.
    pub slot: Slot,
}

/// `ui.status` params: ask the server to emit a `ui.status` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UiStatusParams {
    /// The loop.
    #[serde(rename = "loop")]
    pub loop_id: String,
    /// The status key (one of the manifest's `status_keys`, or a new one).
    pub key: String,
    /// One line of text; empty clears the key.
    pub text: String,
}

/// `ui.widget` params: ask the server to emit a `ui.widget` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UiWidgetParams {
    /// The loop.
    #[serde(rename = "loop")]
    pub loop_id: String,
    /// The widget key.
    pub key: String,
    /// Lines to draw; empty removes the widget.
    pub lines: Vec<String>,
}

/// `ui.notify` params: ask the server to emit a `ui.notify` event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct UiNotifyParams {
    /// The loop.
    #[serde(rename = "loop")]
    pub loop_id: String,
    /// Severity.
    pub level: crate::NotifyLevel,
    /// The message.
    pub text: String,
}

/// `fs.list` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FsListParams {
    /// The loop whose server reads the directory.
    #[serde(rename = "loop")]
    pub loop_id: String,
    /// A directory on that server; relative paths are relative to the loop's cwd.
    pub path: ServerPath,
}

/// What a directory entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FsEntryKind {
    /// A regular file (or a symlink to one).
    File,
    /// A directory.
    Dir,
}

/// One entry of `fs.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FsEntry {
    /// The entry's full path, as the server spells it: hand it back to
    /// `fs.list` or `fs.read` unchanged; never parse or join it (D-31).
    pub path: ServerPath,
    /// The entry's name within the listed directory, for display.
    pub name: String,
    /// File or directory.
    pub kind: FsEntryKind,
    /// Size in bytes, for files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

/// `fs.list` result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FsListResult {
    /// The directory's entries, sorted by name.
    pub entries: Vec<FsEntry>,
}

/// `fs.read` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FsReadParams {
    /// The loop whose server reads the file.
    #[serde(rename = "loop")]
    pub loop_id: String,
    /// A file on that server, or a `ref` from an earlier by-reference payload.
    pub path: ServerPath,
}

/// `fs.read` result: the content inline when it is small enough, otherwise a
/// reference (D-11). A `ref` handed back to `fs.read` is served inline in
/// full, whatever its size.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum FsReadResult {
    /// The file's content as UTF-8 text.
    Content {
        /// The content.
        content: String,
    },
    /// The content is above the threshold; fetch it by its `ref`.
    Ref(crate::Ref),
}

/// `loop.tools` params: set the loop's active tool set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LoopToolsParams {
    /// The loop.
    #[serde(rename = "loop")]
    pub loop_id: String,
    /// Names from the manifest; tools not listed are disabled for the loop.
    pub names: Vec<String>,
}

/// `loop.model` params: change the loop's model between turns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LoopModelParams {
    /// The loop.
    #[serde(rename = "loop")]
    pub loop_id: String,
    /// The new model and thinking level.
    pub spec: ModelSpec,
}

/// `loop.reload` params: re-read the loop's policy files now. The server
/// also does this by itself when one of the loop's own tools writes a policy
/// file (D-33).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LoopReloadParams {
    /// The loop.
    #[serde(rename = "loop")]
    pub loop_id: String,
}

/// `loop.reload` result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LoopReloadResult {
    /// The policy files now loaded, in load order.
    pub files: Vec<ServerPath>,
}

/// `dsl.check` params: run the policy loader and checker on the server, where
/// the files live. What `pirs check` calls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DslCheckParams {
    /// The directory whose policy files (with the user's global ones) to check.
    pub cwd: ServerPath,
}

/// A composition error between two policy files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DslConflict {
    /// What conflicts (a duplicate key, two replacements of one prompt, …).
    pub message: String,
    /// The two files involved.
    pub files: Vec<ServerPath>,
}

/// `dsl.check` result: the merged view a loop in `cwd` would start with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DslCheckResult {
    /// The policy files found, in load order.
    pub files: Vec<ServerPath>,
    /// The merged manifest.
    pub manifest: Manifest,
    /// Conflicts, each naming both files. Empty when the policy is clean.
    pub conflicts: Vec<DslConflict>,
    /// The server's human-readable rendering of the composed policy: the
    /// files with their intents, the merged `[settings]` and where each key
    /// came from, and every slot's entries with their origin. Display text,
    /// not a second schema — a client prints it, it does not parse it (D-40).
    pub rendered: String,
    /// The fully assembled system prompt, with every rewrite applied and
    /// visible (D-21).
    pub system_prompt: String,
}

/// Every request a client may send, tagged by `method` with its `params`.
///
/// Serialises as `{ "method": "<name>", "params": { … } }`, the two members of
/// an [`RpcRequest`]; [`Request::into_rpc`] adds the envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "method", content = "params")]
pub enum Request {
    /// First message on every connection. Result: [`HelloResult`]. Fails with
    /// `VERSION_REFUSED` when the majors differ.
    #[serde(rename = "hello")]
    Hello(HelloParams),
    /// Start a loop. Result: [`LoopInfo`].
    #[serde(rename = "loop.create")]
    LoopCreate(LoopCreateParams),
    /// List running loops and, for a `cwd`, its conversations. Result:
    /// [`LoopListResult`].
    #[serde(rename = "loop.list")]
    LoopList(LoopListParams),
    /// Attach: get the loop, its manifest and its latest `seq`. Result:
    /// [`LoopAttachResult`].
    #[serde(rename = "loop.attach")]
    LoopAttach(LoopAttachParams),
    /// Close a loop and kill its processes. Result: [`Empty`].
    #[serde(rename = "loop.close")]
    LoopClose(LoopCloseParams),
    /// Send a prompt. Result: [`Empty`]; the model's answer arrives as events.
    #[serde(rename = "loop.prompt")]
    LoopPrompt(LoopPromptParams),
    /// Abort the current turn. Result: [`Empty`].
    #[serde(rename = "loop.abort")]
    LoopAbort(LoopAbortParams),
    /// Block until the loop is idle. Result: [`LoopWaitResult`].
    #[serde(rename = "loop.wait")]
    LoopWait(LoopWaitParams),
    /// Observe events. Result: [`Empty`]; replayed events follow if `since`
    /// was given.
    #[serde(rename = "subscribe")]
    Subscribe(SubscribeParams),
    /// Stop observing. Result: [`Empty`].
    #[serde(rename = "unsubscribe")]
    Unsubscribe(UnsubscribeParams),
    /// Handle a slot. Result: [`Empty`].
    #[serde(rename = "register")]
    Register(RegisterParams),
    /// Stop handling a slot. Result: [`Empty`].
    #[serde(rename = "unregister")]
    Unregister(UnregisterParams),
    /// Emit a `ui.status` event. Result: [`Empty`].
    #[serde(rename = "ui.status")]
    UiStatus(UiStatusParams),
    /// Emit a `ui.widget` event. Result: [`Empty`].
    #[serde(rename = "ui.widget")]
    UiWidget(UiWidgetParams),
    /// Emit a `ui.notify` event. Result: [`Empty`].
    #[serde(rename = "ui.notify")]
    UiNotify(UiNotifyParams),
    /// List a directory on the loop's server. Result: [`FsListResult`].
    #[serde(rename = "fs.list")]
    FsList(FsListParams),
    /// Read a file on the loop's server. Result: [`FsReadResult`].
    #[serde(rename = "fs.read")]
    FsRead(FsReadParams),
    /// Set the active tool set. Result: [`Empty`].
    #[serde(rename = "loop.tools")]
    LoopTools(LoopToolsParams),
    /// Change the model. Result: [`Empty`].
    #[serde(rename = "loop.model")]
    LoopModel(LoopModelParams),
    /// Re-read policy files. Result: [`LoopReloadResult`].
    #[serde(rename = "loop.reload")]
    LoopReload(LoopReloadParams),
    /// Check policy files for a directory. Result: [`DslCheckResult`].
    #[serde(rename = "dsl.check")]
    DslCheck(DslCheckParams),
}

/// The typed result of each [`Request`], in the same order. Not a wire type by
/// itself: a response carries only the untagged `result`, and the request it
/// answers determines which variant it is ([`Request::parse_response`]).
#[derive(Debug, Clone, PartialEq)]
pub enum Response {
    /// Result of `hello`.
    Hello(HelloResult),
    /// Result of `loop.create`.
    LoopCreate(LoopInfo),
    /// Result of `loop.list`.
    LoopList(LoopListResult),
    /// Result of `loop.attach`.
    LoopAttach(LoopAttachResult),
    /// Result of `loop.close`.
    LoopClose(Empty),
    /// Result of `loop.prompt`.
    LoopPrompt(Empty),
    /// Result of `loop.abort`.
    LoopAbort(Empty),
    /// Result of `loop.wait`.
    LoopWait(LoopWaitResult),
    /// Result of `subscribe`.
    Subscribe(Empty),
    /// Result of `unsubscribe`.
    Unsubscribe(Empty),
    /// Result of `register`.
    Register(Empty),
    /// Result of `unregister`.
    Unregister(Empty),
    /// Result of `ui.status`.
    UiStatus(Empty),
    /// Result of `ui.widget`.
    UiWidget(Empty),
    /// Result of `ui.notify`.
    UiNotify(Empty),
    /// Result of `fs.list`.
    FsList(FsListResult),
    /// Result of `fs.read`.
    FsRead(FsReadResult),
    /// Result of `loop.tools`.
    LoopTools(Empty),
    /// Result of `loop.model`.
    LoopModel(Empty),
    /// Result of `loop.reload`.
    LoopReload(LoopReloadResult),
    /// Result of `dsl.check`.
    DslCheck(DslCheckResult),
}

impl Response {
    /// The result as the JSON value a response carries in `result`.
    pub fn to_value(&self) -> Value {
        fn v<T: Serialize>(t: &T) -> Value {
            serde_json::to_value(t).expect("protocol result types serialise")
        }
        match self {
            Response::Hello(r) => v(r),
            Response::LoopCreate(r) => v(r),
            Response::LoopList(r) => v(r),
            Response::LoopAttach(r) => v(r),
            Response::LoopWait(r) => v(r),
            Response::FsList(r) => v(r),
            Response::FsRead(r) => v(r),
            Response::LoopReload(r) => v(r),
            Response::DslCheck(r) => v(r),
            Response::LoopClose(r)
            | Response::LoopPrompt(r)
            | Response::LoopAbort(r)
            | Response::Subscribe(r)
            | Response::Unsubscribe(r)
            | Response::Register(r)
            | Response::Unregister(r)
            | Response::UiStatus(r)
            | Response::UiWidget(r)
            | Response::UiNotify(r)
            | Response::LoopTools(r)
            | Response::LoopModel(r) => v(r),
        }
    }
}

impl Request {
    /// The method name this request travels under.
    pub fn method(&self) -> &'static str {
        match self {
            Request::Hello(_) => "hello",
            Request::LoopCreate(_) => "loop.create",
            Request::LoopList(_) => "loop.list",
            Request::LoopAttach(_) => "loop.attach",
            Request::LoopClose(_) => "loop.close",
            Request::LoopPrompt(_) => "loop.prompt",
            Request::LoopAbort(_) => "loop.abort",
            Request::LoopWait(_) => "loop.wait",
            Request::Subscribe(_) => "subscribe",
            Request::Unsubscribe(_) => "unsubscribe",
            Request::Register(_) => "register",
            Request::Unregister(_) => "unregister",
            Request::UiStatus(_) => "ui.status",
            Request::UiWidget(_) => "ui.widget",
            Request::UiNotify(_) => "ui.notify",
            Request::FsList(_) => "fs.list",
            Request::FsRead(_) => "fs.read",
            Request::LoopTools(_) => "loop.tools",
            Request::LoopModel(_) => "loop.model",
            Request::LoopReload(_) => "loop.reload",
            Request::DslCheck(_) => "dsl.check",
        }
    }

    /// Every method name, in table order.
    pub const METHODS: [&'static str; 21] = [
        "hello",
        "loop.create",
        "loop.list",
        "loop.attach",
        "loop.close",
        "loop.prompt",
        "loop.abort",
        "loop.wait",
        "subscribe",
        "unsubscribe",
        "register",
        "unregister",
        "ui.status",
        "ui.widget",
        "ui.notify",
        "fs.list",
        "fs.read",
        "loop.tools",
        "loop.model",
        "loop.reload",
        "dsl.check",
    ];

    /// Wrap in a JSON-RPC request envelope with `id`.
    pub fn into_rpc(self, id: impl Into<Id>) -> RpcRequest {
        let (method, params) =
            split_method_params(serde_json::to_value(&self).expect("requests serialise"));
        RpcRequest {
            jsonrpc: JsonRpcVersion,
            id: id.into(),
            method,
            params,
        }
    }

    /// Recover the typed request from an envelope. Errors name the unknown
    /// method or the bad params, for `METHOD_NOT_FOUND` / `INVALID_PARAMS`.
    pub fn from_rpc(rpc: &RpcRequest) -> Result<Self, serde_json::Error> {
        serde_json::from_value(join_method_params(&rpc.method, rpc.params.clone()))
    }

    /// Parse a response's `result` as this request's result type.
    pub fn parse_response(&self, result: Value) -> Result<Response, serde_json::Error> {
        use serde_json::from_value as f;
        Ok(match self {
            Request::Hello(_) => Response::Hello(f(result)?),
            Request::LoopCreate(_) => Response::LoopCreate(f(result)?),
            Request::LoopList(_) => Response::LoopList(f(result)?),
            Request::LoopAttach(_) => Response::LoopAttach(f(result)?),
            Request::LoopClose(_) => Response::LoopClose(f(result)?),
            Request::LoopPrompt(_) => Response::LoopPrompt(f(result)?),
            Request::LoopAbort(_) => Response::LoopAbort(f(result)?),
            Request::LoopWait(_) => Response::LoopWait(f(result)?),
            Request::Subscribe(_) => Response::Subscribe(f(result)?),
            Request::Unsubscribe(_) => Response::Unsubscribe(f(result)?),
            Request::Register(_) => Response::Register(f(result)?),
            Request::Unregister(_) => Response::Unregister(f(result)?),
            Request::UiStatus(_) => Response::UiStatus(f(result)?),
            Request::UiWidget(_) => Response::UiWidget(f(result)?),
            Request::UiNotify(_) => Response::UiNotify(f(result)?),
            Request::FsList(_) => Response::FsList(f(result)?),
            Request::FsRead(_) => Response::FsRead(f(result)?),
            Request::LoopTools(_) => Response::LoopTools(f(result)?),
            Request::LoopModel(_) => Response::LoopModel(f(result)?),
            Request::LoopReload(_) => Response::LoopReload(f(result)?),
            Request::DslCheck(_) => Response::DslCheck(f(result)?),
        })
    }
}
