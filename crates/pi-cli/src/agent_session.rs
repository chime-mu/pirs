//! AgentSession: wires the agent loop, the session file, the model registry,
//! the extension host, and a UI backend together (port of pi's
//! `agent-session.ts`, reduced to what pirs supports).

use crate::session::SessionManager;
use crate::settings::Settings;
use crate::system_prompt::{build_system_prompt, BuildSystemPromptOptions};
use crate::tools;
use async_trait::async_trait;
use pi_agent::*;
use pi_ai::*;
use pi_ext::{CommandInfo, ContextInfo, ExtensionError, ExtensionHost, ExtensionTool, HostCallbacks, HostConfig, ToolInfo};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// UI backend
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum UiEvent {
    Agent(AgentEvent),
    Notify { message: String, kind: String },
    Status { key: String, text: Option<String> },
    Widget { key: String, lines: Option<Vec<String>>, placement: Option<String> },
    Title(String),
    WorkingMessage(Option<String>),
    ExtensionError(ExtensionError),
    Console { level: String, message: String },
    SetEditorText(String),
    BashOutput { command: String, output: String, exit_code: Option<i32> },
    Shutdown,
}

#[async_trait]
pub trait UiBackend: Send + Sync {
    fn mode(&self) -> &'static str;
    fn has_ui(&self) -> bool;
    fn emit(&self, event: UiEvent);
    async fn select(&self, _title: String, _options: Vec<String>, _opts: Value) -> Option<String> {
        None
    }
    async fn confirm(&self, _title: String, _message: String, _opts: Value) -> bool {
        false
    }
    async fn input(&self, _title: String, _placeholder: String, _opts: Value) -> Option<String> {
        None
    }
    async fn editor(&self, _title: String, _prefill: String) -> Option<String> {
        None
    }
    fn get_editor_text(&self) -> String {
        String::new()
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

pub struct SessionOptions {
    pub cwd: PathBuf,
    pub settings: Settings,
    pub registry: ModelRegistry,
    pub model: Model,
    pub thinking_level: ThinkingLevel,
    pub session: SessionManager,
    pub ui: Arc<dyn UiBackend>,
    pub custom_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
    pub selected_tools: Vec<String>,
    pub tool_execution: ToolExecutionMode,
}

pub struct Inner {
    pub cwd: PathBuf,
    pub agent: Agent,
    pub session: Mutex<SessionManager>,
    pub registry: ModelRegistry,
    pub settings: Settings,
    pub ui: Arc<dyn UiBackend>,
    host: OnceLock<ExtensionHost>,
    builtin_tools: Vec<ToolRef>,
    active_tools: Mutex<Vec<String>>,
    extension_tools: Mutex<Vec<ToolInfo>>,
    commands: Mutex<Vec<CommandInfo>>,
    prompt_options: Mutex<BuildSystemPromptOptions>,
    next_turn_messages: Mutex<Vec<AgentMessage>>,
    shutdown_requested: AtomicBool,
    run_cancel: Mutex<Option<CancellationToken>>,
    extension_errors: Mutex<Vec<ExtensionError>>,
    settled_notify: tokio::sync::Notify,
    running: AtomicBool,
    self_weak: std::sync::Weak<Inner>,
}

#[derive(Clone)]
pub struct AgentSession(pub Arc<Inner>);

impl AgentSession {
    pub async fn new(opts: SessionOptions) -> anyhow::Result<Self> {
        let agent = Agent::new(opts.model.clone(), Arc::new(NoHooks));
        agent.set_thinking_level(opts.thinking_level);
        agent.with_state(|s| s.tool_execution = opts.tool_execution);
        let builtin_tools = tools::builtin_tools(&opts.cwd);
        let prompt_options = BuildSystemPromptOptions {
            custom_prompt: opts.custom_prompt.clone(),
            selected_tools: opts.selected_tools.clone(),
            append_system_prompt: opts.append_system_prompt.clone().unwrap_or_default(),
            cwd: opts.cwd.to_string_lossy().to_string(),
            context_files: crate::system_prompt::load_context_files(&opts.cwd),
            ..Default::default()
        };
        let inner = Arc::new_cyclic(|weak| Inner {
            self_weak: weak.clone(),
            cwd: opts.cwd,
            agent: agent.clone(),
            session: Mutex::new(opts.session),
            registry: opts.registry,
            settings: opts.settings,
            ui: opts.ui,
            host: OnceLock::new(),
            builtin_tools,
            active_tools: Mutex::new(opts.selected_tools),
            extension_tools: Mutex::new(Vec::new()),
            commands: Mutex::new(Vec::new()),
            prompt_options: Mutex::new(prompt_options),
            next_turn_messages: Mutex::new(Vec::new()),
            shutdown_requested: AtomicBool::new(false),
            run_cancel: Mutex::new(None),
            extension_errors: Mutex::new(Vec::new()),
            settled_notify: tokio::sync::Notify::new(),
            running: AtomicBool::new(false),
        });
        let hooks: Arc<dyn AgentHooks> = inner.clone();
        agent.set_hooks(hooks);
        let listener_inner = inner.clone();
        agent.subscribe(Arc::new(move |event| {
            let inner = listener_inner.clone();
            Box::pin(async move { inner.on_agent_event(event).await })
        }));

        // Restore transcript, model, and thinking level from the session file.
        {
            let ctx = inner.session.lock().unwrap().build_session_context();
            if !ctx.messages.is_empty() {
                inner.agent.replace_messages(ctx.messages);
            }
            if let Some((provider, id)) = ctx.model {
                if let Some(m) = inner.registry.get(&provider, &id) {
                    inner.agent.set_model(m);
                }
            }
            if let Some(level) = ctx.thinking_level.as_deref().and_then(ThinkingLevel::parse) {
                inner.agent.set_thinking_level(level);
            }
        }
        inner.refresh_tools();
        Ok(AgentSession(inner))
    }

    /// Start the extension host and load the given extension files.
    pub async fn load_extensions(&self, paths: &[PathBuf]) -> Vec<(PathBuf, Result<pi_ext::LoadedExtension, String>)> {
        let cb: Arc<dyn HostCallbacks> = self.0.clone();
        let host = match ExtensionHost::spawn(cb, HostConfig::new(self.0.cwd.clone())).await {
            Ok(h) => h,
            Err(e) => {
                self.0.ui.emit(UiEvent::Notify { message: format!("Extension host failed to start: {e}"), kind: "error".into() });
                return Vec::new();
            }
        };
        let _ = self.0.host.set(host.clone());
        let mut results = Vec::new();
        for p in paths {
            let r = host.load(p.to_string_lossy().to_string()).await;
            results.push((p.clone(), r));
        }
        host.set_loaded();
        self.0.refresh_extension_registrations().await;
        self.0.refresh_tools();
        results
    }

    pub async fn emit_session_start(&self, reason: &str) {
        self.0.dispatch("session_start", json!({"type": "session_start", "reason": reason}), None).await;
        self.0.dispatch("resources_discover", json!({"type": "resources_discover", "cwd": self.0.cwd.to_string_lossy(), "reason": "startup"}), None).await;
    }

    pub async fn shutdown(&self, reason: &str) {
        self.0.dispatch("session_shutdown", json!({"type": "session_shutdown", "reason": reason}), None).await;
        if let Some(h) = self.0.host.get() {
            h.shutdown();
        }
    }

    pub fn abort(&self) {
        self.0.agent.abort();
    }

    pub async fn wait_for_idle(&self) {
        self.0.wait_for_idle().await
    }

    pub fn commands(&self) -> Vec<CommandInfo> {
        self.0.commands.lock().unwrap().clone()
    }

    pub fn extension_errors(&self) -> Vec<ExtensionError> {
        self.0.extension_errors.lock().unwrap().clone()
    }

    /// Handle user input: extension commands, `!` bash, `input` event, then a
    /// full agent run. While the agent is running the text is queued as a
    /// steering (or follow-up) message instead.
    pub async fn submit(&self, text: String, images: Vec<Content>, deliver_as: Option<&str>) -> anyhow::Result<()> {
        let inner = self.0.clone();
        if inner.running.load(Ordering::SeqCst) {
            let msg = user_message(&text, &images);
            match deliver_as.unwrap_or("steer") {
                "followUp" => inner.agent.follow_up(msg),
                _ => inner.agent.steer(msg),
            }
            return Ok(());
        }
        inner.handle_input(text, images, "interactive").await
    }

    pub fn current_model(&self) -> Option<Model> {
        self.0.agent.model()
    }

    pub async fn set_model(&self, model: Model, source: &str) -> bool {
        let previous = self.0.agent.model();
        self.0.agent.set_model(model.clone());
        let _ = self.0.session.lock().unwrap().append_model_change(&model.provider, &model.id);
        self.0
            .dispatch("model_select", json!({"type": "model_select", "model": model, "previousModel": previous, "source": source}), None)
            .await;
        true
    }

    pub fn set_thinking_level(&self, level: ThinkingLevel) {
        self.0.agent.set_thinking_level(level);
        let _ = self.0.session.lock().unwrap().append_thinking_level_change(level.as_str());
    }

}

pub fn trace(msg: &str) {
    if std::env::var_os("PIRS_TRACE").is_some() {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() % 100000).unwrap_or(0);
        eprintln!("[trace {t}] {msg}");
    }
}

fn user_message(text: &str, images: &[Content]) -> AgentMessage {
    if images.is_empty() {
        AgentMessage::user(text.to_string())
    } else {
        let mut blocks = vec![Content::text(text.to_string())];
        blocks.extend(images.iter().cloned());
        AgentMessage::User(UserMessage { content: UserContent::Blocks(blocks), timestamp: now_ms() })
    }
}

impl Inner {
    fn host(&self) -> Option<&ExtensionHost> {
        self.host.get()
    }

