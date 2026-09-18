//! pi-agent: agent runtime with tool calling and state management
//! (Rust port of `@earendil-works/pi-agent-core`).

pub mod agent;
pub mod agent_loop;
pub mod types;
pub mod validate;

pub use agent::{Agent, AgentState, Listener};
pub use agent_loop::{run_agent_loop, run_agent_loop_continue, LoopInput};
pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use pi_ai::*;
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};
    use tokio_util::sync::CancellationToken;

    struct Echo;
    #[async_trait]
    impl AgentTool for Echo {
        fn name(&self) -> String {
            "echo".into()
        }
        fn description(&self) -> String {
            "echo".into()
        }
        fn parameters(&self) -> Value {
            json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]})
        }
        async fn execute(&self, _id: &str, args: Value, _c: CancellationToken, on_update: UpdateFn) -> anyhow::Result<ToolResult> {
            on_update(ToolResult::text("working"));
            Ok(ToolResult::text(format!("echo: {}", args["text"].as_str().unwrap_or(""))))
        }
    }

    struct Blocker;
    #[async_trait]
    impl AgentHooks for Blocker {
        async fn before_tool_call(&self, ctx: BeforeToolCallContext<'_>, _c: &CancellationToken) -> Option<BeforeToolCallResult> {
            if ctx.args["text"] == "secret" {
                return Some(BeforeToolCallResult { block: true, reason: Some("nope".into()), ..Default::default() });
            }
            None
        }
    }

    #[tokio::test]
    async fn loop_runs_tools_and_hooks() {
        let reg = ModelRegistry::with_builtins();
        let model = reg.get("faux", "scripted").unwrap();
        pi_ai::faux::set_script(vec![
            json!({"text": "calling", "toolCalls": [{"name": "echo", "arguments": {"text": "hi"}}, {"name": "echo", "arguments": {"text": "secret"}}, {"name":"missing","arguments":{}}]}),
            json!("done"),
        ]);
        let agent = Agent::new(model, Arc::new(Blocker));
        agent.set_tools(vec![Arc::new(Echo)]);
        let events: Arc<Mutex<Vec<String>>> = Default::default();
        let ev2 = events.clone();
        agent.subscribe(Arc::new(move |e| {
            let ev2 = ev2.clone();
            Box::pin(async move {
                let name = serde_json::to_value(&e).unwrap()["type"].as_str().unwrap().to_string();
                ev2.lock().unwrap().push(name);
            })
        }));
        let out = agent.prompt(vec![AgentMessage::user("go")]).await.unwrap();
        let results: Vec<&ToolResultMessage> = out.iter().filter_map(|m| if let AgentMessage::ToolResult(t) = m { Some(t) } else { None }).collect();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].content[0].as_text().unwrap(), "echo: hi");
        assert!(results[1].is_error);
        assert_eq!(results[1].content[0].as_text().unwrap(), "nope");
        assert!(results[2].is_error);
        let last = out.last().unwrap();
        if let AgentMessage::Assistant(a) = last {
            assert_eq!(a.text(), "done");
        } else {
            panic!("expected assistant");
        }
        let ev = events.lock().unwrap().clone();
        assert_eq!(ev.first().unwrap(), "agent_start");
        assert_eq!(ev.last().unwrap(), "agent_end");
        assert!(ev.contains(&"tool_execution_update".to_string()));
        assert!(!agent.is_streaming());
        assert_eq!(agent.messages().len(), out.len());
    }
}
