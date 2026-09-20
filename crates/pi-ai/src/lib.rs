//! pi-ai: unified multi-provider LLM streaming API (Rust port of `@earendil-works/pi-ai`).

#![deny(unreachable_pub)]

pub mod anthropic;
pub mod faux;
pub mod json_salvage;
pub mod oauth;
pub mod openai;
pub mod registry;
pub mod sse;
pub mod transform;
pub mod types;

pub use registry::{complete, stream, ModelRegistry, ProviderConfig, ProviderModelConfig};
pub use transform::transform_messages;
pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn message_json_matches_pi_shapes() {
        let m = Message::user("hi");
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["role"], "user");
        assert_eq!(v["content"], "hi");

        let tr = Message::ToolResult(ToolResultMessage {
            tool_call_id: "c1".into(),
            tool_name: "bash".into(),
            content: vec![Content::text("out")],
            details: None,
            usage: None,
            is_error: false,
            timestamp: 1,
        });
        let v = serde_json::to_value(&tr).unwrap();
        assert_eq!(v["role"], "toolResult");
        assert_eq!(v["toolCallId"], "c1");
        assert_eq!(v["isError"], false);

        let c = Content::ToolCall { id: "x".into(), name: "read".into(), arguments: json!({"path": "a"}), thought_signature: None };
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["type"], "toolCall");

        // Round-trip a pi-style assistant message.
        let raw = json!({"role":"assistant","content":[{"type":"text","text":"Hi!"}],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0,"totalTokens":3,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":1733234402000u64});
        let m: Message = serde_json::from_value(raw).unwrap();
        assert!(matches!(m, Message::Assistant(_)));
    }

    #[test]
    fn anthropic_request_shape() {
        let reg = ModelRegistry::with_builtins();
        let model = reg.get("anthropic", "claude-sonnet-4-5").unwrap();
        let ctx = Context {
            system_prompt: Some("sys".into()),
            messages: vec![
                Message::user("hello"),
                Message::Assistant(AssistantMessage {
                    content: vec![Content::ToolCall { id: "t1".into(), name: "read".into(), arguments: json!({"path":"x"}), thought_signature: None }],
                    ..AssistantMessage::new(&model)
                }),
                Message::ToolResult(ToolResultMessage { tool_call_id: "t1".into(), tool_name: "read".into(), content: vec![Content::text("data")], details: None, usage: None, is_error: false, timestamp: 0 }),
            ],
            tools: vec![Tool { name: "read".into(), description: "d".into(), parameters: json!({"type":"object"}) }],
        };
        let body = anthropic::build_request(&model, &ctx, &StreamOptions { reasoning: ThinkingLevel::Medium, ..Default::default() });
        assert_eq!(body["system"][0]["text"], "sys");
        assert_eq!(body["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(body["messages"][2]["content"][0]["type"], "tool_result");
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
    }

    #[test]
    fn anthropic_oauth_request_uses_claude_code_conventions() {
        let reg = ModelRegistry::with_builtins();
        let model = reg.get("anthropic", "claude-sonnet-4-5").unwrap();
        let ctx = Context {
            system_prompt: Some("sys".into()),
            messages: vec![
                Message::user("hello"),
                Message::Assistant(AssistantMessage {
                    content: vec![Content::ToolCall { id: "t1".into(), name: "bash".into(), arguments: json!({"command":"ls"}), thought_signature: None }],
                    ..AssistantMessage::new(&model)
                }),
                Message::ToolResult(ToolResultMessage { tool_call_id: "t1".into(), tool_name: "bash".into(), content: vec![Content::text("ok")], details: None, usage: None, is_error: false, timestamp: 0 }),
            ],
            tools: vec![
                Tool { name: "bash".into(), description: "d".into(), parameters: json!({"type":"object"}) },
                Tool { name: "hello".into(), description: "d".into(), parameters: json!({"type":"object"}) },
            ],
        };
        let opts = StreamOptions { api_key: Some("sk-ant-oat01-token".into()), ..Default::default() };
        let body = anthropic::build_request(&model, &ctx, &opts);
        assert_eq!(body["system"][0]["text"], "You are Claude Code, Anthropic's official CLI for Claude.");
        assert_eq!(body["system"][1]["text"], "sys");
        assert_eq!(body["tools"][0]["name"], "Bash");
        assert_eq!(body["tools"][1]["name"], "hello");
        assert_eq!(body["messages"][1]["content"][0]["name"], "Bash");
        // Plain API keys keep our names and no identity block.
        let body = anthropic::build_request(&model, &ctx, &StreamOptions { api_key: Some("sk-ant-api03-x".into()), ..Default::default() });
        assert_eq!(body["system"][0]["text"], "sys");
        assert_eq!(body["tools"][0]["name"], "bash");
    }

    #[test]
    fn errored_assistant_with_tool_call_is_not_replayed() {
        // Regression: a stream that died mid-tool-call ("Stream error: error decoding response
        // body") left an assistant message with stop_reason=error and a tool_use in context; the
        // next prompt then failed with HTTP 400 "tool_use ids were found without tool_result".
        let reg = ModelRegistry::with_builtins();
        let model = reg.get("anthropic", "claude-sonnet-4-5").unwrap();
        let ctx = Context {
            system_prompt: None,
            messages: vec![
                Message::user("do it"),
                Message::Assistant(AssistantMessage {
                    content: vec![Content::ToolCall { id: "t1".into(), name: "bash".into(), arguments: json!({"command":"ls"}), thought_signature: None }],
                    stop_reason: StopReason::ToolUse,
                    ..AssistantMessage::new(&model)
                }),
                Message::ToolResult(ToolResultMessage { tool_call_id: "t1".into(), tool_name: "bash".into(), content: vec![Content::text("ok")], details: None, usage: None, is_error: false, timestamp: 0 }),
                Message::Assistant(AssistantMessage {
                    content: vec![
                        Content::text("Now writing"),
                        Content::ToolCall { id: "t2".into(), name: "write".into(), arguments: json!({}), thought_signature: None },
                    ],
                    stop_reason: StopReason::Error,
                    error_message: Some("Stream error: error decoding response body".into()),
                    ..AssistantMessage::new(&model)
                }),
                Message::user("What happened?"),
            ],
            tools: vec![],
        };
        let body = anthropic::build_request(&model, &ctx, &StreamOptions::default());
        let msgs = body["messages"].as_array().unwrap();
        // Every tool_use must be answered by a tool_result in the next message.
        for (i, m) in msgs.iter().enumerate() {
            for b in m["content"].as_array().unwrap() {
                if b["type"] == "tool_use" {
                    let next = &msgs[i + 1];
                    assert_eq!(next["role"], "user");
                    assert!(next["content"].as_array().unwrap().iter().any(|r| r["type"] == "tool_result" && r["tool_use_id"] == b["id"]), "tool_use {} unanswered", b["id"]);
                }
            }
        }
        assert!(!body.to_string().contains("t2"), "errored assistant message must be dropped");
        assert_eq!(msgs.last().unwrap()["content"][0]["text"], "What happened?");
    }

    #[test]
    fn orphaned_tool_calls_get_synthetic_results() {
        let reg = ModelRegistry::with_builtins();
        let model = reg.get("anthropic", "claude-sonnet-4-5").unwrap();
        let assistant = Message::Assistant(AssistantMessage {
            content: vec![
                Content::ToolCall { id: "a".into(), name: "read".into(), arguments: json!({"path":"x"}), thought_signature: None },
                Content::ToolCall { id: "b".into(), name: "bash".into(), arguments: json!({"command":"pwd"}), thought_signature: None },
            ],
            stop_reason: StopReason::ToolUse,
            ..AssistantMessage::new(&model)
        });
        let result_a = Message::ToolResult(ToolResultMessage { tool_call_id: "a".into(), tool_name: "read".into(), content: vec![Content::text("done")], details: None, usage: None, is_error: false, timestamp: 0 });

        // Interrupted by a user turn: the missing result is synthesized before the user message.
        let out = transform_messages(&[Message::user("go"), assistant.clone(), result_a.clone(), Message::user("next")]);
        assert_eq!(out.len(), 5);
        match &out[3] {
            Message::ToolResult(t) => {
                assert_eq!(t.tool_call_id, "b");
                assert_eq!(t.tool_name, "bash");
                assert!(t.is_error);
                assert_eq!(t.content[0].as_text(), Some(transform::NO_RESULT_TEXT));
            }
            other => panic!("expected synthetic tool result, got {}", other.role()),
        }
        assert!(matches!(out[4], Message::User(_)));

        // Trailing orphan: synthesized at the end.
        let out = transform_messages(&[Message::user("go"), assistant.clone(), result_a.clone()]);
        assert_eq!(out.len(), 4);
        assert!(matches!(&out[3], Message::ToolResult(t) if t.tool_call_id == "b"));

        // Fully answered: untouched.
        let result_b = Message::ToolResult(ToolResultMessage { tool_call_id: "b".into(), tool_name: "bash".into(), content: vec![Content::text("/")], details: None, usage: None, is_error: false, timestamp: 0 });
        let input = vec![Message::user("go"), assistant, result_a, result_b];
        assert_eq!(transform_messages(&input).len(), input.len());
    }

    #[test]
    fn openai_request_shape() {
        let reg = ModelRegistry::with_builtins();
        let model = reg.get("openai", "gpt-5").unwrap();
        let ctx = Context { system_prompt: Some("sys".into()), messages: vec![Message::user("hello")], tools: vec![] };
        let body = openai::build_request(&model, &ctx, &StreamOptions { reasoning: ThinkingLevel::Low, ..Default::default() });
        assert_eq!(body["messages"][0]["role"], "developer");
        assert_eq!(body["reasoning_effort"], "low");
        assert!(body["max_completion_tokens"].is_number());
    }

    #[tokio::test]
    async fn faux_provider_streams_and_finishes() {
        let reg = ModelRegistry::with_builtins();
        let model = reg.get("faux", "scripted").unwrap();
        faux::set_script(vec![json!({"text": "hello world", "toolCalls": [{"name": "read", "arguments": {"path": "a"}}]})]);
        let msg = complete(&model, Context { messages: vec![Message::user("x")], ..Default::default() }, StreamOptions::default()).await;
        assert_eq!(msg.stop_reason, StopReason::ToolUse);
        assert_eq!(msg.text(), "hello world");
        assert_eq!(msg.tool_calls().len(), 1);
    }

    #[tokio::test]
    async fn sse_parser_splits_events() {
        use futures::StreamExt;
        let chunks: Vec<Result<bytes::Bytes, String>> = vec![
            Ok(bytes::Bytes::from("event: a\ndata: {\"x\":1}\n\nevent: b\ndata: line1\ndata: li")),
            Ok(bytes::Bytes::from("ne2\n\n")),
        ];
        let s = sse::SseStream::new(futures::stream::iter(chunks));
        let events: Vec<_> = s.collect().await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].as_ref().unwrap().data, "{\"x\":1}");
        assert_eq!(events[1].as_ref().unwrap().data, "line1\nline2");
    }
}
