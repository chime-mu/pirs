//! The agent loop: stream an assistant response, execute tool calls, repeat.
//! Mirrors `@earendil-works/pi-agent-core`'s `agent-loop.ts`.

use crate::types::*;
use futures::StreamExt;
use pi_ai::*;
use serde_json::Value;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub struct LoopInput {
    pub system_prompt: String,
    pub tools: Vec<ToolRef>,
    pub messages: Vec<AgentMessage>,
}

/// Run the loop with new prompt messages appended to `context`.
/// Returns the messages produced by this run (including the prompts).
pub async fn run_agent_loop(
    prompts: Vec<AgentMessage>,
    mut input: LoopInput,
    config: AgentLoopConfig,
    hooks: Arc<dyn AgentHooks>,
    cancel: CancellationToken,
    emit: EventSink,
) -> Vec<AgentMessage> {
    let mut new_messages: Vec<AgentMessage> = Vec::new();
    emit(AgentEvent::AgentStart).await;
    emit(AgentEvent::TurnStart).await;
    for m in prompts {
        emit(AgentEvent::MessageStart { message: m.clone() }).await;
        emit(AgentEvent::MessageEnd { message: m.clone() }).await;
        input.messages.push(m.clone());
        new_messages.push(m);
    }
    run_loop(&mut input, &mut new_messages, config, hooks, cancel, emit).await;
    new_messages
}

/// Continue from the existing context without adding a prompt (retries).
pub async fn run_agent_loop_continue(
    mut input: LoopInput,
    config: AgentLoopConfig,
    hooks: Arc<dyn AgentHooks>,
    cancel: CancellationToken,
    emit: EventSink,
) -> Vec<AgentMessage> {
    let mut new_messages = Vec::new();
    emit(AgentEvent::AgentStart).await;
    emit(AgentEvent::TurnStart).await;
    run_loop(&mut input, &mut new_messages, config, hooks, cancel, emit).await;
    new_messages
}

async fn run_loop(
    input: &mut LoopInput,
    new_messages: &mut Vec<AgentMessage>,
    config: AgentLoopConfig,
    hooks: Arc<dyn AgentHooks>,
    cancel: CancellationToken,
    emit: EventSink,
) {
    let mut pending: Vec<AgentMessage> = hooks.get_steering_messages().await;
    let mut first_turn = true;
    loop {
        let mut has_more_tool_calls = true;
        while has_more_tool_calls || !pending.is_empty() {
            if !first_turn {
                emit(AgentEvent::TurnStart).await;
            }
            first_turn = false;
            for m in pending.drain(..) {
                emit(AgentEvent::MessageStart { message: m.clone() }).await;
                emit(AgentEvent::MessageEnd { message: m.clone() }).await;
                input.messages.push(m.clone());
                new_messages.push(m);
            }

            let message = {
                if let Some(tools) = hooks.refresh_tools().await {
                    input.tools = tools;
                }
                stream_assistant_response(input, &config, &hooks, &cancel, &emit).await
            };
            new_messages.push(AgentMessage::Assistant(message.clone()));

            if matches!(message.stop_reason, StopReason::Error | StopReason::Aborted) {
                emit(AgentEvent::TurnEnd { message: AgentMessage::Assistant(message), tool_results: vec![] }).await;
                emit(AgentEvent::AgentEnd { messages: new_messages.clone() }).await;
                return;
            }

            let tool_calls = message.tool_calls();
            let mut tool_results = Vec::new();
            has_more_tool_calls = false;
            if !tool_calls.is_empty() {
                let batch = if message.stop_reason == StopReason::Length {
                    fail_truncated_tool_calls(&tool_calls, &emit).await
                } else {
                    execute_tool_calls(input, &message, &tool_calls, &config, &hooks, &cancel, &emit).await
                };
                has_more_tool_calls = !batch.terminate;
                for r in batch.messages {
                    input.messages.push(AgentMessage::ToolResult(r.clone()));
                    new_messages.push(AgentMessage::ToolResult(r.clone()));
                    tool_results.push(r);
                }
            }
            emit(AgentEvent::TurnEnd { message: AgentMessage::Assistant(message.clone()), tool_results }).await;

            if hooks.should_stop_after_turn(&message).await {
                emit(AgentEvent::AgentEnd { messages: new_messages.clone() }).await;
                return;
            }
            pending = hooks.get_steering_messages().await;
        }
        let follow_ups = hooks.get_follow_up_messages().await;
        if !follow_ups.is_empty() {
            pending = follow_ups;
            continue;
        }
        break;
    }
    emit(AgentEvent::AgentEnd { messages: new_messages.clone() }).await;
}

