//! The DSL: policy files, desugaring, composition and checking.
//!
//! A policy file is `*.pirs.toml` in `~/.pirs/ext/` (global) or
//! `<cwd>/.pirs/ext/` (project). [`load`] finds them, parses each one, and
//! [`compose`]s the results into one [`Policy`]: the merged view a loop in
//! that directory starts with.
//!
//! This module is pure. It reads policy files and nothing else: it spawns no
//! process, opens no socket and touches no loop state. Running a `run = "…"`
//! is the caller's job; everything here only says what would run.
//!
//! Three of the eight slots are sugar and are gone by the time [`compose`]
//! returns (D-17, D-18):
//!
//! - `[[status]]` and `[[widget]]` become one [`OnEntry`] per event in their
//!   `on` list, carrying an [`Emit`] that says where the output goes;
//! - `[[command]]` becomes an [`InputEntry`] matching `^/<name>\b(.*)` with
//!   `handled = true` and a [`CommandInfo`] for the manifest.
//!
//! Every entry keeps its [`Origin`] — file, slot and index — so an error, a
//! conflict and [`render`] can all name where a rule came from.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use pirs_protocol::{CommandInfo, DslConflict, Manifest, OnEvent, ServerPath, ToolInfo};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;

mod compose;
mod parse;

#[cfg(test)]
mod tests;

/// The suffix that makes a file a policy file.
pub const POLICY_SUFFIX: &str = ".pirs.toml";

/// The directory holding policy files, under the agent dir and under a
/// project's `.pirs`.
pub const POLICY_DIR: &str = "ext";

/// A `[[tool]]` without `timeout` gets this one (the plan's default).
pub const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Where an entry came from
// ---------------------------------------------------------------------------

/// The file, slot and position an entry was written at.
///
/// The slot is the one the *author* wrote, so a desugared entry still points
/// at its `[[command]]` or `[[status]]` table rather than at the `[[input]]`
/// or `[[on]]` it became.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// The policy file.
    pub file: PathBuf,
    /// The slot name as written: `input`, `prompt`, `tool_result`, `status`,
    /// `widget`, `on`, `command` or `tool`.
    pub slot: &'static str,
    /// Position within that slot in that file, counted from zero.
    pub index: usize,
}

impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: [[{}]] #{}", self.file.display(), self.slot, self.index + 1)
    }
}

// ---------------------------------------------------------------------------
// Slot entries
// ---------------------------------------------------------------------------

/// `[[input]]`: transform or consume what the user typed.
#[derive(Debug, Clone)]
pub struct InputEntry {
    /// Where it was written.
    pub origin: Origin,
    /// The `match` regex as written.
    pub pattern: String,
    /// The compiled `match`; every entry carries one, so a policy that
    /// reaches this type has no uncompilable pattern.
    pub regex: Regex,
    /// `replace`: the rewritten text, with `$1`… for the capture groups.
    pub replace: Option<String>,
    /// `handled`: stop here, never reach the model.
    pub handled: bool,
    /// `run`: the process that decides.
    pub run: Option<String>,
    /// Set when this entry is a desugared `[[command]]` (D-18); it is what
    /// the manifest lists.
    pub command: Option<CommandInfo>,
}

/// Where a `[[prompt]]` entry's text comes from. Exactly one per entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptSource {
    /// `text`: literal.
    Text(String),
    /// `files`: a glob, relative to the loop's cwd; contents appended.
    Files(String),
    /// `run`: a process whose stdout is appended.
    Run(String),
}

/// `[[prompt]]`: add to the system prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptEntry {
    /// Where it was written.
    pub origin: Origin,
    /// Its one source.
    pub source: PromptSource,
    /// `header`: printed above the output.
    pub header: Option<String>,
}

/// `[[tool_result]]`: rewrite what the model reads back from a tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResultEntry {
    /// Where it was written.
    pub origin: Origin,
    /// The tool whose results this rewrites.
    pub tool: String,
    /// The process that rewrites them.
    pub run: String,
}

