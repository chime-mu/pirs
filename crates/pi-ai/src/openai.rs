//! OpenAI Chat Completions provider (streaming). Also used for
//! OpenAI-compatible servers (OpenRouter, Groq, llama.cpp, vLLM, ...).

use crate::sse::SseStream;
use crate::types::*;
use futures::StreamExt;
use serde_json::{json, Value};
use std::collections::HashMap;

pub const API: &str = "openai-completions";

fn user_blocks(blocks: &[Content], supports_images: bool) -> Value {
    let all_text = blocks.iter().all(|b| matches!(b, Content::Text { .. }));
    if all_text {
        return Value::String(blocks.iter().filter_map(|b| b.as_text()).collect::<Vec<_>>().join("\n"));
    }
    let arr: Vec<Value> = blocks
        .iter()
        .filter_map(|b| match b {
            Content::Text { text, .. } => Some(json!({"type": "text", "text": text})),
            Content::Image { data, mime_type } if supports_images => Some(json!({
                "type": "image_url",
                "image_url": {"url": format!("data:{mime_type};base64,{data}")}
            })),
            _ => None,
        })
        .collect();
    Value::Array(arr)
}

fn convert_messages(context: &Context, model: &Model) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(sp) = context.system_prompt.as_ref().filter(|s| !s.is_empty()) {
        let role = if is_openai_url(&model.base_url) { "developer" } else { "system" };
        out.push(json!({"role": role, "content": sp}));
    }
    for m in &context.messages {
        match m {
            Message::System(_) => {}
            Message::User(u) => {
                out.push(json!({"role": "user", "content": user_blocks(&u.content.blocks(), model.supports_images())}));
            }
            Message::Assistant(a) => {
                let text = a.text();
                let mut msg = json!({"role": "assistant"});
                msg["content"] = if text.is_empty() { Value::Null } else { Value::String(text) };
                let reasoning: String = a
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        Content::Thinking { thinking, redacted: false, .. } => Some(thinking.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if !reasoning.is_empty() && !is_openai_url(&model.base_url) {
                    msg["reasoning_content"] = Value::String(reasoning);
                }
                let calls: Vec<Value> = a
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        Content::ToolCall { id, name, arguments, .. } => Some(json!({
                            "id": id,
                            "type": "function",
                            "function": {"name": name, "arguments": arguments.to_string()}
                        })),
                        _ => None,
                    })
                    .collect();
                if !calls.is_empty() {
                    msg["tool_calls"] = Value::Array(calls);
                }
                out.push(msg);
            }
            Message::ToolResult(t) => {
                let text: String = t.content.iter().filter_map(|c| c.as_text()).collect::<Vec<_>>().join("\n");
                out.push(json!({"role": "tool", "tool_call_id": t.tool_call_id, "content": text}));
                // Images in tool results are sent as a follow-up user message.
                let images: Vec<Value> = t
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        Content::Image { data, mime_type } if model.supports_images() => Some(json!({
                            "type": "image_url", "image_url": {"url": format!("data:{mime_type};base64,{data}")}
                        })),
                        _ => None,
                    })
                    .collect();
                if !images.is_empty() {
                    out.push(json!({"role": "user", "content": images}));
                }
            }
        }
    }
    out
}

fn is_openai_url(url: &str) -> bool {
    url.contains("api.openai.com")
}

pub fn build_request(model: &Model, context: &Context, options: &StreamOptions) -> Value {
    let mut body = json!({
        "model": model.id,
        "messages": convert_messages(context, model),
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    let max_tokens = options.max_tokens.unwrap_or(model.max_tokens);
    if is_openai_url(&model.base_url) {
        body["max_completion_tokens"] = json!(max_tokens);
    } else {
        body["max_tokens"] = json!(max_tokens);
    }
    if !context.tools.is_empty() {
        body["tools"] = Value::Array(
            context
                .tools
                .iter()
                .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.parameters}}))
                .collect(),
        );
    }
    if model.reasoning {
        if let Some(effort) = options.reasoning.effort() {
            body["reasoning_effort"] = json!(effort);
        }
    } else if let Some(t) = options.temperature {
        body["temperature"] = json!(t);
    }
    body
}

fn map_finish_reason(reason: &str) -> StopReason {
    match reason {
        "stop" => StopReason::Stop,
        "length" => StopReason::Length,
        "tool_calls" | "function_call" => StopReason::ToolUse,
        "content_filter" => StopReason::Error,
        _ => StopReason::Stop,
    }
}

pub fn stream(model: Model, context: Context, options: StreamOptions) -> EventStream {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<AssistantMessageEvent>();
    tokio::spawn(async move {
        run(model, context, options, tx).await;
    });
    tokio_stream::wrappers::UnboundedReceiverStream::new(rx).boxed()
}

struct ToolAccum {
    content_index: usize,
    args: String,
}

