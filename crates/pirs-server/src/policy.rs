//! The DSL at runtime: what a loop does with the policy it loaded.
//!
//! `dsl` says what a policy *is*; this module runs it. Every slot has one
//! rule here, and they share three properties:
//!
//! - **Order.** For each slot the policy's own entries run first, in file
//!   order, and the connected registrations (`dispatch`) after them. A
//!   `[[input]]` in a file therefore sees the text before a registered
//!   handler does.
//! - **Timing.** A called process for a policy entry gets the plan's fixed
//!   5 s ([`SLOT_TIMEOUT`]); a `[[tool]]` gets its own `timeout` (60 s by
//!   default). `[[on]]` without an `emit` is fire and forget and has no
//!   timeout at all (D-16).
//! - **Binding.** Every `run` goes through [`RunSpec::resolve`]: an
//!   executable is spawned directly and its stdout is one JSON line in the
//!   slot's reply shape, parsed with [`SlotRequest::parse_reply`]; a shell
//!   string's stdout is its reply as text (D-23, D-24). [`call_run`] is the
//!   one place that distinction is made.
//! - **Failure.** A timeout, a non-zero exit, a process that cannot be
//!   started, or an executable whose stdout is not the slot's reply shape is
//!   "no opinion" for that entry — the loop carries on with what it had —
//!   plus one `ui.notify` warning naming the origin. The exceptions are
//!   [`InputEntry::handled`]: consumption is declared in the file, not
//!   decided by the process, so a `handled` entry whose `run` fails still
//!   consumes the input and the failure is recorded as its output; and a
//!   `[[tool]]`, whose failure is an error result the model reads, with
//!   stderr as the message.
//!
//! Nothing here can stop the model from doing anything (D-19): a policy
//! rewrites text, adds context, emits UI and provides tools.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};
use std::time::Duration;

use async_trait::async_trait;
use pi_agent::{AgentTool, ToolRef, ToolResult, UpdateFn};
use pi_ai::now_ms;
use pirs_protocol::{
    DslCheckResult, DslConflict, InputPayload, InputReply, NotifyLevel, OnEvent, OnPayload, PromptPayload, PromptReply,
    ServerPath, SlotReply, SlotRequest, ToolCallPayload, ToolContent, ToolReply, ToolResultPayload,
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::agent_loop::LoopHandle;
use crate::dsl::{self, Emit, InputEntry, OnEntry, OnSource, Origin, Policy, PromptSource, ResolvedTool, ToolSource};
use crate::process::{self, CallEnv, CallError, RunSpec};

/// How long a called process for `input`, `prompt`, `tool_result`, `status`
/// or `widget` may take before it counts as "no opinion".
pub(crate) const SLOT_TIMEOUT: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------
// Warnings
// ---------------------------------------------------------------------------

/// The warnings a load produced: one line per parse error and per conflict.
///
/// They are not fatal — the loop keeps every entry that did load — so they
/// travel as `ui.notify { warning }` rather than as a failure.
pub(crate) fn load_warnings(policy: &Policy) -> Vec<String> {
    let mut out = Vec::new();
    for (path, message) in &policy.errors {
        out.push(format!("{}: {message}", path.display()));
    }
    for conflict in &policy.conflicts {
        let files: Vec<&str> = conflict.files.iter().map(ServerPath::as_str).collect();
        out.push(format!("{} ({})", conflict.message, files.join(", ")));
    }
    out
}

// ---------------------------------------------------------------------------
// Calling a `run`
// ---------------------------------------------------------------------------

/// What a called `run` produced for a slot that expects a reply.
pub(crate) enum Called {
    /// A shell string's stdout: the reply as text.
    Text(String),
    /// An executable's one JSON line, parsed as the slot's reply.
    Reply(SlotReply),
}

/// Why a `run` produced no reply.
pub(crate) enum RunFailure {
    /// The process itself: timeout, non-zero exit, could not start.
    Call(CallError),
    /// It exited 0 but its stdout was not the slot's reply shape.
    BadReply(String),
}

impl std::fmt::Display for RunFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunFailure::Call(error) => write!(f, "{error}"),
            RunFailure::BadReply(message) => write!(f, "{message}"),
        }
    }
}

/// Call a `run` for `request` and read its reply the way its binding says
/// (D-23, D-24): an executable's stdout is one JSON line in the slot's reply
/// shape, a shell string's stdout is text. `vars` are the caller's
/// interpolation variables and matter only for a shell string.
pub(crate) async fn call_run(
    spec: &RunSpec,
    request: &SlotRequest,
    vars: &BTreeMap<String, String>,
    env: &CallEnv,
    timeout: Duration,
) -> Result<Called, RunFailure> {
    let output = process::call(spec, &request.params(), vars, env, timeout).await.map_err(RunFailure::Call)?;
    if !spec.is_exec() {
        return Ok(Called::Text(output.stdout));
    }
    let slot = request.slot();
    let value: Value = serde_json::from_str(output.stdout.trim()).map_err(|error| {
        RunFailure::BadReply(format!("reply is not one JSON line in the `{slot}` reply shape: {error}"))
    })?;
    request
        .parse_reply(value)
        .map(Called::Reply)
        .map_err(|error| RunFailure::BadReply(format!("reply is not in the `{slot}` reply shape: {error}")))
}