async fn stream_assistant_response(
    input: &mut LoopInput,
    config: &AgentLoopConfig,
    hooks: &Arc<dyn AgentHooks>,
    cancel: &CancellationToken,
    emit: &EventSink,
) -> AssistantMessage {
    let transformed = hooks.transform_context(input.messages.clone(), cancel).await;
    let llm_messages = hooks.convert_to_llm(&transformed).await;
    let context = Context {
        system_prompt: Some(input.system_prompt.clone()),
        messages: llm_messages,
        tools: input.tools.iter().map(|t| t.declaration()).collect(),
    };
    let mut options = hooks.stream_options().await;
    options.api_key = hooks.get_api_key(&config.model.provider).await.or(options.api_key);
    options.reasoning = config.thinking_level;
    options.max_tokens = config.max_tokens;
    options.temperature = config.temperature;
    options.cancel = Some(cancel.clone());

    let mut stream = pi_ai::stream(&config.model, context, options);
    let mut partial: Option<AssistantMessage> = None;
    let mut final_message: Option<AssistantMessage> = None;
    while let Some(ev) = stream.next().await {
        match &ev {
            AssistantMessageEvent::Start { partial: p } => {
                partial = Some(p.clone());
                input.messages.push(AgentMessage::Assistant(p.clone()));
                emit(AgentEvent::MessageStart { message: AgentMessage::Assistant(p.clone()) }).await;
            }
            AssistantMessageEvent::Done { message, .. } => {
                final_message = Some(message.clone());
                break;
            }
            AssistantMessageEvent::Error { error, .. } => {
                final_message = Some(error.clone());
                break;
            }
            other => {
                if let Some(p) = event_partial(other) {
                    partial = Some(p.clone());
                    if let Some(last) = input.messages.last_mut() {
                        *last = AgentMessage::Assistant(p.clone());
                    }
                    emit(AgentEvent::MessageUpdate { message: AgentMessage::Assistant(p.clone()), assistant_message_event: ev.clone() }).await;
                }
            }
        }
    }
    let final_message = final_message.unwrap_or_else(|| AssistantMessage::error(&config.model, "Stream ended unexpectedly", false));
    if partial.is_some() {
        if let Some(last) = input.messages.last_mut() {
            *last = AgentMessage::Assistant(final_message.clone());
        }
    } else {
        input.messages.push(AgentMessage::Assistant(final_message.clone()));
        emit(AgentEvent::MessageStart { message: AgentMessage::Assistant(final_message.clone()) }).await;
    }
    emit(AgentEvent::MessageEnd { message: AgentMessage::Assistant(final_message.clone()) }).await;
    final_message
}

fn event_partial(ev: &AssistantMessageEvent) -> Option<&AssistantMessage> {
    use AssistantMessageEvent::*;
    match ev {
        TextStart { partial, .. }
        | TextDelta { partial, .. }
        | TextEnd { partial, .. }
        | ThinkingStart { partial, .. }
        | ThinkingDelta { partial, .. }
        | ThinkingEnd { partial, .. }
        | ToolCallStart { partial, .. }
        | ToolCallDelta { partial, .. }
        | ToolCallEnd { partial, .. } => Some(partial),
        _ => None,
    }
}

struct Batch {
    messages: Vec<ToolResultMessage>,
    terminate: bool,
}

struct Finalized {
    tool_call: ToolCallRef,
    result: ToolResult,
    is_error: bool,
}

fn error_result(msg: impl Into<String>) -> ToolResult {
    ToolResult::error(msg)
}

async fn fail_truncated_tool_calls(tool_calls: &[ToolCallRef], emit: &EventSink) -> Batch {
    let mut messages = Vec::new();
    for tc in tool_calls {
        emit(AgentEvent::ToolExecutionStart { tool_call_id: tc.id.clone(), tool_name: tc.name.clone(), args: tc.arguments.clone() }).await;
        let f = Finalized {
            tool_call: tc.clone(),
            result: error_result(format!(
                "Tool call \"{}\" was not executed: the response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments.",
                tc.name
            )),
            is_error: true,
        };
        emit_end(&f, emit).await;
        let m = to_message(&f);
        emit_result_message(&m, emit).await;
        messages.push(m);
    }
    Batch { messages, terminate: false }
}

