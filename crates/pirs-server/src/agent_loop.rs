//! A running loop: a `pi_agent::Agent` over one conversation in one cwd.
//!
//! [`LoopHandle`] owns the agent, the session log (as [`LoopLog`]), the
//! active tool set, the handler registrations and the loop's state. A run
//! starts from `loop.prompt` and is `working` until the agent is idle again;
//! everything the agent does arrives through [`AgentHooks`] (tool results,
//! credentials) and the agent-event listener, which turns each finished
//! message into a log entry and the turn and run boundaries into `custom`
//! entries (see `log`).
//!
//! What the old in-process `pi-cli` glue did in `agent_session.rs` lives here,
//! reduced to the protocol: no UI backend, no extension host, no dialogs.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use pi_agent::{
    AfterToolCallContext, AfterToolCallResult, Agent, AgentEvent, AgentHooks, AgentMessage, NoHooks, ToolExecutionMode,
    ToolRef, ToolResult,
};
use pi_ai::{now_ms, AssistantMessageEvent, Model, ModelRegistry, StopReason, StreamOptions, SystemMessage, UserContent};
use pirs_protocol::{
    ChangedBy, Delta, Event, LoopInfo, LoopState, Manifest, ModelSpec, NotifyLevel, OnPayload, OnReloadPayload,
    OnStartPayload, OnToolResultPayload, PromptWhen, Role, ServerPath, ToolReply,
};
use serde_json::{json, Value};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::convert;
use crate::dispatch::Registration;
use crate::log::{self, LoopLog, StarSubscribers};
use crate::session::{get_current_system_message, SessionManager};
use crate::settings::{self, Settings};
use crate::system_prompt::{build_system_prompt_sections, load_context_files, BuildSystemPromptOptions};
use crate::tools;

/// Why a loop could not be created.
#[derive(Debug)]
pub(crate) enum CreateError {
    /// `session` named no stored conversation.
    NotFound(String),
    /// A parameter was unusable (cwd, model).
    Invalid(String),
    /// Something else failed.
    Internal(anyhow::Error),
}

/// `loop.create` parameters, already typed.
pub(crate) struct CreateOptions {
    pub(crate) cwd: PathBuf,
    pub(crate) model: Option<ModelSpec>,
    pub(crate) name: Option<String>,
    pub(crate) session: Option<String>,
}

/// Per-run bookkeeping: which seqs the current turn and run appended, and
/// the arguments of the tool calls seen, by call id.
#[derive(Default)]
struct RunState {
    turn_seqs: Vec<u64>,
    run_seqs: Vec<u64>,
    tool_args: HashMap<String, Value>,
}

/// The original of a `tool_result` rewrite, waiting for its message entry.
struct Rewrite {
    original: ToolReply,
    by: String,
}

/// What the last persisted system message declared, so a run only logs a
/// new one when the prompt or the tool loadout changed.
#[derive(PartialEq)]
struct SystemFingerprint(String);

impl SystemFingerprint {
    fn of(message: &SystemMessage) -> Self {
        let mut tools: Vec<Value> =
            message.tools_added.iter().flatten().map(|t| json!({"name": t.name, "parameters": t.parameters})).collect();
        tools.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        let sections: BTreeMap<&String, &Option<String>> = message.sections.iter().flatten().collect();
        SystemFingerprint(json!({"content": message.content.plain_text(), "sections": sections, "tools": tools}).to_string())
    }
}

pub(crate) struct LoopHandle {
    pub(crate) id: String,
    pub(crate) name: Option<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) conversation: String,
    pub(crate) log: Mutex<LoopLog>,
    pub(crate) agent: Agent,
    pub(crate) handlers: Mutex<Vec<Registration>>,
    registry: ModelRegistry,
    status: Mutex<(LoopState, u64)>,
    working: AtomicBool,
    idle: Notify,
    builtin: Vec<ToolRef>,
    active_tools: Mutex<Vec<String>>,
    prompt_options: Mutex<BuildSystemPromptOptions>,
    next_input: Mutex<Vec<String>>,
    run: Mutex<RunState>,
    rewrites: Mutex<HashMap<String, Rewrite>>,
    status_keys: Mutex<BTreeSet<String>>,
    widget_keys: Mutex<BTreeSet<String>>,
    last_system: Mutex<Option<SystemFingerprint>>,
    /// Every process spawned for this loop; killed on close (D-23). Nothing
    /// spawns one this phase; the structure is what phase 2 and 4 fill.
    processes: Mutex<Vec<tokio::process::Child>>,
    closed: AtomicBool,
    /// Cancelled by `abort` and `close`; replaced by a fresh token when a run
    /// starts. `Agent::abort` only bites once the run has reached the agent,
    /// so this is what an abort landing while the `input` and `prompt`
    /// handlers run has to catch: `run` checks it before the model call.
    cancel: Mutex<CancellationToken>,
}