// ---------------------------------------------------------------------------
// `[[prompt]]`
// ---------------------------------------------------------------------------

/// What a `[[prompt]]` entry could not do; the caller decides whether that is
/// a warning on a loop or a conflict in `dsl.check`.
pub(crate) struct PromptFailure {
    /// The entry.
    pub(crate) origin: Origin,
    /// Why it contributed nothing.
    pub(crate) message: String,
}

/// The `policy` section of the system prompt and what could not go in it.
pub(crate) struct PromptSection {
    /// The section: every text, files and shell-string `[[prompt]]` entry in
    /// file order, each under a `# <origin>` line.
    pub(crate) text: String,
    /// The entries that contributed nothing, and why.
    pub(crate) failures: Vec<PromptFailure>,
    /// The executable `[[prompt]] run` entries, in file order. An executable
    /// is a `prompt` handler — it receives `{ system_prompt }` and replies
    /// `{ append }` or `{ replace }` — so it runs once the base prompt
    /// exists, through [`prompt_executables`]. Each carries its `header`,
    /// which prefixes whatever it appends, as it does for every other source.
    pub(crate) executables: Vec<PromptExecutable>,
}

/// One executable `[[prompt]]` entry, held until the base prompt exists.
pub(crate) struct PromptExecutable {
    pub(crate) origin: Origin,
    pub(crate) header: Option<String>,
    pub(crate) spec: RunSpec,
}

/// Assemble the `policy` section of the system prompt: every `[[prompt]]`
/// entry in file order, each under a `# <origin>` line so a reader of
/// `pirs check` or of the session log can see which file asked for it (D-21).
///
/// `env` is the environment a `run` entry is called with; its `cwd` is also
/// what `files` globs, relative paths and executables resolve against.
pub(crate) async fn prompt_section(policy: &Policy, env: &CallEnv) -> PromptSection {
    let mut blocks: Vec<String> = Vec::new();
    let mut failures = Vec::new();
    let mut executables = Vec::new();
    for entry in &policy.prompt {
        let body = match &entry.source {
            PromptSource::Text(text) => Some(text.clone()),
            PromptSource::Files(pattern) => {
                let files = glob_files_off_thread(env.cwd.clone(), pattern.clone()).await;
                if files.is_empty() {
                    failures.push(PromptFailure {
                        origin: entry.origin.clone(),
                        message: format!("files {pattern:?} matched nothing"),
                    });
                    None
                } else {
                    let mut parts = Vec::new();
                    for path in files {
                        match std::fs::read_to_string(&path) {
                            Ok(text) => {
                                parts.push(format!("<file path=\"{}\">\n{}\n</file>", path.display(), text.trim_end()))
                            }
                            Err(error) => failures.push(PromptFailure {
                                origin: entry.origin.clone(),
                                message: format!("cannot read {}: {error}", path.display()),
                            }),
                        }
                    }
                    (!parts.is_empty()).then(|| parts.join("\n"))
                }
            }
            PromptSource::Run(run) => {
                let spec = RunSpec::resolve(run, &env.cwd);
                if spec.is_exec() {
                    executables.push(PromptExecutable {
                        origin: entry.origin.clone(),
                        header: entry.header.clone(),
                        spec,
                    });
                    continue;
                }
                // A shell string sees the section so far; its stdout is text.
                let request = SlotRequest::Prompt(PromptPayload { system_prompt: blocks.join("\n\n") });
                match call_run(&spec, &request, &BTreeMap::new(), env, SLOT_TIMEOUT).await {
                    Ok(Called::Text(text)) => Some(text),
                    Ok(Called::Reply(_)) => unreachable!("a shell string replies as text"),
                    Err(error) => {
                        failures.push(PromptFailure { origin: entry.origin.clone(), message: error.to_string() });
                        None
                    }
                }
            }
        };
        let Some(body) = body else { continue };
        let mut block = format!("# {}\n", entry.origin);
        if let Some(header) = &entry.header {
            block.push_str(header);
            block.push('\n');
        }
        block.push_str(body.trim_end());
        blocks.push(block);
    }
    PromptSection { text: blocks.join("\n\n"), failures, executables }
}

/// Run the executable `[[prompt]]` entries over the assembled base prompt,
/// in file order, each seeing the previous one's outcome: `{ append }` adds
/// a block under the entry's `# <origin>` line (D-21), with the entry's
/// `header` as the block's first line; `{ replace }` replaces the whole
/// prompt. A failure is "no opinion" and lands in `failures`.
pub(crate) async fn prompt_executables(
    executables: &[PromptExecutable],
    mut prompt: String,
    env: &CallEnv,
    failures: &mut Vec<PromptFailure>,
) -> String {
    for PromptExecutable { origin, header, spec } in executables {
        let request = SlotRequest::Prompt(PromptPayload { system_prompt: prompt.clone() });
        match call_run(spec, &request, &BTreeMap::new(), env, SLOT_TIMEOUT).await {
            Ok(Called::Reply(SlotReply::Prompt(PromptReply::Append { append }))) => {
                if !append.trim().is_empty() {
                    let header = header.as_deref().map(|h| format!("{h}\n")).unwrap_or_default();
                    prompt.push_str(&format!("\n\n# {origin}\n{header}{}", append.trim_end()));
                }
            }
            Ok(Called::Reply(SlotReply::Prompt(PromptReply::Replace { replace }))) => prompt = replace,
            Ok(_) => {}
            Err(error) => failures.push(PromptFailure { origin: origin.clone(), message: error.to_string() }),
        }
    }
    prompt
}

