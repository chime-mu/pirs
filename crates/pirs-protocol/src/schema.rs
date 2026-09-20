//! The root type the JSON schema snapshot is generated from.

use schemars::JsonSchema;

use crate::*;

/// Every wire type, referenced once, so that `schema_for!(ProtocolSchema)`
/// covers the whole protocol. `docs/protocol.schema.json` is this type's
/// schema; the `$defs` are the protocol.
///
/// Not a message: nothing serialises a `ProtocolSchema`. Its fields are grouped
/// by the three tables of `30-protocol.md` — events, slots, requests — plus
/// the envelope and the shared types.
#[derive(JsonSchema)]
#[allow(dead_code)]
pub struct ProtocolSchema {
    /// One line on the wire.
    envelope: Envelope,
    /// Failure member of a response.
    rpc_error: RpcError,

    /// Client → server, tagged by `method`.
    request: Request,
    /// Result of `hello`.
    hello_result: HelloResult,
    /// Result of `loop.create`.
    loop_create_result: LoopInfo,
    /// Result of `loop.list`.
    loop_list_result: LoopListResult,
    /// Result of `loop.attach`.
    loop_attach_result: LoopAttachResult,
    /// Result of `loop.wait`.
    loop_wait_result: LoopWaitResult,
    /// Result of `fs.list`.
    fs_list_result: FsListResult,
    /// Result of `fs.read`.
    fs_read_result: FsReadResult,
    /// Result of `loop.reload`.
    loop_reload_result: LoopReloadResult,
    /// Result of `dsl.check`.
    dsl_check_result: DslCheckResult,
    /// Result of every other request.
    empty_result: Empty,

    /// Server → observers, tagged by `method`.
    event: Event,

    /// A slot name.
    slot: Slot,
    /// Server → handler `input` request params.
    input_payload: InputPayload,
    /// Reply to `input`.
    input_reply: InputReply,
    /// Server → handler `prompt` request params.
    prompt_payload: PromptPayload,
    /// Reply to `prompt`.
    prompt_reply: PromptReply,
    /// Server → handler `tool_result` request params.
    tool_result_payload: ToolResultPayload,
    /// Reply to `tool_result`.
    tool_result_reply: ToolResultReply,
    /// Server → handler `tool.<name>` request params.
    tool_call_payload: ToolCallPayload,
    /// Reply to `tool.<name>`.
    tool_reply: ToolReply,
    /// Server → handler `on.start` request params.
    on_start_payload: OnStartPayload,
    /// Server → handler `on.turn_end` request params.
    on_turn_end_payload: LoopTurnEndEvent,
    /// Server → handler `on.run_end` request params.
    on_run_end_payload: LoopRunEndEvent,
    /// Server → handler `on.tool_result` request params.
    on_tool_result_payload: OnToolResultPayload,
    /// Server → handler `on.reload` request params.
    on_reload_payload: OnReloadPayload,

    /// A conversation message.
    message: Message,
    /// A by-reference payload.
    by_ref: Ref,
    /// An opaque server path.
    server_path: ServerPath,
}
