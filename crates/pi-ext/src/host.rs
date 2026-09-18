//! The extension host: a dedicated thread running QuickJS with an async
//! runtime. The application talks to it through `ExtensionHost` (channel
//! based, `Send + Clone`) and receives callbacks through `HostCallbacks`.

use crate::loader::{PiLoader, PiResolver};
use crate::types::*;
use async_trait::async_trait;
use pi_agent::{ToolResult, UpdateFn};
use rquickjs::function::{Async, Func};
use rquickjs::{AsyncContext, AsyncRuntime, Ctx, Exception, Function, Value as JsValue};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Callbacks implemented by the application
// ---------------------------------------------------------------------------

/// Everything an extension can ask the application for. All methods have
/// no-op defaults so hosts only implement what they support.
#[async_trait]
pub trait HostCallbacks: Send + Sync {
    fn context_info(&self) -> ContextInfo;

    async fn ui_select(&self, _title: String, _options: Vec<String>, _opts: Value) -> Option<String> {
        None
    }
    async fn ui_confirm(&self, _title: String, _message: String, _opts: Value) -> bool {
        false
    }
    async fn ui_input(&self, _title: String, _placeholder: String, _opts: Value) -> Option<String> {
        None
    }
    async fn ui_editor(&self, _title: String, _prefill: String) -> Option<String> {
        None
    }
    fn ui_dismiss(&self, _dialog_id: String) {}
    fn ui_notify(&self, message: String, kind: String) {
        eprintln!("[{kind}] {message}");
    }
    fn ui_set_status(&self, _key: String, _text: Option<String>) {}
    fn ui_set_working_message(&self, _message: Option<String>) {}
    fn ui_set_widget(&self, _key: String, _lines: Option<Vec<String>>, _placement: Option<String>) {}
    fn ui_set_title(&self, _title: String) {}
    fn ui_set_editor_text(&self, _text: String) {}
    fn ui_get_editor_text(&self) -> String {
        String::new()
    }
    fn console(&self, level: String, message: String) {
        eprintln!("[ext:{level}] {message}");
    }

    fn session_entries(&self) -> Value {
        json!([])
    }
    fn session_branch(&self) -> Value {
        json!([])
    }
    fn session_leaf_id(&self) -> Option<String> {
        None
    }
    fn session_label(&self, _entry_id: String) -> Option<String> {
        None
    }
    fn get_session_name(&self) -> Option<String> {
        None
    }
    fn set_session_name(&self, _name: String) {}
    fn set_label(&self, _entry_id: String, _label: Option<String>) {}
    async fn sessions_list(&self, _cwd: Option<String>) -> Value {
        json!([])
    }

    fn send_message(&self, _message: Value, _options: Value) {}
    fn send_user_message(&self, _content: Value, _options: Value) {}
    fn append_entry(&self, _custom_type: String, _data: Value) {}

    fn get_active_tools(&self) -> Vec<String> {
        Vec::new()
    }
    fn set_active_tools(&self, _names: Vec<String>) {}
    fn get_all_tools(&self) -> Value {
        json!([])
    }
    fn get_commands(&self) -> Value {
        json!([])
    }
    fn tools_changed(&self) {}

    async fn set_model(&self, _model: Value) -> bool {
        false
    }
    fn get_thinking_level(&self) -> String {
        "off".into()
    }
    fn set_thinking_level(&self, _level: String) {}
    fn register_provider(&self, _name: String, _config: Value) {}
    fn unregister_provider(&self, _name: String) {}
    fn models_available(&self) -> Value {
        json!([])
    }
    fn models_all(&self) -> Value {
        json!([])
    }
    fn models_find(&self, _spec: String) -> Option<Value> {
        None
    }
    fn models_get(&self, _provider: String, _id: String) -> Option<Value> {
        None
    }
    fn models_provider_auth(&self, _provider: String) -> Value {
        Value::Null
    }
    async fn models_complete(&self, _model: Value, _context: Value, _options: Value) -> Value {
        json!({"role":"assistant","content":[],"stopReason":"error","errorMessage":"model calls from extensions are not supported by this host"})
    }

    fn abort(&self) {}
    fn shutdown(&self) {}
    fn has_pending_messages(&self) -> bool {
        false
    }
    fn context_usage(&self) -> Option<Value> {
        None
    }
    fn compact(&self, _options: Value) {}
    fn system_prompt(&self) -> String {
        String::new()
    }
    fn system_prompt_options(&self) -> Value {
        json!({})
    }
    async fn wait_for_idle(&self) {}
    async fn new_session(&self, _options: Value) -> Value {
        json!({"cancelled": true})
    }
    async fn fork(&self, _entry_id: String, _options: Value) -> Value {
        json!({"cancelled": true})
    }
    async fn navigate_tree(&self, _target_id: String, _options: Value) -> Value {
        json!({"cancelled": true})
    }
    async fn switch_session(&self, _path: String) -> Value {
        json!({"cancelled": true})
    }
    async fn reload(&self) {}

    fn settings(&self) -> Value {
        json!({})
    }
    fn agent_dir(&self) -> String {
        dirs_home().join(".pi").join("agent").to_string_lossy().to_string()
    }
    fn sessions_dir(&self) -> String {
        dirs_home().join(".pi").join("agent").join("sessions").to_string_lossy().to_string()
    }
    fn truncate(&self, text: String, _tail: bool, _options: Value) -> Value {
        json!({"text": text, "truncated": false})
    }
    fn build_system_prompt(&self, _options: Value) -> String {
        String::new()
    }

    /// Declaration of a built-in tool (for `createBashTool()` and friends).
    fn builtin_tool_info(&self, _name: String) -> Option<Value> {
        None
    }
    /// Run a built-in tool on behalf of an extension.
    async fn execute_builtin_tool(&self, name: String, _tool_call_id: String, _args: Value, _cwd: Option<String>, _cancel: CancellationToken) -> Result<ToolResult, String> {
        Err(format!("built-in tool '{name}' is not available from this host"))
    }

    fn report_error(&self, err: ExtensionError) {
        eprintln!("[extension error] {} ({}): {}", err.extension_path, err.event, err.error);
    }
}

fn dirs_home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

// ---------------------------------------------------------------------------
// Requests
// ---------------------------------------------------------------------------

