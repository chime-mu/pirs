//! Provider-independent transcript repair applied before a request is built.
//! Mirrors the second pass of pi's `transformMessages` (`packages/ai/src/api/transform-messages.ts`).
//!
//! Two things can leave a transcript in a shape every provider rejects:
//! - an assistant message that ended in `error`/`aborted` (partial text, tool calls with
//!   truncated arguments, thinking without a signature) — replaying it fails validation;
//! - a `toolCall` whose result never arrived (the run stopped before the tool executed) —
//!   Anthropic requires a `tool_result` for every `tool_use` in the very next message.
//!
//! Errored/aborted assistant messages are dropped, and every tool call that is still
//! unanswered when the next assistant/user message starts (or the transcript ends) gets a
//! synthetic error result so the model sees "no result" instead of the request failing.

use crate::types::*;

pub const NO_RESULT_TEXT: &str = "No result provided";

pub fn transform_messages(messages: &[Message]) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::with_capacity(messages.len());
    let mut pending: Vec<(String, String)> = Vec::new();
    let mut answered: std::collections::HashSet<String> = Default::default();
    // System messages between a tool call and its results are held back and emitted after
    // the results so they never split a tool_use from its tool_result.
    let mut held_system: Vec<Message> = Vec::new();

    fn close(out: &mut Vec<Message>, pending: &mut Vec<(String, String)>, answered: &mut std::collections::HashSet<String>, held: &mut Vec<Message>) {
        for (id, name) in pending.drain(..) {
            if !answered.contains(&id) {
                out.push(Message::ToolResult(ToolResultMessage {
                    tool_call_id: id,
                    tool_name: name,
                    content: vec![Content::text(NO_RESULT_TEXT)],
                    details: None,
                    usage: None,
                    is_error: true,
                    timestamp: now_ms(),
                }));
            }
        }
        answered.clear();
        out.append(held);
    }

    for m in messages {
        match m {
            Message::Assistant(a) => {
                close(&mut out, &mut pending, &mut answered, &mut held_system);
                if matches!(a.stop_reason, StopReason::Error | StopReason::Aborted) {
                    continue;
                }
                let calls: Vec<(String, String)> = a
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        Content::ToolCall { id, name, .. } => Some((id.clone(), name.clone())),
                        _ => None,
                    })
                    .collect();
                if !calls.is_empty() {
                    pending = calls;
                }
                out.push(m.clone());
            }
            Message::ToolResult(t) => {
                answered.insert(t.tool_call_id.clone());
                out.push(m.clone());
            }
            Message::System(_) => {
                if pending.is_empty() {
                    out.push(m.clone());
                } else {
                    held_system.push(m.clone());
                }
            }
            Message::User(_) => {
                close(&mut out, &mut pending, &mut answered, &mut held_system);
                out.push(m.clone());
            }
        }
    }
    close(&mut out, &mut pending, &mut answered, &mut held_system);
    out
}
