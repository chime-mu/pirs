//! The one place in the server that constructs a child process.
//!
//! `clippy.toml` disallows `std::process::Command::new` and
//! `tokio::process::Command::new` everywhere in this crate; this module carries
//! the module-level allow (D-09), so every process the server spawns — the
//! `bash` tool's shell, and the called policy processes — is created here and
//! nowhere else.
//!
//! # Called processes (D-23, D-24)
//!
//! A DSL `run =` is a *called* process: the server spawns it in the loop's cwd,
//! writes the slot payload as one compact JSON line on its stdin, closes stdin,
//! reads stdout to end, and kills it when the loop closes. [`call`] is that,
//! with a timeout; [`spawn_detached`] is the fire-and-forget form for `[[on]]`
//! entries and the `on start` process that becomes a connected client (D-16).
//!
//! The contract, as implemented:
//!
//! - **cwd** is [`CallEnv::cwd`].
//! - **stdin** is the payload as one compact JSON line plus `\n`, then closed.
//!   Writing is done from a background task, so a process that never reads
//!   stdin cannot block the call, and a broken pipe is not an error.
//! - **environment** is the server's, plus `PIRS_SOCKET`, `PIRS_LOOP`,
//!   `PIRS_SLOT`, `PIRS_SESSION_DIR`, and [`CallEnv::extra`] last (so it can
//!   override any of them). A **shell string** additionally gets one
//!   `PIRS_ARG_<field>` per top-level payload field (D-24); an **executable**
//!   does not — it reads the JSON line. A field whose name is not a shell
//!   identifier, or whose value is larger than [`MAX_ENV_VALUE`] (64 KiB), is
//!   not exported — it is still on stdin. The same cap applies to `$name`
//!   interpolation, which substitutes the empty string for a value that
//!   large and warns: a payload field over 64 KiB is available **on stdin
//!   only**, because the kernel refuses an `exec` whose environment or
//!   argument strings exceed 128 KiB (`E2BIG`, "Argument list too long").
//! - **stdout** is captured in full and returned with a single trailing
//!   newline stripped (`\n`, and the `\r` before it if any); nothing else is
//!   trimmed. It is the reply: one JSON line for an executable, plain text for
//!   a shell one-liner.
//! - **stderr** is captured in full, concurrently with stdout, so a chatty
//!   process cannot deadlock.
//! - **exit** `0` is [`CallOutput`]; anything else is [`CallError::NonZero`],
//!   whose `stderr` is the message the caller reports (D-24). A process killed
//!   by a signal reports `128 + signal`.
//! - **timeout** kills the whole process group and yields
//!   [`CallError::Timeout`]. The deadline covers reading the pipes as well as
//!   the exit, so a child that exits while a grandchild still holds stdout
//!   open times out rather than hanging.
//! - **cancellation** — dropping the [`call`] future, which is what an
//!   aborted tool call does — kills the whole process group too, not only
//!   the child `kill_on_drop` would take.
//!
//! # Which binding a `run` gets
//!
//! [`RunSpec::resolve`] is the one rule: a `run` value that is a single token
//! (no whitespace) naming an existing file with the executable bit set —
//! relative to the loop's cwd, or absolute — is an **executable**, spawned
//! directly with no arguments. Everything else is a **shell string**,
//! `sh -c <interpolated>`; see [`interpolate`] for the substitution rules and
//! why they are textual.

#![allow(clippy::disallowed_methods)]

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// An async command builder. The only way to build one inside this crate.
pub(crate) fn tokio_command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    tokio::process::Command::new(program)
}

/// A blocking command builder, for the paths that cannot await (process-group
/// teardown from a `Drop` impl, for instance).
pub(crate) fn std_command(program: impl AsRef<OsStr>) -> std::process::Command {
    std::process::Command::new(program)
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// What a called process is told about the loop that called it.
#[derive(Debug, Clone)]
pub(crate) struct CallEnv {
    /// The loop's working directory; the process's cwd.
    pub(crate) cwd: PathBuf,
    /// `PIRS_SOCKET`: the server's socket, so the process can connect as a
    /// client if it wants to.
    pub(crate) socket: PathBuf,
    /// `PIRS_LOOP`: the loop's id.
    pub(crate) loop_id: String,
    /// `PIRS_SLOT`: the slot that fired, in its string form (`input`,
    /// `tool.fetch`, `on.turn_end`, …).
    pub(crate) slot: String,
    /// `PIRS_SESSION_DIR`: the loop's session directory, where by-reference
    /// payloads live.
    pub(crate) session_dir: PathBuf,
    /// Extra variables, applied after the four above and after `PIRS_ARG_*`.
    pub(crate) extra: Vec<(String, String)>,
}

impl CallEnv {
    /// The four `PIRS_*` variables with empty paths and no extras; for tests
    /// and for the paths that have no socket yet.
    pub(crate) fn new(cwd: PathBuf, slot: impl Into<String>) -> Self {
        Self {
            cwd,
            socket: PathBuf::new(),
            loop_id: String::new(),
            slot: slot.into(),
            session_dir: PathBuf::new(),
            extra: Vec::new(),
        }
    }
}

/// How a `run =` is executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunSpec {
    /// A shell string: `sh -c <interpolate(s, vars)>`. Gets `PIRS_ARG_*` and
    /// `$name` interpolation on top of the stdin payload (D-24).
    Shell(String),
    /// An executable and its arguments: no shell, no interpolation, no
    /// `PIRS_ARG_*`. It still gets the JSON line on stdin and the four
    /// `PIRS_*` variables, so the same handler works under either binding
    /// (D-23).
    Exec {
        /// The program to run.
        program: PathBuf,
        /// Its arguments, passed verbatim.
        args: Vec<String>,
    },
}

impl RunSpec {
    /// The binding a `run =` value gets (the one rule, shared with the docs):
    /// a single token — no whitespace — that, resolved against `cwd` when
    /// relative or taken as given when absolute, names an existing file with
    /// the executable bit set is [`RunSpec::Exec`] with no arguments; anything
    /// else is [`RunSpec::Shell`].
    ///
    /// The executable is stored as the resolved path, so a bare `fetch.py`
    /// in the cwd runs that file and is never looked up on `PATH`.
    pub(crate) fn resolve(run: &str, cwd: &Path) -> RunSpec {
        let token = run.trim();
        if token.is_empty() || token != run || token.chars().any(char::is_whitespace) {
            return RunSpec::Shell(run.to_owned());
        }
        let path = Path::new(token);
        let candidate = if path.is_absolute() { path.to_path_buf() } else { cwd.join(path) };
        if is_executable_file(&candidate) {
            RunSpec::Exec { program: candidate, args: Vec::new() }
        } else {
            RunSpec::Shell(run.to_owned())
        }
    }

