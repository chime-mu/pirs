//! Anthropic Messages API provider (streaming).

use crate::sse::SseStream;
use crate::types::*;
use futures::StreamExt;
use serde_json::{json, Value};
use std::collections::HashMap;

pub const API: &str = "anthropic-messages";

/// Claude Code version reported in the user agent for OAuth requests.
pub const CLAUDE_CODE_VERSION: &str = "2.1.251";
const CLAUDE_CODE_IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
/// Claude Code tool names (canonical casing). OAuth requests must use these
/// names for matching tools; responses are mapped back to our names.
const CLAUDE_CODE_TOOLS: &[&str] = &[
    "Read", "Write", "Edit", "Bash", "Grep", "Glob", "AskUserQuestion", "EnterPlanMode", "ExitPlanMode", "KillShell", "NotebookEdit", "Skill", "Task", "TaskOutput", "TodoWrite", "WebFetch", "WebSearch",
];

pub fn to_claude_code_name(name: &str) -> String {
    CLAUDE_CODE_TOOLS.iter().find(|t| t.eq_ignore_ascii_case(name)).map(|t| t.to_string()).unwrap_or_else(|| name.to_string())
}

fn from_claude_code_name(name: &str, tools: &[Tool]) -> String {
    tools.iter().find(|t| t.name.eq_ignore_ascii_case(name)).map(|t| t.name.clone()).unwrap_or_else(|| name.to_string())
}

fn convert_content_for_user(blocks: &[Content]) -> Vec<Value> {
    blocks
        .iter()
        .filter_map(|c| match c {
            Content::Text { text, .. } => Some(json!({"type": "text", "text": text})),
            Content::Image { data, mime_type } => Some(json!({
                "type": "image",
                "source": {"type": "base64", "media_type": mime_type, "data": data}
            })),
            _ => None,
        })
        .collect()
}

fn convert_messages(messages: &[Message], supports_images: bool, oauth: bool) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for m in messages {
        match m {
            Message::System(_) => {}
            Message::User(u) => {
                let mut blocks = convert_content_for_user(&u.content.blocks());
                if !supports_images {
                    blocks.retain(|b| b["type"] != "image");
                }
                if blocks.is_empty() {
                    blocks.push(json!({"type": "text", "text": "(empty)"}));
                }
                out.push(json!({"role": "user", "content": blocks}));
            }
            Message::Assistant(a) => {
                let mut blocks = Vec::new();
                for c in &a.content {
                    match c {
                        Content::Text { text, .. } => {
                            if !text.trim().is_empty() {
                                blocks.push(json!({"type": "text", "text": text}));
                            }
                        }
                        Content::Thinking { thinking, thinking_signature, redacted } => {
                            if *redacted {
                                if let Some(sig) = thinking_signature {
                                    blocks.push(json!({"type": "redacted_thinking", "data": sig}));
                                }
                            } else if let Some(sig) = thinking_signature.as_ref().filter(|s| !s.is_empty()) {
                                blocks.push(json!({"type": "thinking", "thinking": thinking, "signature": sig}));
                            }
                            // Thinking without a signature cannot be replayed; drop it.
                        }
                        Content::ToolCall { id, name, arguments, .. } => {
                            let name = if oauth { to_claude_code_name(name) } else { name.clone() };
                            blocks.push(json!({"type": "tool_use", "id": id, "name": name, "input": arguments}));
                        }
                        Content::Image { .. } => {}
                    }
                }
                if blocks.is_empty() {
                    blocks.push(json!({"type": "text", "text": "(empty)"}));
                }
                out.push(json!({"role": "assistant", "content": blocks}));
            }
            Message::ToolResult(t) => {
                let mut inner = convert_content_for_user(&t.content);
                if !supports_images {
                    inner.retain(|b| b["type"] != "image");
                }
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": t.tool_call_id,
                    "content": inner,
                    "is_error": t.is_error,
                });
                // Consecutive tool results must be merged into one user message.
                if let Some(last) = out.last_mut() {
                    if last["role"] == "user"
                        && last["content"]
                            .as_array()
                            .map(|a| a.iter().all(|b| b["type"] == "tool_result"))
                            .unwrap_or(false)
                    {
                        last["content"].as_array_mut().unwrap().push(block);
                        continue;
                    }
                }
                out.push(json!({"role": "user", "content": [block]}));
            }
        }
    }
    out
}