/// `[[status]]`: one key in the status line. Desugars to [`OnEntry`]s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEntry {
    /// Where it was written.
    pub origin: Origin,
    /// The status key.
    pub key: String,
    /// The process whose stdout becomes the value.
    pub run: String,
    /// The events that refresh it.
    pub on: Vec<OnEvent>,
}

/// Where a `[[widget]]`'s lines come from. Exactly one per entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WidgetSource {
    /// `file`: read it, relative to the loop's cwd.
    File(PathBuf),
    /// `run`: a process whose stdout is the content.
    Run(String),
}

/// `[[widget]]`: lines the UI shows beside the conversation. Desugars to
/// [`OnEntry`]s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidgetEntry {
    /// Where it was written.
    pub origin: Origin,
    /// The widget key.
    pub key: String,
    /// Its one source.
    pub source: WidgetSource,
    /// The events that refresh it.
    pub on: Vec<OnEvent>,
}

/// Where an [`OnEntry`]'s output goes, for the two sugared slots (D-17).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Emit {
    /// Send the output as `ui.status` under this key.
    Status {
        /// The status key.
        key: String,
    },
    /// Send the output as `ui.widget` under this key.
    Widget {
        /// The widget key.
        key: String,
    },
}

/// What an [`OnEntry`] does when its event fires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnSource {
    /// Run a process; its stdout is the output.
    Run(String),
    /// Read a file (only a desugared `[[widget]] file = …`).
    File(PathBuf),
}

/// `[[on]]`: run something at an event. The one execution path under
/// `[[on]]`, `[[status]]` and `[[widget]]` (D-17).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnEntry {
    /// Where it was written.
    pub origin: Origin,
    /// The event that fires it.
    pub event: OnEvent,
    /// What it does.
    pub source: OnSource,
    /// `quiet`: discard the output.
    pub quiet: bool,
    /// Where the output goes, for a desugared `[[status]]` or `[[widget]]`.
    pub emit: Option<Emit>,
}

/// `[[command]]`: a `/name` the user can type. Desugars to an [`InputEntry`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandEntry {
    /// Where it was written.
    pub origin: Origin,
    /// The name, without the leading `/`.
    pub name: String,
    /// One line of help, for the manifest.
    pub description: String,
    /// The process the command runs.
    pub run: String,
}

/// `loop = { model, prompt, wait }`: a `[[tool]]` that asks a second loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopSpec {
    /// The model the second loop runs.
    pub model: String,
    /// Its first prompt, with `$field` interpolation from the call's
    /// arguments.
    pub prompt: String,
    /// When the call returns; `"idle"` is the loop going idle.
    pub wait: String,
}

/// `[[tool]]`: give the model an executable, or disable or wrap one it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolEntry {
    /// Where it was written.
    pub origin: Origin,
    /// The name the model calls.
    pub name: String,
    /// What the tool does, as the model reads it.
    pub description: Option<String>,
    /// `params.<field> = { type, description, default, … }`, each a JSON
    /// Schema fragment for one argument.
    pub params: BTreeMap<String, Value>,
    /// The executable or shell string the call runs.
    pub run: Option<String>,
    /// `timeout`, in seconds; [`DEFAULT_TOOL_TIMEOUT`] when absent.
    pub timeout: Duration,
    /// `loop = { … }` instead of `run`.
    pub loop_spec: Option<LoopSpec>,
    /// `disabled`: the model is not offered this tool.
    pub disabled: bool,
    /// `wrap`: a program the real call is routed through.
    pub wrap: Option<String>,
}

impl ToolEntry {
    /// A *base* entry defines the tool; a modifier only disables or wraps
    /// one defined elsewhere (or a built-in).
    fn is_base(&self) -> bool {
        self.run.is_some() || self.loop_spec.is_some()
    }
}

/// `[settings]`: what `settings.json` used to hold.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsTable {
    /// The model a loop starts on: `provider/id` or an id the registry
    /// resolves.
    pub model: Option<String>,
    /// The thinking level a loop starts on.
    pub thinking: Option<String>,
    /// The tools the model is offered; absent means the server's default
    /// selection.
    pub tools: Option<Vec<String>>,
    /// Whether the tool calls of one assistant message run together or one
    /// after the other. This is what `settings.json`'s `toolExecution` was.
    pub tool_execution: Option<ToolExecution>,
}