type Reply<T> = oneshot::Sender<T>;

enum Request {
    Load { path: String, reply: Reply<Result<LoadedExtension, String>> },
    SetLoaded,
    SetFlagValues(Value),
    Dispatch { event: String, payload: Value, cancel: Option<CancellationToken>, reply: Reply<Result<DispatchOutcome, String>> },
    HasHandlers { event: String, reply: Reply<bool> },
    ExecuteTool { name: String, tool_call_id: String, args: Value, cancel: CancellationToken, on_update: UpdateFn, reply: Reply<Result<ToolResult, String>> },
    PrepareArguments { name: String, args: Value, reply: Reply<Value> },
    RunCommand { name: String, args: String, reply: Reply<Result<(), String>> },
    CommandCompletions { name: String, prefix: String, reply: Reply<Option<Value>> },
    RunShortcut { shortcut: String, reply: Reply<bool> },
    ListTools { reply: Reply<Vec<ToolInfo>> },
    ListCommands { reply: Reply<Vec<CommandInfo>> },
    ListExtensions { reply: Reply<Vec<LoadedExtension>> },
    RenderToolCall { name: String, args: Value, width: usize, reply: Reply<Option<Vec<String>>> },
    RenderToolResult { name: String, result: Value, expanded: bool, width: usize, reply: Reply<Option<Vec<String>>> },
    RenderMessage { custom_type: String, message: Value, width: usize, reply: Reply<Option<Vec<String>>> },
    Eval { source: String, reply: Reply<Result<Value, String>> },
    Invalidate { reply: Reply<()> },
    Shutdown,
}

#[derive(Clone)]
pub struct HostConfig {
    pub cwd: PathBuf,
    /// Stack size for the JS runtime thread.
    pub thread_stack_bytes: usize,
    pub js_max_stack_bytes: usize,
    pub js_memory_limit_bytes: Option<usize>,
}

impl HostConfig {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        HostConfig { cwd: cwd.into(), thread_stack_bytes: 32 * 1024 * 1024, js_max_stack_bytes: 8 * 1024 * 1024, js_memory_limit_bytes: None }
    }
}

/// Handle to the extension host thread. Clone freely.
#[derive(Clone)]
pub struct ExtensionHost {
    tx: mpsc::UnboundedSender<Request>,
}

fn send_err<T>(_: T) -> String {
    "extension host is not running".to_string()
}

impl ExtensionHost {
    /// Start the host thread. Returns once the runtime is initialized.
    pub async fn spawn(callbacks: Arc<dyn HostCallbacks>, config: HostConfig) -> anyhow::Result<Self> {
        let (tx, rx) = mpsc::unbounded_channel::<Request>();
        let (ready_tx, ready_rx) = oneshot::channel::<Result<(), String>>();
        let cfg = config.clone();
        std::thread::Builder::new()
            .name("pi-ext".into())
            .stack_size(config.thread_stack_bytes)
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(rt) => rt,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e.to_string()));
                        return;
                    }
                };
                let local = tokio::task::LocalSet::new();
                local.block_on(&rt, run_host(rx, callbacks, cfg, ready_tx));
            })?;
        ready_rx.await.map_err(|_| anyhow::anyhow!("extension host thread died during startup"))?.map_err(|e| anyhow::anyhow!(e))?;
        Ok(ExtensionHost { tx })
    }

    async fn request<T>(&self, build: impl FnOnce(Reply<T>) -> Request) -> Result<T, String> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(build(tx)).map_err(send_err)?;
        rx.await.map_err(send_err)
    }

    pub async fn load(&self, path: impl Into<String>) -> Result<LoadedExtension, String> {
        let path = path.into();
        self.request(|reply| Request::Load { path, reply }).await?
    }
    /// Mark loading complete: later `registerTool` calls trigger `tools_changed`.
    pub fn set_loaded(&self) {
        let _ = self.tx.send(Request::SetLoaded);
    }
    pub fn set_flag_values(&self, values: Value) {
        let _ = self.tx.send(Request::SetFlagValues(values));
    }
    pub async fn dispatch(&self, event: &str, payload: Value, cancel: Option<CancellationToken>) -> Result<DispatchOutcome, String> {
        let event = event.to_string();
        self.request(|reply| Request::Dispatch { event, payload, cancel, reply }).await?
    }
    pub async fn has_handlers(&self, event: &str) -> bool {
        let event = event.to_string();
        self.request(|reply| Request::HasHandlers { event, reply }).await.unwrap_or(false)
    }
    pub async fn execute_tool(&self, name: &str, tool_call_id: &str, args: Value, cancel: CancellationToken, on_update: UpdateFn) -> Result<ToolResult, String> {
        let (name, tool_call_id) = (name.to_string(), tool_call_id.to_string());
        self.request(|reply| Request::ExecuteTool { name, tool_call_id, args, cancel, on_update, reply }).await?
    }
    pub async fn prepare_arguments(&self, name: &str, args: Value) -> Value {
        let name = name.to_string();
        let fallback = args.clone();
        self.request(|reply| Request::PrepareArguments { name, args, reply }).await.unwrap_or(fallback)
    }
    pub async fn run_command(&self, name: &str, args: &str) -> Result<(), String> {
        let (name, args) = (name.to_string(), args.to_string());
        self.request(|reply| Request::RunCommand { name, args, reply }).await?
    }
    pub async fn command_completions(&self, name: &str, prefix: &str) -> Option<Value> {
        let (name, prefix) = (name.to_string(), prefix.to_string());
        self.request(|reply| Request::CommandCompletions { name, prefix, reply }).await.ok().flatten()
    }
    pub async fn run_shortcut(&self, shortcut: &str) -> bool {
        let shortcut = shortcut.to_string();
        self.request(|reply| Request::RunShortcut { shortcut, reply }).await.unwrap_or(false)
    }
    pub async fn list_tools(&self) -> Vec<ToolInfo> {
        self.request(|reply| Request::ListTools { reply }).await.unwrap_or_default()
    }
    pub async fn list_commands(&self) -> Vec<CommandInfo> {
        self.request(|reply| Request::ListCommands { reply }).await.unwrap_or_default()
    }
    pub async fn list_extensions(&self) -> Vec<LoadedExtension> {
        self.request(|reply| Request::ListExtensions { reply }).await.unwrap_or_default()
    }
    pub async fn render_tool_call(&self, name: &str, args: Value, width: usize) -> Option<Vec<String>> {
        let name = name.to_string();
        self.request(|reply| Request::RenderToolCall { name, args, width, reply }).await.ok().flatten()
    }
    pub async fn render_tool_result(&self, name: &str, result: Value, expanded: bool, width: usize) -> Option<Vec<String>> {
        let name = name.to_string();
        self.request(|reply| Request::RenderToolResult { name, result, expanded, width, reply }).await.ok().flatten()
    }
    pub async fn render_message(&self, custom_type: &str, message: Value, width: usize) -> Option<Vec<String>> {
        let custom_type = custom_type.to_string();
        self.request(|reply| Request::RenderMessage { custom_type, message, width, reply }).await.ok().flatten()
    }
    /// Evaluate a JS expression (tests and diagnostics). Result is JSON.
    pub async fn eval(&self, source: &str) -> Result<Value, String> {
        let source = source.to_string();
        self.request(|reply| Request::Eval { source, reply }).await?
    }
    /// Deactivate all loaded extensions (session replacement / reload).
    pub async fn invalidate(&self) {
        let _ = self.request(|reply| Request::Invalidate { reply }).await;
    }
    pub fn shutdown(&self) {
        let _ = self.tx.send(Request::Shutdown);
    }
}

