//! `bash` tool (port of pi's `bash.ts` + `output-accumulator.ts`).
//!
//! Runs `bash -c <command>` (falling back to `sh`) in the working directory,
//! merges stdout and stderr in arrival order, streams partial output through
//! `on_update`, and truncates the final output from the tail. When output is
//! truncated the full output is saved to a temp file whose path is reported.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use pi_agent::{AgentTool, ToolResult, UpdateFn};
use pi_ai::Content;
use serde_json::{json, Map, Value};
use tokio::io::AsyncReadExt as _;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::arg_str;
use super::truncate::{format_size, truncate_tail, TruncatedBy, TruncationOptions, TruncationResult, DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES};

const MAX_TIMEOUT_MS: f64 = 2_147_483_647.0;
const BASH_UPDATE_THROTTLE_MS: u64 = 100;
/// After the shell exits, wait this long for inherited pipes to fall idle.
const EXIT_STDIO_GRACE_MS: u64 = 100;
const TEMP_FILE_PREFIX: &str = "pirs-bash";

pub struct BashTool {
    cwd: PathBuf,
}

impl BashTool {
    pub fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }
}

// ---------------------------------------------------------------------------
// Output accumulator
// ---------------------------------------------------------------------------

pub struct OutputSnapshot {
    pub content: String,
    pub truncation: TruncationResult,
    pub full_output_path: Option<PathBuf>,
}

/// Incrementally tracks streaming output with bounded memory.
///
/// Decodes chunks with a streaming UTF-8 decoder, keeps only a decoded tail
/// for display snapshots, and opens a temp file when the full output needs to
/// be preserved.
struct OutputAccumulator {
    max_lines: usize,
    max_bytes: usize,
    max_rolling_bytes: usize,
    temp_file_prefix: &'static str,

    raw_chunks: Vec<u8>,
    pending_utf8: Vec<u8>,
    tail_text: String,
    tail_starts_at_line_boundary: bool,
    total_raw_bytes: usize,
    total_decoded_bytes: usize,
    completed_lines: usize,
    total_lines: usize,
    current_line_bytes: usize,
    has_open_line: bool,
    finished: bool,

    temp_file_path: Option<PathBuf>,
    temp_file: Option<std::fs::File>,
}

fn default_temp_file_path(prefix: &str) -> PathBuf {
    let id = uuid::Uuid::new_v4().simple().to_string();
    std::env::temp_dir().join(format!("{prefix}-{}.log", &id[..16]))
}

impl OutputAccumulator {
    fn new(max_lines: usize, max_bytes: usize, temp_file_prefix: &'static str) -> Self {
        Self {
            max_lines,
            max_bytes,
            max_rolling_bytes: (max_bytes * 2).max(1),
            temp_file_prefix,
            raw_chunks: Vec::new(),
            pending_utf8: Vec::new(),
            tail_text: String::new(),
            tail_starts_at_line_boundary: true,
            total_raw_bytes: 0,
            total_decoded_bytes: 0,
            completed_lines: 0,
            total_lines: 0,
            current_line_bytes: 0,
            has_open_line: false,
            finished: false,
            temp_file_path: None,
            temp_file: None,
        }
    }

    fn append(&mut self, data: &[u8]) {
        if self.finished {
            return;
        }
        self.total_raw_bytes += data.len();
        let decoded = self.decode_streaming(data);
        self.append_decoded_text(&decoded);

        if self.temp_file.is_some() || self.should_use_temp_file() {
            self.ensure_temp_file();
            if let Some(file) = self.temp_file.as_mut() {
                let _ = file.write_all(data);
            }
        } else if !data.is_empty() {
            self.raw_chunks.extend_from_slice(data);
        }
    }

    fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        if !self.pending_utf8.is_empty() {
            self.pending_utf8.clear();
            self.append_decoded_text("\u{FFFD}");
        }
        if self.should_use_temp_file() {
            self.ensure_temp_file();
        }
    }

    /// Streaming UTF-8 decode: keeps an incomplete trailing sequence for the next chunk.
    fn decode_streaming(&mut self, data: &[u8]) -> String {
        self.pending_utf8.extend_from_slice(data);
        let mut out = String::new();
        let mut buf = std::mem::take(&mut self.pending_utf8);
        loop {
            match std::str::from_utf8(&buf) {
                Ok(s) => {
                    out.push_str(s);
                    buf.clear();
                    break;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    out.push_str(std::str::from_utf8(&buf[..valid]).unwrap_or(""));
                    match e.error_len() {
                        None => {
                            // Incomplete sequence at the end; keep it for the next chunk.
                            buf.drain(..valid);
                            break;
                        }
                        Some(len) => {
                            out.push('\u{FFFD}');
                            buf.drain(..valid + len);
                        }
                    }
                }
            }
        }
        self.pending_utf8 = buf;
        out
    }

    fn snapshot(&mut self, persist_if_truncated: bool) -> OutputSnapshot {
        let tail = truncate_tail(
            &self.snapshot_text(),
            TruncationOptions { max_lines: self.max_lines, max_bytes: self.max_bytes },
        );
        let truncated = self.total_lines > self.max_lines || self.total_decoded_bytes > self.max_bytes;
        let truncated_by = if truncated {
            tail.truncated_by
                .or(Some(if self.total_decoded_bytes > self.max_bytes { TruncatedBy::Bytes } else { TruncatedBy::Lines }))
        } else {
            None
        };
        let truncation = TruncationResult {
            truncated,
            truncated_by,
            total_lines: self.total_lines,
            total_bytes: self.total_decoded_bytes,
            max_lines: self.max_lines,
            max_bytes: self.max_bytes,
            ..tail
        };
        if persist_if_truncated && truncation.truncated {
            self.ensure_temp_file();
        }
        OutputSnapshot { content: truncation.content.clone(), truncation, full_output_path: self.temp_file_path.clone() }
    }

    fn close_temp_file(&mut self) {
        if let Some(mut file) = self.temp_file.take() {
            let _ = file.flush();
        }
    }

    fn last_line_bytes(&self) -> usize {
        self.current_line_bytes
    }

    fn append_decoded_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        let bytes = text.len();
        self.total_decoded_bytes += bytes;
        self.tail_text.push_str(text);
        if self.tail_text.len() > self.max_rolling_bytes * 2 {
            self.trim_tail();
        }

        let newlines = text.bytes().filter(|b| *b == b'\n').count();
        if newlines == 0 {
            self.current_line_bytes += bytes;
            self.has_open_line = true;
        } else {
            self.completed_lines += newlines;
            let tail = text.rsplit('\n').next().unwrap_or("");
            self.current_line_bytes = tail.len();
            self.has_open_line = !tail.is_empty();
        }
        self.total_lines = self.completed_lines + usize::from(self.has_open_line);
    }

    fn trim_tail(&mut self) {
        if self.tail_text.len() <= self.max_rolling_bytes {
            return;
        }
        let mut start = self.tail_text.len() - self.max_rolling_bytes;
        while start < self.tail_text.len() && !self.tail_text.is_char_boundary(start) {
            start += 1;
        }
        self.tail_starts_at_line_boundary =
            if start == 0 { self.tail_starts_at_line_boundary } else { self.tail_text.as_bytes()[start - 1] == b'\n' };
        self.tail_text = self.tail_text[start..].to_string();
    }

    fn snapshot_text(&self) -> String {
        if self.tail_starts_at_line_boundary {
            return self.tail_text.clone();
        }
        match self.tail_text.find('\n') {
            Some(i) => self.tail_text[i + 1..].to_string(),
            None => self.tail_text.clone(),
        }
    }

    fn should_use_temp_file(&self) -> bool {
        self.total_raw_bytes > self.max_bytes || self.total_decoded_bytes > self.max_bytes || self.total_lines > self.max_lines
    }

    fn ensure_temp_file(&mut self) {
        if self.temp_file_path.is_some() {
            return;
        }
        let path = default_temp_file_path(self.temp_file_prefix);
        match std::fs::File::create(&path) {
            Ok(mut file) => {
                let _ = file.write_all(&self.raw_chunks);
                self.raw_chunks = Vec::new();
                self.temp_file = Some(file);
                self.temp_file_path = Some(path);
            }
            Err(_) => {
                // Leave the raw chunks in memory; the notice will simply lack a path.
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Process handling
// ---------------------------------------------------------------------------

fn resolve_timeout(timeout: Option<&Value>) -> Result<Option<Duration>> {
    let Some(value) = timeout else { return Ok(None) };
    if value.is_null() {
        return Ok(None);
    }
    let seconds = match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    };
    let Some(seconds) = seconds.filter(|s| s.is_finite() && *s > 0.0) else {
        return Err(anyhow!("Invalid timeout: must be a finite number of seconds"));
    };
    let ms = seconds * 1000.0;
    if ms > MAX_TIMEOUT_MS {
        return Err(anyhow!("Invalid timeout: maximum is {} seconds", MAX_TIMEOUT_MS / 1000.0));
    }
    Ok(Some(Duration::from_secs_f64(seconds)))
}

/// Format the `timeout` argument the way the TS error message prints it.
fn timeout_display(value: Option<&Value>) -> String {
    match value {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.trim().to_string(),
        _ => String::new(),
    }
}

/// Kill the whole process group (the shell was spawned as a group leader),
/// falling back to just the child pid.
fn kill_process_tree_sync(pid: u32) {
    let group_kill = crate::process::std_command("kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let killed_group = matches!(group_kill, Ok(status) if status.success());
    if !killed_group {
        let _ = crate::process::std_command("kill")
            .args(["-KILL", "--", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Kills the process tree if the tool future is dropped before completion.
struct KillGuard {
    pid: Option<u32>,
    armed: bool,
}

impl Drop for KillGuard {
    fn drop(&mut self) {
        if self.armed {
            if let Some(pid) = self.pid {
                kill_process_tree_sync(pid);
            }
        }
    }
}

fn spawn_shell(command: &str, cwd: &Path) -> std::io::Result<tokio::process::Child> {
    let mut last_err = None;
    for shell in ["bash", "sh"] {
        let mut cmd = crate::process::tokio_command(shell);
        cmd.arg("-c")
            .arg(command)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        match cmd.spawn() {
            Ok(child) => return Ok(child),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => last_err = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last_err.unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no shell found")))
}

fn spawn_reader<R: tokio::io::AsyncRead + Unpin + Send + 'static>(mut reader: R, tx: mpsc::UnboundedSender<Vec<u8>>) {
    tokio::spawn(async move {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

enum ExecOutcome {
    Exited(i32),
    Aborted,
    TimedOut,
}

fn exit_code_of(status: std::process::ExitStatus) -> i32 {
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
    1
}

fn details_json(truncation: Option<&TruncationResult>, full_output_path: Option<&Path>) -> Option<Value> {
    let mut map = Map::new();
    if let Some(t) = truncation {
        if let Ok(v) = serde_json::to_value(t) {
            map.insert("truncation".into(), v);
        }
    }
    if let Some(p) = full_output_path {
        map.insert("fullOutputPath".into(), Value::String(p.to_string_lossy().to_string()));
    }
    if map.is_empty() {
        None
    } else {
        Some(Value::Object(map))
    }
}

fn format_output(output: &OutputAccumulator, snapshot: &OutputSnapshot, empty_text: &str) -> (String, Option<Value>) {
    let truncation = &snapshot.truncation;
    let mut text = if snapshot.content.is_empty() { empty_text.to_string() } else { snapshot.content.clone() };
    let mut details = None;
    if truncation.truncated {
        details = details_json(Some(truncation), snapshot.full_output_path.as_deref());
        let full_path = snapshot
            .full_output_path
            .as_ref()
            .map_or_else(|| "(unavailable)".to_string(), |p| p.to_string_lossy().to_string());
        let start_line = truncation.total_lines - truncation.output_lines + 1;
        let end_line = truncation.total_lines;
        if truncation.last_line_partial {
            let last_line_size = format_size(output.last_line_bytes());
            text.push_str(&format!(
                "\n\n[Showing last {} of line {end_line} (line is {last_line_size}). Full output: {full_path}]",
                format_size(truncation.output_bytes)
            ));
        } else if truncation.truncated_by == Some(TruncatedBy::Lines) {
            text.push_str(&format!(
                "\n\n[Showing lines {start_line}-{end_line} of {}. Full output: {full_path}]",
                truncation.total_lines
            ));
        } else {
            text.push_str(&format!(
                "\n\n[Showing lines {start_line}-{end_line} of {} ({} limit). Full output: {full_path}]",
                truncation.total_lines,
                format_size(DEFAULT_MAX_BYTES)
            ));
        }
    }
    (text, details)
}

fn append_status(text: &str, status: &str) -> String {
    if text.is_empty() {
        status.to_string()
    } else {
        format!("{text}\n\n{status}")
    }
}

#[async_trait]
impl AgentTool for BashTool {
    fn name(&self) -> String {
        "bash".into()
    }

    fn description(&self) -> String {
        format!(
            "Execute a bash command in the current working directory. Returns stdout and stderr. Output is truncated to last {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.",
            DEFAULT_MAX_BYTES / 1024
        )
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to execute" },
                "timeout": { "type": "number", "description": "Timeout in seconds (optional, no default timeout)" }
            },
            "required": ["command"]
        })
    }

    fn prompt_snippet(&self) -> Option<String> {
        Some("Execute bash commands (ls, grep, find, etc.)".into())
    }

    async fn execute(
        &self,
        _tool_call_id: &str,
        args: Value,
        cancel: CancellationToken,
        on_update: UpdateFn,
    ) -> Result<ToolResult> {
        let command = arg_str(&args, "command")?.to_string();
        let timeout = resolve_timeout(args.get("timeout"))?;
        if cancel.is_cancelled() {
            return Err(anyhow!("Command aborted"));
        }
        if !self.cwd.exists() {
            return Err(anyhow!("Working directory does not exist: {}\nCannot execute bash commands.", self.cwd.display()));
        }

        let mut output = OutputAccumulator::new(DEFAULT_MAX_LINES, DEFAULT_MAX_BYTES, TEMP_FILE_PREFIX);
        on_update(ToolResult { content: Vec::new(), ..Default::default() });

        let mut child = spawn_shell(&command, &self.cwd).map_err(|e| anyhow!("Failed to spawn shell: {e}"))?;
        let pid = child.id();
        let mut guard = KillGuard { pid, armed: true };

        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        if let Some(stdout) = child.stdout.take() {
            spawn_reader(stdout, tx.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_reader(stderr, tx.clone());
        }
        drop(tx);

        let mut wait_task = tokio::spawn(async move { child.wait().await });

        let deadline = timeout.map(|d| tokio::time::Instant::now() + d);
        let far_future = tokio::time::Instant::now() + Duration::from_secs(60 * 60 * 24 * 365);
        let mut exit_status: Option<std::io::Result<std::process::ExitStatus>> = None;
        let mut rx_closed = false;
        let mut aborted = false;
        let mut timed_out = false;
        let mut idle_deadline: Option<tokio::time::Instant> = None;
        let mut update_dirty = false;
        let mut last_update_at: Option<Instant> = None;

        let emit_update = |output: &mut OutputAccumulator, dirty: &mut bool, last: &mut Option<Instant>| {
            if !*dirty {
                return;
            }
            *dirty = false;
            *last = Some(Instant::now());
            let snapshot = output.snapshot(true);
            on_update(ToolResult {
                content: vec![Content::text(snapshot.content.clone())],
                details: details_json(
                    if snapshot.truncation.truncated { Some(&snapshot.truncation) } else { None },
                    snapshot.full_output_path.as_deref(),
                ),
                ..Default::default()
            });
        };

        let kill = |pid: Option<u32>| async move {
            if let Some(pid) = pid {
                let _ = tokio::task::spawn_blocking(move || kill_process_tree_sync(pid)).await;
            }
        };

        loop {
            if exit_status.is_some() && rx_closed {
                break;
            }
            let update_due = match (update_dirty, last_update_at) {
                (true, Some(last)) => tokio::time::Instant::from_std(last + Duration::from_millis(BASH_UPDATE_THROTTLE_MS)),
                (true, None) => tokio::time::Instant::now(),
                (false, _) => far_future,
            };
            tokio::select! {
                biased;
                _ = cancel.cancelled(), if !aborted && !timed_out => {
                    aborted = true;
                    kill(pid).await;
                }
                _ = tokio::time::sleep_until(deadline.unwrap_or(far_future)), if deadline.is_some() && !timed_out && !aborted => {
                    timed_out = true;
                    kill(pid).await;
                }
                chunk = rx.recv(), if !rx_closed => {
                    match chunk {
                        Some(data) => {
                            output.append(&data);
                            update_dirty = true;
                            if exit_status.is_some() {
                                idle_deadline = Some(tokio::time::Instant::now() + Duration::from_millis(EXIT_STDIO_GRACE_MS));
                            }
                        }
                        None => rx_closed = true,
                    }
                }
                status = &mut wait_task, if exit_status.is_none() => {
                    exit_status = Some(status.unwrap_or_else(|e| Err(std::io::Error::other(e))));
                    idle_deadline = Some(tokio::time::Instant::now() + Duration::from_millis(EXIT_STDIO_GRACE_MS));
                }
                _ = tokio::time::sleep_until(idle_deadline.unwrap_or(far_future)), if idle_deadline.is_some() && !rx_closed => {
                    // The shell exited and its pipes have been quiet: stop waiting on inherited handles.
                    break;
                }
                _ = tokio::time::sleep_until(update_due), if update_dirty => {
                    emit_update(&mut output, &mut update_dirty, &mut last_update_at);
                }
            }
        }
        guard.armed = false;

        let outcome = if aborted {
            ExecOutcome::Aborted
        } else if timed_out {
            ExecOutcome::TimedOut
        } else {
            match exit_status {
                Some(Ok(status)) => ExecOutcome::Exited(exit_code_of(status)),
                Some(Err(e)) => return Err(anyhow!("Failed to wait for shell: {e}")),
                None => ExecOutcome::Exited(1),
            }
        };

        output.finish();
        update_dirty = true;
        emit_update(&mut output, &mut update_dirty, &mut last_update_at);
        let snapshot = output.snapshot(true);
        output.close_temp_file();

        match outcome {
            ExecOutcome::Aborted => {
                let (text, _) = format_output(&output, &snapshot, "");
                Err(anyhow!(append_status(&text, "Command aborted")))
            }
            ExecOutcome::TimedOut => {
                let (text, _) = format_output(&output, &snapshot, "");
                Err(anyhow!(append_status(
                    &text,
                    &format!("Command timed out after {} seconds", timeout_display(args.get("timeout")))
                )))
            }
            ExecOutcome::Exited(code) => {
                let (text, details) = format_output(&output, &snapshot, "(no output)");
                if code != 0 {
                    return Err(anyhow!(append_status(&text, &format!("Command exited with code {code}"))));
                }
                Ok(ToolResult { content: vec![Content::text(text)], details, ..Default::default() })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::test_support::{first_text, noop_update, recording_update};

    fn tool(dir: &tempfile::TempDir) -> BashTool {
        BashTool::new(dir.path().to_path_buf())
    }

    #[tokio::test]
    async fn runs_command_and_merges_streams() {
        let dir = tempfile::tempdir().expect("tempdir");
        let r = tool(&dir)
            .execute("id", json!({"command": "echo out; echo err 1>&2; echo done"}), CancellationToken::new(), noop_update())
            .await
            .expect("ok");
        // stdout ordering is preserved; stderr is merged in (cross-pipe order is not deterministic).
        let text = first_text(&r);
        let stdout_only: String = text.lines().filter(|l| *l != "err").map(|l| format!("{l}\n")).collect();
        assert_eq!(stdout_only, "out\ndone\n");
        assert!(text.contains("err\n"), "{text}");
        assert!(r.details.is_none());
    }

    #[tokio::test]
    async fn empty_output_and_cwd() {
        let dir = tempfile::tempdir().expect("tempdir");
        let r = tool(&dir).execute("id", json!({"command": "true"}), CancellationToken::new(), noop_update()).await.expect("ok");
        assert_eq!(first_text(&r), "(no output)");
        let r = tool(&dir).execute("id", json!({"command": "pwd"}), CancellationToken::new(), noop_update()).await.expect("ok");
        let expected = dir.path().canonicalize().expect("canon");
        assert_eq!(first_text(&r).trim(), expected.to_string_lossy());
    }

    #[tokio::test]
    async fn nonzero_exit_is_error_with_output() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = tool(&dir)
            .execute("id", json!({"command": "echo boom; exit 3"}), CancellationToken::new(), noop_update())
            .await
            .expect_err("nonzero");
        assert_eq!(err.to_string(), "boom\n\n\nCommand exited with code 3");
    }

    #[tokio::test]
    async fn timeout_kills_process() {
        let dir = tempfile::tempdir().expect("tempdir");
        let started = Instant::now();
        let err = tool(&dir)
            .execute("id", json!({"command": "echo start; sleep 20; echo never", "timeout": 0.3}), CancellationToken::new(), noop_update())
            .await
            .expect_err("timeout");
        assert!(started.elapsed() < Duration::from_secs(10), "took {:?}", started.elapsed());
        assert_eq!(err.to_string(), "start\n\n\nCommand timed out after 0.3 seconds");
    }

    #[tokio::test]
    async fn timeout_uses_argument_value() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = tool(&dir)
            .execute("id", json!({"command": "sleep 20", "timeout": 0.2}), CancellationToken::new(), noop_update())
            .await
            .expect_err("timeout");
        assert_eq!(err.to_string(), "Command timed out after 0.2 seconds");
        let err = tool(&dir)
            .execute("id", json!({"command": "true", "timeout": -1}), CancellationToken::new(), noop_update())
            .await
            .expect_err("invalid");
        assert_eq!(err.to_string(), "Invalid timeout: must be a finite number of seconds");
    }

    #[tokio::test]
    async fn cancel_aborts_command() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cancel = CancellationToken::new();
        let canceller = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            canceller.cancel();
        });
        let started = Instant::now();
        let err = tool(&dir)
            .execute("id", json!({"command": "echo a; sleep 20"}), cancel, noop_update())
            .await
            .expect_err("aborted");
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(err.to_string(), "a\n\n\nCommand aborted");
    }

    #[tokio::test]
    async fn streams_partial_output_through_on_update() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (update, store) = recording_update();
        let r = tool(&dir)
            .execute("id", json!({"command": "echo first; sleep 0.4; echo second"}), CancellationToken::new(), update)
            .await
            .expect("ok");
        assert_eq!(first_text(&r), "first\nsecond\n");
        let updates = store.lock().expect("lock");
        assert!(updates.first().is_some_and(|u| u.content.is_empty()), "first update is empty");
        let texts: Vec<String> = updates.iter().map(first_text).collect();
        assert!(texts.iter().any(|t| t == "first\n"), "saw partial output: {texts:?}");
        assert_eq!(texts.last().map(String::as_str), Some("first\nsecond\n"));
    }

    #[tokio::test]
    async fn truncates_output_and_saves_full_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let r = tool(&dir)
            .execute("id", json!({"command": "seq 1 3000"}), CancellationToken::new(), noop_update())
            .await
            .expect("ok");
        let text = first_text(&r);
        assert!(text.starts_with("1001\n1002\n"), "{}", &text[..40]);
        assert!(text.contains("[Showing lines 1001-3000 of 3000. Full output: "), "{text}");
        let details = r.details.expect("details");
        assert_eq!(details["truncation"]["truncatedBy"], "lines");
        let full_path = details["fullOutputPath"].as_str().expect("path");
        let full = std::fs::read_to_string(full_path).expect("full output");
        assert_eq!(full.lines().count(), 3000);
        let _ = std::fs::remove_file(full_path);
    }

    #[tokio::test]
    async fn byte_truncation_of_single_long_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let r = tool(&dir)
            .execute("id", json!({"command": "head -c 60000 /dev/zero | tr '\\0' 'x'"}), CancellationToken::new(), noop_update())
            .await
            .expect("ok");
        let text = first_text(&r);
        assert!(text.contains("[Showing last 50.0KB of line 1 (line is 58.6KB). Full output: "), "{}", &text[text.len() - 120..]);
        let details = r.details.expect("details");
        assert_eq!(details["truncation"]["lastLinePartial"], true);
        if let Some(p) = details["fullOutputPath"].as_str() {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn accumulator_decodes_split_utf8() {
        let mut acc = OutputAccumulator::new(10, 100, "test");
        let bytes = "héllo\n".as_bytes();
        acc.append(&bytes[..2]);
        acc.append(&bytes[2..]);
        acc.finish();
        let snap = acc.snapshot(false);
        assert_eq!(snap.content, "héllo\n");
        assert_eq!(snap.truncation.total_lines, 1);
        assert!(!snap.truncation.truncated);
    }

    #[test]
    fn accumulator_counts_open_lines() {
        let mut acc = OutputAccumulator::new(2, 1000, "test");
        acc.append(b"a\nb\nc");
        assert_eq!(acc.total_lines, 3);
        assert_eq!(acc.last_line_bytes(), 1);
        let snap = acc.snapshot(false);
        assert!(snap.truncation.truncated);
        assert_eq!(snap.content, "b\nc");
        if let Some(p) = acc.temp_file_path.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}