    async fn dispatch(&self, event: &str, payload: Value, cancel: Option<CancellationToken>) -> Value {
        let Some(host) = self.host() else { return Value::Null };
        trace(&format!("dispatch {event} start"));
        let r = host.dispatch(event, payload, cancel).await;
        trace(&format!("dispatch {event} end"));
        match r {
            Ok(outcome) => outcome.result,
            Err(e) => {
                self.record_error(ExtensionError { extension_path: "<host>".into(), event: event.into(), error: e, stack: None });
                Value::Null
            }
        }
    }

    async fn dispatch_full(&self, event: &str, payload: Value, cancel: Option<CancellationToken>) -> Option<pi_ext::DispatchOutcome> {
        let host = self.host()?;
        match host.dispatch(event, payload, cancel).await {
            Ok(o) => Some(o),
            Err(e) => {
                self.record_error(ExtensionError { extension_path: "<host>".into(), event: event.into(), error: e, stack: None });
                None
            }
        }
    }

    fn record_error(&self, err: ExtensionError) {
        self.extension_errors.lock().unwrap().push(err.clone());
        self.ui.emit(UiEvent::ExtensionError(err));
    }

    async fn refresh_extension_registrations(&self) {
        if let Some(h) = self.host() {
            *self.extension_tools.lock().unwrap() = h.list_tools().await;
            *self.commands.lock().unwrap() = h.list_commands().await;
        }
    }