// ---------------------------------------------------------------------------
// Host thread
// ---------------------------------------------------------------------------

struct HostState {
    callbacks: Arc<dyn HostCallbacks>,
    config: HostConfig,
    kills: Mutex<HashMap<String, CancellationToken>>,
}

async fn run_host(mut rx: mpsc::UnboundedReceiver<Request>, callbacks: Arc<dyn HostCallbacks>, config: HostConfig, ready: oneshot::Sender<Result<(), String>>) {
    let state = Arc::new(HostState { callbacks, config, kills: Default::default() });
    let rt = match AsyncRuntime::new() {
        Ok(rt) => rt,
        Err(e) => {
            let _ = ready.send(Err(e.to_string()));
            return;
        }
    };
    rt.set_max_stack_size(state.config.js_max_stack_bytes).await;
    if let Some(limit) = state.config.js_memory_limit_bytes {
        rt.set_memory_limit(limit).await;
    }
    rt.set_loader(PiResolver, PiLoader).await;
    let ctx = match AsyncContext::full(&rt).await {
        Ok(c) => c,
        Err(e) => {
            let _ = ready.send(Err(e.to_string()));
            return;
        }
    };
    let state2 = state.clone();
    ctx.async_with(async |ctx| {
        if let Err(e) = install_globals(&ctx, state2.clone()) {
            let _ = ready.send(Err(format!("failed to install host globals: {}", describe_error(&ctx, e))));
            return;
        }
        if let Err(e) = ctx.eval::<(), _>(include_str!("js/runtime.js")) {
            let _ = ready.send(Err(format!("failed to evaluate runtime: {}", describe_error(&ctx, e))));
            return;
        }
        let _ = ready.send(Ok(()));
        loop {
            let Some(req) = rx.recv().await else { break };
            if matches!(req, Request::Shutdown) {
                break;
            }
            let ctx2 = ctx.clone();
            ctx.spawn(async move { handle_request(ctx2, req).await });
        }
    })
    .await;
    rt.idle().await;
}

fn describe_error(ctx: &Ctx<'_>, err: rquickjs::Error) -> String {
    match err {
        rquickjs::Error::Exception => {
            let v = ctx.catch();
            describe_value(ctx, v)
        }
        other => other.to_string(),
    }
}

fn describe_value<'js>(ctx: &Ctx<'js>, v: JsValue<'js>) -> String {
    if let Some(obj) = v.clone().into_object() {
        if let Some(ex) = Exception::from_object(obj.clone()) {
            let msg = ex.message().unwrap_or_default();
            let stack = ex.stack().unwrap_or_default();
            return if stack.is_empty() { msg } else { format!("{msg}\n{stack}") };
        }
        if let Ok(Some(s)) = ctx.json_stringify(obj) {
            if let Ok(s) = s.to_string() {
                return s;
            }
        }
    }
    if let Some(s) = v.as_string() {
        return s.to_string().unwrap_or_default();
    }
    format!("{v:?}")
}

async fn await_js<'js>(ctx: &Ctx<'js>, v: JsValue<'js>) -> Result<JsValue<'js>, String> {
    if let Some(p) = v.as_promise() {
        p.clone().into_future::<JsValue>().await.map_err(|e| describe_error(ctx, e))
    } else {
        Ok(v)
    }
}

fn js_json<'js>(ctx: &Ctx<'js>, v: JsValue<'js>) -> Result<Value, String> {
    if v.is_undefined() || v.is_null() {
        return Ok(Value::Null);
    }
    if let Some(s) = v.as_string() {
        let s = s.to_string().map_err(|e| e.to_string())?;
        return serde_json::from_str(&s).map_err(|e| format!("invalid JSON from runtime: {e}: {s}"));
    }
    let s = ctx.json_stringify(v).map_err(|e| describe_error(ctx, e))?;
    match s {
        Some(s) => serde_json::from_str(&s.to_string().map_err(|e| e.to_string())?).map_err(|e| e.to_string()),
        None => Ok(Value::Null),
    }
}

fn global_fn<'js>(ctx: &Ctx<'js>, name: &str) -> Result<Function<'js>, String> {
    ctx.globals().get::<_, Function>(name).map_err(|e| format!("runtime function {name} missing: {}", describe_error(ctx, e)))
}