async fn execute_tool_calls(
    input: &LoopInput,
    assistant: &AssistantMessage,
    tool_calls: &[ToolCallRef],
    config: &AgentLoopConfig,
    hooks: &Arc<dyn AgentHooks>,
    cancel: &CancellationToken,
    emit: &EventSink,
) -> Batch {
    let has_sequential = tool_calls.iter().any(|tc| {
        input.tools.iter().find(|t| t.name() == tc.name).map(|t| t.execution_mode() == ToolExecutionMode::Sequential).unwrap_or(false)
    });
    if config.tool_execution == ToolExecutionMode::Sequential || has_sequential {
        execute_sequential(input, assistant, tool_calls, hooks, cancel, emit).await
    } else {
        execute_parallel(input, assistant, tool_calls, hooks, cancel, emit).await
    }
}

enum Prepared {
    Immediate { result: ToolResult, is_error: bool },
    Ready { tool: ToolRef, args: Value },
}

async fn prepare(
    input: &LoopInput,
    assistant: &AssistantMessage,
    tc: &ToolCallRef,
    hooks: &Arc<dyn AgentHooks>,
    cancel: &CancellationToken,
) -> Prepared {
    let Some(tool) = input.tools.iter().find(|t| t.name() == tc.name).cloned() else {
        return Prepared::Immediate { result: error_result(format!("Tool {} not found", tc.name)), is_error: true };
    };
    let mut args = tool.prepare_arguments(tc.arguments.clone());
    if let Err(e) = crate::validate::validate(&tool.parameters(), &args) {
        return Prepared::Immediate { result: error_result(format!("Invalid arguments for tool {}: {e}", tc.name)), is_error: true };
    }
    let before = hooks.before_tool_call(BeforeToolCallContext { assistant_message: assistant, tool_call: tc, args: &args }, cancel).await;
    if cancel.is_cancelled() {
        return Prepared::Immediate { result: error_result("Operation aborted"), is_error: true };
    }
    if let Some(b) = before {
        if b.block {
            let mut result = error_result(b.reason.unwrap_or_else(|| "Tool execution was blocked".into()));
            result.terminate = b.terminate;
            return Prepared::Immediate { result, is_error: true };
        }
        if let Some(a) = b.args {
            args = a;
        }
    }
    Prepared::Ready { tool, args }
}

async fn execute_ready(tool: &ToolRef, tc: &ToolCallRef, args: &Value, cancel: &CancellationToken, emit: &EventSink) -> (ToolResult, bool) {
    let (utx, mut urx) = tokio::sync::mpsc::unbounded_channel::<ToolResult>();
    let on_update: UpdateFn = Arc::new(move |partial| {
        let _ = utx.send(partial);
    });
    let emit2 = emit.clone();
    let tc2 = tc.clone();
    let forwarder = tokio::spawn(async move {
        while let Some(p) = urx.recv().await {
            emit2(AgentEvent::ToolExecutionUpdate { tool_call_id: tc2.id.clone(), tool_name: tc2.name.clone(), args: tc2.arguments.clone(), partial_result: p }).await;
        }
    });
    let outcome = tool.execute(&tc.id, args.clone(), cancel.clone(), on_update).await;
    let _ = forwarder.await;
    match outcome {
        Ok(r) => (r, false),
        Err(e) => (error_result(e.to_string()), true),
    }
}

async fn finalize(
    assistant: &AssistantMessage,
    tc: &ToolCallRef,
    args: &Value,
    mut result: ToolResult,
    mut is_error: bool,
    hooks: &Arc<dyn AgentHooks>,
    cancel: &CancellationToken,
) -> Finalized {
    if let Some(after) = hooks
        .after_tool_call(AfterToolCallContext { assistant_message: assistant, tool_call: tc, args, result: &result, is_error }, cancel)
        .await
    {
        if let Some(c) = after.content {
            result.content = c;
        }
        if let Some(d) = after.details {
            result.details = Some(d);
        }
        if let Some(u) = after.usage {
            result.usage = Some(u);
        }
        if let Some(t) = after.terminate {
            result.terminate = t;
        }
        if let Some(e) = after.is_error {
            is_error = e;
        }
    }
    Finalized { tool_call: tc.clone(), result, is_error }
}