impl SettingsTable {
    /// The keys this table defines, in declaration order.
    pub const KEYS: [&'static str; 4] = ["model", "thinking", "tools", "tool_execution"];
}

/// `[settings] tool_execution`: how a turn's tool calls are run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecution {
    /// All of a message's tool calls at once (the default).
    Parallel,
    /// One at a time, in the order the model asked for them.
    Sequential,
}

impl ToolExecution {
    /// The name as it is written in a policy file.
    pub fn as_str(self) -> &'static str {
        match self {
            ToolExecution::Parallel => "parallel",
            ToolExecution::Sequential => "sequential",
        }
    }
}

// ---------------------------------------------------------------------------
// A parsed file
// ---------------------------------------------------------------------------

/// One parsed `*.pirs.toml`, before composition.
#[derive(Debug, Clone)]
pub struct PolicyFile {
    /// Where it was read from.
    pub path: PathBuf,
    /// `intent`: required, the shareable unit.
    pub intent: String,
    /// `priority`: higher moves the file earlier; default 0.
    pub priority: i64,
    /// `[settings]`, if the file has one.
    pub settings: Option<SettingsTable>,
    /// `[[input]]` entries, in file order.
    pub input: Vec<InputEntry>,
    /// `[[prompt]]` entries, in file order.
    pub prompt: Vec<PromptEntry>,
    /// `[[tool_result]]` entries, in file order.
    pub tool_result: Vec<ToolResultEntry>,
    /// `[[status]]` entries, in file order.
    pub status: Vec<StatusEntry>,
    /// `[[widget]]` entries, in file order.
    pub widget: Vec<WidgetEntry>,
    /// `[[on]]` entries, in file order.
    pub on: Vec<OnEntry>,
    /// `[[command]]` entries, in file order.
    pub command: Vec<CommandEntry>,
    /// `[[tool]]` entries, in file order.
    pub tool: Vec<ToolEntry>,
}

// ---------------------------------------------------------------------------
// The composed result
// ---------------------------------------------------------------------------

/// Where a resolved tool's behaviour comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolSource {
    /// One of the server's built-ins, only disabled or wrapped here.
    Builtin,
    /// A process the server calls.
    Run(String),
    /// A second loop.
    Loop(LoopSpec),
}

/// A tool after every `[[tool]]` entry for its name has been applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTool {
    /// The name the model calls.
    pub name: String,
    /// What the tool does; empty means "whatever the built-in says".
    pub description: String,
    /// JSON Schema for the arguments, built from `params`: a param without
    /// a `default` is required.
    pub parameters: Value,
    /// Built-in, a process, or a second loop.
    pub source: ToolSource,
    /// The model is not offered this tool.
    pub disabled: bool,
    /// A program the real call is routed through.
    pub wrap: Option<String>,
    /// How long a call may take.
    pub timeout: Duration,
    /// Every `[[tool]]` entry that contributed, in file order.
    pub origins: Vec<Origin>,
}

impl ResolvedTool {
    /// Whether the policy declared arguments of its own, as opposed to
    /// leaving a built-in's schema alone.
    pub fn declares_params(&self) -> bool {
        self.parameters
            .get("properties")
            .and_then(Value::as_object)
            .is_some_and(|properties| !properties.is_empty())
    }
}