    /// Rebuild the agent's tool list and system prompt from active tool names.
    fn refresh_tools(&self) {
        let active = self.active_tools.lock().unwrap().clone();
        let ext_tools = self.extension_tools.lock().unwrap().clone();
        let mut all: Vec<ToolRef> = Vec::new();
        let mut snippets: HashMap<String, String> = HashMap::new();
        let mut guidelines: HashMap<String, Vec<String>> = HashMap::new();
        for t in &self.builtin_tools {
            if ext_tools.iter().any(|e| e.name == t.name()) {
                continue; // extension override
            }
            all.push(t.clone());
        }
        if let Some(host) = self.host() {
            for info in ext_tools {
                all.push(Arc::new(ExtensionTool { host: host.clone(), info }));
            }
        }
        for t in &all {
            if let Some(s) = t.prompt_snippet() {
                snippets.insert(t.name(), s);
            }
            let g = t.prompt_guidelines();
            if !g.is_empty() {
                guidelines.insert(t.name(), g);
            }
        }
        // Extension tools are active by default when they are registered.
        let mut selected: Vec<String> = active.clone();
        for t in &all {
            let n = t.name();
            let is_builtin = self.builtin_tools.iter().any(|b| b.name() == n);
            if !is_builtin && !selected.contains(&n) {
                selected.push(n);
            }
        }
        let chosen: Vec<ToolRef> = all.into_iter().filter(|t| selected.contains(&t.name())).collect();
        self.agent.set_tools(chosen);
        let prompt = {
            let mut o = self.prompt_options.lock().unwrap();
            o.selected_tools = selected;
            o.tool_snippets = snippets;
            o.tool_guidelines = guidelines;
            build_system_prompt(&o)
        };
        self.agent.set_system_prompt(prompt);
    }

    fn all_tool_infos(&self) -> Vec<Value> {
        let ext = self.extension_tools.lock().unwrap().clone();
        let mut out: Vec<Value> = self
            .builtin_tools
            .iter()
            .filter(|t| !ext.iter().any(|e| e.name == t.name()))
            .map(|t| json!({"name": t.name(), "label": t.label(), "description": t.description(), "parameters": t.parameters(), "source": "builtin"}))
            .collect();
        for e in ext {
            out.push(json!({"name": e.name, "label": e.label, "description": e.description, "parameters": e.parameters, "source": "extension", "extensionPath": e.extension_path}));
        }
        out
    }

    async fn wait_for_idle(&self) {
        while self.running.load(Ordering::SeqCst) {
            self.settled_notify.notified().await;
        }
    }

    // -----------------------------------------------------------------------
    // Input handling
    // -----------------------------------------------------------------------

    async fn handle_input(self: &Arc<Self>, text: String, images: Vec<Content>, source: &str) -> anyhow::Result<()> {
        let trimmed = text.trim();
        // Extension commands.
        if let Some(rest) = trimmed.strip_prefix('/') {
            let (name, args) = rest.split_once(char::is_whitespace).map(|(n, a)| (n.to_string(), a.trim().to_string())).unwrap_or((rest.to_string(), String::new()));
            let known = self.commands.lock().unwrap().iter().any(|c| c.invocation.as_deref() == Some(name.as_str()) || c.name == name);
            if known {
                if let Some(h) = self.host() {
                    if let Err(e) = h.run_command(&name, &args).await {
                        self.ui.emit(UiEvent::Notify { message: format!("/{name} failed: {e}"), kind: "error".into() });
                    }
                }
                return Ok(());
            }
        }
        // User bash.
        if let Some(cmd) = trimmed.strip_prefix('!') {
            let exclude = cmd.starts_with('!');
            let cmd = cmd.trim_start_matches('!').trim().to_string();
            return self.run_user_bash(cmd, exclude).await;
        }
        // `input` event.
        let mut text = text;
        let mut images = images;
        if self.host().map(|_| true).unwrap_or(false) {
            let r = self.dispatch("input", json!({"type": "input", "text": text, "images": images, "source": source}), None).await;
            match r["action"].as_str() {
                Some("handled") => return Ok(()),
                Some("transform") => {
                    if let Some(t) = r["text"].as_str() {
                        text = t.to_string();
                    }
                    if let Some(i) = r.get("images").and_then(|i| serde_json::from_value::<Vec<Content>>(i.clone()).ok()) {
                        images = i;
                    }
                }
                _ => {}
            }
        }
        self.run_prompt(vec![user_message(&text, &images)], Some(text)).await
    }