pub fn build_request(model: &Model, context: &Context, options: &StreamOptions) -> Value {
    let oauth = options.api_key.as_deref().map(crate::oauth::is_oauth_token).unwrap_or(false);
    let mut body = json!({
        "model": model.id,
        "messages": convert_messages(&context.messages, model.supports_images(), oauth),
        "max_tokens": options.max_tokens.unwrap_or(model.max_tokens),
        "stream": true,
    });
    let mut system: Vec<Value> = Vec::new();
    if oauth {
        // OAuth tokens are only accepted with the Claude Code identity as the first system block.
        system.push(json!({"type": "text", "text": CLAUDE_CODE_IDENTITY, "cache_control": {"type": "ephemeral"}}));
    }
    if let Some(sp) = context.system_prompt.as_ref().filter(|s| !s.is_empty()) {
        system.push(json!({"type": "text", "text": sp, "cache_control": {"type": "ephemeral"}}));
    }
    if !system.is_empty() {
        body["system"] = Value::Array(system);
    }
    if !context.tools.is_empty() {
        let mut tools: Vec<Value> = context
            .tools
            .iter()
            .map(|t| json!({"name": if oauth { to_claude_code_name(&t.name) } else { t.name.clone() }, "description": t.description, "input_schema": t.parameters}))
            .collect();
        if let Some(last) = tools.last_mut() {
            last["cache_control"] = json!({"type": "ephemeral"});
        }
        body["tools"] = Value::Array(tools);
    }
    let mut thinking = false;
    if model.reasoning {
        if let Some(budget) = options.reasoning.budget_tokens() {
            let max_tokens = body["max_tokens"].as_u64().unwrap_or(model.max_tokens);
            let budget = budget.min(max_tokens.saturating_sub(1024)).max(1024);
            body["thinking"] = json!({"type": "enabled", "budget_tokens": budget});
            thinking = true;
        }
    }
    if let (Some(t), false) = (options.temperature, thinking) {
        body["temperature"] = json!(t);
    }
    body
}