/// Every policy file for one directory, merged and checked.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    /// The files, in load order (the order everything below is in).
    pub files: Vec<PathBuf>,
    /// Each file's `intent`, in the same order.
    pub intents: Vec<(PathBuf, String)>,
    /// `[[input]]` and desugared `[[command]]` entries, in file order; at
    /// runtime the first `handled` one stops the rest.
    pub input: Vec<InputEntry>,
    /// `[[prompt]]` entries, concatenated in file order.
    pub prompt: Vec<PromptEntry>,
    /// `[[tool_result]]` entries, in file order; several for one tool apply
    /// in that order.
    pub tool_result: Vec<ToolResultEntry>,
    /// `[[on]]` entries and the desugared `[[status]]` and `[[widget]]`
    /// ones, in file order; all of them run.
    pub on: Vec<OnEntry>,
    /// Tools the policy names, resolved. Built-ins the policy says nothing
    /// about are not here; [`manifest`] adds them.
    pub tools: Vec<ResolvedTool>,
    /// Slash commands, for the manifest.
    pub commands: Vec<CommandInfo>,
    /// Status keys, in file order.
    pub status_keys: Vec<String>,
    /// Widget keys, in file order.
    pub widget_keys: Vec<String>,
    /// `[settings]` merged: the last file to set a key wins.
    pub settings: SettingsTable,
    /// Which file set each `[settings]` key.
    pub settings_sources: Vec<(&'static str, PathBuf)>,
    /// Composition conflicts, each naming the files involved.
    pub conflicts: Vec<DslConflict>,
    /// Per-file errors: a file that could not be read or parsed, or a
    /// `[[tool]]` that defines nothing. The other files still load.
    pub errors: Vec<(PathBuf, String)>,
}

// ---------------------------------------------------------------------------
// Locating
// ---------------------------------------------------------------------------

/// The two directories policy files live in: global, then project.
pub fn policy_dirs(cwd: &Path) -> [PathBuf; 2] {
    [
        crate::settings::agent_dir().join(POLICY_DIR),
        cwd.join(crate::settings::CONFIG_DIR_NAME).join(POLICY_DIR),
    ]
}

/// Every `*.pirs.toml` that applies to a loop in `cwd`: the global ones then
/// the project ones, each set sorted by path.
///
/// A directory that does not exist contributes nothing; that is the normal
/// case and not an error.
pub fn policy_paths(cwd: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for dir in policy_dirs(cwd) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut here: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.len() > POLICY_SUFFIX.len() && name.ends_with(POLICY_SUFFIX))
                    && path.is_file()
            })
            .collect();
        here.sort();
        found.append(&mut here);
    }
    found
}

/// Whether `path` is a policy file for a loop in `cwd`, so that the loop's
/// own write to it reloads the policy (D-33).
///
/// The file need not exist: a write that has just happened and a delete both
/// answer this the same way.
pub fn is_policy_path(cwd: &Path, path: &Path) -> bool {
    let name_ok = path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.len() > POLICY_SUFFIX.len() && name.ends_with(POLICY_SUFFIX));
    if !name_ok {
        return false;
    }
    let Some(parent) = path.parent() else {
        return false;
    };
    let parent = real(parent);
    policy_dirs(cwd).iter().any(|dir| real(dir) == parent)
}