    async fn run_user_bash(self: &Arc<Self>, command: String, exclude_from_context: bool) -> anyhow::Result<()> {
        let r = self.dispatch("user_bash", json!({"type": "user_bash", "command": command, "excludeFromContext": exclude_from_context, "cwd": self.cwd.to_string_lossy()}), None).await;
        let (output, exit_code, cancelled) = if let Some(res) = r.get("result").filter(|v| v.is_object()) {
            (res["output"].as_str().unwrap_or("").to_string(), res["exitCode"].as_i64().map(|c| c as i32), res["cancelled"].as_bool().unwrap_or(false))
        } else {
            let out = tokio::process::Command::new("bash").arg("-c").arg(&command).current_dir(&self.cwd).stdin(std::process::Stdio::null()).output().await?;
            let mut text = String::from_utf8_lossy(&out.stdout).to_string();
            if !out.stderr.is_empty() {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&String::from_utf8_lossy(&out.stderr));
            }
            (text, out.status.code(), false)
        };
        self.ui.emit(UiEvent::BashOutput { command: command.clone(), output: output.clone(), exit_code });
        let msg = AgentMessage::BashExecution(BashExecutionMessage { command, output, exit_code, cancelled, truncated: false, full_output_path: None, exclude_from_context, timestamp: now_ms() });
        self.agent.append_message(msg.clone());
        let _ = self.session.lock().unwrap().append_message(msg);
        Ok(())
    }

    /// Run the agent with the given prompt messages (before_agent_start,
    /// loop, retries via follow-ups, agent_settled).
    async fn run_prompt(self: &Arc<Self>, mut prompts: Vec<AgentMessage>, prompt_text: Option<String>) -> anyhow::Result<()> {
        if self.running.swap(true, Ordering::SeqCst) {
            anyhow::bail!("agent is already running");
        }
        let result = self.run_prompt_inner(&mut prompts, prompt_text).await;
        self.running.store(false, Ordering::SeqCst);
        self.settled_notify.notify_waiters();
        self.dispatch("agent_settled", json!({"type": "agent_settled"}), None).await;
        if self.shutdown_requested.load(Ordering::SeqCst) {
            self.ui.emit(UiEvent::Shutdown);
        }
        result
    }

    async fn run_prompt_inner(self: &Arc<Self>, prompts: &mut Vec<AgentMessage>, prompt_text: Option<String>) -> anyhow::Result<()> {
        // Messages queued with deliverAs: "nextTurn".
        let queued: Vec<AgentMessage> = std::mem::take(&mut *self.next_turn_messages.lock().unwrap());
        let mut all: Vec<AgentMessage> = queued;
        all.append(prompts);

        let base_prompt = self.agent.with_state(|s| s.system_prompt.clone());
        let mut run_prompt = base_prompt.clone();
        if let Some(text) = &prompt_text {
            let options = self.prompt_options.lock().unwrap().clone();
            let r = self
                .dispatch("before_agent_start", json!({"type": "before_agent_start", "prompt": text, "systemPrompt": base_prompt, "systemPromptOptions": options}), None)
                .await;
            if let Some(msgs) = r["messages"].as_array() {
                for m in msgs {
                    all.push(AgentMessage::Custom(CustomMessage {
                        custom_type: m["customType"].as_str().unwrap_or("extension").to_string(),
                        content: serde_json::from_value(m["content"].clone()).unwrap_or(UserContent::Text(String::new())),
                        display: m["display"].as_bool().unwrap_or(false),
                        details: m.get("details").cloned().filter(|d| !d.is_null()),
                        timestamp: now_ms(),
                    }));
                }
            }
            if let Some(sp) = r["systemPrompt"].as_str() {
                run_prompt = sp.to_string();
            }
        }
        self.agent.set_system_prompt(run_prompt);
        let cancel_probe = CancellationToken::new();
        *self.run_cancel.lock().unwrap() = Some(cancel_probe.clone());
        let r = self.agent.prompt(all).await;
        *self.run_cancel.lock().unwrap() = None;
        self.agent.set_system_prompt(base_prompt);
        r.map(|_| ())
    }

    // -----------------------------------------------------------------------
    // Agent events: persist, forward to UI, forward to extensions
    // -----------------------------------------------------------------------

    async fn on_agent_event(self: &Arc<Self>, event: AgentEvent) {
        let event = match event {
            AgentEvent::MessageEnd { message } => {
                let mut message = message;
                if matches!(message, AgentMessage::Assistant(_)) {
                    let r = self.dispatch("message_end", json!({"type": "message_end", "message": message}), None).await;
                    if let Some(m) = r.get("message").and_then(|m| serde_json::from_value::<AgentMessage>(m.clone()).ok()) {
                        if m.role() == message.role() {
                            message = m;
                        }
                    }
                }
                // Persist finished messages (skip aborted/errored partial assistants with no content).
                let persist = match &message {
                    AgentMessage::Assistant(a) => !(a.content.is_empty() && matches!(a.stop_reason, StopReason::Aborted | StopReason::Error)),
                    _ => true,
                };
                if persist {
                    let _ = self.session.lock().unwrap().append_message(message.clone());
                }
                self.ui.emit(UiEvent::Agent(AgentEvent::MessageEnd { message: message.clone() }));
                return;
            }
            other => other,
        };
        self.ui.emit(UiEvent::Agent(event.clone()));
        let name = serde_json::to_value(&event).ok().and_then(|v| v["type"].as_str().map(|s| s.to_string())).unwrap_or_default();
        // Extensions get every lifecycle event except the high-frequency
        // message_update stream unless a handler is registered.
        let Some(host) = self.host() else { return };
        if name == "message_update" && !host.has_handlers("message_update").await {
            return;
        }
        let mut payload = serde_json::to_value(&event).unwrap_or(Value::Null);
        if name == "turn_start" || name == "turn_end" {
            payload["turnIndex"] = json!(0);
            payload["timestamp"] = json!(now_ms());
        }
        let cancel = self.run_cancel.lock().unwrap().clone();
        self.dispatch(&name, payload, cancel).await;
    }
}

