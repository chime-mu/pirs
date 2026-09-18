//! pi-ai: unified multi-provider LLM streaming API (Rust port of `@earendil-works/pi-ai`).

pub mod anthropic;
pub mod faux;
pub mod json_salvage;
pub mod openai;
pub mod registry;
pub mod sse;
pub mod types;

pub use registry::{complete, stream, ModelRegistry, ProviderConfig, ProviderModelConfig};
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