async fn run(
    model: Model,
    context: Context,
    options: StreamOptions,
    tx: tokio::sync::mpsc::UnboundedSender<AssistantMessageEvent>,
) {
    let mut partial = AssistantMessage::new(&model);
    let emit = |ev: AssistantMessageEvent| {
        let _ = tx.send(ev);
    };

    let mut body = build_request(&model, &context, &options);
    if let Some(hook) = &options.on_payload {
        if let Some(replacement) = hook(body.clone()).await {
            body = replacement;
        }
    }

    let url = format!("{}/chat/completions", model.base_url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let mut req = client.post(&url).header("content-type", "application/json").header("accept", "text/event-stream");
    if let Some(key) = options.api_key.as_ref().filter(|k| !k.is_empty()) {
        req = req.header("authorization", format!("Bearer {key}"));
    }
    if let Some(h) = &model.headers {
        for (k, v) in h {
            req = req.header(k, v);
        }
    }
    for (k, v) in &options.headers {
        req = req.header(k, v);
    }
    let cancel = options.cancel.clone().unwrap_or_default();
    let response = tokio::select! {
        r = req.json(&body).send() => r,
        _ = cancel.cancelled() => {
            emit(AssistantMessageEvent::Error { reason: StopReason::Aborted, error: AssistantMessage::error(&model, "Request aborted", true) });
            return;
        }
    };
    let response = match response {
        Ok(r) => r,
        Err(e) => {
            emit(AssistantMessageEvent::Error { reason: StopReason::Error, error: AssistantMessage::error(&model, format!("Request failed: {e}"), false) });
            return;
        }
    };
    let status = response.status().as_u16();
    if let Some(hook) = &options.on_response {
        let headers: HashMap<String, String> = response
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
            .collect();
        hook(status, headers).await;
    }
    if !response.status().is_success() {
        let text = response.text().await.unwrap_or_default();
        emit(AssistantMessageEvent::Error { reason: StopReason::Error, error: AssistantMessage::error(&model, format!("HTTP {status}: {text}"), false) });
        return;
    }

    emit(AssistantMessageEvent::Start { partial: partial.clone() });
    let mut sse = SseStream::new(response.bytes_stream());
    let mut text_index: Option<usize> = None;
    let mut thinking_index: Option<usize> = None;
    let mut tools: HashMap<u64, ToolAccum> = HashMap::new();
    let mut finish: Option<StopReason> = None;

    loop {
        let item = tokio::select! {
            i = sse.next() => i,
            _ = cancel.cancelled() => {
                partial.stop_reason = StopReason::Aborted;
                partial.error_message = Some("Request aborted".into());
                emit(AssistantMessageEvent::Error { reason: StopReason::Aborted, error: partial.clone() });
                return;
            }
        };
        let Some(item) = item else { break };
        let ev = match item {
            Ok(ev) => ev,
            Err(e) => {
                partial.stop_reason = StopReason::Error;
                partial.error_message = Some(format!("Stream error: {e}"));
                emit(AssistantMessageEvent::Error { reason: StopReason::Error, error: partial.clone() });
                return;
            }
        };
        if ev.data.trim() == "[DONE]" {
            break;
        }
        let data: Value = match serde_json::from_str(&ev.data) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(err) = data.get("error") {
            let msg = err["message"].as_str().unwrap_or(&err.to_string()).to_string();
            partial.stop_reason = StopReason::Error;
            partial.error_message = Some(msg);
            emit(AssistantMessageEvent::Error { reason: StopReason::Error, error: partial.clone() });
            return;
        }
        if let Some(id) = data["id"].as_str() {
            partial.response_id = Some(id.to_string());
        }
        if let Some(u) = data.get("usage").filter(|u| u.is_object()) {
            partial.usage.input = u["prompt_tokens"].as_u64().unwrap_or(0);
            partial.usage.output = u["completion_tokens"].as_u64().unwrap_or(0);
            partial.usage.cache_read = u["prompt_tokens_details"]["cached_tokens"]
                .as_u64()
                .or_else(|| u["prompt_cache_hit_tokens"].as_u64())
                .unwrap_or(0);
            partial.usage.input = partial.usage.input.saturating_sub(partial.usage.cache_read);
            partial.usage.reasoning = u["completion_tokens_details"]["reasoning_tokens"].as_u64();
            partial.usage.compute_cost(&model.cost);
        }
        let Some(choice) = data["choices"].as_array().and_then(|c| c.first()) else { continue };
        if let Some(fr) = choice["finish_reason"].as_str() {
            partial.raw_stop_reason = Some(fr.to_string());
            finish = Some(map_finish_reason(fr));
        }
        let delta = &choice["delta"];
        for field in ["reasoning_content", "reasoning", "reasoning_text"] {
            if let Some(r) = delta[field].as_str().filter(|s| !s.is_empty()) {
                let ci = match thinking_index {
                    Some(i) => i,
                    None => {
                        let i = partial.content.len();
                        partial.content.push(Content::Thinking { thinking: String::new(), thinking_signature: None, redacted: false });
                        thinking_index = Some(i);
                        emit(AssistantMessageEvent::ThinkingStart { content_index: i, partial: partial.clone() });
                        i
                    }
                };
                if let Some(Content::Thinking { thinking, .. }) = partial.content.get_mut(ci) {
                    thinking.push_str(r);
                }
                emit(AssistantMessageEvent::ThinkingDelta { content_index: ci, delta: r.to_string(), partial: partial.clone() });
                break;
            }
        }
        if let Some(t) = delta["content"].as_str().filter(|s| !s.is_empty()) {
            if let Some(ti) = thinking_index.take() {
                if let Some(Content::Thinking { thinking, .. }) = partial.content.get(ti) {
                    emit(AssistantMessageEvent::ThinkingEnd { content_index: ti, content: thinking.clone(), partial: partial.clone() });
                }
            }
            let ci = match text_index {
                Some(i) => i,
                None => {
                    let i = partial.content.len();
                    partial.content.push(Content::text(""));
                    text_index = Some(i);
                    emit(AssistantMessageEvent::TextStart { content_index: i, partial: partial.clone() });
                    i
                }
            };
            if let Some(Content::Text { text, .. }) = partial.content.get_mut(ci) {
                text.push_str(t);
            }
            emit(AssistantMessageEvent::TextDelta { content_index: ci, delta: t.to_string(), partial: partial.clone() });
        }
        if let Some(calls) = delta["tool_calls"].as_array() {
            for tc in calls {
                let idx = tc["index"].as_u64().unwrap_or(0);
                let entry = match tools.get_mut(&idx) {
                    Some(e) => e,
                    None => {
                        if let Some(ti) = text_index.take() {
                            if let Some(Content::Text { text, .. }) = partial.content.get(ti) {
                                emit(AssistantMessageEvent::TextEnd { content_index: ti, content: text.clone(), partial: partial.clone() });
                            }
                        }
                        let ci = partial.content.len();
                        partial.content.push(Content::ToolCall {
                            id: tc["id"].as_str().unwrap_or(&format!("call_{idx}")).to_string(),
                            name: tc["function"]["name"].as_str().unwrap_or("").to_string(),
                            arguments: json!({}),
                            thought_signature: None,
                        });
                        emit(AssistantMessageEvent::ToolCallStart { content_index: ci, partial: partial.clone() });
                        tools.insert(idx, ToolAccum { content_index: ci, args: String::new() });
                        tools.get_mut(&idx).unwrap()
                    }
                };
                if let Some(id) = tc["id"].as_str() {
                    if let Some(Content::ToolCall { id: cid, .. }) = partial.content.get_mut(entry.content_index) {
                        if cid.is_empty() {
                            *cid = id.to_string();
                        }
                    }
                }
                if let Some(n) = tc["function"]["name"].as_str() {
                    if let Some(Content::ToolCall { name, .. }) = partial.content.get_mut(entry.content_index) {
                        if name.is_empty() {
                            *name = n.to_string();
                        }
                    }
                }
                if let Some(a) = tc["function"]["arguments"].as_str() {
                    entry.args.push_str(a);
                    emit(AssistantMessageEvent::ToolCallDelta { content_index: entry.content_index, delta: a.to_string(), partial: partial.clone() });
                }
            }
        }
    }

    if let Some(ti) = thinking_index.take() {
        if let Some(Content::Thinking { thinking, .. }) = partial.content.get(ti) {
            emit(AssistantMessageEvent::ThinkingEnd { content_index: ti, content: thinking.clone(), partial: partial.clone() });
        }
    }
    if let Some(ti) = text_index.take() {
        if let Some(Content::Text { text, .. }) = partial.content.get(ti) {
            emit(AssistantMessageEvent::TextEnd { content_index: ti, content: text.clone(), partial: partial.clone() });
        }
    }
    let mut tool_entries: Vec<(u64, ToolAccum)> = tools.into_iter().collect();
    tool_entries.sort_by_key(|(i, _)| *i);
    for (_, acc) in tool_entries {
        let parsed: Value = if acc.args.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&acc.args).unwrap_or_else(|_| crate::json_salvage::salvage(&acc.args))
        };
        if let Some(Content::ToolCall { id, name, arguments, .. }) = partial.content.get_mut(acc.content_index) {
            *arguments = parsed.clone();
            let tc = ToolCallRef { id: id.clone(), name: name.clone(), arguments: parsed };
            emit(AssistantMessageEvent::ToolCallEnd { content_index: acc.content_index, tool_call: tc, partial: partial.clone() });
        }
    }

    let reason = match finish {
        Some(r) => r,
        None => {
            if partial.content.iter().any(Content::is_tool_call) { StopReason::ToolUse } else { StopReason::Stop }
        }
    };
    if reason == StopReason::Error {
        partial.stop_reason = StopReason::Error;
        partial.error_message = Some(format!("Provider stopped with reason {:?}", partial.raw_stop_reason));
        emit(AssistantMessageEvent::Error { reason: StopReason::Error, error: partial.clone() });
        return;
    }
    partial.stop_reason = reason;
    emit(AssistantMessageEvent::Done { reason, message: partial });
}