/// [`glob_files`] on a blocking thread: a walk of a large tree is filesystem
/// work, and the runtime that serves every other connection should not wait
/// on it.
async fn glob_files_off_thread(cwd: PathBuf, pattern: String) -> Vec<PathBuf> {
    match tokio::task::spawn_blocking(move || glob_files(&cwd, &pattern)).await {
        Ok(files) => files,
        Err(error) => {
            tracing::warn!("the glob walk did not finish: {error}");
            Vec::new()
        }
    }
}

/// The files a `[[prompt]] files = "…"` glob names, sorted.
///
/// The pattern is relative to the loop's cwd; `~/` is the home directory and
/// an absolute pattern is taken as written. Only files are returned.
///
/// The walk honours `.gitignore` (and `.git/info/exclude`) and never enters
/// `.git`, so `files = "**/*.md"` in a project root is the project's
/// markdown, not its build output and not the object store. Hidden files are
/// *not* skipped: a pattern is allowed to name one, and `.claude/rules/*.md`
/// is a pattern people write.
fn glob_files(cwd: &Path, pattern: &str) -> Vec<PathBuf> {
    let expanded = match pattern.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")).join(rest).to_string_lossy().into_owned(),
        None => pattern.to_owned(),
    };
    let absolute = if Path::new(&expanded).is_absolute() {
        expanded
    } else {
        cwd.join(&expanded).to_string_lossy().into_owned()
    };
    let Ok(matcher) = globset::Glob::new(&absolute).map(|g| g.compile_matcher()) else {
        return Vec::new();
    };
    // Walk from the last directory before the first wildcard, so a pattern
    // rooted deep in a tree does not walk the whole tree.
    let is_magic = |part: &str| part.contains(['*', '?', '[', '{']);
    let mut base = PathBuf::new();
    let mut rest = 0usize;
    let mut seen_magic = false;
    let mut double_star = false;
    for part in Path::new(&absolute).components() {
        let text = part.as_os_str().to_string_lossy().into_owned();
        if seen_magic || is_magic(&text) {
            seen_magic = true;
            rest += 1;
            double_star |= text.contains("**");
        } else {
            base.push(part);
        }
    }
    if !seen_magic {
        let path = PathBuf::from(&absolute);
        return if path.is_file() { vec![path] } else { Vec::new() };
    }
    let mut walk = ignore::WalkBuilder::new(&base);
    walk.hidden(false)
        .parents(true)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .require_git(false)
        // The repository itself is never prompt material, and nothing in it
        // is listed in `.gitignore`.
        .filter_entry(|entry| entry.file_name() != ".git");
    if !double_star {
        walk.max_depth(Some(rest));
    }
    let mut found: Vec<PathBuf> = walk
        .build()
        .flatten()
        .filter(|entry| entry.file_type().is_some_and(|t| t.is_file()))
        .map(ignore::DirEntry::into_path)
        .filter(|path| !path.components().any(|part| part.as_os_str() == ".git"))
        .filter(|path| matcher.is_match(path))
        .collect();
    found.sort();
    found
}

// ---------------------------------------------------------------------------
// `dsl.check`
// ---------------------------------------------------------------------------

/// `dsl.check { cwd }`: the merged policy, the manifest and the system prompt
/// a loop in `cwd` would be given — without starting one (S4, D-21).
///
/// A `[[prompt]] run` is executed here too, because its output *is* part of
/// the prompt; it runs with an empty `PIRS_LOOP`, since no loop exists, and
/// a failure is reported as a conflict rather than silently dropped.
pub(crate) async fn check(cwd: &Path, socket: &Path) -> DslCheckResult {
    let policy = dsl::load(cwd);
    let builtin = crate::tools::builtin_tools(cwd);
    let infos: Vec<pirs_protocol::ToolInfo> = builtin.iter().map(crate::convert::tool_info).collect();
    let manifest = dsl::manifest(&policy, &infos);

    let mut env = CallEnv::new(cwd.to_path_buf(), "prompt");
    env.socket = socket.to_path_buf();
    let PromptSection { text: section, mut failures, executables } = prompt_section(&policy, &env).await;

    let selected = selected_tool_names(&policy, &manifest);
    let chosen: Vec<&ToolRef> = builtin
        .iter()
        .filter(|tool| selected.contains(&tool.name()) && manifest.tools.iter().any(|t| t.name == tool.name()))
        .collect();
    let mut options = crate::system_prompt::BuildSystemPromptOptions {
        cwd: cwd.to_string_lossy().into_owned(),
        context_files: crate::system_prompt::load_context_files(cwd),
        selected_tools: chosen.iter().map(|t| t.name()).collect(),
        tool_snippets: chosen.iter().filter_map(|t| t.prompt_snippet().map(|s| (t.name(), s))).collect(),
        tool_guidelines: chosen.iter().map(|t| (t.name(), t.prompt_guidelines())).filter(|(_, g)| !g.is_empty()).collect(),
        ..Default::default()
    };
    // A policy-defined tool has no built-in snippet; its description is what
    // the `<tools>` section can say about it.
    for tool in &manifest.tools {
        if builtin.iter().any(|b| b.name() == tool.name) || !selected.contains(&tool.name) {
            continue;
        }
        options.selected_tools.push(tool.name.clone());
        if !tool.description.is_empty() {
            options.tool_snippets.insert(tool.name.clone(), tool.description.clone());
        }
    }
    if !section.is_empty() {
        options.sections.insert(PROMPT_SECTION.to_owned(), section);
    }

    let base = crate::system_prompt::build_system_prompt(&options);
    let system_prompt = prompt_executables(&executables, base, &env, &mut failures).await;

    let mut conflicts = dsl::check_conflicts(&policy);
    for failure in failures {
        conflicts.push(DslConflict {
            message: format!("{}: {}", failure.origin, failure.message),
            files: vec![ServerPath::from(failure.origin.file.to_string_lossy().into_owned())],
        });
    }
    DslCheckResult {
        files: dsl::check_files(&policy),
        manifest,
        conflicts,
        rendered: dsl::render(&policy),
        system_prompt,
    }
}