    /// Whether this is the executable binding.
    pub(crate) fn is_exec(&self) -> bool {
        matches!(self, RunSpec::Exec { .. })
    }
}

/// An existing regular file with any executable bit set.
fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// What a called process produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallOutput {
    /// Everything on stdout, with one trailing newline stripped. The reply.
    pub(crate) stdout: String,
    /// Everything on stderr, verbatim.
    pub(crate) stderr: String,
    /// The exit status, always `0` here.
    pub(crate) status: i32,
}

/// Why a called process produced no reply.
#[derive(Debug)]
pub(crate) enum CallError {
    /// The deadline passed; the process group was killed.
    Timeout(Duration),
    /// The process exited non-zero. `stderr` is the message to report (D-24).
    NonZero {
        /// The exit status, or `128 + signal` if it was killed.
        status: i32,
        /// Everything on stderr.
        stderr: String,
        /// Everything on stdout, in case the caller wants it anyway.
        stdout: String,
    },
    /// The process could not be started (no such file, not executable, …).
    Spawn(std::io::Error),
    /// The process started but its pipes failed.
    Io(std::io::Error),
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallError::Timeout(d) => write!(f, "timed out after {:.1}s", d.as_secs_f64()),
            CallError::NonZero { status, stderr, .. } => {
                let msg = stderr.trim();
                if msg.is_empty() {
                    write!(f, "exited with status {status}")
                } else {
                    write!(f, "{msg}")
                }
            }
            CallError::Spawn(e) => write!(f, "could not start: {e}"),
            CallError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for CallError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CallError::Spawn(e) | CallError::Io(e) => Some(e),
            _ => None,
        }
    }
}

/// A process started and not waited for: an `[[on]]` entry, or the `on start`
/// handler that goes on to open the socket and become a connected client
/// (D-16). The payload has already been written to its stdin and stdin closed.
///
/// Its stdout and stderr are **discarded** (`/dev/null`), so nothing it prints
/// can fill a pipe and block it; a caller that needs the output wants [`call`]
/// with a generous timeout instead.
///
/// The child is handed back so the loop can track it and kill it on close
/// (D-23). Killing must go through [`Spawned::kill`], which kills the whole
/// process group — `Child::start_kill` alone would leave the grandchildren of
/// `run = "watch.sh & tail -f log"` running.
#[derive(Debug)]
pub(crate) struct Spawned {
    /// The child. `kill_on_drop` is set, as a backstop.
    pub(crate) child: tokio::process::Child,
    /// Its pid, and the id of its process group (it is the group leader).
    /// `None` only if it has already been reaped.
    pub(crate) pid: Option<u32>,
}

impl Spawned {
    /// Kill the whole process group and reap the child.
    pub(crate) async fn kill(&mut self) {
        if let Some(pid) = self.pid {
            kill_group(pid).await;
        }
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }
}

// ---------------------------------------------------------------------------
// Interpolation (D-24)
// ---------------------------------------------------------------------------

/// Substitute `$name` and `${name}` in a shell `run` string.
///
/// The substitution is **raw and textual**, before the shell sees the string:
/// `run = "$1"` with `1 = "ls -l"` executes `ls -l`, which is what
/// `[[input]] match = '^!(.*)'` wants. That also means an interpolated value is
/// not quoted for you — for a value that must arrive as one word, use the
/// environment instead: `"$PIRS_ARG_url"`.
///
/// The rules:
///
/// - `$name` (an identifier: a letter or `_` then letters, digits, `_`) and
///   `${name}` are replaced when `name` is a key of `vars`.
/// - A value larger than `max` (the caller's cap: [`MAX_ENV_VALUE`] for a
///   command line, none for text that never becomes one) is substituted as
///   the **empty string**, with one warning: the kernel refuses an `exec` whose argument
///   is over 128 KiB, so pasting it would fail the whole call, and leaving
///   `$name` in place would hand the shell a variable it does not have. A
///   payload field that big is available on stdin only.
/// - `$1`…`$9` take exactly one digit, as a shell does; `${10}` is how a
///   longer numeric name is written.
/// - An unknown name is **left untouched**, so `$HOME`, `$PATH` and any other
///   ordinary shell variable still reach the shell and still work.
/// - `$$` is left as `$$`, so the shell still expands it to its own pid.
/// - A `$` before anything else (a space, the end of the string) is literal.
fn substitute(out: &mut String, name: &str, value: &str, max: Option<usize>) {
    if max.is_some_and(|max| value.len() > max) {
        tracing::warn!(
            variable = %name,
            bytes = value.len(),
            max = max.unwrap_or_default(),
            "value too large to interpolate into a command line; substituting the empty string (it is on stdin in full)"
        );
        return;
    }
    out.push_str(value);
}

pub(crate) fn interpolate(template: &str, vars: &BTreeMap<String, String>) -> String {
    interpolate_with(template, vars, Some(MAX_ENV_VALUE))
}

/// The same substitution for text that never becomes a command line — a
/// `[[tool]] loop`'s prompt — so a value larger than [`MAX_ENV_VALUE`] is
/// pasted in full instead of being dropped: nothing is `exec`ed with it.
pub(crate) fn interpolate_text(template: &str, vars: &BTreeMap<String, String>) -> String {
    interpolate_with(template, vars, None)
}