// ---------------------------------------------------------------------------
// AgentHooks: extension interception of the loop
// ---------------------------------------------------------------------------

#[async_trait]
impl AgentHooks for Inner {
    async fn transform_context(&self, messages: Vec<AgentMessage>, _cancel: &CancellationToken) -> Vec<AgentMessage> {
        let Some(host) = self.host() else { return messages };
        if !host.has_handlers("context").await {
            return messages;
        }
        let r = self.dispatch("context", json!({"type": "context", "messages": messages}), None).await;
        match r.get("messages").and_then(|m| serde_json::from_value::<Vec<AgentMessage>>(m.clone()).ok()) {
            Some(m) => m,
            None => messages,
        }
    }

    async fn before_tool_call(&self, ctx: BeforeToolCallContext<'_>, cancel: &CancellationToken) -> Option<BeforeToolCallResult> {
        let host = self.host()?;
        if !host.has_handlers("tool_call").await {
            return None;
        }
        let outcome = self
            .dispatch_full("tool_call", json!({"type": "tool_call", "toolName": ctx.tool_call.name, "toolCallId": ctx.tool_call.id, "input": ctx.args}), Some(cancel.clone()))
            .await?;
        let mut result = BeforeToolCallResult::default();
        if let Some(input) = outcome.event.get("input") {
            if input != ctx.args {
                result.args = Some(input.clone());
            }
        }
        if outcome.result["block"].as_bool().unwrap_or(false) {
            result.block = true;
            result.reason = outcome.result["reason"].as_str().map(|s| s.to_string());
            result.terminate = outcome.result["terminate"].as_bool().unwrap_or(false);
        }
        Some(result)
    }

    async fn after_tool_call(&self, ctx: AfterToolCallContext<'_>, cancel: &CancellationToken) -> Option<AfterToolCallResult> {
        let host = self.host()?;
        if !host.has_handlers("tool_result").await {
            return None;
        }
        let r = self
            .dispatch(
                "tool_result",
                json!({"type": "tool_result", "toolName": ctx.tool_call.name, "toolCallId": ctx.tool_call.id, "input": ctx.args, "content": ctx.result.content, "details": ctx.result.details, "isError": ctx.is_error, "usage": ctx.result.usage}),
                Some(cancel.clone()),
            )
            .await;
        if r.is_null() {
            return None;
        }
        Some(AfterToolCallResult {
            content: r.get("content").and_then(|c| serde_json::from_value(c.clone()).ok()),
            details: r.get("details").cloned(),
            is_error: r["isError"].as_bool(),
            usage: r.get("usage").and_then(|u| serde_json::from_value(u.clone()).ok()),
            terminate: None,
        })
    }

    async fn get_api_key(&self, provider: &str) -> Option<String> {
        self.registry.resolve_api_key(provider)
    }

    async fn stream_options(&self) -> StreamOptions {
        let mut o = StreamOptions::default();
        if let Some(model) = self.agent.model() {
            o.headers = self.registry.provider_headers(&model.provider);
        }
        let Some(host) = self.host() else { return o };
        if host.has_handlers("before_provider_headers").await {
            let r = self.dispatch("before_provider_headers", json!({"type": "before_provider_headers", "headers": o.headers}), None).await;
            if let Some(h) = r.get("headers").and_then(|h| h.as_object()) {
                o.headers = h.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect();
            }
        }
        if host.has_handlers("before_provider_request").await {
            let h = host.clone();
            o.on_payload = Some(Arc::new(move |payload: Value| {
                let h = h.clone();
                Box::pin(async move {
                    match h.dispatch("before_provider_request", json!({"type": "before_provider_request", "payload": payload}), None).await {
                        Ok(out) => out.result.get("payload").cloned(),
                        Err(_) => None,
                    }
                })
            }));
        }
        if host.has_handlers("after_provider_response").await {
            let h = host.clone();
            o.on_response = Some(Arc::new(move |status: u16, headers: HashMap<String, String>| {
                let h = h.clone();
                Box::pin(async move {
                    let _ = h.dispatch("after_provider_response", json!({"type": "after_provider_response", "status": status, "headers": headers}), None).await;
                })
            }));
        }
        o
    }
}

// ---------------------------------------------------------------------------
// HostCallbacks: what extensions can ask the app for
// ---------------------------------------------------------------------------