/// The section name the assembled policy prompt lands under, so the built
/// prompt carries it as `<policy>…</policy>` and the logged system message
/// records it under its own key (D-21).
pub(crate) const PROMPT_SECTION: &str = "policy";

/// The tool names a loop in this directory starts with: `[settings] tools`
/// when it is set, the server's default selection otherwise, plus every tool
/// the policy defines (a declared tool is offered without being asked for).
pub(crate) fn selected_tool_names(policy: &Policy, manifest: &pirs_protocol::Manifest) -> Vec<String> {
    let mut selected: Vec<String> = match &policy.settings.tools {
        Some(names) => names.clone(),
        None => crate::tools::DEFAULT_TOOL_NAMES.iter().map(|s| (*s).to_owned()).collect(),
    };
    for tool in &manifest.tools {
        if !crate::tools::ALL_TOOL_NAMES.contains(&tool.name.as_str()) && !selected.contains(&tool.name) {
            selected.push(tool.name.clone());
        }
    }
    selected.retain(|name| manifest.tools.iter().any(|t| t.name == *name));
    selected
}

// ---------------------------------------------------------------------------
// The loop's side
// ---------------------------------------------------------------------------

impl LoopHandle {
    /// The environment a called process for `slot` is given (D-23, D-24).
    pub(crate) fn call_env(&self, slot: impl Into<String>) -> CallEnv {
        CallEnv {
            cwd: self.cwd.clone(),
            socket: self.socket.clone(),
            loop_id: self.id.clone(),
            slot: slot.into(),
            session_dir: self.session_dir.clone(),
            extra: Vec::new(),
        }
    }

    /// The binding a `run` gets on this loop: resolved against its cwd.
    pub(crate) fn run_spec(&self, run: &str) -> RunSpec {
        RunSpec::resolve(run, &self.cwd)
    }

    /// One warning naming the entry that produced it.
    pub(crate) fn policy_warn(&self, origin: &Origin, message: impl std::fmt::Display) {
        let text = format!("{origin}: {message}");
        tracing::warn!(loop_id = %self.id, "{text}");
        self.ui_notify(NotifyLevel::Warning, text);
    }

    // ----- input ----------------------------------------------------------

    /// The policy's `[[input]]` entries, in file order, before any registered
    /// handler sees the text. `None` means the input was consumed.
    pub(crate) async fn dsl_input(self: &Arc<Self>, mut text: String) -> Option<String> {
        for entry in self.policy().input.clone() {
            let Some(vars) = input_vars(&entry, &text) else { continue };
            if let Some(replace) = &entry.replace {
                text = entry.regex.replace(&text, replace.as_str()).into_owned();
            }
            match &entry.run {
                None => {
                    if entry.handled {
                        return None;
                    }
                }
                Some(run) => {
                    let spec = self.run_spec(run);
                    let command = if spec.is_exec() { run.clone() } else { process::interpolate(run, &vars) };
                    let request = SlotRequest::Input(InputPayload { text: text.clone() });
                    let env = self.call_env("input");
                    let outcome = call_run(&spec, &request, &vars, &env, SLOT_TIMEOUT).await;
                    // A shell string's stdout and an executable's `{ text }`
                    // are the same thing: the text to hand on.
                    let replied = match outcome {
                        Ok(Called::Text(out)) => Ok(Some(out)),
                        Ok(Called::Reply(SlotReply::Input(InputReply::Text { text }))) => Ok(Some(text)),
                        Ok(Called::Reply(SlotReply::Input(InputReply::Handled { .. }))) => Ok(None),
                        Ok(Called::Reply(_)) => unreachable!("an input request parses input replies"),
                        Err(error) => Err(error),
                    };
                    match (replied, entry.handled) {
                        // The output of a consumed command enters the
                        // conversation as the shell execution it was (D-32).
                        (Ok(Some(out)), true) => {
                            self.record_command(command, out, Some(0));
                            return None;
                        }
                        (Ok(Some(out)), false) => text = out,
                        // The executable said `{ handled: true }`: consumed,
                        // whatever the file said.
                        (Ok(None), _) => return None,
                        (Err(error), true) => {
                            self.policy_warn(&entry.origin, &error);
                            let (output, code) = failure_output(&error);
                            self.record_command(command, output, code);
                            return None;
                        }
                        (Err(error), false) => self.policy_warn(&entry.origin, &error),
                    }
                }
            }
        }
        Some(text)
    }