async fn handle_request<'js>(ctx: Ctx<'js>, req: Request) {
    match req {
        Request::Load { path, reply } => {
            let r = async {
                let f = global_fn(&ctx, "__loadExtension")?;
                let v: JsValue = f.call((path.clone(),)).map_err(|e| describe_error(&ctx, e))?;
                let v = await_js(&ctx, v).await?;
                let json = js_json(&ctx, v)?;
                serde_json::from_value::<LoadedExtension>(json).map_err(|e| e.to_string())
            }
            .await;
            let _ = reply.send(r);
        }
        Request::SetLoaded => {
            if let Ok(f) = global_fn(&ctx, "__setLoaded") {
                let _: Result<(), _> = f.call(());
            }
        }
        Request::SetFlagValues(v) => {
            if let Ok(f) = global_fn(&ctx, "__setFlagValues") {
                let _: Result<(), _> = f.call((v.to_string(),));
            }
        }
        Request::Dispatch { event, payload, cancel, reply } => {
            let r = async {
                let f = global_fn(&ctx, "__dispatch")?;
                let signal_id = cancel.as_ref().map(|_| format!("sig_{}", uuid::Uuid::new_v4().simple()));
                let v: JsValue = f.call((event.clone(), payload.to_string(), signal_id.clone())).map_err(|e| describe_error(&ctx, e))?;
                let v = match (v.as_promise(), cancel) {
                    (Some(p), Some(cancel)) => {
                        let mut fut = p.clone().into_future::<JsValue>();
                        tokio::select! {
                            r = &mut fut => r.map_err(|e| describe_error(&ctx, e))?,
                            _ = cancel.cancelled() => {
                                if let (Ok(abort), Some(id)) = (global_fn(&ctx, "__abortSignal"), signal_id.clone()) {
                                    let _: Result<(), _> = abort.call((id,));
                                }
                                (&mut fut).await.map_err(|e| describe_error(&ctx, e))?
                            }
                        }
                    }
                    _ => await_js(&ctx, v).await?,
                };
                let json = js_json(&ctx, v)?;
                serde_json::from_value::<DispatchOutcome>(json).map_err(|e| e.to_string())
            }
            .await;
            let _ = reply.send(r);
        }
        Request::HasHandlers { event, reply } => {
            let r = global_fn(&ctx, "__hasHandlers").and_then(|f| f.call::<_, bool>((event,)).map_err(|e| describe_error(&ctx, e))).unwrap_or(false);
            let _ = reply.send(r);
        }
        Request::ExecuteTool { name, tool_call_id, args, cancel, on_update, reply } => {
            let r = async {
                let f = global_fn(&ctx, "__executeTool")?;
                let signal_id = format!("sig_{}", uuid::Uuid::new_v4().simple());
                let update = Func::from(move |json: String| {
                    if let Ok(partial) = serde_json::from_str::<ToolResult>(&json) {
                        on_update(partial);
                    }
                });
                let v: JsValue = f.call((name.clone(), tool_call_id, args.to_string(), signal_id.clone(), update)).map_err(|e| describe_error(&ctx, e))?;
                let v = match v.as_promise() {
                    Some(p) => {
                        let mut fut = p.clone().into_future::<JsValue>();
                        tokio::select! {
                            r = &mut fut => r.map_err(|e| describe_error(&ctx, e))?,
                            _ = cancel.cancelled() => {
                                if let Ok(abort) = global_fn(&ctx, "__abortSignal") {
                                    let _: Result<(), _> = abort.call((signal_id.clone(),));
                                }
                                (&mut fut).await.map_err(|e| describe_error(&ctx, e))?
                            }
                        }
                    }
                    None => v,
                };
                let json = js_json(&ctx, v)?;
                serde_json::from_value::<ToolResult>(json).map_err(|e| e.to_string())
            }
            .await;
            let _ = reply.send(r);
        }
        Request::PrepareArguments { name, args, reply } => {
            let r = global_fn(&ctx, "__prepareToolArguments")
                .and_then(|f| f.call::<_, String>((name, args.to_string())).map_err(|e| describe_error(&ctx, e)))
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .unwrap_or(args);
            let _ = reply.send(r);
        }
        Request::RunCommand { name, args, reply } => {
            let r = async {
                let f = global_fn(&ctx, "__runCommand")?;
                let v: JsValue = f.call((name, args)).map_err(|e| describe_error(&ctx, e))?;
                await_js(&ctx, v).await.map(|_| ())
            }
            .await;
            let _ = reply.send(r);
        }
        Request::CommandCompletions { name, prefix, reply } => {
            let r = global_fn(&ctx, "__commandCompletions")
                .and_then(|f| f.call::<_, String>((name, prefix)).map_err(|e| describe_error(&ctx, e)))
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .filter(|v| !v.is_null());
            let _ = reply.send(r);
        }
        Request::RunShortcut { shortcut, reply } => {
            let r = async {
                let f = global_fn(&ctx, "__runShortcut")?;
                let v: JsValue = f.call((shortcut,)).map_err(|e| describe_error(&ctx, e))?;
                let v = await_js(&ctx, v).await?;
                Ok::<bool, String>(v.as_string().and_then(|s| s.to_string().ok()).map(|s| s == "true").unwrap_or(false))
            }
            .await
            .unwrap_or(false);
            let _ = reply.send(r);
        }
        Request::ListTools { reply } => {
            let r = call_string_fn(&ctx, "__listTools", ()).and_then(|s| serde_json::from_str::<Vec<ToolInfo>>(&s).map_err(|e| e.to_string())).unwrap_or_default();
            let _ = reply.send(r);
        }
        Request::ListCommands { reply } => {
            let r = call_string_fn(&ctx, "__listCommands", ()).and_then(|s| serde_json::from_str::<Vec<CommandInfo>>(&s).map_err(|e| e.to_string())).unwrap_or_default();
            let _ = reply.send(r);
        }
        Request::ListExtensions { reply } => {
            let r = call_string_fn(&ctx, "__listExtensions", ()).and_then(|s| serde_json::from_str::<Vec<LoadedExtension>>(&s).map_err(|e| e.to_string())).unwrap_or_default();
            let _ = reply.send(r);
        }
        Request::RenderToolCall { name, args, width, reply } => {
            let r = call_string_fn(&ctx, "__renderToolCall", (name, args.to_string(), width as i32)).ok().and_then(|s| serde_json::from_str::<Option<Vec<String>>>(&s).ok()).flatten();
            let _ = reply.send(r);
        }
        Request::RenderToolResult { name, result, expanded, width, reply } => {
            let r = call_string_fn(&ctx, "__renderToolResult", (name, result.to_string(), expanded, width as i32)).ok().and_then(|s| serde_json::from_str::<Option<Vec<String>>>(&s).ok()).flatten();
            let _ = reply.send(r);
        }
        Request::RenderMessage { custom_type, message, width, reply } => {
            let r = call_string_fn(&ctx, "__renderMessage", (custom_type, message.to_string(), width as i32)).ok().and_then(|s| serde_json::from_str::<Option<Vec<String>>>(&s).ok()).flatten();
            let _ = reply.send(r);
        }
        Request::Eval { source, reply } => {
            let r = async {
                let v: JsValue = ctx.eval(source).map_err(|e| describe_error(&ctx, e))?;
                let v = await_js(&ctx, v).await?;
                if v.is_undefined() {
                    return Ok(Value::Null);
                }
                let s = ctx.json_stringify(v).map_err(|e| describe_error(&ctx, e))?;
                match s {
                    Some(s) => serde_json::from_str(&s.to_string().map_err(|e| e.to_string())?).map_err(|e| e.to_string()),
                    None => Ok(Value::Null),
                }
            }
            .await;
            let _ = reply.send(r);
        }
        Request::Invalidate { reply } => {
            if let Ok(f) = global_fn(&ctx, "__invalidateAll") {
                let _: Result<(), _> = f.call(());
            }
            let _ = reply.send(());
        }
        Request::Shutdown => {}
    }
}