/// Pick the model: explicit spec, settings, then the first provider with
/// credentials (as `pi-cli` did).
pub(crate) fn resolve_startup_model(registry: &ModelRegistry, spec: Option<&str>, settings: &Settings) -> Result<Model, String> {
    if let Some(s) = spec {
        return registry.find(s).ok_or_else(|| format!("unknown model {s:?}"));
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
    Err("no model available: set ANTHROPIC_API_KEY or OPENAI_API_KEY, add providers to ~/.pirs/models.json, or use faux/scripted".into())
}

/// A registry with the built-ins, `models.json` overlays and OAuth lookup.
pub(crate) fn registry_for(cwd: &Path) -> ModelRegistry {
    let registry = ModelRegistry::with_builtins();
    registry.set_agent_dir(settings::agent_dir());
    settings::load_models_json(cwd, &registry);
    registry
}

impl LoopHandle {
    pub(crate) fn create(id: String, opts: CreateOptions, star: StarSubscribers) -> Result<Arc<Self>, CreateError> {
        let cwd = std::fs::canonicalize(&opts.cwd).map_err(|e| CreateError::Invalid(format!("cwd {}: {e}", opts.cwd.display())))?;
        if !cwd.is_dir() {
            return Err(CreateError::Invalid(format!("cwd {} is not a directory", cwd.display())));
        }
        let cwd_str = cwd.to_string_lossy().into_owned();
        let settings = settings::load_settings(&cwd);
        let registry = registry_for(&cwd);

        let mut session = match &opts.session {
            Some(wanted) => {
                let path = SessionManager::find_by_id(&cwd_str, wanted, None)
                    .and_then(|p| match p {
                        Some(p) => Ok(Some(p)),
                        None => SessionManager::find_by_name(&cwd_str, wanted, None),
                    })
                    .map_err(CreateError::Internal)?
                    .ok_or_else(|| CreateError::NotFound(format!("no conversation {wanted:?} in {cwd_str}")))?;
                SessionManager::open_with(path, None, Some(&cwd_str)).map_err(CreateError::Internal)?
            }
            None => SessionManager::create(&cwd_str, None).map_err(CreateError::Internal)?,
        };
        if let Some(name) = opts.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            session.set_session_name(name).map_err(CreateError::Internal)?;
        }
        let name = session.get_session_name();
        let conversation = session.get_session_id().to_owned();

        let ctx = session.build_session_context();
        let model = match &opts.model {
            Some(spec) => registry.find(&spec.model).ok_or_else(|| CreateError::Invalid(format!("unknown model {:?}", spec.model)))?,
            None => match ctx.model.as_ref().and_then(|(p, i)| registry.get(p, i)) {
                Some(m) => m,
                None => resolve_startup_model(&registry, None, &settings).map_err(CreateError::Invalid)?,
            },
        };
        let thinking = opts
            .model
            .as_ref()
            .and_then(|s| s.thinking)
            .map(convert::thinking_from_wire)
            .or_else(|| ctx.thinking_level.as_deref().and_then(pi_ai::ThinkingLevel::parse))
            .or_else(|| settings.default_thinking_level.as_deref().and_then(pi_ai::ThinkingLevel::parse))
            .unwrap_or_default();

        let agent = Agent::new(model, Arc::new(NoHooks));
        agent.set_thinking_level(thinking);
        let sequential = settings.tool_execution.as_deref() == Some("sequential");
        agent.with_state(|s| s.tool_execution = if sequential { ToolExecutionMode::Sequential } else { ToolExecutionMode::Parallel });
        let last_system = get_current_system_message(&ctx.messages).map(|m| SystemFingerprint::of(&m));
        if !ctx.messages.is_empty() {
            agent.replace_messages(ctx.messages);
        }

        let prompt_options = BuildSystemPromptOptions {
            cwd: cwd_str.clone(),
            context_files: load_context_files(&cwd),
            ..Default::default()
        };
        let handle = Arc::new(LoopHandle {
            id: id.clone(),
            name,
            cwd: cwd.clone(),
            conversation,
            log: Mutex::new(LoopLog::new(id, session, star)),
            agent: agent.clone(),
            handlers: Mutex::new(Vec::new()),
            registry,
            status: Mutex::new((LoopState::Idle, now_ms())),
            working: AtomicBool::new(false),
            idle: Notify::new(),
            builtin: tools::builtin_tools(&cwd),
            active_tools: Mutex::new(tools::DEFAULT_TOOL_NAMES.iter().map(|s| s.to_string()).collect()),
            prompt_options: Mutex::new(prompt_options),
            next_input: Mutex::new(Vec::new()),
            run: Mutex::new(RunState::default()),
            rewrites: Mutex::new(HashMap::new()),
            status_keys: Mutex::new(BTreeSet::new()),
            widget_keys: Mutex::new(BTreeSet::new()),
            last_system: Mutex::new(last_system),
            processes: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
            cancel: Mutex::new(CancellationToken::new()),
        });
        agent.set_hooks(Arc::new(LoopHooks { handle: Arc::downgrade(&handle) }));
        let weak = Arc::downgrade(&handle);
        agent.subscribe(Arc::new(move |event| {
            let weak = weak.clone();
            Box::pin(async move {
                if let Some(handle) = weak.upgrade() {
                    handle.on_agent_event(event);
                }
            })
        }));
        handle.refresh_tools();
        handle.set_status(LoopState::Idle, Some("created".to_owned()));
        handle.notify_on(OnPayload::Start(OnStartPayload {
            loop_id: handle.id.clone(),
            cwd: ServerPath::from(cwd_str),
        }));
        Ok(handle)
    }

    // ----- state ----------------------------------------------------------

    pub(crate) fn is_working(&self) -> bool {
        self.working.load(Ordering::SeqCst)
    }

    /// Resolves when the loop is idle; immediately if it is.
    pub(crate) async fn wait_idle(&self) {
        loop {
            let notified = self.idle.notified();
            if !self.is_working() {
                return;
            }
            notified.await;
        }
    }

    pub(crate) fn model_spec(&self) -> ModelSpec {
        ModelSpec {
            model: self.agent.model().map(|m| m.key()).unwrap_or_default(),
            thinking: Some(convert::thinking_to_wire(self.agent.thinking_level())),
        }
    }

    pub(crate) fn info(&self) -> LoopInfo {
        let (state, since) = *self.status.lock().unwrap_or_else(|e| e.into_inner());
        LoopInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            cwd: ServerPath::from(self.cwd.to_string_lossy().into_owned()),
            model: self.model_spec(),
            state,
            since,
            conversation: self.conversation.clone(),
        }
    }

    /// Every tool the model could be given: built-ins (unless a handler
    /// provides one of the same name) then handler tools.
    pub(crate) fn available_tools(self: &Arc<Self>) -> Vec<ToolRef> {
        let handler_tools = self.handler_tools();
        let mut all: Vec<ToolRef> =
            self.builtin.iter().filter(|b| !handler_tools.iter().any(|h| h.name() == b.name())).cloned().collect();
        all.extend(handler_tools);
        all
    }

    pub(crate) fn manifest(self: &Arc<Self>) -> Manifest {
        Manifest {
            tools: self.available_tools().iter().map(convert::tool_info).collect(),
            commands: Vec::new(),
            status_keys: self.status_keys.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect(),
            widget_keys: self.widget_keys.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect(),
        }
    }

    /// Rebuild the agent's tool list and base system prompt from the active
    /// names. Handler tools are active whenever registered.
    pub(crate) fn refresh_tools(self: &Arc<Self>) -> (Vec<ToolRef>, String) {
        let all = self.available_tools();
        let mut selected = self.active_tools.lock().unwrap_or_else(|e| e.into_inner()).clone();
        for t in &all {
            let n = t.name();
            if !self.builtin.iter().any(|b| b.name() == n) && !selected.contains(&n) {
                selected.push(n);
            }
        }
        let chosen: Vec<ToolRef> = all.into_iter().filter(|t| selected.contains(&t.name())).collect();
        self.agent.set_tools(chosen.clone());
        let prompt = {
            let mut o = self.prompt_options.lock().unwrap_or_else(|e| e.into_inner());
            o.selected_tools = chosen.iter().map(|t| t.name()).collect();
            o.tool_snippets = chosen.iter().filter_map(|t| t.prompt_snippet().map(|s| (t.name(), s))).collect();
            o.tool_guidelines =
                chosen.iter().map(|t| (t.name(), t.prompt_guidelines())).filter(|(_, g)| !g.is_empty()).collect();
            crate::system_prompt::build_system_prompt(&o)
        };
        self.agent.set_system_prompt(prompt.clone());
        (chosen, prompt)
    }

    /// `loop.tools`: set the active tool set by name.
    pub(crate) fn set_tools(self: &Arc<Self>, names: Vec<String>) -> Result<(), String> {
        let available: Vec<String> = self.available_tools().iter().map(|t| t.name()).collect();
        if let Some(unknown) = names.iter().find(|n| !available.contains(n)) {
            return Err(format!("unknown tool {unknown:?}; available: {}", available.join(", ")));
        }
        *self.active_tools.lock().unwrap_or_else(|e| e.into_inner()) = names;
        self.refresh_tools();
        Ok(())
    }

    /// `loop.model`: switch model and thinking level; takes effect at the
    /// next LLM request. Logged as `model_change` / `thinking_level_change`.
    pub(crate) fn set_model(&self, spec: &ModelSpec) -> Result<(), String> {
        let model = self.registry.find(&spec.model).ok_or_else(|| format!("unknown model {:?}", spec.model))?;
        if self.agent.model().map(|m| m.key()) != Some(model.key()) {
            self.agent.set_model(model.clone());
            let mut log = self.log.lock().unwrap_or_else(|e| e.into_inner());
            if let Err(e) = log.session_mut().append_model_change(&model.provider, &model.id) {
                tracing::warn!(loop_id = %self.id, "could not log model change: {e}");
            }
        }
        if let Some(level) = spec.thinking.map(convert::thinking_from_wire) {
            if level != self.agent.thinking_level() {
                self.agent.set_thinking_level(level);
                let mut log = self.log.lock().unwrap_or_else(|e| e.into_inner());
                if let Err(e) = log.session_mut().append_thinking_level_change(level.as_str()) {
                    tracing::warn!(loop_id = %self.id, "could not log thinking level change: {e}");
                }
            }
        }
        Ok(())
    }

    /// `loop.reload`: re-read the context files (`AGENTS.md` …). Policy files
    /// arrive in phase 2, so the returned list is empty. Refused while working.
    pub(crate) fn reload(self: &Arc<Self>) -> Result<Vec<ServerPath>, String> {
        if self.is_working() {
            return Err("cannot reload while the loop is working".into());
        }
        let files = load_context_files(&self.cwd);
        self.prompt_options.lock().unwrap_or_else(|e| e.into_inner()).context_files = files;
        self.refresh_tools();
        let files: Vec<ServerPath> = Vec::new();
        self.notify_on(OnPayload::Reload(OnReloadPayload { loop_id: self.id.clone(), files: files.clone() }));
        Ok(files)
    }

    // ----- prompting --------------------------------------------------------

    /// `loop.prompt`. Idle: start a run (or hold the text for the next one).
    /// Working: steer now, follow up after the turn, or hold.
    pub(crate) fn prompt(self: &Arc<Self>, text: String, when: PromptWhen) {
        if when == PromptWhen::NextInput {
            self.next_input.lock().unwrap_or_else(|e| e.into_inner()).push(text);
            return;
        }
        if self.closed.load(Ordering::SeqCst) {
            tracing::debug!(loop_id = %self.id, "prompt on a closed loop, ignored");
            return;
        }
        // The token is installed under the same lock `abort` takes, so an
        // abort can never land between claiming the run and arming it.
        let fresh = {
            let mut cancel = self.cancel.lock().unwrap_or_else(|e| e.into_inner());
            if self.working.swap(true, Ordering::SeqCst) {
                None
            } else {
                *cancel = CancellationToken::new();
                Some(cancel.clone())
            }
        };
        let Some(cancel) = fresh else {
            // A run is in progress: the text passes through `input` and joins
            // the run's queues. If the run ends meanwhile the message waits in
            // the queue for the next run, as it does in pi.
            let this = self.clone();
            tokio::spawn(async move {
                if let Some(text) = this.dispatch_input(text).await {
                    let message = AgentMessage::user(text);
                    match when {
                        PromptWhen::AfterTurn => this.agent.follow_up(message),
                        _ => this.agent.steer(message),
                    }
                }
            });
            return;
        };
        let this = self.clone();
        tokio::spawn(async move { this.run(text, cancel).await });
    }

    /// `loop.abort`: cancel the current run. Idle loops are unaffected.
    ///
    /// Both halves matter: the token stops a run still in its handlers (and
    /// any handler tool call, which sees the agent's own token), and
    /// `Agent::abort` stops one the agent is already running.
    pub(crate) fn abort(&self) {
        self.cancel.lock().unwrap_or_else(|e| e.into_inner()).cancel();
        self.agent.abort();
    }

    async fn run(self: Arc<Self>, text: String, cancel: CancellationToken) {
        self.set_status(LoopState::Working, None);
        let held: Vec<String> = std::mem::take(&mut *self.next_input.lock().unwrap_or_else(|e| e.into_inner()));
        let mut prompts = Vec::new();
        for t in held.into_iter().chain(std::iter::once(text)) {
            if let Some(t) = self.dispatch_input(t).await {
                prompts.push(AgentMessage::user(t));
            }
        }
        if prompts.is_empty() {
            self.finish_run(Some("handled".into()));
            return;
        }
        let (chosen, base_prompt) = self.refresh_tools();
        let prompt = self.dispatch_prompt(base_prompt.clone()).await;
        if cancel.is_cancelled() {
            // An abort or a close landed while the handlers ran: no model
            // call, no system message, straight to idle.
            self.finish_run(Some("aborted".to_owned()));
            return;
        }
        *self.run.lock().unwrap_or_else(|e| e.into_inner()) = RunState::default();
        self.persist_system_message(&base_prompt, &prompt, &chosen);
        self.agent.set_system_prompt(prompt);

        let detail = match self.agent.prompt(prompts).await {
            Err(e) => Some(format!("error: {e}")),
            Ok(messages) => messages.iter().rev().find_map(|m| match m {
                AgentMessage::Assistant(a) => Some(match a.stop_reason {
                    StopReason::Aborted => Some("aborted".to_owned()),
                    StopReason::Error => Some(format!("error: {}", a.error_message.clone().unwrap_or_default())),
                    _ => None,
                }),
                _ => None,
            })
            .flatten(),
        };
        self.finish_run(detail);
    }

    fn finish_run(&self, detail: Option<String>) {
        self.set_status(LoopState::Idle, detail);
        let seqs = std::mem::take(&mut self.run.lock().unwrap_or_else(|e| e.into_inner()).run_seqs);
        if let Some(Event::LoopRunEnd(event)) = self.log_custom(log::CT_RUN_END, json!({ "messages": seqs })) {
            self.notify_on(OnPayload::RunEnd(event));
        }
        self.working.store(false, Ordering::SeqCst);
        self.idle.notify_waiters();
    }

    /// Log a system message when the prompt or tool loadout differs from the
    /// last one logged (pi's leading system message with `sections` and
    /// `toolsAdded`). A `prompt` handler's change is visible here (D-21).
    fn persist_system_message(&self, base: &str, prompt: &str, tools: &[ToolRef]) {
        let sections: BTreeMap<String, Option<String>> = {
            let o = self.prompt_options.lock().unwrap_or_else(|e| e.into_inner());
            build_system_prompt_sections(&o).into_iter().map(|(k, v)| (k, Some(v))).collect()
        };
        let (content, sections) = if prompt == base {
            (String::new(), Some(sections))
        } else if let Some(rest) = prompt.strip_prefix(base) {
            let mut sections = sections;
            sections.insert("handlers".into(), Some(rest.trim().to_owned()));
            (String::new(), Some(sections))
        } else {
            (prompt.to_owned(), None)
        };
        let sections: Option<HashMap<String, Option<String>>> = sections.map(|s| s.into_iter().collect());
        let message = SystemMessage {
            content: UserContent::Text(content),
            sections,
            tools_added: Some(tools.iter().map(|t| t.declaration()).collect()),
            timestamp: now_ms(),
        };
        let fingerprint = SystemFingerprint::of(&message);
        let mut last = self.last_system.lock().unwrap_or_else(|e| e.into_inner());
        if last.as_ref() == Some(&fingerprint) {
            return;
        }
        *last = Some(fingerprint);
        drop(last);
        let message = AgentMessage::System(message);
        self.agent.append_message(message.clone());
        let seq = {
            let mut log = self.log.lock().unwrap_or_else(|e| e.into_inner());
            log.append_message(message)
        };
        match seq {
            Ok(seq) => self.run.lock().unwrap_or_else(|e| e.into_inner()).run_seqs.push(seq),
            Err(e) => tracing::warn!(loop_id = %self.id, "could not log system message: {e}"),
        }
    }

    /// `loop.close`: abort a run, wait, kill the loop's processes. The
    /// conversation stays on disk.
    pub(crate) async fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.abort();
        if self.is_working() {
            self.wait_idle().await;
        }
        let children: Vec<tokio::process::Child> =
            std::mem::take(&mut *self.processes.lock().unwrap_or_else(|e| e.into_inner()));
        for mut child in children {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        self.set_status(LoopState::Idle, Some("closed".to_owned()));
        self.handlers.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    // ----- events ---------------------------------------------------------

    fn set_status(&self, state: LoopState, detail: Option<String>) {
        let since = now_ms();
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = (state, since);
        let mut data = json!({ "state": state, "since": since });
        if let Some(detail) = detail {
            data["detail"] = Value::String(detail);
        }
        self.log_custom(log::CT_STATUS, data);
    }

    /// Append a `custom` entry and return the event it produced.
    pub(crate) fn log_custom(&self, custom_type: &str, data: Value) -> Option<Event> {
        let mut log = self.log.lock().unwrap_or_else(|e| e.into_inner());
        match log.append_custom(custom_type, data) {
            Ok(seq) => log.event_at(seq),
            Err(e) => {
                tracing::warn!(loop_id = %self.id, custom_type, "could not append log entry: {e}");
                None
            }
        }
    }

    pub(crate) fn ui_status(&self, key: String, text: String) {
        self.status_keys.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone());
        self.log_custom(log::CT_UI_STATUS, json!({ "key": key, "text": text }));
    }

    pub(crate) fn ui_widget(&self, key: String, lines: Vec<String>) {
        self.widget_keys.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone());
        self.log_custom(log::CT_UI_WIDGET, json!({ "key": key, "lines": lines }));
    }

    pub(crate) fn ui_notify(&self, level: NotifyLevel, text: String) {
        self.log_custom(log::CT_UI_NOTIFY, json!({ "level": level, "text": text }));
    }

    fn on_agent_event(self: &Arc<Self>, event: AgentEvent) {
        match event {
            AgentEvent::TurnStart => self.run.lock().unwrap_or_else(|e| e.into_inner()).turn_seqs.clear(),
            AgentEvent::MessageUpdate { assistant_message_event, .. } => {
                let delta = match *assistant_message_event {
                    AssistantMessageEvent::TextDelta { content_index, delta, .. } => Some(Delta::Text { index: content_index, text: delta }),
                    AssistantMessageEvent::ThinkingDelta { content_index, delta, .. } => {
                        Some(Delta::Thinking { index: content_index, thinking: delta })
                    }
                    _ => None,
                };
                if let Some(delta) = delta {
                    self.log.lock().unwrap_or_else(|e| e.into_inner()).emit_delta(Role::Assistant, delta);
                }
            }
            AgentEvent::MessageEnd { message } => self.on_message_end(message),
            AgentEvent::TurnEnd { .. } => {
                let seqs = self.run.lock().unwrap_or_else(|e| e.into_inner()).turn_seqs.clone();
                if let Some(Event::LoopTurnEnd(event)) = self.log_custom(log::CT_TURN_END, json!({ "messages": seqs })) {
                    self.notify_on(OnPayload::TurnEnd(event));
                }
            }
            // `run_end` is logged after the idle status, in `finish_run`.
            AgentEvent::AgentStart | AgentEvent::AgentEnd { .. } | AgentEvent::MessageStart { .. } => {}
            // Partial tool output has no wire event this phase.
            AgentEvent::ToolExecutionStart { .. } | AgentEvent::ToolExecutionUpdate { .. } | AgentEvent::ToolExecutionEnd { .. } => {}
        }
    }

    fn on_message_end(&self, message: AgentMessage) {
        // An aborted or errored assistant message with nothing in it is not
        // worth a log entry (as in pi).
        if let AgentMessage::Assistant(a) = &message {
            if a.content.is_empty() && matches!(a.stop_reason, StopReason::Aborted | StopReason::Error) {
                return;
            }
        }
        let seq = match self.log.lock().unwrap_or_else(|e| e.into_inner()).append_message(message.clone()) {
            Ok(seq) => seq,
            Err(e) => {
                tracing::warn!(loop_id = %self.id, "could not log message: {e}");
                return;
            }
        };
        {
            let mut run = self.run.lock().unwrap_or_else(|e| e.into_inner());
            run.turn_seqs.push(seq);
            run.run_seqs.push(seq);
            if let AgentMessage::Assistant(a) = &message {
                for call in a.tool_calls() {
                    run.tool_args.insert(call.id, call.arguments);
                }
            }
        }
        let AgentMessage::ToolResult(result) = &message else { return };

        // D-21: the rewritten result is what the log's message entry holds;
        // the original goes right after it, naming the handler.
        if let Some(rewrite) = self.rewrites.lock().unwrap_or_else(|e| e.into_inner()).remove(&result.tool_call_id) {
            self.log_custom(
                log::CT_TOOL_RESULT_REWRITE,
                json!({
                    "toolCallId": result.tool_call_id,
                    "tool": result.tool_name,
                    "messageSeq": seq,
                    "by": rewrite.by,
                    "original": rewrite.original,
                }),
            );
        }

        // `fs.changed` comes from the loop's own file tools only: `write` and
        // `edit` report the absolute path they wrote in `details.path`. `bash`
        // may write anything and reports nothing; there is no watcher.
        if matches!(result.tool_name.as_str(), "write" | "edit") && !result.is_error {
            if let Some(path) = convert::value_str(result.details.as_ref().and_then(|d| d.get("path"))) {
                self.log_custom(log::CT_FS_CHANGED, json!({ "path": path, "by": ChangedBy::Tool }));
            }
        }

        let args = self
            .run
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .tool_args
            .get(&result.tool_call_id)
            .cloned()
            .unwrap_or_else(|| json!({}));
        let as_result = ToolResult {
            content: result.content.clone(),
            details: result.details.clone(),
            usage: result.usage.clone(),
            terminate: false,
        };
        self.notify_on(OnPayload::ToolResult(OnToolResultPayload {
            loop_id: self.id.clone(),
            tool: result.tool_name.clone(),
            args,
            result: convert::reply_from_result(&as_result, result.is_error),
        }));
    }
}

