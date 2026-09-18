//! High-level `Agent`: owns state, queues, and subscriptions around the loop.

use crate::agent_loop::{run_agent_loop, run_agent_loop_continue, LoopInput};
use crate::types::*;
use async_trait::async_trait;
use pi_ai::*;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct AgentState {
    pub system_prompt: String,
    pub model: Option<Model>,
    pub thinking_level: ThinkingLevel,
    pub tools: Vec<ToolRef>,
    pub messages: Vec<AgentMessage>,
    pub is_streaming: bool,
    pub streaming_message: Option<AgentMessage>,
    pub pending_tool_calls: std::collections::HashSet<String>,
    pub error_message: Option<String>,
    pub tool_execution: ToolExecutionMode,
    pub max_tokens: Option<u64>,
    pub temperature: Option<f64>,
}

#[derive(Default)]
struct Queues {
    steering: VecDeque<AgentMessage>,
    follow_up: VecDeque<AgentMessage>,
}

pub type Listener = Arc<dyn Fn(AgentEvent) -> futures::future::BoxFuture<'static, ()> + Send + Sync>;

/// The agent. Cheap to clone; all clones share state.
#[derive(Clone)]
pub struct Agent {
    pub state: Arc<Mutex<AgentState>>,
    queues: Arc<Mutex<Queues>>,
    hooks: Arc<Mutex<Arc<dyn AgentHooks>>>,
    listeners: Arc<Mutex<Vec<Listener>>>,
    cancel: Arc<Mutex<Option<CancellationToken>>>,
    idle: Arc<Notify>,
}

/// Hooks that wrap the user's hooks and drain the agent's queues.
struct QueueHooks {
    inner: Arc<dyn AgentHooks>,
    queues: Arc<Mutex<Queues>>,
}

#[async_trait]
impl AgentHooks for QueueHooks {
    async fn convert_to_llm(&self, messages: &[AgentMessage]) -> Vec<Message> {
        self.inner.convert_to_llm(messages).await
    }
    async fn transform_context(&self, messages: Vec<AgentMessage>, cancel: &CancellationToken) -> Vec<AgentMessage> {
        self.inner.transform_context(messages, cancel).await
    }
    async fn before_tool_call(&self, ctx: BeforeToolCallContext<'_>, cancel: &CancellationToken) -> Option<BeforeToolCallResult> {
        self.inner.before_tool_call(ctx, cancel).await
    }
    async fn after_tool_call(&self, ctx: AfterToolCallContext<'_>, cancel: &CancellationToken) -> Option<AfterToolCallResult> {
        self.inner.after_tool_call(ctx, cancel).await
    }
    async fn get_steering_messages(&self) -> Vec<AgentMessage> {
        let mut q = self.queues.lock().unwrap();
        q.steering.drain(..).collect()
    }
    async fn get_follow_up_messages(&self) -> Vec<AgentMessage> {
        let mut q = self.queues.lock().unwrap();
        q.follow_up.drain(..).collect()
    }
    async fn get_api_key(&self, provider: &str) -> Option<String> {
        self.inner.get_api_key(provider).await
    }
    async fn stream_options(&self) -> StreamOptions {
        self.inner.stream_options().await
    }
    async fn should_stop_after_turn(&self, message: &AssistantMessage) -> bool {
        self.inner.should_stop_after_turn(message).await
    }
}

impl Agent {
    pub fn new(model: Model, hooks: Arc<dyn AgentHooks>) -> Self {
        let state = AgentState { model: Some(model), ..Default::default() };
        Agent {
            state: Arc::new(Mutex::new(state)),
            queues: Default::default(),
            hooks: Arc::new(Mutex::new(hooks)),
            listeners: Default::default(),
            cancel: Default::default(),
            idle: Arc::new(Notify::new()),
        }
    }

    pub fn set_hooks(&self, hooks: Arc<dyn AgentHooks>) {
        *self.hooks.lock().unwrap() = hooks;
    }

    pub fn subscribe(&self, listener: Listener) {
        self.listeners.lock().unwrap().push(listener);
    }

    pub fn with_state<R>(&self, f: impl FnOnce(&mut AgentState) -> R) -> R {
        f(&mut self.state.lock().unwrap())
    }

    pub fn is_streaming(&self) -> bool {
        self.state.lock().unwrap().is_streaming
    }

    pub fn model(&self) -> Option<Model> {
        self.state.lock().unwrap().model.clone()
    }
    pub fn set_model(&self, model: Model) {
        self.state.lock().unwrap().model = Some(model);
    }
    pub fn thinking_level(&self) -> ThinkingLevel {
        self.state.lock().unwrap().thinking_level
    }
    pub fn set_thinking_level(&self, level: ThinkingLevel) {
        self.state.lock().unwrap().thinking_level = level;
    }
    pub fn set_system_prompt(&self, prompt: String) {
        self.state.lock().unwrap().system_prompt = prompt;
    }
    pub fn set_tools(&self, tools: Vec<ToolRef>) {
        self.state.lock().unwrap().tools = tools;
    }
    pub fn tools(&self) -> Vec<ToolRef> {
        self.state.lock().unwrap().tools.clone()
    }
    pub fn messages(&self) -> Vec<AgentMessage> {
        self.state.lock().unwrap().messages.clone()
    }
    pub fn replace_messages(&self, messages: Vec<AgentMessage>) {
        self.state.lock().unwrap().messages = messages;
    }
    pub fn append_message(&self, m: AgentMessage) {
        self.state.lock().unwrap().messages.push(m);
    }