#[async_trait]
impl HostCallbacks for Inner {
    fn context_info(&self) -> ContextInfo {
        let session = self.session.lock().unwrap();
        ContextInfo {
            cwd: self.cwd.to_string_lossy().to_string(),
            mode: self.ui.mode().to_string(),
            has_ui: self.ui.has_ui(),
            model: self.agent.model().and_then(|m| serde_json::to_value(m).ok()),
            thinking_level: self.agent.thinking_level().as_str().to_string(),
            is_idle: !self.running.load(Ordering::SeqCst),
            session_file: session.get_session_file().map(|p| p.to_string_lossy().to_string()),
            session_id: session.get_session_id().to_string(),
        }
    }
    async fn ui_select(&self, title: String, options: Vec<String>, opts: Value) -> Option<String> {
        trace("ui_select start");
        let r = self.ui.select(title, options, opts).await;
        trace("ui_select end");
        r
    }
    async fn ui_confirm(&self, title: String, message: String, opts: Value) -> bool {
        self.ui.confirm(title, message, opts).await
    }
    async fn ui_input(&self, title: String, placeholder: String, opts: Value) -> Option<String> {
        self.ui.input(title, placeholder, opts).await
    }
    async fn ui_editor(&self, title: String, prefill: String) -> Option<String> {
        self.ui.editor(title, prefill).await
    }
    fn ui_notify(&self, message: String, kind: String) {
        self.ui.emit(UiEvent::Notify { message, kind });
    }
    fn ui_set_status(&self, key: String, text: Option<String>) {
        self.ui.emit(UiEvent::Status { key, text });
    }
    fn ui_set_working_message(&self, message: Option<String>) {
        self.ui.emit(UiEvent::WorkingMessage(message));
    }
    fn ui_set_widget(&self, key: String, lines: Option<Vec<String>>, placement: Option<String>) {
        self.ui.emit(UiEvent::Widget { key, lines, placement });
    }
    fn ui_set_title(&self, title: String) {
        self.ui.emit(UiEvent::Title(title));
    }
    fn ui_set_editor_text(&self, text: String) {
        self.ui.emit(UiEvent::SetEditorText(text));
    }
    fn ui_get_editor_text(&self) -> String {
        self.ui.get_editor_text()
    }
    fn console(&self, level: String, message: String) {
        self.ui.emit(UiEvent::Console { level, message });
    }
    fn session_entries(&self) -> Value {
        serde_json::to_value(self.session.lock().unwrap().get_entries()).unwrap_or(json!([]))
    }
    fn session_branch(&self) -> Value {
        serde_json::to_value(self.session.lock().unwrap().get_branch()).unwrap_or(json!([]))
    }
    fn session_leaf_id(&self) -> Option<String> {
        self.session.lock().unwrap().get_leaf_id().map(|s| s.to_string())
    }
    fn session_label(&self, id: String) -> Option<String> {
        self.session.lock().unwrap().get_label(&id).map(|s| s.to_string())
    }
    fn get_session_name(&self) -> Option<String> {
        self.session.lock().unwrap().get_session_name()
    }
    fn set_session_name(&self, name: String) {
        let _ = self.session.lock().unwrap().set_session_name(&name);
        let inner = self.clone_arc();
        tokio::spawn(async move {
            inner.dispatch("session_info_changed", json!({"type": "session_info_changed", "name": name}), None).await;
        });
    }
    fn set_label(&self, entry_id: String, label: Option<String>) {
        let _ = self.session.lock().unwrap().set_label(&entry_id, label.as_deref());
    }
    async fn sessions_list(&self, cwd: Option<String>) -> Value {
        let cwd = cwd.unwrap_or_else(|| self.cwd.to_string_lossy().to_string());
        let dir = self.session.lock().unwrap().get_session_dir().to_path_buf();
        let infos = SessionManager::list(&cwd, Some(&dir)).unwrap_or_default();
        json!(infos.iter().map(|i| json!({"path": i.path, "id": i.id, "cwd": i.cwd, "name": i.name, "created": i.created.to_rfc3339(), "modified": i.modified.to_rfc3339(), "messageCount": i.message_count, "firstMessage": i.first_message, "file": i.path})).collect::<Vec<_>>())
    }
    fn send_message(&self, message: Value, options: Value) {
        let msg = AgentMessage::Custom(CustomMessage {
            custom_type: message["customType"].as_str().unwrap_or("extension").to_string(),
            content: serde_json::from_value(message["content"].clone()).unwrap_or(UserContent::Text(String::new())),
            display: message["display"].as_bool().unwrap_or(false),
            details: message.get("details").cloned().filter(|d| !d.is_null()),
            timestamp: now_ms(),
        });
        self.deliver(msg, options["deliverAs"].as_str().unwrap_or("steer"), options["triggerTurn"].as_bool().unwrap_or(false), false);
    }
    fn send_user_message(&self, content: Value, options: Value) {
        let content: UserContent = match content {
            Value::String(s) => UserContent::Text(s),
            other => serde_json::from_value(other).unwrap_or(UserContent::Text(String::new())),
        };
        let msg = AgentMessage::User(UserMessage { content, timestamp: now_ms() });
        self.deliver(msg, options["deliverAs"].as_str().unwrap_or("steer"), true, options["expandPromptTemplates"].as_bool().unwrap_or(false));
    }
    fn append_entry(&self, custom_type: String, data: Value) {
        let _ = self.session.lock().unwrap().append_custom_entry(&custom_type, if data.is_null() { None } else { Some(data) });
    }
    fn get_active_tools(&self) -> Vec<String> {
        self.agent.tools().iter().map(|t| t.name()).collect()
    }
    fn set_active_tools(&self, names: Vec<String>) {
        *self.active_tools.lock().unwrap() = names;
        self.refresh_tools();
    }
    fn get_all_tools(&self) -> Value {
        Value::Array(self.all_tool_infos())
    }
    fn get_commands(&self) -> Value {
        let cmds = self.commands.lock().unwrap().clone();
        json!(cmds.iter().map(|c| json!({"name": c.invocation.clone().unwrap_or(c.name.clone()), "description": c.description, "source": "extension", "sourceInfo": {"path": c.extension_path}})).collect::<Vec<_>>())
    }
    fn tools_changed(&self) {
        let inner = self.clone_arc();
        tokio::spawn(async move {
            inner.refresh_extension_registrations().await;
            inner.refresh_tools();
        });
    }
    async fn set_model(&self, model: Value) -> bool {
        let Ok(m) = serde_json::from_value::<Model>(model) else { return false };
        let previous = self.agent.model();
        self.agent.set_model(m.clone());
        let _ = self.session.lock().unwrap().append_model_change(&m.provider, &m.id);
        self.dispatch("model_select", json!({"type": "model_select", "model": m, "previousModel": previous, "source": "set"}), None).await;
        true
    }
    fn get_thinking_level(&self) -> String {
        self.agent.thinking_level().as_str().to_string()
    }
    fn set_thinking_level(&self, level: String) {
        if let Some(l) = ThinkingLevel::parse(&level) {
            let previous = self.agent.thinking_level();
            self.agent.set_thinking_level(l);
            let _ = self.session.lock().unwrap().append_thinking_level_change(l.as_str());
            let inner = self.clone_arc();
            tokio::spawn(async move {
                inner.dispatch("thinking_level_select", json!({"type": "thinking_level_select", "level": l, "previousLevel": previous}), None).await;
            });
        }
    }
    fn register_provider(&self, name: String, config: Value) {
        if let Ok(cfg) = serde_json::from_value::<pi_ai::ProviderConfig>(config) {
            self.registry.register_provider(&name, &cfg);
        }
    }
    fn unregister_provider(&self, name: String) {
        self.registry.unregister_provider(&name);
    }
    fn models_available(&self) -> Value {
        json!(self.registry.available())
    }
    fn models_all(&self) -> Value {
        json!(self.registry.models())
    }
    fn models_find(&self, spec: String) -> Option<Value> {
        self.registry.find(&spec).and_then(|m| serde_json::to_value(m).ok())
    }
    fn models_get(&self, provider: String, id: String) -> Option<Value> {
        self.registry.get(&provider, &id).and_then(|m| serde_json::to_value(m).ok())
    }
    fn models_provider_auth(&self, provider: String) -> Value {
        json!({"apiKey": self.registry.resolve_api_key(&provider), "headers": self.registry.provider_headers(&provider)})
    }
    async fn models_complete(&self, model: Value, context: Value, options: Value) -> Value {
        let model: Model = match serde_json::from_value(model) {
            Ok(m) => m,
            Err(e) => return json!({"role": "assistant", "content": [], "stopReason": "error", "errorMessage": format!("invalid model: {e}")}),
        };
        let mut ctx: Context = serde_json::from_value(context).unwrap_or_default();
        // Extension contexts may include non-LLM roles; drop them.
        ctx.messages.retain(|m| !matches!(m, Message::System(_)));
        let mut opts = StreamOptions { api_key: self.registry.resolve_api_key(&model.provider), ..Default::default() };
        if let Some(r) = options["reasoning"].as_str().and_then(ThinkingLevel::parse) {
            opts.reasoning = r;
        }
        opts.max_tokens = options["maxTokens"].as_u64();
        let msg = pi_ai::complete(&model, ctx, opts).await;
        serde_json::to_value(AgentMessage::Assistant(msg)).unwrap_or(Value::Null)
    }
    fn abort(&self) {
        self.agent.abort();
    }
    fn shutdown(&self) {
        self.shutdown_requested.store(true, Ordering::SeqCst);
        if !self.running.load(Ordering::SeqCst) {
            self.ui.emit(UiEvent::Shutdown);
        }
    }
    fn has_pending_messages(&self) -> bool {
        self.agent.has_queued_messages()
    }
    fn context_usage(&self) -> Option<Value> {
        let model = self.agent.model()?;
        let messages = self.agent.messages();
        let last_usage = messages.iter().rev().find_map(|m| if let AgentMessage::Assistant(a) = m { Some(a.usage.clone()) } else { None });
        let tokens = last_usage.map(|u| u.input + u.output + u.cache_read + u.cache_write);
        Some(json!({"tokens": tokens, "contextWindow": model.context_window, "percent": tokens.map(|t| t as f64 / model.context_window as f64 * 100.0)}))
    }
    fn system_prompt(&self) -> String {
        self.agent.with_state(|s| s.system_prompt.clone())
    }
    fn system_prompt_options(&self) -> Value {
        serde_json::to_value(self.prompt_options.lock().unwrap().clone()).unwrap_or(json!({}))
    }
    async fn wait_for_idle(&self) {
        Inner::wait_for_idle(self).await
    }
    fn settings(&self) -> Value {
        self.settings.raw.clone()
    }
    fn truncate(&self, text: String, tail: bool, options: Value) -> Value {
        let opts = tools::truncate::TruncationOptions {
            max_lines: options["maxLines"].as_u64().map(|v| v as usize).unwrap_or(tools::truncate::DEFAULT_MAX_LINES),
            max_bytes: options["maxBytes"].as_u64().map(|v| v as usize).unwrap_or(tools::truncate::DEFAULT_MAX_BYTES),
        };
        let r = if tail { tools::truncate::truncate_tail(&text, opts) } else { tools::truncate::truncate_head(&text, opts) };
        serde_json::to_value(r).unwrap_or(json!({"content": text, "truncated": false}))
    }
    fn build_system_prompt(&self, options: Value) -> String {
        let mut o = self.prompt_options.lock().unwrap().clone();
        if let Ok(over) = serde_json::from_value::<BuildSystemPromptOptions>(options) {
            if over.custom_prompt.is_some() {
                o.custom_prompt = over.custom_prompt;
            }
            if !over.selected_tools.is_empty() {
                o.selected_tools = over.selected_tools;
            }
            if !over.append_system_prompt.is_empty() {
                o.append_system_prompt = over.append_system_prompt;
            }
        }
        build_system_prompt(&o)
    }
    fn builtin_tool_info(&self, name: String) -> Option<Value> {
        let t = self.builtin_tools.iter().find(|t| t.name() == name)?;
        Some(json!({"name": t.name(), "label": t.label(), "description": t.description(), "parameters": t.parameters(), "promptSnippet": t.prompt_snippet(), "promptGuidelines": t.prompt_guidelines()}))
    }
    async fn execute_builtin_tool(&self, name: String, tool_call_id: String, args: Value, cwd: Option<String>, cancel: CancellationToken) -> Result<ToolResult, String> {
        let cwd = cwd.map(PathBuf::from).unwrap_or_else(|| self.cwd.clone());
        let tool = tools::tool_by_name(&cwd, &name).ok_or_else(|| format!("unknown built-in tool {name}"))?;
        let args = tool.prepare_arguments(args);
        tool.execute(&tool_call_id, args, cancel, Arc::new(|_| {})).await.map_err(|e| e.to_string())
    }
    fn report_error(&self, err: ExtensionError) {
        self.record_error(err);
    }
}