async fn execute_sequential(
    input: &LoopInput,
    assistant: &AssistantMessage,
    tool_calls: &[ToolCallRef],
    hooks: &Arc<dyn AgentHooks>,
    cancel: &CancellationToken,
    emit: &EventSink,
) -> Batch {
    let mut finalized = Vec::new();
    let mut messages = Vec::new();
    for tc in tool_calls {
        emit(AgentEvent::ToolExecutionStart { tool_call_id: tc.id.clone(), tool_name: tc.name.clone(), args: tc.arguments.clone() }).await;
        let f = match prepare(input, assistant, tc, hooks, cancel).await {
            Prepared::Immediate { result, is_error } => Finalized { tool_call: tc.clone(), result, is_error },
            Prepared::Ready { tool, args } => {
                let (r, e) = execute_ready(&tool, tc, &args, cancel, emit).await;
                finalize(assistant, tc, &args, r, e, hooks, cancel).await
            }
        };
        emit_end(&f, emit).await;
        let m = to_message(&f);
        emit_result_message(&m, emit).await;
        finalized.push(f);
        messages.push(m);
        if cancel.is_cancelled() {
            break;
        }
    }
    Batch { terminate: should_terminate(&finalized), messages }
}

async fn execute_parallel(
    input: &LoopInput,
    assistant: &AssistantMessage,
    tool_calls: &[ToolCallRef],
    hooks: &Arc<dyn AgentHooks>,
    cancel: &CancellationToken,
    emit: &EventSink,
) -> Batch {
    enum Entry {
        Done(Finalized),
        Pending(ToolCallRef, ToolRef, Value),
    }
    let mut entries = Vec::new();
    for tc in tool_calls {
        emit(AgentEvent::ToolExecutionStart { tool_call_id: tc.id.clone(), tool_name: tc.name.clone(), args: tc.arguments.clone() }).await;
        match prepare(input, assistant, tc, hooks, cancel).await {
            Prepared::Immediate { result, is_error } => {
                let f = Finalized { tool_call: tc.clone(), result, is_error };
                emit_end(&f, emit).await;
                entries.push(Entry::Done(f));
            }
            Prepared::Ready { tool, args } => entries.push(Entry::Pending(tc.clone(), tool, args)),
        }
        if cancel.is_cancelled() {
            break;
        }
    }
    let futures: Vec<_> = entries
        .into_iter()
        .map(|e| async move {
            match e {
                Entry::Done(f) => f,
                Entry::Pending(tc, tool, args) => {
                    if cancel.is_cancelled() {
                        let f = Finalized { tool_call: tc, result: error_result("Operation aborted"), is_error: true };
                        emit_end(&f, emit).await;
                        return f;
                    }
                    let (r, err) = execute_ready(&tool, &tc, &args, cancel, emit).await;
                    let f = finalize(assistant, &tc, &args, r, err, hooks, cancel).await;
                    emit_end(&f, emit).await;
                    f
                }
            }
        })
        .collect();
    let finalized = futures::future::join_all(futures).await;
    let mut messages = Vec::new();
    for f in &finalized {
        let m = to_message(f);
        emit_result_message(&m, emit).await;
        messages.push(m);
    }
    Batch { terminate: should_terminate(&finalized), messages }
}

fn should_terminate(finalized: &[Finalized]) -> bool {
    !finalized.is_empty() && finalized.iter().all(|f| f.result.terminate)
}

async fn emit_end(f: &Finalized, emit: &EventSink) {
    emit(AgentEvent::ToolExecutionEnd { tool_call_id: f.tool_call.id.clone(), tool_name: f.tool_call.name.clone(), result: f.result.clone(), is_error: f.is_error }).await;
}

fn to_message(f: &Finalized) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: f.tool_call.id.clone(),
        tool_name: f.tool_call.name.clone(),
        content: f.result.content.clone(),
        details: f.result.details.clone(),
        usage: f.result.usage.clone(),
        is_error: f.is_error,
        timestamp: now_ms(),
    }
}

async fn emit_result_message(m: &ToolResultMessage, emit: &EventSink) {
    emit(AgentEvent::MessageStart { message: AgentMessage::ToolResult(m.clone()) }).await;
    emit(AgentEvent::MessageEnd { message: AgentMessage::ToolResult(m.clone()) }).await;
}