/// The agent hooks: tool-result rewriting through registered handlers, and
/// credentials as `pi-cli` resolved them (env keys, `models.json`, Anthropic
/// OAuth from `~/.pirs`).
struct LoopHooks {
    handle: Weak<LoopHandle>,
}

#[async_trait]
impl AgentHooks for LoopHooks {
    async fn after_tool_call(&self, ctx: AfterToolCallContext<'_>, _cancel: &CancellationToken) -> Option<AfterToolCallResult> {
        let handle = self.handle.upgrade()?;
        let original = convert::reply_from_result(ctx.result, ctx.is_error);
        let (rewritten, by) = handle.dispatch_tool_result(&ctx.tool_call.name, ctx.args, original.clone()).await?;
        handle
            .rewrites
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(ctx.tool_call.id.clone(), Rewrite { original, by });
        let (result, is_error) = convert::result_from_reply(&rewritten);
        Some(AfterToolCallResult {
            content: Some(result.content),
            details: result.details,
            is_error: Some(is_error),
            usage: None,
            terminate: None,
        })
    }

    async fn get_api_key(&self, provider: &str) -> Option<String> {
        let handle = self.handle.upgrade()?;
        match handle.registry.resolve_api_key_async(provider).await {
            Ok(key) => key,
            Err(e) => {
                handle.ui_notify(NotifyLevel::Error, format!("credentials for {provider}: {e}"));
                None
            }
        }
    }

    async fn stream_options(&self) -> StreamOptions {
        let mut options = StreamOptions::default();
        if let Some(handle) = self.handle.upgrade() {
            if let Some(model) = handle.agent.model() {
                options.headers = handle.registry.provider_headers(&model.provider);
            }
        }
        options
    }
}