impl Inner {
    /// Re-acquire the owning `Arc` (Inner is always constructed inside one).
    fn clone_arc(&self) -> Arc<Inner> {
        self.self_weak.upgrade().expect("session inner alive")
    }

    /// Deliver an extension-originated message.
    fn deliver(&self, msg: AgentMessage, deliver_as: &str, trigger_turn: bool, _expand: bool) {
        let running = self.running.load(Ordering::SeqCst);
        match deliver_as {
            "nextTurn" => {
                self.next_turn_messages.lock().unwrap().push(msg);
            }
            "followUp" if running => self.agent.follow_up(msg),
            _ if running => self.agent.steer(msg),
            _ => {
                if trigger_turn {
                    let inner = self.clone_arc();
                    tokio::spawn(async move {
                        let text = if let AgentMessage::User(u) = &msg { Some(u.content.plain_text()) } else { None };
                        if let Err(e) = inner.run_prompt(vec![msg], text).await {
                            inner.ui.emit(UiEvent::Notify { message: format!("extension-triggered run failed: {e}"), kind: "error".into() });
                        }
                    });
                } else {
                    // Idle without triggering a turn: append to context and persist.
                    self.agent.append_message(msg.clone());
                    let _ = self.session.lock().unwrap().append_message(msg.clone());
                    self.ui.emit(UiEvent::Agent(AgentEvent::MessageEnd { message: msg }));
                }
            }
        }
    }
}