fn call_string_fn<'js, A: rquickjs::function::IntoArgs<'js>>(ctx: &Ctx<'js>, name: &str, args: A) -> Result<String, String> {
    let f = global_fn(ctx, name)?;
    f.call::<_, String>(args).map_err(|e| describe_error(ctx, e))
}

// ---------------------------------------------------------------------------
// Native functions available to the runtime
// ---------------------------------------------------------------------------

fn install_globals<'js>(ctx: &Ctx<'js>, state: Arc<HostState>) -> rquickjs::Result<()> {
    let globals = ctx.globals();
    let s1 = state.clone();
    globals.set(
        "__hostSync",
        Func::from(move |ctx: Ctx<'js>, name: String, args: String| -> rquickjs::Result<JsValue<'js>> {
            let args: Vec<Value> = serde_json::from_str(&args).unwrap_or_default();
            match host_sync(&s1, &name, args) {
                Ok(Some(v)) => rquickjs::String::from_str(ctx.clone(), &v.to_string()).map(|s| s.into_value()),
                Ok(None) => Ok(JsValue::new_undefined(ctx)),
                Err(e) => Err(Exception::throw_message(&ctx, &e)),
            }
        }),
    )?;
    let s2 = state.clone();
    globals.set(
        "__hostAsync",
        Func::from(Async(move |ctx: Ctx<'js>, name: String, args: String| {
            let state = s2.clone();
            async move {
                let args: Vec<Value> = serde_json::from_str(&args).unwrap_or_default();
                match host_async(&state, &name, args).await {
                    Ok(Some(v)) => rquickjs::String::from_str(ctx.clone(), &v.to_string()).map(|s| s.into_value()),
                    Ok(None) => Ok(JsValue::new_undefined(ctx)),
                    Err(e) => Err(Exception::throw_message(&ctx, &e)),
                }
            }
        })),
    )?;
    Ok(())
}

fn arg_str(args: &[Value], i: usize) -> String {
    args.get(i).and_then(|v| v.as_str()).map(|s| s.to_string()).unwrap_or_default()
}
fn arg_opt_str(args: &[Value], i: usize) -> Option<String> {
    args.get(i).and_then(|v| v.as_str()).map(|s| s.to_string())
}
fn arg_val(args: &[Value], i: usize) -> Value {
    args.get(i).cloned().unwrap_or(Value::Null)
}

fn platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