fn interpolate_with(template: &str, vars: &BTreeMap<String, String>, max: Option<usize>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(pos) = rest.find('$') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 1..];
        let mut chars = after.chars();
        match chars.next() {
            // `$$` stays `$$`: the shell's own pid.
            Some('$') => {
                out.push_str("$$");
                rest = &after[1..];
            }
            // `${name}`.
            Some('{') => match after.find('}') {
                Some(end) => {
                    let name = &after[1..end];
                    match vars.get(name) {
                        Some(value) => substitute(&mut out, name, value, max),
                        None => {
                            out.push('$');
                            out.push_str(&after[..=end]);
                        }
                    }
                    rest = &after[end + 1..];
                }
                None => {
                    out.push('$');
                    rest = after;
                }
            },
            // `$1`: one digit, as in a shell.
            Some(d) if d.is_ascii_digit() => {
                let name = &after[..1];
                match vars.get(name) {
                    Some(value) => substitute(&mut out, name, value, max),
                    None => {
                        out.push('$');
                        out.push_str(name);
                    }
                }
                rest = &after[1..];
            }
            // `$name`.
            Some(c) if c.is_ascii_alphabetic() || c == '_' => {
                let end = after
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(after.len());
                let name = &after[..end];
                match vars.get(name) {
                    Some(value) => substitute(&mut out, name, value, max),
                    None => {
                        out.push('$');
                        out.push_str(name);
                    }
                }
                rest = &after[end..];
            }
            // A lone `$`.
            _ => {
                out.push('$');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A top-level payload field as an interpolation variable: strings, numbers
/// and booleans as text, everything else not a variable at all (an object is
/// not something `$name` can usefully paste into a command line).
fn var_of(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// A top-level payload field as `PIRS_ARG_<field>`: strings, numbers and
/// booleans as text, objects and arrays as compact JSON (so a shell one-liner
/// can pipe them to `jq` without parsing the stdin line), `null` as the empty
/// string.
pub(crate) fn env_value(value: &Value) -> String {
    env_of(value)
}

fn env_of(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// The largest value exported as `PIRS_ARG_<field>`. The kernel refuses an
/// `exec` with an argument or environment string over 128 KiB (`E2BIG`), so a
/// field larger than this is left off the environment rather than made to fail
/// the whole call; the payload on stdin still has it in full. It matches the
/// protocol's by-reference threshold, above which a payload does not carry
/// content at all.
pub(crate) const MAX_ENV_VALUE: usize = 64 * 1024;

/// Whether a payload field can name an environment variable a shell can read.
/// A field like `content-type` is skipped rather than mangled.
fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The interpolation variables a payload contributes on its own: every
/// top-level string, number or boolean field. The caller's own `vars` (regex
/// groups `0`…`9`, named groups, `args`) are laid over these, and win.
pub(crate) fn payload_vars(payload: &Value) -> BTreeMap<String, String> {
    let mut vars = BTreeMap::new();
    if let Some(map) = payload.as_object() {
        for (key, value) in map {
            if let Some(text) = var_of(value) {
                vars.insert(key.clone(), text);
            }
        }
    }
    vars
}

// ---------------------------------------------------------------------------
// Spawning
// ---------------------------------------------------------------------------

/// The payload as the one line written to stdin (no trailing newline here;
/// [`write_stdin`] adds it).
fn payload_line(payload: &Value) -> String {
    serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_owned())
}

/// Build the command for a spec, with cwd, environment and process group set.
/// `vars` is already the merged map.
fn build(spec: &RunSpec, payload: &Value, vars: &BTreeMap<String, String>, env: &CallEnv) -> tokio::process::Command {
    let mut cmd = match spec {
        RunSpec::Shell(s) => {
            let mut cmd = tokio_command("sh");
            cmd.arg("-c").arg(interpolate(s, vars));
            cmd
        }
        RunSpec::Exec { program, args } => {
            let mut cmd = tokio_command(program);
            cmd.args(args);
            cmd
        }
    };
    cmd.current_dir(&env.cwd);
    cmd.env("PIRS_SOCKET", &env.socket);
    cmd.env("PIRS_LOOP", &env.loop_id);
    cmd.env("PIRS_SLOT", &env.slot);
    cmd.env("PIRS_SESSION_DIR", &env.session_dir);
    // `PIRS_ARG_*` is the shell string's convenience (D-24); an executable
    // reads the JSON line and gets none of them.
    let exported = if spec.is_exec() { None } else { payload.as_object() };
    if let Some(map) = exported {
        for (key, value) in map {
            if !is_env_name(key) {
                continue;
            }
            let text = env_of(value);
            if text.len() > MAX_ENV_VALUE {
                tracing::debug!(field = %key, bytes = text.len(), "payload field too large for the environment; stdin only");
                continue;
            }
            cmd.env(format!("PIRS_ARG_{key}"), text);
        }
    }
    // The caller's own variables go through the same two gates: a name a
    // shell cannot read, or a value over `MAX_ENV_VALUE`, would either be
    // unusable or fail the `exec` with `E2BIG` (D-24).
    for (key, value) in &env.extra {
        if !is_env_name(key) {
            tracing::debug!(name = %key, "not a shell identifier; not exported");
            continue;
        }
        if value.len() > MAX_ENV_VALUE {
            tracing::debug!(name = %key, bytes = value.len(), "value too large for the environment; stdin only");
            continue;
        }
        cmd.env(key, value);
    }
    cmd.kill_on_drop(true);
    // Its own process group, so a timeout can kill everything it started.
    #[cfg(unix)]
    cmd.process_group(0);
    cmd
}

/// Spawn, retrying briefly on `ETXTBSY`: a script written a moment ago can
/// still be held open for writing by a child another thread forked while
/// the write was in flight (the descriptor closes at that child's `exec`).
/// The agent writing `tools/fetch.py` and calling it in the same turn is
/// exactly that moment.
async fn spawn_retrying(cmd: &mut tokio::process::Command) -> std::io::Result<tokio::process::Child> {
    let mut attempt = 0;
    loop {
        match cmd.spawn() {
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempt < 10 => {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            other => return other,
        }
    }
}

/// Write the payload line and close stdin, from a task of its own so a process
/// that never reads stdin cannot block the caller. A broken pipe is normal.
fn write_stdin(child: &mut tokio::process::Child, line: String) {
    let Some(mut stdin) = child.stdin.take() else {
        return;
    };
    tokio::spawn(async move {
        if let Err(e) = stdin.write_all(line.as_bytes()).await {
            tracing::debug!("called process did not read its payload: {e}");
            return;
        }
        let _ = stdin.shutdown().await;
    });
}

/// Kill a process group, falling back to the single pid. Uses `kill(1)`: the
/// server has no libc dependency and this is the same path the `bash` tool
/// takes.
pub(crate) async fn kill_group(pid: u32) {
    signal_group(pid, "KILL").await;
}

/// Ask a process group to stop, falling back to the single pid. `SIGTERM` is
/// the one a `trap` can catch, so a watcher started by `on start` gets to
/// clean up after itself; `SIGKILL` is what follows if it does not.
pub(crate) async fn signal_group(pid: u32, signal: &str) {
    let flag = format!("-{signal}");
    let group = format!("-{pid}");
    let signalled = tokio_command("kill")
        .args([flag.as_str(), "--", group.as_str()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    if !matches!(signalled, Ok(status) if status.success()) {
        let _ = tokio_command("kill")
            .args([flag.as_str(), "--", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
}

/// `128 + signal` for a process killed by one, the code otherwise, `-1` if
/// neither is known.
fn status_code(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    -1
}

/// Strip one trailing newline, and the `\r` before it if any. Nothing else is
/// trimmed: leading and interior whitespace is the reply's own.
fn strip_one_newline(mut s: String) -> String {
    if s.ends_with('\n') {
        s.pop();
        if s.ends_with('\r') {
            s.pop();
        }
    }
    s
}

/// Call a process and read its reply (D-23, D-24).
///
/// `vars` are the caller's interpolation variables — regex groups `0`…`9` and
/// any named groups, `args` for a `[[command]]` — laid over the ones the
/// payload contributes by itself ([`payload_vars`]), so the caller's win. They
/// matter only for [`RunSpec::Shell`].
///
/// The full contract is in the module documentation. In short: cwd is
/// `env.cwd`, the payload is one JSON line on stdin, the `PIRS_*` variables
/// are in the environment, stdout comes back with one trailing newline
/// stripped, a non-zero exit is [`CallError::NonZero`] with stderr as the
/// message, and `timeout` kills the process group.
pub(crate) async fn call(
    spec: &RunSpec,
    payload: &Value,
    vars: &BTreeMap<String, String>,
    env: &CallEnv,
    timeout: Duration,
) -> Result<CallOutput, CallError> {
    let mut merged = payload_vars(payload);
    merged.extend(vars.iter().map(|(k, v)| (k.clone(), v.clone())));

    let mut cmd = build(spec, payload, &merged, env);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = spawn_retrying(&mut cmd).await.map_err(CallError::Spawn)?;
    let pid = child.id();
    // Dropped without completing — the `select!` of an aborted tool call —
    // this kills the group; completing disarms it.
    let mut guard = GroupGuard { pid, armed: true };

    write_stdin(&mut child, format!("{}\n", payload_line(payload)));

    // Both pipes are drained by tasks of their own, started before anything is
    // awaited, so neither a chatty stderr nor a chatty stdout can deadlock.
    let out_task = child.stdout.take().map(|mut p| {
        tokio::spawn(async move {
            let mut buf = Vec::new();
            p.read_to_end(&mut buf).await.map(|_| buf)
        })
    });
    let err_task = child.stderr.take().map(|mut p| {
        tokio::spawn(async move {
            let mut buf = Vec::new();
            p.read_to_end(&mut buf).await.map(|_| buf)
        })
    });

    // One deadline over the exit and both reads: a child that exits while a
    // grandchild still holds a pipe open times out instead of hanging.
    let waited = tokio::time::timeout(timeout, async {
        let status = child.wait().await?;
        let stdout = match out_task {
            Some(t) => t.await.unwrap_or_else(|e| Err(std::io::Error::other(e)))?,
            None => Vec::new(),
        };
        let stderr = match err_task {
            Some(t) => t.await.unwrap_or_else(|e| Err(std::io::Error::other(e)))?,
            None => Vec::new(),
        };
        Ok::<_, std::io::Error>((status, stdout, stderr))
    })
    .await;
    guard.armed = false;

    let (status, stdout, stderr) = match waited {
        Ok(Ok(triple)) => triple,
        Ok(Err(e)) => {
            if let Some(pid) = pid {
                kill_group(pid).await;
            }
            let _ = child.wait().await;
            return Err(CallError::Io(e));
        }
        Err(_elapsed) => {
            if let Some(pid) = pid {
                kill_group(pid).await;
            }
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(CallError::Timeout(timeout));
        }
    };

    let stdout = String::from_utf8_lossy(&stdout).into_owned();
    let stderr = String::from_utf8_lossy(&stderr).into_owned();
    let code = status_code(status);
    if code != 0 {
        return Err(CallError::NonZero {
            status: code,
            stderr,
            stdout: strip_one_newline(stdout),
        });
    }
    Ok(CallOutput {
        stdout: strip_one_newline(stdout),
        stderr,
        status: code,
    })
}

/// Kills a called process's group when the [`call`] future is dropped before
/// it finished: `kill_on_drop` takes the child, this takes what the child
/// started. Synchronous, because `Drop` is; `kill(1)` is quick.
struct GroupGuard {
    pid: Option<u32>,
    armed: bool,
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some(pid) = self.pid {
            let _ = std_command("kill")
                .args(["-KILL", "--", &format!("-{pid}")])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

/// Start a process and do not wait for it: an `[[on]]` entry, or the `on
/// start` handler of a long-lived extension (D-16). The payload is written to
/// its stdin and stdin is closed; its stdout and stderr go to `/dev/null`.
///
/// The returned [`Spawned`] is the loop's to track and to kill on close
/// (D-23), through [`Spawned::kill`] so the whole process group goes.
pub(crate) async fn spawn_detached(
    spec: &RunSpec,
    payload: &Value,
    vars: &BTreeMap<String, String>,
    env: &CallEnv,
) -> Result<Spawned, CallError> {
    let mut merged = payload_vars(payload);
    merged.extend(vars.iter().map(|(k, v)| (k.clone(), v.clone())));

    let mut cmd = build(spec, payload, &merged, env);
    cmd.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = spawn_retrying(&mut cmd).await.map_err(CallError::Spawn)?;
    let pid = child.id();
    write_stdin(&mut child, format!("{}\n", payload_line(payload)));
    Ok(Spawned { child, pid })
}

/// A [`Spawned`] the loop keeps: its exit is watched (so a non-zero one can
/// be reported) and [`Detached::kill`] takes down its whole process group on
/// `loop.close` (D-23).
///
/// The watcher owns the child while it waits, so the kill goes to the group
/// first — that is what ends the wait — and only then takes the child back to
/// reap it through [`Spawned::kill`].
pub(crate) struct Detached {
    pid: Option<u32>,
    spawned: Arc<tokio::sync::Mutex<Spawned>>,
    killed: Arc<AtomicBool>,
    exited: Arc<AtomicBool>,
}

/// Watch a detached process and call `on_exit` with its status when it ends
/// on its own with a non-zero one.
pub(crate) fn supervise(spawned: Spawned, on_exit: impl FnOnce(i32) + Send + 'static) -> Detached {
    let pid = spawned.pid;
    let spawned = Arc::new(tokio::sync::Mutex::new(spawned));
    let killed = Arc::new(AtomicBool::new(false));
    let exited = Arc::new(AtomicBool::new(false));
    let watched = spawned.clone();
    let was_killed = killed.clone();
    let has_exited = exited.clone();
    tokio::spawn(async move {
        let status = {
            let mut guard = watched.lock().await;
            guard.child.wait().await
        };
        has_exited.store(true, Ordering::SeqCst);
        if was_killed.load(Ordering::SeqCst) {
            return;
        }
        let code = match status {
            Ok(status) => status_code(status),
            Err(e) => {
                tracing::debug!("could not wait for a detached process: {e}");
                return;
            }
        };
        if code != 0 {
            on_exit(code);
        }
    });
    Detached { pid, spawned, killed, exited }
}

/// How long a detached process is given to finish on its own when the loop
/// closes. An `[[on]] event = "turn_end" run = "git commit …"` that has just
/// been started should be allowed to finish; a watcher started by `on start`
/// will not, and is asked to stop.
///
/// The same grace is given twice: once for the process to finish by itself,
/// and once more, after `SIGTERM`, for it to shut down — see [`wind_down`].
pub(crate) const CLOSE_GRACE: Duration = Duration::from_secs(1);

/// End the loop's detached processes, politely first (D-23).
///
/// The caller has already given them [`CLOSE_GRACE`] to finish on their own.
/// Whatever is still running has its process group sent `SIGTERM`, so a shell
/// `trap` or a watcher's own shutdown runs; after a further [`CLOSE_GRACE`]
/// anything left gets `SIGKILL`. Every child is reaped either way.
pub(crate) async fn wind_down(children: Vec<Detached>) {
    let running: Vec<&Detached> = children.iter().filter(|child| !child.has_exited()).collect();
    for child in &running {
        child.terminate().await;
    }
    if !running.is_empty() {
        let deadline = tokio::time::Instant::now() + CLOSE_GRACE;
        while running.iter().any(|child| !child.has_exited()) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    for child in children {
        child.kill().await;
    }
}

impl Detached {
    /// Whether the process has already exited on its own.
    pub(crate) fn has_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    /// Ask the process group to stop: `SIGTERM`, which a `trap` can catch.
    /// Its exit is no longer reported as a failure — this is a shutdown, not
    /// a crash.
    pub(crate) async fn terminate(&self) {
        self.killed.store(true, Ordering::SeqCst);
        if let Some(pid) = self.pid {
            signal_group(pid, "TERM").await;
        }
    }

    /// Kill the process group and reap the child.
    pub(crate) async fn kill(&self) {
        self.killed.store(true, Ordering::SeqCst);
        if let Some(pid) = self.pid {
            kill_group(pid).await;
        }
        // The watcher holds the child until the group dies; two seconds is
        // long after a SIGKILL, and `kill_on_drop` is the backstop.
        if let Ok(mut spawned) = tokio::time::timeout(Duration::from_secs(2), self.spawned.lock()).await {
            spawned.kill().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    fn no_vars() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    fn shell(s: &str) -> RunSpec {
        RunSpec::Shell(s.to_owned())
    }

    #[test]
    fn builders_carry_the_program_name() {
        assert_eq!(std_command("true").get_program(), OsStr::new("true"));
        assert_eq!(tokio_command("true").as_std().get_program(), OsStr::new("true"));
    }

    // ----- interpolation --------------------------------------------------

    #[test]
    fn interpolates_groups_and_names() {
        let v = vars(&[("1", "ls -l"), ("2", "b"), ("url", "https://x"), ("args", "a b")]);
        assert_eq!(interpolate("$1", &v), "ls -l");
        assert_eq!(interpolate("${1}", &v), "ls -l");
        assert_eq!(interpolate("fetch $url now", &v), "fetch https://x now");
        assert_eq!(interpolate("./handoff.sh $args", &v), "./handoff.sh a b");
        assert_eq!(interpolate("$1$2", &v), "ls -lb");
        assert_eq!(interpolate("${url}/p", &v), "https://x/p");
    }

    #[test]
    fn text_interpolation_pastes_a_value_a_command_line_could_not_take() {
        // A diff handed to a `[[tool]] loop` prompt is not `exec`ed, so the
        // command-line cap does not apply to it.
        let big = "d".repeat(MAX_ENV_VALUE + 1);
        let v = vars(&[("diff", big.as_str())]);
        assert_eq!(interpolate("review $diff", &v), "review ");
        assert_eq!(interpolate_text("review $diff", &v), format!("review {big}"));
        assert_eq!(interpolate_text("cd $HOME", &v), "cd $HOME", "an unknown name is still left alone");
    }

    #[test]
    fn leaves_unknown_and_shell_forms_alone() {
        let v = vars(&[("url", "https://x")]);
        // Ordinary shell variables keep working.
        assert_eq!(interpolate("cd $HOME && pwd", &v), "cd $HOME && pwd");
        assert_eq!(interpolate("${HOME}", &v), "${HOME}");
        assert_eq!(interpolate("$9", &v), "$9");
        // `$$` is the shell's pid, not a variable.
        assert_eq!(interpolate("echo $$ > f", &v), "echo $$ > f");
        assert_eq!(interpolate("$$url", &v), "$$url");
        // A lone `$` and an unterminated `${`.
        assert_eq!(interpolate("100$ and ${url", &v), "100$ and ${url");
        assert_eq!(interpolate("", &v), "");
        assert_eq!(interpolate("no dollars", &v), "no dollars");
    }

    #[test]
    fn numbers_and_bools_render_as_text() {
        let payload = json!({"n": 42, "f": 1.5, "b": true, "s": "x", "obj": {"a": 1}, "arr": [1], "nil": null});
        let v = payload_vars(&payload);
        assert_eq!(interpolate("$n $f $b $s", &v), "42 1.5 true x");
        // Objects, arrays and null are not interpolation variables.
        assert_eq!(interpolate("$obj $arr $nil", &v), "$obj $arr $nil");
    }

    #[test]
    fn caller_vars_win_over_payload_fields() {
        let payload = json!({"text": "from payload"});
        let mut v = payload_vars(&payload);
        assert_eq!(interpolate("$text", &v), "from payload");
        v.extend(vars(&[("text", "from caller")]));
        assert_eq!(interpolate("$text", &v), "from caller");
    }

    // ----- resolve --------------------------------------------------------

    #[test]
    fn a_single_token_naming_an_executable_file_is_exec_and_everything_else_is_shell() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("tools")).unwrap();
        let script = dir.path().join("tools/fetch.py");
        std::fs::write(&script, "#!/bin/sh\ncat\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let plain = dir.path().join("notes.txt");
        std::fs::write(&plain, "x").unwrap();

        let exec = |program: PathBuf| RunSpec::Exec { program, args: vec![] };
        // Relative to the cwd, and absolute.
        assert_eq!(RunSpec::resolve("./tools/fetch.py", dir.path()), exec(dir.path().join("./tools/fetch.py")));
        assert_eq!(RunSpec::resolve("tools/fetch.py", dir.path()), exec(dir.path().join("tools/fetch.py")));
        assert_eq!(RunSpec::resolve(script.to_str().unwrap(), Path::new("/nowhere")), exec(script.clone()));
        // Whitespace makes it a shell string even when the first word is the executable.
        assert_eq!(RunSpec::resolve("./tools/fetch.py --all", dir.path()), shell("./tools/fetch.py --all"));
        assert_eq!(RunSpec::resolve(" ./tools/fetch.py", dir.path()), shell(" ./tools/fetch.py"));
        // A file without the bit, a missing file, a command on PATH, a pipeline.
        assert_eq!(RunSpec::resolve("notes.txt", dir.path()), shell("notes.txt"));
        assert_eq!(RunSpec::resolve("./tools/missing.py", dir.path()), shell("./tools/missing.py"));
        assert_eq!(RunSpec::resolve("cat", dir.path()), shell("cat"));
        assert_eq!(RunSpec::resolve("git log --oneline -5", dir.path()), shell("git log --oneline -5"));
        assert_eq!(RunSpec::resolve("", dir.path()), shell(""));
        // A directory is not an executable file.
        assert_eq!(RunSpec::resolve("tools", dir.path()), shell("tools"));
    }

    #[tokio::test]
    async fn an_executable_gets_no_pirs_arg_variables() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("env.sh");
        std::fs::write(&script, "#!/bin/sh\nprintenv PIRS_ARG_url || echo unset; printenv PIRS_SLOT\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let env = CallEnv::new(dir.path().to_path_buf(), "tool.fetch");
        let spec = RunSpec::resolve("./env.sh", dir.path());
        assert!(spec.is_exec());
        let out = call(&spec, &json!({"url": "https://x"}), &no_vars(), &env, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(out.stdout, "unset\ntool.fetch", "no PIRS_ARG_*, the PIRS_* four still there");
    }

    #[tokio::test]
    async fn dropping_a_call_kills_the_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pids");
        let env = CallEnv::new(dir.path().to_path_buf(), "tool.slow");
        let run = format!("echo $$ >> {p}; sleep 300 & echo $! >> {p}; sleep 300", p = pidfile.display());
        let payload = json!({});
        let vars = no_vars();
        let spec = shell(&run);
        // Boxed, so `drop(fut)` drops the future itself (a `pin!` would leave
        // it alive in its hidden local until the end of the scope).
        let mut fut = Box::pin(call(&spec, &payload, &vars, &env, Duration::from_secs(60)));
        // Poll it until the script has written both pids, then drop it: that
        // is what an aborted tool call does to the future.
        let pids = loop {
            tokio::select! {
                _ = fut.as_mut() => panic!("the call finished by itself"),
                () = tokio::time::sleep(Duration::from_millis(20)) => {}
            }
            let text = std::fs::read_to_string(&pidfile).unwrap_or_default();
            let pids: Vec<u32> = text.lines().filter_map(|l| l.trim().parse().ok()).collect();
            if pids.len() == 2 {
                break pids;
            }
        };
        assert!(alive(pids[1]).await, "the backgrounded grandchild is running");
        drop(fut);
        for (which, pid) in ["leader", "grandchild"].iter().zip(pids) {
            assert!(!alive(pid).await, "{which} pid {pid} survived the drop");
        }
    }

    // ----- call -----------------------------------------------------------

    #[tokio::test]
    async fn cat_echoes_the_payload_line() {
        let payload = json!({"text": "/review src"});
        let env = CallEnv::new(std::env::temp_dir(), "input");
        let out = call(&shell("cat"), &payload, &no_vars(), &env, Duration::from_secs(5))
            .await
            .unwrap();
        // The line on stdin, with the trailing newline stripped from stdout.
        assert_eq!(out.stdout, r#"{"text":"/review src"}"#);
        assert_eq!(out.status, 0);
        assert!(out.stderr.is_empty());
        // And it round-trips as the payload.
        let back: Value = serde_json::from_str(&out.stdout).unwrap();
        assert_eq!(back, payload);
    }

    #[tokio::test]
    async fn an_executable_reads_the_same_line() {
        let payload = json!({"args": {"url": "https://x"}, "id": "call_1"});
        let env = CallEnv::new(std::env::temp_dir(), "tool.fetch");
        let spec = RunSpec::Exec {
            program: PathBuf::from("cat"),
            args: vec![],
        };
        let out = call(&spec, &payload, &no_vars(), &env, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(serde_json::from_str::<Value>(&out.stdout).unwrap(), payload);
    }

    #[tokio::test]
    async fn exec_does_not_interpolate() {
        // `$1` is an argument, not a substitution: no shell is involved.
        let env = CallEnv::new(std::env::temp_dir(), "input");
        let spec = RunSpec::Exec {
            program: PathBuf::from("printf"),
            args: vec!["%s".to_owned(), "$1 $HOME".to_owned()],
        };
        let out = call(&spec, &json!({}), &vars(&[("1", "x")]), &env, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(out.stdout, "$1 $HOME");
    }

    #[tokio::test]
    async fn the_environment_is_visible() {
        let dir = tempfile::tempdir().unwrap();
        let env = CallEnv {
            cwd: dir.path().to_path_buf(),
            socket: PathBuf::from("/run/pirs.sock"),
            loop_id: "a7f3".to_owned(),
            slot: "tool.fetch".to_owned(),
            session_dir: PathBuf::from("/home/me/.pirs/sessions/x"),
            extra: vec![("PIRS_EXTRA".to_owned(), "yes".to_owned())],
        };
        let payload = json!({"url": "https://x", "n": 7, "flag": false, "opts": {"deep": true}, "bad-name": "skipped"});
        let run = "printenv PIRS_SOCKET PIRS_LOOP PIRS_SLOT PIRS_SESSION_DIR PIRS_ARG_url PIRS_ARG_n PIRS_ARG_flag PIRS_ARG_opts PIRS_EXTRA";
        let out = call(&shell(run), &payload, &no_vars(), &env, Duration::from_secs(5))
            .await
            .unwrap();
        let lines: Vec<&str> = out.stdout.lines().collect();
        assert_eq!(
            lines,
            vec![
                "/run/pirs.sock",
                "a7f3",
                "tool.fetch",
                "/home/me/.pirs/sessions/x",
                "https://x",
                "7",
                "false",
                r#"{"deep":true}"#,
                "yes",
            ]
        );
        // A field that cannot name a shell variable is skipped, not mangled.
        let out = call(&shell("printenv PIRS_ARG_bad_name || echo unset"), &payload, &no_vars(), &env, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(out.stdout, "unset");
    }

    #[tokio::test]
    async fn a_quoted_env_use_survives_spaces() {
        // The documented way to pass a value as one word.
        let env = CallEnv::new(std::env::temp_dir(), "tool.fetch");
        let payload = json!({"url": "a b c"});
        let out = call(&shell(r#"printf '[%s]' "$PIRS_ARG_url""#), &payload, &no_vars(), &env, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(out.stdout, "[a b c]");
    }

    #[tokio::test]
    async fn interpolation_is_raw() {
        // `run = "$1"` executes the user's command (D-24, `[[input]] match = '^!(.*)'`).
        let env = CallEnv::new(std::env::temp_dir(), "input");
        let out = call(
            &shell("$1"),
            &json!({"text": "!echo hi"}),
            &vars(&[("1", "echo hi")]),
            &env,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(out.stdout, "hi");
    }

    #[tokio::test]
    async fn cwd_is_the_loops() {
        let dir = tempfile::tempdir().unwrap();
        let want = dir.path().canonicalize().unwrap();
        let env = CallEnv::new(want.clone(), "on.start");
        let out = call(&shell("pwd"), &json!({}), &no_vars(), &env, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(PathBuf::from(&out.stdout), want);
    }

    #[tokio::test]
    async fn only_one_trailing_newline_is_stripped() {
        let env = CallEnv::new(std::env::temp_dir(), "prompt");
        let out = call(&shell(r"printf 'a\n\n'"), &json!({}), &no_vars(), &env, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(out.stdout, "a\n");
        let out = call(&shell(r"printf ' a '"), &json!({}), &no_vars(), &env, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(out.stdout, " a ");
    }

    #[tokio::test]
    async fn non_zero_exit_reports_stderr() {
        let env = CallEnv::new(std::env::temp_dir(), "tool.fetch");
        let run = "echo partial; echo 'connection refused' >&2; exit 3";
        let err = call(&shell(run), &json!({}), &no_vars(), &env, Duration::from_secs(5))
            .await
            .unwrap_err();
        match err {
            CallError::NonZero { status, stderr, stdout } => {
                assert_eq!(status, 3);
                assert_eq!(stderr, "connection refused\n");
                assert_eq!(stdout, "partial");
            }
            other => panic!("expected NonZero, got {other:?}"),
        }
        // The Display form is the message a caller reports.
        let err = call(&shell("echo boom >&2; exit 1"), &json!({}), &no_vars(), &env, Duration::from_secs(5))
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "boom");
    }

    #[tokio::test]
    async fn a_missing_program_is_a_spawn_error() {
        let env = CallEnv::new(std::env::temp_dir(), "input");
        let spec = RunSpec::Exec {
            program: PathBuf::from("./no-such-handler-xyz"),
            args: vec![],
        };
        let err = call(&spec, &json!({}), &no_vars(), &env, Duration::from_secs(5))
            .await
            .unwrap_err();
        assert!(matches!(err, CallError::Spawn(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn timeout_kills_the_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let env = CallEnv::new(dir.path().to_path_buf(), "on.turn_end");
        // `$$` reaches the shell: the group leader's pid.
        let run = format!("echo $$ > {}; sleep 5", pidfile.display());
        let started = std::time::Instant::now();
        let err = call(&shell(&run), &json!({}), &no_vars(), &env, Duration::from_millis(300))
            .await
            .unwrap_err();
        let waited = started.elapsed();
        assert!(matches!(err, CallError::Timeout(d) if d == Duration::from_millis(300)), "got {err:?}");
        assert!(waited < Duration::from_millis(500), "returned after {waited:?}");
        let pid: u32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
        assert!(!alive(pid).await, "pid {pid} survived the timeout");
    }

    #[tokio::test]
    async fn a_chatty_stderr_does_not_deadlock() {
        let env = CallEnv::new(std::env::temp_dir(), "on.turn_end");
        let run = "yes | head -c 200000 >&2; echo done";
        let out = call(&shell(run), &json!({}), &no_vars(), &env, Duration::from_secs(20))
            .await
            .unwrap();
        assert_eq!(out.stdout, "done");
        assert_eq!(out.stderr.len(), 200_000);
    }

    #[tokio::test]
    async fn a_process_that_ignores_stdin_still_works() {
        // A big payload and a program that never reads it: the write task
        // takes the broken pipe, the call does not.
        let env = CallEnv::new(std::env::temp_dir(), "prompt");
        let big = "x".repeat(300_000);
        let out = call(&shell("echo ok"), &json!({"text": big}), &no_vars(), &env, Duration::from_secs(10))
            .await
            .unwrap();
        assert_eq!(out.stdout, "ok");
    }

    #[tokio::test]
    async fn an_oversized_field_stays_off_the_environment() {
        // Over MAX_ENV_VALUE the exec would fail with E2BIG; the field is
        // dropped from the environment and read from stdin instead.
        let env = CallEnv::new(std::env::temp_dir(), "prompt");
        let payload = json!({"small": "s", "big": "x".repeat(MAX_ENV_VALUE + 1)});
        let run = "printenv PIRS_ARG_small; printenv PIRS_ARG_big || echo unset; wc -c";
        let out = call(&shell(run), &payload, &no_vars(), &env, Duration::from_secs(10))
            .await
            .unwrap();
        let lines: Vec<&str> = out.stdout.lines().collect();
        assert_eq!(lines[0], "s");
        assert_eq!(lines[1], "unset");
        let on_stdin: usize = lines[2].trim().parse().unwrap();
        assert!(on_stdin > MAX_ENV_VALUE, "the payload still carried it: {on_stdin} bytes");
    }

    #[tokio::test]
    async fn an_oversized_caller_variable_is_dropped_and_interpolates_as_nothing() {
        // A 200 KB tool argument: exported as `PIRS_ARG_text` and pasted into
        // the command line it would be `E2BIG` and the spawn would fail with
        // "Argument list too long". Both gates drop it; stdin still has it.
        let mut env = CallEnv::new(std::env::temp_dir(), "tool.write");
        let big = "x".repeat(200_000);
        env.extra.push(("PIRS_ARG_text".to_owned(), big.clone()));
        env.extra.push(("PIRS_ARG_path".to_owned(), "notes.md".to_owned()));
        env.extra.push(("PIRS_ARG_content-type".to_owned(), "text/plain".to_owned()));
        let payload = json!({"args": {"path": "notes.md", "text": big}, "id": "t1"});
        let vars = vars(&[("text", big.as_str()), ("path", "notes.md")]);
        let run = "echo \"[$text]\"; printenv PIRS_ARG_path; printenv PIRS_ARG_text || echo unset; \
                   printenv PIRS_ARG_content-type || echo no-such-name; wc -c";
        let out = call(&shell(run), &payload, &vars, &env, Duration::from_secs(20))
            .await
            .expect("the spawn must not fail with E2BIG");
        let lines: Vec<&str> = out.stdout.lines().collect();
        assert_eq!(lines[0], "[]", "the oversized value interpolated as the empty string");
        assert_eq!(lines[1], "notes.md", "a small argument is still exported");
        assert_eq!(lines[2], "unset", "the oversized one is not");
        assert_eq!(lines[3], "no-such-name", "nor is a name no shell could read");
        let on_stdin: usize = lines[4].trim().parse().unwrap();
        assert!(on_stdin > 200_000, "the payload still carried it: {on_stdin} bytes");
    }

    // ----- spawn_detached -------------------------------------------------

    #[tokio::test]
    async fn detached_runs_on_and_is_killed_by_group() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pids");
        let env = CallEnv::new(dir.path().to_path_buf(), "on.start");
        let run = format!(
            "echo $$ >> {p}; sleep 30 & echo $! >> {p}; sleep 30",
            p = pidfile.display()
        );
        let mut spawned = spawn_detached(&shell(&run), &json!({"loop": "a7f3"}), &no_vars(), &env)
            .await
            .unwrap();
        // It is live and not waited for.
        let leader = spawned.pid.expect("a pid");
        let pids = loop {
            let text = std::fs::read_to_string(&pidfile).unwrap_or_default();
            let pids: Vec<u32> = text.lines().filter_map(|l| l.trim().parse().ok()).collect();
            if pids.len() == 2 {
                break pids;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert_eq!(pids[0], leader);
        assert!(alive(pids[1]).await, "the backgrounded child should be running");
        assert!(spawned.child.try_wait().unwrap().is_none(), "still running");

        spawned.kill().await;
        for pid in pids {
            assert!(!alive(pid).await, "pid {pid} survived the group kill");
        }
    }

    #[tokio::test]
    async fn detached_receives_the_payload() {
        let dir = tempfile::tempdir().unwrap();
        let seen = dir.path().join("seen");
        let env = CallEnv::new(dir.path().to_path_buf(), "on.turn_end");
        let run = format!("cat > {}", seen.display());
        let payload = json!({"loop": "a7f3", "messages": []});
        let mut spawned = spawn_detached(&shell(&run), &payload, &no_vars(), &env).await.unwrap();
        let status = tokio::time::timeout(Duration::from_secs(5), spawned.child.wait())
            .await
            .expect("the cat exits once stdin closes")
            .unwrap();
        assert!(status.success());
        let line = std::fs::read_to_string(&seen).unwrap();
        assert_eq!(line, format!("{}\n", serde_json::to_string(&payload).unwrap()));
    }

    // ----- wind_down ------------------------------------------------------

    #[tokio::test]
    async fn a_process_that_traps_term_shuts_down_and_is_never_killed() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("stopped");
        let env = CallEnv::new(dir.path().to_path_buf(), "on.start");
        let run = format!("trap 'echo bye > {}; exit 0' TERM; sleep 30", note.display());
        let spawned = spawn_detached(&shell(&run), &json!({}), &no_vars(), &env).await.unwrap();
        let pid = spawned.pid.expect("a pid");
        let child = supervise(spawned, |code| panic!("reported an exit of {code}"));
        // The shell must be in `sleep` before the signal, or the trap is not
        // installed yet.
        tokio::time::sleep(Duration::from_millis(150)).await;

        let started = std::time::Instant::now();
        wind_down(vec![child]).await;
        let waited = started.elapsed();

        assert_eq!(std::fs::read_to_string(&note).unwrap_or_default(), "bye\n", "the trap ran");
        assert!(waited < CLOSE_GRACE, "waited {waited:?}: it should not have needed the kill grace");
        assert!(!alive(pid).await, "pid {pid} survived");
    }

    #[tokio::test]
    async fn a_process_that_ignores_term_is_killed_after_the_second_grace() {
        let env = CallEnv::new(std::env::temp_dir(), "on.start");
        let spawned = spawn_detached(&shell("trap '' TERM; sleep 30"), &json!({}), &no_vars(), &env)
            .await
            .unwrap();
        let pid = spawned.pid.expect("a pid");
        let child = supervise(spawned, |_| {});
        tokio::time::sleep(Duration::from_millis(150)).await;

        let started = std::time::Instant::now();
        wind_down(vec![child]).await;
        let waited = started.elapsed();

        assert!(waited >= CLOSE_GRACE, "waited {waited:?}: it was killed before the grace was up");
        assert!(waited < CLOSE_GRACE * 3, "waited {waited:?}: the kill came far too late");
        assert!(!alive(pid).await, "pid {pid} survived the kill");
    }

    /// `kill -0`, polled: a killed process may take a moment to be reaped by
    /// init once its parent is gone.
    async fn alive(pid: u32) -> bool {
        for _ in 0..100 {
            let ok = tokio_command("kill")
                .args(["-0", "--", &pid.to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        true
    }
}