impl AgentSession {
    /// Render a tool call through the extension's `renderCall` (if any).
    pub async fn render_tool_call(&self, name: &str, args: &Value, width: usize) -> Option<Vec<String>> {
        let host = self.0.host()?;
        if !self.0.extension_tools.lock().unwrap().iter().any(|t| t.name == name && t.has_render_call) {
            return None;
        }
        host.render_tool_call(name, args.clone(), width).await
    }
    /// Render a tool result through the extension's `renderResult` (if any).
    pub async fn render_tool_result(&self, name: &str, result: &Value, expanded: bool, width: usize) -> Option<Vec<String>> {
        let host = self.0.host()?;
        if !self.0.extension_tools.lock().unwrap().iter().any(|t| t.name == name && t.has_render_result) {
            return None;
        }
        host.render_tool_result(name, result.clone(), expanded, width).await
    }
    pub async fn render_message(&self, custom_type: &str, message: &Value, width: usize) -> Option<Vec<String>> {
        let host = self.0.host()?;
        host.render_message(custom_type, message.clone(), width).await
    }
}

/// Pick the startup model: explicit spec, settings, then the first model
/// whose provider has credentials.
pub fn resolve_startup_model(registry: &ModelRegistry, spec: Option<&str>, settings: &Settings) -> anyhow::Result<Model> {
    if let Some(s) = spec {
        return registry.find(s).ok_or_else(|| anyhow::anyhow!("Unknown model '{s}'. Use --list-models to see the catalog."));
    }
    if let (Some(p), Some(m)) = (&settings.default_provider, &settings.default_model) {
        if let Some(model) = registry.get(p, m) {
            return Ok(model);
        }
    }
    for provider in ["anthropic", "openai", "openrouter"] {
        if registry.has_credentials(provider) {
            if let Some(id) = pi_ai::registry::default_model_for_provider(provider) {
                if let Some(m) = registry.get(provider, id) {
                    return Ok(m);
                }
            }
        }
    }
    if let Some(m) = registry.available().into_iter().find(|m| m.provider != "faux") {
        return Ok(m);
    }
    anyhow::bail!("No model available. Set ANTHROPIC_API_KEY or OPENAI_API_KEY, add providers to ~/.pi/agent/models.json, or use --model faux/scripted for an offline demo.")
}