fn map_stop_reason(reason: &str) -> StopReason {
    match reason {
        "end_turn" | "stop_sequence" | "pause_turn" => StopReason::Stop,
        "max_tokens" => StopReason::Length,
        "tool_use" => StopReason::ToolUse,
        "refusal" => StopReason::Error,
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

    let Some(api_key) = options.api_key.clone().filter(|k| !k.is_empty()) else {
        emit(AssistantMessageEvent::Error {
            reason: StopReason::Error,
            error: AssistantMessage::error(&model, format!("No API key for provider '{}'. Set ANTHROPIC_API_KEY.", model.provider), false),
        });
        return;
    };

    let mut body = build_request(&model, &context, &options);
    if let Some(hook) = &options.on_payload {
        if let Some(replacement) = hook(body.clone()).await {
            body = replacement;
        }
    }

    let url = format!("{}/v1/messages", model.base_url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let mut req = client
        .post(&url)
        .header("content-type", "application/json")
        .header("anthropic-version", "2023-06-01")
        .header("accept", "text/event-stream");
    let oauth = crate::oauth::is_oauth_token(&api_key);
    let mut betas: Vec<&str> = Vec::new();
    if oauth {
        req = req
            .header("authorization", format!("Bearer {api_key}"))
            .header("user-agent", format!("claude-cli/{CLAUDE_CODE_VERSION}"))
            .header("x-app", "cli")
            .header("anthropic-dangerous-direct-browser-access", "true");
        betas.extend(["claude-code-20250219", "oauth-2025-04-20"]);
    } else {
        req = req.header("x-api-key", api_key);
    }
    if body.get("thinking").is_some() {
        betas.push("interleaved-thinking-2025-05-14");
    }
    if !betas.is_empty() {
        req = req.header("anthropic-beta", betas.join(","));
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

    let send = req.json(&body).send();
    let response = tokio::select! {
        r = send => r,
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

    let mut sse = SseStream::new(response.bytes_stream());
    emit(AssistantMessageEvent::Start { partial: partial.clone() });
    // index in the API stream -> index in partial.content
    let mut index_map: HashMap<u64, usize> = HashMap::new();
    let mut tool_json: HashMap<usize, String> = HashMap::new();
    let mut stop_reason: Option<StopReason> = None;

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
        let data: Value = match serde_json::from_str(&ev.data) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let kind = data["type"].as_str().unwrap_or("");
        match kind {
            "message_start" => {
                let msg = &data["message"];
                if let Some(id) = msg["id"].as_str() {
                    partial.response_id = Some(id.to_string());
                }
                let u = &msg["usage"];
                partial.usage.input = u["input_tokens"].as_u64().unwrap_or(0);
                partial.usage.cache_read = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
                partial.usage.cache_write = u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                partial.usage.output = u["output_tokens"].as_u64().unwrap_or(0);
                partial.usage.compute_cost(&model.cost);
            }
            "content_block_start" => {
                let idx = data["index"].as_u64().unwrap_or(0);
                let block = &data["content_block"];
                let ci = partial.content.len();
                index_map.insert(idx, ci);
                match block["type"].as_str().unwrap_or("") {
                    "text" => {
                        partial.content.push(Content::text(block["text"].as_str().unwrap_or("")));
                        emit(AssistantMessageEvent::TextStart { content_index: ci, partial: partial.clone() });
                    }
                    "thinking" => {
                        partial.content.push(Content::Thinking {
                            thinking: block["thinking"].as_str().unwrap_or("").to_string(),
                            thinking_signature: Some(block["signature"].as_str().unwrap_or("").to_string()),
                            redacted: false,
                        });
                        emit(AssistantMessageEvent::ThinkingStart { content_index: ci, partial: partial.clone() });
                    }
                    "redacted_thinking" => {
                        partial.content.push(Content::Thinking {
                            thinking: "[Reasoning redacted]".into(),
                            thinking_signature: block["data"].as_str().map(|s| s.to_string()),
                            redacted: true,
                        });
                        emit(AssistantMessageEvent::ThinkingStart { content_index: ci, partial: partial.clone() });
                    }
                    "tool_use" => {
                        partial.content.push(Content::ToolCall {
                            id: block["id"].as_str().unwrap_or("").to_string(),
                            name: from_claude_code_name(block["name"].as_str().unwrap_or(""), &context.tools),
                            arguments: json!({}),
                            thought_signature: None,
                        });
                        tool_json.insert(ci, String::new());
                        emit(AssistantMessageEvent::ToolCallStart { content_index: ci, partial: partial.clone() });
                    }
                    _ => {
                        // Unknown block; keep indices aligned with a placeholder.
                        partial.content.push(Content::text(""));
                    }
                }
            }
            "content_block_delta" => {
                let idx = data["index"].as_u64().unwrap_or(0);
                let Some(&ci) = index_map.get(&idx) else { continue };
                let delta = &data["delta"];
                match delta["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        let d = delta["text"].as_str().unwrap_or("").to_string();
                        if let Some(Content::Text { text, .. }) = partial.content.get_mut(ci) {
                            text.push_str(&d);
                        }
                        emit(AssistantMessageEvent::TextDelta { content_index: ci, delta: d, partial: partial.clone() });
                    }
                    "thinking_delta" => {
                        let d = delta["thinking"].as_str().unwrap_or("").to_string();
                        if let Some(Content::Thinking { thinking, .. }) = partial.content.get_mut(ci) {
                            thinking.push_str(&d);
                        }
                        emit(AssistantMessageEvent::ThinkingDelta { content_index: ci, delta: d, partial: partial.clone() });
                    }
                    "signature_delta" => {
                        let d = delta["signature"].as_str().unwrap_or("");
                        if let Some(Content::Thinking { thinking_signature, .. }) = partial.content.get_mut(ci) {
                            thinking_signature.get_or_insert_with(String::new).push_str(d);
                        }
                    }
                    "input_json_delta" => {
                        let d = delta["partial_json"].as_str().unwrap_or("").to_string();
                        tool_json.entry(ci).or_default().push_str(&d);
                        emit(AssistantMessageEvent::ToolCallDelta { content_index: ci, delta: d, partial: partial.clone() });
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let idx = data["index"].as_u64().unwrap_or(0);
                let Some(&ci) = index_map.get(&idx) else { continue };
                match partial.content.get_mut(ci) {
                    Some(Content::Text { text, .. }) => {
                        let content = text.clone();
                        emit(AssistantMessageEvent::TextEnd { content_index: ci, content, partial: partial.clone() });
                    }
                    Some(Content::Thinking { thinking, .. }) => {
                        let content = thinking.clone();
                        emit(AssistantMessageEvent::ThinkingEnd { content_index: ci, content, partial: partial.clone() });
                    }
                    Some(Content::ToolCall { id, name, arguments, .. }) => {
                        let raw = tool_json.remove(&ci).unwrap_or_default();
                        let parsed: Value = if raw.trim().is_empty() {
                            json!({})
                        } else {
                            serde_json::from_str(&raw).unwrap_or_else(|_| crate::json_salvage::salvage(&raw))
                        };
                        *arguments = parsed.clone();
                        let tc = ToolCallRef { id: id.clone(), name: name.clone(), arguments: parsed };
                        emit(AssistantMessageEvent::ToolCallEnd { content_index: ci, tool_call: tc, partial: partial.clone() });
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(sr) = data["delta"]["stop_reason"].as_str() {
                    partial.raw_stop_reason = Some(sr.to_string());
                    stop_reason = Some(map_stop_reason(sr));
                }
                let u = &data["usage"];
                if let Some(o) = u["output_tokens"].as_u64() {
                    partial.usage.output = o;
                }
                if let Some(i) = u["input_tokens"].as_u64() {
                    partial.usage.input = i;
                }
                if let Some(c) = u["cache_read_input_tokens"].as_u64() {
                    partial.usage.cache_read = c;
                }
                if let Some(c) = u["cache_creation_input_tokens"].as_u64() {
                    partial.usage.cache_write = c;
                }
                partial.usage.compute_cost(&model.cost);
            }
            "message_stop" => break,
            "error" => {
                let msg = data["error"]["message"].as_str().unwrap_or("Unknown error").to_string();
                partial.stop_reason = StopReason::Error;
                partial.error_message = Some(msg);
                emit(AssistantMessageEvent::Error { reason: StopReason::Error, error: partial.clone() });
                return;
            }
            _ => {}
        }
    }

    let reason = stop_reason.unwrap_or_else(|| {
        if partial.content.iter().any(Content::is_tool_call) { StopReason::ToolUse } else { StopReason::Stop }
    });
    if reason == StopReason::Error {
        partial.stop_reason = StopReason::Error;
        partial.error_message = Some(format!("Provider stopped with reason {:?}", partial.raw_stop_reason));
        emit(AssistantMessageEvent::Error { reason: StopReason::Error, error: partial.clone() });
        return;
    }
    partial.stop_reason = reason;
    emit(AssistantMessageEvent::Done { reason, message: partial });
}
