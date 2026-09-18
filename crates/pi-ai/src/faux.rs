//! A scripted provider for tests and offline demos.
//!
//! The script is a JSON array of responses. Each response is either a string
//! (assistant text) or an object `{ "text"?: string, "toolCalls"?: [{name, arguments}] }`.
//! Responses are consumed in order; when the script runs out, the provider
//! echoes the last user message.
//!
//! Script sources, in priority order: `StreamOptions` cannot carry it, so the
//! provider reads `PIRS_FAUX_SCRIPT` (path to a JSON file) or falls back to
//! echo mode. The cursor is kept in a process-wide counter so multi-turn runs
//! advance through the script.

use crate::types::*;
use futures::StreamExt;
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

pub const API: &str = "faux";

static CURSOR: AtomicUsize = AtomicUsize::new(0);
static SCRIPT: OnceLock<Mutex<Option<Vec<Value>>>> = OnceLock::new();

/// Install a script programmatically (overrides the env var).
pub fn set_script(script: Vec<Value>) {
    let cell = SCRIPT.get_or_init(|| Mutex::new(None));
    *cell.lock().unwrap() = Some(script);
    CURSOR.store(0, Ordering::SeqCst);
}

fn load_script() -> Vec<Value> {
    let cell = SCRIPT.get_or_init(|| Mutex::new(None));
    let mut guard = cell.lock().unwrap();
    if let Some(s) = guard.as_ref() {
        return s.clone();
    }
    let script = std::env::var("PIRS_FAUX_SCRIPT")
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<Vec<Value>>(&s).ok())
        .unwrap_or_default();
    *guard = Some(script.clone());
    script
}

fn next_response(context: &Context) -> (String, Vec<(String, Value)>) {
    let script = load_script();
    let i = CURSOR.fetch_add(1, Ordering::SeqCst);
    if let Some(step) = script.get(i) {
        match step {
            Value::String(s) => return (s.clone(), vec![]),
            Value::Object(o) => {
                let text = o.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
                let calls = o
                    .get("toolCalls")
                    .and_then(|c| c.as_array())
                    .map(|arr| {
                        arr.iter()
                            .map(|c| {
                                (
                                    c["name"].as_str().unwrap_or("").to_string(),
                                    c.get("arguments").cloned().unwrap_or(Value::Object(Default::default())),
                                )
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                return (text, calls);
            }
            _ => {}
        }
    }
    // Echo mode.
    let last = context.messages.iter().rev().find_map(|m| match m {
        Message::User(u) => Some(u.content.plain_text()),
        Message::ToolResult(t) => Some(format!(
            "Tool {} returned: {}",
            t.tool_name,
            t.content.iter().filter_map(|c| c.as_text()).collect::<Vec<_>>().join("")
        )),
        _ => None,
    });
    (format!("(faux) {}", last.unwrap_or_default()), vec![])
}

pub fn stream(model: Model, context: Context, options: StreamOptions) -> EventStream {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<AssistantMessageEvent>();
    tokio::spawn(async move {
        let emit = |ev: AssistantMessageEvent| {
            let _ = tx.send(ev);
        };
        let cancel = options.cancel.clone().unwrap_or_default();
        let mut partial = AssistantMessage::new(&model);
        emit(AssistantMessageEvent::Start { partial: partial.clone() });
        let (text, calls) = next_response(&context);
        if !text.is_empty() {
            let ci = partial.content.len();
            partial.content.push(Content::text(""));
            emit(AssistantMessageEvent::TextStart { content_index: ci, partial: partial.clone() });
            for word in text.split_inclusive(' ') {
                if cancel.is_cancelled() {
                    partial.stop_reason = StopReason::Aborted;
                    partial.error_message = Some("Request aborted".into());
                    emit(AssistantMessageEvent::Error { reason: StopReason::Aborted, error: partial.clone() });
                    return;
                }
                if let Some(Content::Text { text, .. }) = partial.content.get_mut(ci) {
                    text.push_str(word);
                }
                emit(AssistantMessageEvent::TextDelta { content_index: ci, delta: word.to_string(), partial: partial.clone() });
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            emit(AssistantMessageEvent::TextEnd { content_index: ci, content: text.clone(), partial: partial.clone() });
        }
        for (n, (name, args)) in calls.iter().enumerate() {
            let ci = partial.content.len();
            let id = format!("faux_call_{}_{}", CURSOR.load(Ordering::SeqCst), n);
            partial.content.push(Content::ToolCall { id: id.clone(), name: name.clone(), arguments: args.clone(), thought_signature: None });
            emit(AssistantMessageEvent::ToolCallStart { content_index: ci, partial: partial.clone() });
            emit(AssistantMessageEvent::ToolCallEnd {
                content_index: ci,
                tool_call: ToolCallRef { id, name: name.clone(), arguments: args.clone() },
                partial: partial.clone(),
            });
        }
        partial.usage.input = context.messages.len() as u64 * 10;
        partial.usage.output = text.len() as u64 / 4;
        partial.usage.compute_cost(&model.cost);
        let reason = if calls.is_empty() { StopReason::Stop } else { StopReason::ToolUse };
        partial.stop_reason = reason;
        emit(AssistantMessageEvent::Done { reason, message: partial });
    });
    tokio_stream::wrappers::UnboundedReceiverStream::new(rx).boxed()
}