fn host_sync(state: &HostState, name: &str, args: Vec<Value>) -> Result<Option<Value>, String> {
    let cb = &state.callbacks;
    let ok = |v: Value| Ok(Some(v));
    let none: Result<Option<Value>, String> = Ok(None);
    match name {
        "console" => {
            cb.console(arg_str(&args, 0), arg_str(&args, 1));
            none
        }
        "process.env" => ok(json!(std::env::vars().collect::<HashMap<String, String>>())),
        "process.env.get" => ok(std::env::var(arg_str(&args, 0)).map(Value::String).unwrap_or(Value::Null)),
        "process.env.set" => {
            match arg_opt_str(&args, 1) {
                Some(v) => std::env::set_var(arg_str(&args, 0), v),
                None => std::env::remove_var(arg_str(&args, 0)),
            }
            none
        }
        "process.cwd" => ok(json!(state.config.cwd.to_string_lossy())),
        "os.platform" => ok(json!(platform())),
        "os.arch" => ok(json!(match std::env::consts::ARCH {
            "x86_64" => "x64",
            "aarch64" => "arm64",
            o => o,
        })),
        "os.homedir" => ok(json!(dirs_home().to_string_lossy())),
        "os.tmpdir" => ok(json!(std::env::temp_dir().to_string_lossy().trim_end_matches('/'))),
        "os.hostname" => ok(json!(std::env::var("HOSTNAME").unwrap_or_else(|_| "localhost".into()))),
        "time.now" => ok(json!(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0))),
        "crypto.randomUUID" => ok(json!(uuid::Uuid::new_v4().to_string())),
        "crypto.uuidv7" => ok(json!(uuid_v7())),
        "fs.mkdtemp" => {
            let prefix = arg_str(&args, 0);
            let dir = format!("{prefix}{}", &uuid::Uuid::new_v4().simple().to_string()[..6]);
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            ok(json!(dir))
        }
        "tools.info" => ok(cb.builtin_tool_info(arg_str(&args, 0)).unwrap_or(Value::Null)),
        "crypto.randomBytes" => {
            let n = args.first().and_then(|v| v.as_u64()).unwrap_or(16) as usize;
            let bytes: Vec<u8> = (0..n).map(|_| rand_byte()).collect();
            ok(json!(bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()))
        }
        "crypto.hash" => Err("crypto.createHash is not supported in pirs extensions".into()),
        "base64.encode" => {
            use base64::Engine;
            ok(json!(base64::engine::general_purpose::STANDARD.encode(arg_str(&args, 0).as_bytes())))
        }
        "base64.decode" => {
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD.decode(arg_str(&args, 0)).map_err(|e| e.to_string())?;
            ok(json!(String::from_utf8_lossy(&bytes)))
        }
        "utf8.encode" => ok(json!(arg_str(&args, 0).as_bytes())),
        "utf8.decode" => {
            let bytes: Vec<u8> = args.first().and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|b| b.as_u64()).map(|b| b as u8).collect()).unwrap_or_default();
            ok(json!(String::from_utf8_lossy(&bytes)))
        }
        "validate" => match pi_agent::validate::validate(&arg_val(&args, 0), &arg_val(&args, 1)) {
            Ok(()) => none,
            Err(e) => ok(json!(e)),
        },
        // ---- filesystem
        "fs.exists" => ok(json!(std::path::Path::new(&arg_str(&args, 0)).exists())),
        "fs.readFile" => {
            let path = arg_str(&args, 0);
            let bytes = std::fs::read(&path).map_err(|e| format!("ENOENT: {e}, open '{path}'"))?;
            if arg_str(&args, 1) == "base64" {
                use base64::Engine;
                ok(json!(base64::engine::general_purpose::STANDARD.encode(bytes)))
            } else {
                ok(json!(String::from_utf8_lossy(&bytes)))
            }
        }
        "fs.writeFile" => {
            let path = arg_str(&args, 0);
            let data = arg_str(&args, 1);
            let append = args.get(2).and_then(|v| v.as_bool()).unwrap_or(false);
            if append {
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path).map_err(|e| format!("{e}, open '{path}'"))?;
                f.write_all(data.as_bytes()).map_err(|e| e.to_string())?;
            } else {
                std::fs::write(&path, data).map_err(|e| format!("{e}, open '{path}'"))?;
            }
            none
        }
        "fs.mkdir" => {
            let path = arg_str(&args, 0);
            let recursive = args.get(1).and_then(|v| v.as_bool()).unwrap_or(false);
            let r = if recursive { std::fs::create_dir_all(&path) } else { std::fs::create_dir(&path) };
            r.map_err(|e| format!("{e}, mkdir '{path}'"))?;
            none
        }
        "fs.readdir" => {
            let path = arg_str(&args, 0);
            let mut entries = Vec::new();
            for e in std::fs::read_dir(&path).map_err(|e| format!("ENOENT: {e}, scandir '{path}'"))? {
                let e = e.map_err(|e| e.to_string())?;
                let ft = e.file_type().ok();
                entries.push(json!({"name": e.file_name().to_string_lossy(), "isFile": ft.map(|t| t.is_file()).unwrap_or(false), "isDirectory": ft.map(|t| t.is_dir()).unwrap_or(false)}));
            }
            entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
            ok(Value::Array(entries))
        }
        "fs.stat" => {
            let path = arg_str(&args, 0);
            match std::fs::metadata(&path) {
                Ok(m) => {
                    let mtime = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0);
                    let link = std::fs::symlink_metadata(&path).map(|s| s.file_type().is_symlink()).unwrap_or(false);
                    ok(json!({"isFile": m.is_file(), "isDirectory": m.is_dir(), "isSymbolicLink": link, "size": m.len(), "mtimeMs": mtime}))
                }
                Err(_) => ok(Value::Null),
            }
        }
        "fs.remove" => {
            let path = arg_str(&args, 0);
            let recursive = args.get(1).and_then(|v| v.as_bool()).unwrap_or(false);
            let p = std::path::Path::new(&path);
            let r = if p.is_dir() {
                if recursive { std::fs::remove_dir_all(p) } else { std::fs::remove_dir(p) }
            } else {
                std::fs::remove_file(p)
            };
            r.map_err(|e| format!("{e}, rm '{path}'"))?;
            none
        }
        "fs.rename" => {
            std::fs::rename(arg_str(&args, 0), arg_str(&args, 1)).map_err(|e| e.to_string())?;
            none
        }
        "fs.copy" => {
            std::fs::copy(arg_str(&args, 0), arg_str(&args, 1)).map_err(|e| e.to_string())?;
            none
        }
        "fs.realpath" => ok(json!(std::fs::canonicalize(arg_str(&args, 0)).map_err(|e| e.to_string())?.to_string_lossy())),
        "bash.execSync" => {
            let command = arg_str(&args, 0);
            let cwd = arg_opt_str(&args, 1).unwrap_or_else(|| state.config.cwd.to_string_lossy().to_string());
            let out = std::process::Command::new("bash").arg("-c").arg(&command).current_dir(&cwd).stdin(std::process::Stdio::null()).output().map_err(|e| e.to_string())?;
            ok(json!({"stdout": String::from_utf8_lossy(&out.stdout), "stderr": String::from_utf8_lossy(&out.stderr), "code": out.status.code().unwrap_or(-1)}))
        }
        "exec.kill" | "fetch.abort" => {
            if let Some(t) = state.kills.lock().unwrap().remove(&arg_str(&args, 0)) {
                t.cancel();
            }
            none
        }
        // ---- app callbacks
        "context.info" => ok(serde_json::to_value(cb.context_info()).unwrap_or(Value::Null)),
        "reportError" => {
            if let Ok(err) = serde_json::from_value::<ExtensionError>(arg_val(&args, 0)) {
                cb.report_error(err);
            }
            none
        }
        "tools.changed" => {
            cb.tools_changed();
            none
        }
        "ui.notify" => {
            cb.ui_notify(arg_str(&args, 0), arg_str(&args, 1));
            none
        }
        "ui.setStatus" => {
            cb.ui_set_status(arg_str(&args, 0), arg_opt_str(&args, 1));
            none
        }
        "ui.setWorkingMessage" => {
            cb.ui_set_working_message(arg_opt_str(&args, 0));
            none
        }
        "ui.setWidget" => {
            let lines = args.get(1).and_then(|v| v.as_array()).map(|a| a.iter().map(|l| l.as_str().unwrap_or("").to_string()).collect::<Vec<_>>());
            cb.ui_set_widget(arg_str(&args, 0), lines, arg_opt_str(&args, 2));
            none
        }
        "ui.setTitle" => {
            cb.ui_set_title(arg_str(&args, 0));
            none
        }
        "ui.setEditorText" => {
            cb.ui_set_editor_text(arg_str(&args, 0));
            none
        }
        "ui.getEditorText" => ok(json!(cb.ui_get_editor_text())),
        "ui.dismiss" => {
            cb.ui_dismiss(arg_str(&args, 0));
            none
        }
        "session.entries" => ok(cb.session_entries()),
        "session.branch" => ok(cb.session_branch()),
        "session.leafId" => ok(json!(cb.session_leaf_id())),
        "session.label" => ok(json!(cb.session_label(arg_str(&args, 0)))),
        "getSessionName" => ok(json!(cb.get_session_name())),
        "setSessionName" => {
            cb.set_session_name(arg_str(&args, 0));
            none
        }
        "setLabel" => {
            cb.set_label(arg_str(&args, 0), arg_opt_str(&args, 1));
            none
        }
        "sendMessage" => {
            cb.send_message(arg_val(&args, 0), arg_val(&args, 1));
            none
        }
        "sendUserMessage" => {
            cb.send_user_message(arg_val(&args, 0), arg_val(&args, 1));
            none
        }
        "appendEntry" => {
            cb.append_entry(arg_str(&args, 0), arg_val(&args, 1));
            none
        }
        "getActiveTools" => ok(json!(cb.get_active_tools())),
        "setActiveTools" => {
            let names = args.first().and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|n| n.as_str().map(|s| s.to_string())).collect()).unwrap_or_default();
            cb.set_active_tools(names);
            none
        }
        "getAllTools" => ok(cb.get_all_tools()),
        "getCommands" => ok(cb.get_commands()),
        "getThinkingLevel" => ok(json!(cb.get_thinking_level())),
        "setThinkingLevel" => {
            cb.set_thinking_level(arg_str(&args, 0));
            none
        }
        "registerProvider" => {
            cb.register_provider(arg_str(&args, 0), arg_val(&args, 1));
            none
        }
        "unregisterProvider" => {
            cb.unregister_provider(arg_str(&args, 0));
            none
        }
        "models.available" => ok(cb.models_available()),
        "models.all" => ok(cb.models_all()),
        "models.find" => ok(cb.models_find(arg_str(&args, 0)).unwrap_or(Value::Null)),
        "models.get" => ok(cb.models_get(arg_str(&args, 0), arg_str(&args, 1)).unwrap_or(Value::Null)),
        "models.providerAuth" => ok(cb.models_provider_auth(arg_str(&args, 0))),
        "abort" => {
            cb.abort();
            none
        }
        "shutdown" => {
            cb.shutdown();
            none
        }
        "hasPendingMessages" => ok(json!(cb.has_pending_messages())),
        "contextUsage" => ok(cb.context_usage().unwrap_or(Value::Null)),
        "compact" => {
            cb.compact(arg_val(&args, 0));
            none
        }
        "systemPrompt" => ok(json!(cb.system_prompt())),
        "systemPromptOptions" => ok(cb.system_prompt_options()),
        "systemPrompt.build" => ok(json!(cb.build_system_prompt(arg_val(&args, 0)))),
        "settings" => ok(cb.settings()),
        "paths.agentDir" => ok(json!(cb.agent_dir())),
        "paths.sessionsDir" => ok(json!(cb.sessions_dir())),
        "truncate.head" => ok(cb.truncate(arg_str(&args, 0), false, arg_val(&args, 1))),
        "truncate.tail" => ok(cb.truncate(arg_str(&args, 0), true, arg_val(&args, 1))),
        other => Err(format!("unknown host function: {other}")),
    }
}