    pub fn steer(&self, m: AgentMessage) {
        self.queues.lock().unwrap().steering.push_back(m);
    }
    pub fn follow_up(&self, m: AgentMessage) {
        self.queues.lock().unwrap().follow_up.push_back(m);
    }
    pub fn has_queued_messages(&self) -> bool {
        let q = self.queues.lock().unwrap();
        !q.steering.is_empty() || !q.follow_up.is_empty()
    }
    pub fn clear_queues(&self) -> Vec<AgentMessage> {
        let mut q = self.queues.lock().unwrap();
        let mut out: Vec<AgentMessage> = q.steering.drain(..).collect();
        out.extend(q.follow_up.drain(..));
        out
    }

    pub fn abort(&self) {
        if let Some(c) = self.cancel.lock().unwrap().as_ref() {
            c.cancel();
        }
    }

    pub async fn wait_for_idle(&self) {
        loop {
            if !self.is_streaming() {
                return;
            }
            self.idle.notified().await;
        }
    }

    fn make_sink(&self) -> EventSink {
        let listeners = self.listeners.clone();
        let state = self.state.clone();
        Arc::new(move |event: AgentEvent| {
            let listeners = listeners.clone();
            let state = state.clone();
            Box::pin(async move {
                // Track streaming state for observers.
                {
                    let mut s = state.lock().unwrap();
                    match &event {
                        AgentEvent::MessageStart { message } | AgentEvent::MessageUpdate { message, .. } => {
                            if matches!(message, AgentMessage::Assistant(_)) {
                                s.streaming_message = Some(message.clone());
                            }
                        }
                        AgentEvent::MessageEnd { message } => {
                            if let AgentMessage::Assistant(a) = message {
                                s.streaming_message = None;
                                if matches!(a.stop_reason, StopReason::Error | StopReason::Aborted) {
                                    s.error_message = a.error_message.clone();
                                }
                            }
                        }
                        AgentEvent::ToolExecutionStart { tool_call_id, .. } => {
                            s.pending_tool_calls.insert(tool_call_id.clone());
                        }
                        AgentEvent::ToolExecutionEnd { tool_call_id, .. } => {
                            s.pending_tool_calls.remove(tool_call_id);
                        }
                        _ => {}
                    }
                }
                let ls: Vec<Listener> = listeners.lock().unwrap().clone();
                for l in ls {
                    l(event.clone()).await;
                }
            })
        })
    }

    fn loop_input_and_config(&self) -> (LoopInput, AgentLoopConfig) {
        let s = self.state.lock().unwrap();
        (
            LoopInput { system_prompt: s.system_prompt.clone(), tools: s.tools.clone(), messages: s.messages.clone() },
            AgentLoopConfig {
                model: s.model.clone().expect("model must be set"),
                thinking_level: s.thinking_level,
                tool_execution: s.tool_execution,
                max_tokens: s.max_tokens,
                temperature: s.temperature,
            },
        )
    }

    /// Run a prompt to completion. Errors if the agent is already running.
    pub async fn prompt(&self, prompts: Vec<AgentMessage>) -> anyhow::Result<Vec<AgentMessage>> {
        self.run(Some(prompts)).await
    }

    /// Continue from the current context (last message must be user/toolResult).
    pub async fn continue_run(&self) -> anyhow::Result<Vec<AgentMessage>> {
        self.run(None).await
    }

    async fn run(&self, prompts: Option<Vec<AgentMessage>>) -> anyhow::Result<Vec<AgentMessage>> {
        {
            let mut s = self.state.lock().unwrap();
            if s.is_streaming {
                anyhow::bail!("Agent is already processing");
            }
            if s.model.is_none() {
                anyhow::bail!("No model configured");
            }
            s.is_streaming = true;
            s.error_message = None;
        }
        let cancel = CancellationToken::new();
        *self.cancel.lock().unwrap() = Some(cancel.clone());
        let (input, config) = self.loop_input_and_config();
        let hooks: Arc<dyn AgentHooks> = Arc::new(QueueHooks { inner: self.hooks.lock().unwrap().clone(), queues: self.queues.clone() });
        let sink = self.make_sink();

        // Mirror the loop's context into agent state as events arrive.
        let state = self.state.clone();
        let mirror_sink: EventSink = Arc::new(move |event: AgentEvent| {
            let sink = sink.clone();
            let state = state.clone();
            Box::pin(async move {
                {
                    let mut s = state.lock().unwrap();
                    match &event {
                        AgentEvent::MessageStart { message } => {
                            s.messages.push(message.clone());
                        }
                        AgentEvent::MessageUpdate { message, .. } | AgentEvent::MessageEnd { message } => {
                            if let Some(last) = s.messages.last_mut() {
                                if last.role() == message.role() {
                                    *last = message.clone();
                                }
                            }
                        }
                        _ => {}
                    }
                }
                sink(event).await;
            })
        });

        let result = match prompts {
            Some(p) => run_agent_loop(p, input, config, hooks, cancel, mirror_sink).await,
            None => run_agent_loop_continue(input, config, hooks, cancel, mirror_sink).await,
        };
        {
            let mut s = self.state.lock().unwrap();
            s.is_streaming = false;
            s.streaming_message = None;
        }
        *self.cancel.lock().unwrap() = None;
        self.idle.notify_waiters();
        Ok(result)
    }
}