    /// Record a consumed command and its output as the `bashExecution` the
    /// model reads on its next request, logged and sent to observers.
    fn record_command(self: &Arc<Self>, command: String, output: String, exit_code: Option<i32>) {
        let message = pi_agent::AgentMessage::BashExecution(pi_agent::BashExecutionMessage {
            command,
            output,
            exit_code,
            cancelled: false,
            truncated: false,
            full_output_path: None,
            exclude_from_context: false,
            timestamp: now_ms(),
        });
        // A run is in flight: steering puts it in the conversation at the
        // next turn boundary, and the agent's own event logs it. Idle: append
        // and log it here.
        if self.agent.is_streaming() {
            self.agent.steer(message);
            return;
        }
        self.agent.append_message(message.clone());
        if let Err(error) = self.log.lock().unwrap_or_else(|e| e.into_inner()).append_message(message) {
            tracing::warn!(loop_id = %self.id, "could not log a command's output: {error}");
        }
    }

    // ----- tool_result ----------------------------------------------------

    /// The policy's `[[tool_result]]` entries for `tool`, in file order, each
    /// seeing the previous one's result. `Some((result, by))` when at least
    /// one rewrote it.
    pub(crate) async fn dsl_tool_result(
        self: &Arc<Self>,
        tool: &str,
        args: &Value,
        mut result: ToolReply,
    ) -> Option<(ToolReply, String)> {
        let mut by: Option<String> = None;
        for entry in self.policy().tool_result.clone() {
            if entry.tool != tool {
                continue;
            }
            let request = SlotRequest::ToolResult(ToolResultPayload {
                tool: tool.to_owned(),
                args: args.clone(),
                result: result.clone(),
            });
            let env = self.call_env("tool_result");
            let spec = self.run_spec(&entry.run);
            match call_run(&spec, &request, &BTreeMap::new(), &env, SLOT_TIMEOUT).await {
                Ok(Called::Text(text)) => {
                    result = ToolReply::Ok { content: ToolContent::Text(text), details: None };
                    by = Some(entry.origin.to_string());
                }
                Ok(Called::Reply(SlotReply::ToolResult(reply))) => {
                    result = reply.result;
                    by = Some(entry.origin.to_string());
                }
                Ok(Called::Reply(_)) => unreachable!("a tool_result request parses tool_result replies"),
                Err(error) => self.policy_warn(&entry.origin, &error),
            }
        }
        by.map(|by| (result, by))
    }

    // ----- on, status, widget ---------------------------------------------