/// UUID v7 (time-ordered), matching pi's session/entry id generator.
fn uuid_v7() -> String {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let rnd = uuid::Uuid::new_v4();
    let r = rnd.as_bytes();
    let mut b = [0u8; 16];
    b[..6].copy_from_slice(&ms.to_be_bytes()[2..8]);
    b[6..].copy_from_slice(&r[6..]);
    b[6] = (b[6] & 0x0f) | 0x70;
    b[8] = (b[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(b).to_string()
}

fn rand_byte() -> u8 {
    (uuid::Uuid::new_v4().as_bytes()[0]) ^ (uuid::Uuid::new_v4().as_bytes()[5])
}

async fn host_async(state: &HostState, name: &str, args: Vec<Value>) -> Result<Option<Value>, String> {
    let cb = &state.callbacks;
    let ok = |v: Value| Ok(Some(v));
    match name {
        "sleep" => {
            let ms = args.first().and_then(|v| v.as_f64()).unwrap_or(0.0).max(0.0);
            tokio::time::sleep(std::time::Duration::from_millis(ms as u64)).await;
            Ok(None)
        }
        "exec" => {
            let id = arg_str(&args, 0);
            let command = arg_str(&args, 1);
            let cmd_args: Vec<String> = args.get(2).and_then(|v| v.as_array()).map(|a| a.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect()).unwrap_or_default();
            let opts = arg_val(&args, 3);
            let cwd = opts["cwd"].as_str().map(|s| s.to_string()).unwrap_or_else(|| state.config.cwd.to_string_lossy().to_string());
            let timeout = opts["timeout"].as_f64().filter(|t| *t > 0.0).map(|t| std::time::Duration::from_millis(t as u64));
            let token = CancellationToken::new();
            state.kills.lock().unwrap().insert(id.clone(), token.clone());
            let r = run_exec(&command, &cmd_args, &cwd, timeout, token).await;
            state.kills.lock().unwrap().remove(&id);
            ok(serde_json::to_value(r?).unwrap_or(Value::Null))
        }
        "bash.exec" => {
            let command = arg_str(&args, 0);
            let cwd = arg_opt_str(&args, 1).unwrap_or_else(|| state.config.cwd.to_string_lossy().to_string());
            let opts = arg_val(&args, 2);
            let timeout = opts["timeout"].as_f64().filter(|t| *t > 0.0).map(|t| std::time::Duration::from_millis(t as u64));
            let r = run_exec("bash", &["-c".to_string(), command], &cwd, timeout, CancellationToken::new()).await?;
            let mut output = r.stdout;
            if !r.stderr.is_empty() {
                if !output.is_empty() && !output.ends_with('\n') {
                    output.push('\n');
                }
                output.push_str(&r.stderr);
            }
            ok(json!({"output": output, "exitCode": r.code, "cancelled": r.killed, "truncated": false}))
        }
        "fetch" => {
            let id = arg_str(&args, 0);
            let url = arg_str(&args, 1);
            let method = arg_str(&args, 2);
            let headers = arg_val(&args, 3);
            let body = arg_opt_str(&args, 4);
            let token = CancellationToken::new();
            state.kills.lock().unwrap().insert(id.clone(), token.clone());
            let r = run_fetch(&url, &method, &headers, body, token).await;
            state.kills.lock().unwrap().remove(&id);
            ok(r)
        }
        "ui.select" => {
            let options = args.get(1).and_then(|v| v.as_array()).map(|a| a.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect()).unwrap_or_default();
            ok(json!(cb.ui_select(arg_str(&args, 0), options, arg_val(&args, 2)).await))
        }
        "ui.confirm" => ok(json!(cb.ui_confirm(arg_str(&args, 0), arg_str(&args, 1), arg_val(&args, 2)).await)),
        "ui.input" => ok(json!(cb.ui_input(arg_str(&args, 0), arg_str(&args, 1), arg_val(&args, 2)).await)),
        "ui.editor" => ok(json!(cb.ui_editor(arg_str(&args, 0), arg_str(&args, 1)).await)),
        "setModel" => ok(json!(cb.set_model(arg_val(&args, 0)).await)),
        "models.complete" => ok(cb.models_complete(arg_val(&args, 0), arg_val(&args, 1), arg_val(&args, 2)).await),
        "waitForIdle" => {
            cb.wait_for_idle().await;
            Ok(None)
        }
        "newSession" => ok(cb.new_session(arg_val(&args, 0)).await),
        "fork" => ok(cb.fork(arg_str(&args, 0), arg_val(&args, 1)).await),
        "navigateTree" => ok(cb.navigate_tree(arg_str(&args, 0), arg_val(&args, 1)).await),
        "switchSession" => ok(cb.switch_session(arg_str(&args, 0)).await),
        "reload" => {
            cb.reload().await;
            Ok(None)
        }
        "sessions.list" => ok(cb.sessions_list(arg_opt_str(&args, 0)).await),
        "tools.execute" => {
            let id = arg_str(&args, 0);
            let token = CancellationToken::new();
            state.kills.lock().unwrap().insert(id.clone(), token.clone());
            let r = cb.execute_builtin_tool(arg_str(&args, 1), arg_str(&args, 2), arg_val(&args, 3), arg_opt_str(&args, 4), token).await;
            state.kills.lock().unwrap().remove(&id);
            ok(match r {
                Ok(result) => serde_json::to_value(result).unwrap_or(Value::Null),
                Err(e) => json!({"error": e}),
            })
        }
        other => Err(format!("unknown async host function: {other}")),
    }
}

async fn run_exec(command: &str, args: &[String], cwd: &str, timeout: Option<std::time::Duration>, cancel: CancellationToken) -> Result<ExecResult, String> {
    let child = tokio::process::Command::new(command)
        .args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("failed to spawn {command}: {e}"))?;
    let wait = child.wait_with_output();
    let timeout_fut = async {
        match timeout {
            Some(t) => tokio::time::sleep(t).await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        out = wait => {
            let out = out.map_err(|e| e.to_string())?;
            Ok(ExecResult { stdout: String::from_utf8_lossy(&out.stdout).to_string(), stderr: String::from_utf8_lossy(&out.stderr).to_string(), code: out.status.code().unwrap_or(-1), killed: false })
        }
        _ = timeout_fut => Ok(ExecResult { stdout: String::new(), stderr: "timed out".into(), code: -1, killed: true }),
        _ = cancel.cancelled() => Ok(ExecResult { stdout: String::new(), stderr: "killed".into(), code: -1, killed: true }),
    }
}

async fn run_fetch(url: &str, method: &str, headers: &Value, body: Option<String>, cancel: CancellationToken) -> Value {
    let client = reqwest::Client::new();
    let m = reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET);
    let mut req = client.request(m, url);
    if let Some(h) = headers.as_object() {
        for (k, v) in h {
            if let Some(v) = v.as_str() {
                req = req.header(k, v);
            }
        }
    }
    if let Some(b) = body {
        req = req.body(b);
    }
    let resp = tokio::select! {
        r = req.send() => r,
        _ = cancel.cancelled() => return json!({"error": "The operation was aborted"}),
    };
    match resp {
        Ok(r) => {
            let status = r.status().as_u16();
            let status_text = r.status().canonical_reason().unwrap_or("").to_string();
            let headers: HashMap<String, String> = r.headers().iter().map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string())).collect();
            let text = tokio::select! {
                t = r.text() => t.unwrap_or_default(),
                _ = cancel.cancelled() => return json!({"error": "The operation was aborted"}),
            };
            json!({"status": status, "statusText": status_text, "headers": headers, "body": text})
        }
        Err(e) => json!({"error": e.to_string()}),
    }
}