/// A path with symlinks and `..` resolved when it exists, and as written
/// when it does not.
fn real(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Parse one policy file's text.
///
/// The error is every problem found in the file, not just the first, so one
/// `pirs check` shows them all.
pub fn parse_file(path: &Path, text: &str) -> Result<PolicyFile, Vec<String>> {
    parse::parse_file(path, text)
}

/// Merge parsed files into one policy: order by priority, desugar, union,
/// check.
pub fn compose(files: Vec<PolicyFile>) -> Policy {
    compose::compose(files)
}

/// Find, parse and compose every policy file that applies to a loop in `cwd`.
///
/// A file that cannot be read or parsed lands in [`Policy::errors`] and the
/// others still load.
pub fn load(cwd: &Path) -> Policy {
    let mut parsed = Vec::new();
    let mut errors = Vec::new();
    for path in policy_paths(cwd) {
        match std::fs::read_to_string(&path) {
            Ok(text) => match parse_file(&path, &text) {
                Ok(file) => parsed.push(file),
                Err(messages) => errors.extend(messages.into_iter().map(|m| (path.clone(), m))),
            },
            Err(error) => errors.push((path.clone(), format!("cannot read it: {error}"))),
        }
    }
    let mut policy = compose(parsed);
    // Parse errors come first: a file that did not load explains a rule that
    // is missing below.
    errors.append(&mut policy.errors);
    policy.errors = errors;
    policy
}

// ---------------------------------------------------------------------------
// The manifest
// ---------------------------------------------------------------------------

/// What a client is told at attach: the tools after disabling and wrapping,
/// the commands, and the keys `ui.status` and `ui.widget` may carry.
///
/// `builtin_tools` is the server's own list; a built-in the policy disables
/// is dropped, one it wraps keeps its declaration, and one it redefines with
/// `run` or `loop` keeps the built-in's description and schema unless the
/// policy gave its own.
pub fn manifest(policy: &Policy, builtin_tools: &[ToolInfo]) -> Manifest {
    let mut tools = Vec::new();
    for builtin in builtin_tools {
        match policy.tools.iter().find(|tool| tool.name == builtin.name) {
            Some(tool) if tool.disabled => continue,
            Some(tool) => {
                let mut info = builtin.clone();
                if !tool.description.is_empty() {
                    info.description = tool.description.clone();
                }
                if tool.declares_params() {
                    info.parameters = tool.parameters.clone();
                }
                tools.push(info);
            }
            None => tools.push(builtin.clone()),
        }
    }
    for tool in &policy.tools {
        if tool.disabled || builtin_tools.iter().any(|builtin| builtin.name == tool.name) {
            continue;
        }
        tools.push(ToolInfo {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.parameters.clone(),
        });
    }
    Manifest {
        tools,
        commands: policy.commands.clone(),
        status_keys: policy.status_keys.clone(),
        widget_keys: policy.widget_keys.clone(),
    }
}

/// The files and conflicts as `dsl.check` reports them. Errors travel as
/// conflicts naming one file, because the result type has no other place for
/// them.
pub fn check_files(policy: &Policy) -> Vec<ServerPath> {
    policy.files.iter().map(|path| ServerPath::from(path.to_string_lossy().into_owned())).collect()
}

/// Every conflict plus every per-file error, as the protocol's conflict list.
pub fn check_conflicts(policy: &Policy) -> Vec<DslConflict> {
    let mut conflicts: Vec<DslConflict> = policy
        .errors
        .iter()
        .map(|(path, message)| DslConflict {
            message: message.clone(),
            files: vec![ServerPath::from(path.to_string_lossy().into_owned())],
        })
        .collect();
    conflicts.extend(policy.conflicts.iter().cloned());
    conflicts
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// The merged policy, for a person: the files in load order, every slot's
/// entries with the file they came from, then the conflicts and errors.
///
/// This is what `pirs check` prints above the assembled system prompt (D-21).
pub fn render(policy: &Policy) -> String {
    let mut out = String::new();
    if policy.files.is_empty() {
        out.push_str("files: none\n");
    } else {
        out.push_str("files:\n");
        for path in &policy.files {
            let intent = policy
                .intents
                .iter()
                .find(|(file, _)| file == path)
                .map(|(_, intent)| intent.as_str())
                .unwrap_or_default();
            out.push_str(&format!("  {}\n", path.display()));
            for line in intent.lines().map(str::trim).filter(|line| !line.is_empty()) {
                out.push_str(&format!("    {line}\n"));
            }
        }
    }

    if policy.settings != SettingsTable::default() {
        out.push_str("settings:\n");
        let source = |key: &str| {
            policy
                .settings_sources
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, path)| format!("  ({})", path.display()))
                .unwrap_or_default()
        };
        if let Some(model) = &policy.settings.model {
            out.push_str(&format!("  model = {model}{}\n", source("model")));
        }
        if let Some(thinking) = &policy.settings.thinking {
            out.push_str(&format!("  thinking = {thinking}{}\n", source("thinking")));
        }
        if let Some(tools) = &policy.settings.tools {
            out.push_str(&format!("  tools = [{}]{}\n", tools.join(", "), source("tools")));
        }
        if let Some(mode) = policy.settings.tool_execution {
            out.push_str(&format!("  tool_execution = {}{}\n", mode.as_str(), source("tool_execution")));
        }
    }

    if !policy.input.is_empty() {
        out.push_str("input:\n");
        for entry in &policy.input {
            let mut what = Vec::new();
            if let Some(replace) = &entry.replace {
                what.push(format!("replace {replace:?}"));
            }
            if let Some(run) = &entry.run {
                what.push(format!("run {run:?}"));
            }
            if entry.handled {
                what.push("handled".to_owned());
            }
            out.push_str(&format!("  {} -> {}  ({})\n", entry.pattern, what.join(", "), entry.origin));
        }
    }

    if !policy.prompt.is_empty() {
        out.push_str("prompt:\n");
        for entry in &policy.prompt {
            let what = match &entry.source {
                PromptSource::Text(text) => format!("text {text:?}"),
                PromptSource::Files(glob) => format!("files {glob:?}"),
                PromptSource::Run(run) => format!("run {run:?}"),
            };
            let header = entry.header.as_ref().map(|h| format!(", header {h:?}")).unwrap_or_default();
            out.push_str(&format!("  {what}{header}  ({})\n", entry.origin));
        }
    }

    if !policy.tool_result.is_empty() {
        out.push_str("tool_result:\n");
        for entry in &policy.tool_result {
            out.push_str(&format!("  {} -> run {:?}  ({})\n", entry.tool, entry.run, entry.origin));
        }
    }

    if !policy.on.is_empty() {
        out.push_str("on:\n");
        for entry in &policy.on {
            let what = match &entry.source {
                OnSource::Run(run) => format!("run {run:?}"),
                OnSource::File(path) => format!("file {:?}", path.display().to_string()),
            };
            let emit = match &entry.emit {
                Some(Emit::Status { key }) => format!(", status {key:?}"),
                Some(Emit::Widget { key }) => format!(", widget {key:?}"),
                None => String::new(),
            };
            let quiet = if entry.quiet { ", quiet" } else { "" };
            out.push_str(&format!("  {} {what}{emit}{quiet}  ({})\n", entry.event, entry.origin));
        }
    }

    if !policy.tools.is_empty() {
        out.push_str("tools:\n");
        for tool in &policy.tools {
            let source = match &tool.source {
                ToolSource::Builtin => "built-in".to_owned(),
                ToolSource::Run(run) => format!("run {run:?}"),
                ToolSource::Loop(spec) => format!("loop {} wait {}", spec.model, spec.wait),
            };
            let mut extra = Vec::new();
            if tool.disabled {
                extra.push("disabled".to_owned());
            }
            if let Some(wrap) = &tool.wrap {
                extra.push(format!("wrap {wrap:?}"));
            }
            if tool.declares_params() {
                let params: Vec<&str> = tool
                    .parameters
                    .get("properties")
                    .and_then(Value::as_object)
                    .map(|properties| properties.keys().map(String::as_str).collect())
                    .unwrap_or_default();
                extra.push(format!("params {}", params.join(", ")));
            }
            extra.push(format!("timeout {}s", tool.timeout.as_secs()));
            let origins: Vec<String> = tool.origins.iter().map(Origin::to_string).collect();
            out.push_str(&format!("  {}  {source}, {}  ({})\n", tool.name, extra.join(", "), origins.join("; ")));
        }
    }

    if !policy.commands.is_empty() {
        out.push_str("commands:\n");
        for command in &policy.commands {
            out.push_str(&format!("  /{}  {}\n", command.name, command.description));
        }
    }
    if !policy.status_keys.is_empty() {
        out.push_str(&format!("status keys: {}\n", policy.status_keys.join(", ")));
    }
    if !policy.widget_keys.is_empty() {
        out.push_str(&format!("widget keys: {}\n", policy.widget_keys.join(", ")));
    }

    for (path, message) in &policy.errors {
        out.push_str(&format!("error: {}: {message}\n", path.display()));
    }
    for conflict in &policy.conflicts {
        let files: Vec<&str> = conflict.files.iter().map(ServerPath::as_str).collect();
        out.push_str(&format!("conflict: {}  ({})\n", conflict.message, files.join(", ")));
    }
    out
}