    /// Run the policy's `[[on]]` entries for this event. Never waited for by
    /// the caller: the loop does not stall on its own policy.
    pub(crate) fn fire_dsl_on(self: &Arc<Self>, payload: &OnPayload) {
        let event = payload.event();
        let entries: Vec<OnEntry> = self.policy().on.iter().filter(|e| e.event == event).cloned().collect();
        if entries.is_empty() {
            return;
        }
        let params = SlotRequest::On(payload.clone()).params();
        let this = self.clone();
        // Counted, so `loop.close` waits for the firing to have started its
        // processes before it takes the list of them.
        this.on_tasks.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            for entry in entries {
                if event == OnEvent::Start && !this.mark_started(&entry) {
                    continue;
                }
                this.run_on_entry(&entry, &params).await;
            }
            this.on_tasks.fetch_sub(1, Ordering::SeqCst);
        });
    }

    async fn run_on_entry(self: &Arc<Self>, entry: &OnEntry, params: &Value) {
        let slot = format!("on.{}", entry.event.as_str());
        let Some(emit) = entry.emit.clone() else {
            // A plain `[[on]]` is fire and forget (D-16): the process may
            // outlive the event and become a connected client.
            let OnSource::Run(run) = &entry.source else { return };
            let env = self.call_env(slot);
            match process::spawn_detached(&self.run_spec(run), params, &BTreeMap::new(), &env).await {
                Ok(spawned) => {
                    let this = self.clone();
                    let origin = entry.origin.clone();
                    let quiet = entry.quiet;
                    let detached = process::supervise(spawned, move |status| {
                        if !quiet {
                            this.policy_warn(&origin, format!("exited {status}"));
                        }
                    });
                    self.track_process(detached);
                }
                Err(error) => self.policy_warn(&entry.origin, &error),
            }
            return;
        };
        // `on.*` takes no reply, so an executable's stdout is its output as
        // text here too: the status value, the widget's lines.
        let text = match &entry.source {
            OnSource::Run(run) => {
                let env = self.call_env(slot);
                match process::call(&self.run_spec(run), params, &BTreeMap::new(), &env, SLOT_TIMEOUT).await {
                    Ok(output) => output.stdout,
                    Err(error) => {
                        self.policy_warn(&entry.origin, &error);
                        return;
                    }
                }
            }
            OnSource::File(path) => {
                let path = if path.is_absolute() { path.clone() } else { self.cwd.join(path) };
                match std::fs::read_to_string(&path) {
                    Ok(text) => text,
                    Err(error) => {
                        self.policy_warn(&entry.origin, format!("cannot read {}: {error}", path.display()));
                        return;
                    }
                }
            }
        };
        match emit {
            Emit::Status { key } => self.ui_status(key, text.trim().to_owned()),
            Emit::Widget { key } => self.ui_widget(key, text.lines().map(str::to_owned).collect()),
        }
    }

    // ----- tools ----------------------------------------------------------

    /// The [`AgentTool`] a resolved `[[tool]]` becomes. `builtin` is the
    /// built-in of the same name, if there is one: a `wrap` keeps its name,
    /// description and schema and only changes what runs.
    pub(crate) fn policy_tool(self: &Arc<Self>, tool: &ResolvedTool, builtin: Option<&ToolRef>) -> ToolRef {
        let backend = match (&tool.wrap, &tool.source) {
            (Some(wrap), _) => Backend::Run(wrap.clone()),
            (None, ToolSource::Run(run)) => Backend::Run(run.clone()),
            (None, ToolSource::Handler) => Backend::Handler,
            (None, ToolSource::Loop(_)) => Backend::Loop,
            // A modifier entry that neither disables nor wraps leaves the
            // built-in exactly as it was.
            (None, ToolSource::Builtin) => return builtin.cloned().expect("a built-in source has a built-in"),
        };
        let description = match (tool.description.is_empty(), builtin) {
            (true, Some(builtin)) => builtin.description(),
            _ => tool.description.clone(),
        };
        let parameters = match (tool.declares_params(), builtin) {
            (false, Some(builtin)) => builtin.parameters(),
            _ => tool.parameters.clone(),
        };
        Arc::new(PolicyTool {
            handle: Arc::downgrade(self),
            name: tool.name.clone(),
            description,
            parameters,
            snippet: builtin.and_then(|b| b.prompt_snippet()),
            guidelines: builtin.map(|b| b.prompt_guidelines()).unwrap_or_default(),
            backend,
            timeout: tool.timeout,
        })
    }
}

/// The interpolation variables an `[[input]]` match contributes, or `None`
/// when the entry does not match (D-24).
///
/// `0`…`9` are the numbered groups, every named group is itself, `args` is
/// the first group (a `[[command]]`'s tail) or else the text after the match,
/// and `text` is the whole input.
fn input_vars(entry: &InputEntry, text: &str) -> Option<BTreeMap<String, String>> {
    let captures = entry.regex.captures(text)?;
    let mut vars = BTreeMap::new();
    for index in 0..=9usize {
        if let Some(group) = captures.get(index) {
            vars.insert(index.to_string(), group.as_str().to_owned());
        }
    }
    for name in entry.regex.capture_names().flatten() {
        if let Some(group) = captures.name(name) {
            vars.insert(name.to_owned(), group.as_str().to_owned());
        }
    }
    let args = match captures.get(1) {
        Some(group) => group.as_str().trim().to_owned(),
        None => captures.get(0).map(|m| text[m.end()..].trim().to_owned()).unwrap_or_default(),
    };
    vars.insert("args".to_owned(), args);
    vars.insert("text".to_owned(), text.to_owned());
    Some(vars)
}

/// What a failed `handled` command records as its output.
fn failure_output(error: &RunFailure) -> (String, Option<i32>) {
    match error {
        RunFailure::Call(CallError::NonZero { status, stderr, stdout }) => {
            let mut text = stdout.clone();
            if !stderr.trim().is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(stderr.trim_end());
            }
            (text, Some(*status))
        }
        other => (other.to_string(), None),
    }
}

// ---------------------------------------------------------------------------
// A tool the policy declared
// ---------------------------------------------------------------------------

/// What answers a policy tool's calls.
enum Backend {
    /// A `run` (or `wrap`): a called process. An executable reads
    /// `{ args, id }` on stdin and replies `{ content, details? }` or
    /// `{ error }` on stdout; a shell string also gets every argument as
    /// `$name` and as `PIRS_ARG_<name>`, so `run = "curl $url"` and
    /// `run = "curl \"$PIRS_ARG_url\""` both work (D-24), and its stdout is
    /// the result as text.
    Run(String),
    /// A declaration: the connected client that registered `tool.<name>`
    /// answers, with its registered timeout (D-23).
    Handler,
    /// `loop = { … }`, which phase 5 fills in.
    Loop,
}

/// A `[[tool]]` the policy defines: a called process, a declaration a
/// connected handler serves, or a built-in the policy wrapped.
struct PolicyTool {
    handle: Weak<LoopHandle>,
    name: String,
    description: String,
    parameters: Value,
    snippet: Option<String>,
    guidelines: Vec<String>,
    backend: Backend,
    timeout: Duration,
}

#[async_trait]
impl AgentTool for PolicyTool {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn description(&self) -> String {
        self.description.clone()
    }

    fn parameters(&self) -> Value {
        self.parameters.clone()
    }