// ---------------------------------------------------------------------------
// AgentTool wrapper for extension tools
// ---------------------------------------------------------------------------

/// Exposes an extension-registered tool to the agent loop.
pub struct ExtensionTool {
    pub host: ExtensionHost,
    pub info: ToolInfo,
}

#[async_trait]
impl pi_agent::AgentTool for ExtensionTool {
    fn name(&self) -> String {
        self.info.name.clone()
    }
    fn label(&self) -> String {
        if self.info.label.is_empty() { self.info.name.clone() } else { self.info.label.clone() }
    }
    fn description(&self) -> String {
        self.info.description.clone()
    }
    fn parameters(&self) -> Value {
        self.info.parameters.clone()
    }
    fn execution_mode(&self) -> pi_agent::ToolExecutionMode {
        match self.info.execution_mode.as_deref() {
            Some("sequential") => pi_agent::ToolExecutionMode::Sequential,
            _ => pi_agent::ToolExecutionMode::Parallel,
        }
    }
    fn prompt_snippet(&self) -> Option<String> {
        self.info.prompt_snippet.clone()
    }
    fn prompt_guidelines(&self) -> Vec<String> {
        self.info.prompt_guidelines.clone().unwrap_or_default()
    }
    async fn execute(&self, tool_call_id: &str, args: Value, cancel: CancellationToken, on_update: UpdateFn) -> anyhow::Result<ToolResult> {
        let args = self.host.prepare_arguments(&self.info.name, args).await;
        self.host.execute_tool(&self.info.name, tool_call_id, args, cancel, on_update).await.map_err(|e| anyhow::anyhow!(e))
    }
}