    fn prompt_snippet(&self) -> Option<String> {
        self.snippet.clone().or_else(|| (!self.description.is_empty()).then(|| self.description.clone()))
    }

    fn prompt_guidelines(&self) -> Vec<String> {
        self.guidelines.clone()
    }

    async fn execute(
        &self,
        tool_call_id: &str,
        args: Value,
        cancel: CancellationToken,
        _on_update: UpdateFn,
    ) -> anyhow::Result<ToolResult> {
        let Some(handle) = self.handle.upgrade() else {
            anyhow::bail!("loop closed");
        };
        let run = match &self.backend {
            Backend::Run(run) => run,
            Backend::Loop => anyhow::bail!("tool {}: loop tools arrive in phase 5", self.name),
            Backend::Handler => {
                // The registrant's timeout applies; an abort or close must
                // not wait it out.
                let reply = tokio::select! {
                    biased;
                    () = cancel.cancelled() => anyhow::bail!("aborted"),
                    reply = handle.call_tool(&self.name, args, tool_call_id) => reply,
                };
                return reply_to_result(&reply);
            }
        };
        let request = SlotRequest::Tool {
            name: self.name.clone(),
            payload: ToolCallPayload { args: args.clone(), id: tool_call_id.to_owned() },
        };
        let spec = handle.run_spec(run);
        let mut env = handle.call_env(format!("tool.{}", self.name));
        // The arguments are one level down in the payload, so the shell
        // string's `$name` and `PIRS_ARG_<name>` come from here (D-24). An
        // executable reads the JSON line and gets neither.
        let vars = if spec.is_exec() { BTreeMap::new() } else { process::payload_vars(&args) };
        if !spec.is_exec() {
            if let Some(map) = args.as_object() {
                for (key, value) in map {
                    env.extra.push((format!("PIRS_ARG_{key}"), process::env_value(value)));
                }
            }
        }
        // Dropping the call — an abort — kills the process group (D-23).
        let call = call_run(&spec, &request, &vars, &env, self.timeout);
        let outcome = tokio::select! {
            biased;
            () = cancel.cancelled() => anyhow::bail!("aborted"),
            outcome = call => outcome,
        };
        match outcome {
            Ok(Called::Text(text)) => Ok(ToolResult::text(text)),
            Ok(Called::Reply(SlotReply::Tool(reply))) => reply_to_result(&reply),
            Ok(Called::Reply(_)) => unreachable!("a tool request parses tool replies"),
            // A non-zero exit is an error result with stderr as the message
            // (or "exited with status N" when stderr is empty); a malformed
            // reply is one too, and is also worth a warning, because the
            // model cannot fix the script.
            Err(RunFailure::Call(error)) => anyhow::bail!("{error}"),
            Err(RunFailure::BadReply(message)) => {
                if let Some(origin) = handle.policy().tools.iter().find(|t| t.name == self.name).and_then(|t| t.origins.first()) {
                    handle.policy_warn(origin, &message);
                }
                anyhow::bail!("{message}")
            }
        }
    }
}

/// A handler's or executable's [`ToolReply`] as the agent's result: `Ok` as
/// is, `Error` as the `Err` the agent records as an error result.
fn reply_to_result(reply: &ToolReply) -> anyhow::Result<ToolResult> {
    let (result, is_error) = crate::convert::result_from_reply(reply);
    if is_error {
        anyhow::bail!("{}", result.content.iter().filter_map(pi_ai::Content::as_text).collect::<Vec<_>>().join(""));
    }
    Ok(result)
}

/// `[[on]] event = "start"` identity across a reload: the same file, slot,
/// position and command is the same entry, and is not started again (D-23).
pub(crate) fn entry_key(entry: &OnEntry) -> String {
    let what = match &entry.source {
        OnSource::Run(run) => run.clone(),
        OnSource::File(path) => path.to_string_lossy().into_owned(),
    };
    format!("{}|{}|{}|{what}", entry.origin.file.display(), entry.origin.slot, entry.origin.index)
}

impl LoopHandle {
    /// Whether this `on start` entry still has to be started, remembering
    /// that it was.
    fn mark_started(&self, entry: &OnEntry) -> bool {
        let mut started = self.started_on.lock().unwrap_or_else(|e| e.into_inner());
        started.insert(entry_key(entry))
    }

    /// Keep a detached process so `loop.close` can kill its group. One
    /// started after the close began is not tracked; it is given the same
    /// grace and then killed, so nothing the loop started outlives it.
    fn track_process(&self, detached: process::Detached) {
        if self.closed.load(Ordering::SeqCst) {
            tokio::spawn(async move {
                let deadline = tokio::time::Instant::now() + process::CLOSE_GRACE;
                while !detached.has_exited() && tokio::time::Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                detached.kill().await;
            });
            return;
        }
        self.processes.lock().unwrap_or_else(|e| e.into_inner()).push(detached);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsl::{parse_file, PolicyFile};

    fn file(body: &str) -> PolicyFile {
        parse_file(Path::new("/p/a.pirs.toml"), body).expect("parses")
    }

    #[test]
    fn a_command_entry_gives_args_the_text_after_the_name() {
        let policy = dsl::compose(vec![file(
            "intent = 'x'\n[[command]]\nname = 'handoff'\ndescription = 'd'\nrun = 'echo $args'\n",
        )]);
        let entry = &policy.input[0];
        let vars = input_vars(entry, "/handoff notes.md and more").expect("matches");
        assert_eq!(vars["args"], "notes.md and more");
        assert_eq!(vars["1"], " notes.md and more", "the raw group keeps its spacing");
        assert_eq!(vars["text"], "/handoff notes.md and more");
        assert!(input_vars(entry, "handoff without the slash").is_none());
    }

    #[test]
    fn an_input_entry_without_a_group_still_has_args_and_the_numbered_zero() {
        let policy = dsl::compose(vec![file("intent = 'x'\n[[input]]\nmatch = '^!'\nrun = 'true'\n")]);
        let vars = input_vars(&policy.input[0], "!ls -l").expect("matches");
        assert_eq!(vars["0"], "!");
        assert_eq!(vars["args"], "ls -l", "what follows the match");
    }

    #[test]
    fn named_groups_are_variables_of_their_own() {
        let policy =
            dsl::compose(vec![file("intent = 'x'\n[[input]]\nmatch = '^@(?<who>\\w+) (.*)'\nrun = 'true'\n")]);
        let vars = input_vars(&policy.input[0], "@ada do the thing").expect("matches");
        assert_eq!(vars["who"], "ada");
        assert_eq!(vars["2"], "do the thing");
    }

    #[test]
    fn a_glob_finds_files_under_the_cwd_and_nothing_outside_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("rules/deep")).expect("dirs");
        std::fs::write(dir.path().join("rules/b.md"), "b").expect("b");
        std::fs::write(dir.path().join("rules/a.md"), "a").expect("a");
        std::fs::write(dir.path().join("rules/a.txt"), "not markdown").expect("txt");
        std::fs::write(dir.path().join("rules/deep/c.md"), "c").expect("c");

        let shallow = glob_files(dir.path(), "rules/*.md");
        assert_eq!(
            shallow,
            [dir.path().join("rules/a.md"), dir.path().join("rules/b.md")],
            "sorted, one level, matching the suffix"
        );
        let deep = glob_files(dir.path(), "rules/**/*.md");
        assert!(deep.contains(&dir.path().join("rules/deep/c.md")), "{deep:?}");
        assert_eq!(glob_files(dir.path(), "rules/a.md"), [dir.path().join("rules/a.md")], "a plain path is itself");
        assert!(glob_files(dir.path(), "nothing/*.md").is_empty());
    }

    #[test]
    fn a_glob_skips_what_gitignore_names_and_never_enters_dot_git() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(".gitignore"), "target/\n").expect("gitignore");
        std::fs::create_dir_all(dir.path().join("target/doc")).expect("target");
        std::fs::create_dir_all(dir.path().join(".git/objects")).expect("git");
        std::fs::create_dir_all(dir.path().join("docs")).expect("docs");
        std::fs::write(dir.path().join("docs/a.md"), "a").expect("a");
        std::fs::write(dir.path().join("target/doc/built.md"), "built").expect("built");
        std::fs::write(dir.path().join(".git/objects/loose.md"), "loose").expect("loose");

        let found = glob_files(dir.path(), "**/*.md");
        assert_eq!(found, [dir.path().join("docs/a.md")], "only the tracked markdown: {found:?}");
    }

    #[tokio::test]
    async fn the_prompt_section_names_every_entry_and_reports_what_failed() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("rule.md"), "Be kind.\n").expect("rule");
        let policy = dsl::compose(vec![file(
            "intent = 'x'\n\
             [[prompt]]\ntext = 'Always.'\n\
             [[prompt]]\nfiles = '*.md'\n\
             [[prompt]]\nrun = 'echo ran'\nheader = 'Head:'\n\
             [[prompt]]\nrun = 'exit 3'\n",
        )]);
        let env = CallEnv::new(dir.path().to_path_buf(), "prompt");
        let PromptSection { text, failures, executables } = prompt_section(&policy, &env).await;
        assert!(executables.is_empty(), "shell strings are not deferred");
        assert!(text.contains("# /p/a.pirs.toml: [[prompt]] #1\nAlways."), "{text}");
        assert!(text.contains("<file path=") && text.contains("Be kind."), "{text}");
        assert!(text.contains("Head:\nran"), "{text}");
        assert_eq!(failures.len(), 1, "only the failing entry");
        assert_eq!(failures[0].origin.index, 3);
    }

    #[test]
    fn an_on_entry_is_the_same_entry_across_a_reload_only_if_nothing_about_it_moved() {
        let one = dsl::compose(vec![file("intent = 'x'\n[[on]]\nevent = 'start'\nrun = './watch'\n")]);
        let same = dsl::compose(vec![file("intent = 'x'\n[[on]]\nevent = 'start'\nrun = './watch'\n")]);
        let changed = dsl::compose(vec![file("intent = 'x'\n[[on]]\nevent = 'start'\nrun = './watch --all'\n")]);
        assert_eq!(entry_key(&one.on[0]), entry_key(&same.on[0]));
        assert_ne!(entry_key(&one.on[0]), entry_key(&changed.on[0]));
    }
}
