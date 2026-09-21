//! Every process the TUI spawns, in one place: `[[render]]` hooks (S6, D-37),
//! UI-side shell commands, and the editor pane (D-29). The TUI is a client
//! and may spawn processes; nothing else in this crate does.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// How long a hook or a UI command may run.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(5);

/// What a `[[render]]` command answers: lines to draw in place of the default
/// rendering and, optionally, options for a native picker.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct HookOutput {
    #[serde(default)]
    pub lines: Vec<String>,
    #[serde(default)]
    pub options: Option<Vec<String>>,
}

/// Run `sh -c run` with one JSON line on stdin and read one JSON line back.
pub(crate) async fn run_hook(run: &str, input: &Value) -> Result<HookOutput, String> {
    let stdout = run_sh(run, None, Some(&format!("{input}\n"))).await?;
    let line = stdout
        .lines()
        .find(|l| !l.trim().is_empty())
        .ok_or_else(|| "printed nothing".to_owned())?;
    serde_json::from_str(line).map_err(|e| format!("bad JSON on stdout: {e}"))
}

/// Run a UI-side shell command in `cwd`; its stdout becomes a notice.
pub(crate) async fn run_shell(command: &str, cwd: &Path) -> Result<String, String> {
    let stdout = run_sh(command, Some(cwd), None).await?;
    Ok(stdout.trim_end().to_owned())
}

/// Open `$EDITOR` (fallback `vi`) on `path` in a tmux pane beside the UI.
/// `prefix` is the server's bridge prefix (`ssh build`), empty locally.
/// Outside tmux there is no pane to ask for (S14).
pub(crate) async fn open_editor_pane(prefix: &str, path: &str) -> Result<(), String> {
    if std::env::var_os("TMUX").is_none() {
        return Err("editor pane needs tmux (S14)".to_owned());
    }
    let editor = std::env::var("EDITOR")
        .ok()
        .filter(|e| !e.trim().is_empty())
        .unwrap_or_else(|| "vi".to_owned());
    let mut command = String::new();
    if !prefix.trim().is_empty() {
        command.push_str(prefix.trim());
        command.push(' ');
    }
    command.push_str(&editor);
    command.push(' ');
    command.push_str(&shell_quote(path));
    let status = tokio::time::timeout(
        TIMEOUT,
        Command::new("tmux")
            .args(["split-window", "-h", &command])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status(),
    )
    .await
    .map_err(|_| "tmux did not answer".to_owned())?
    .map_err(|e| format!("cannot run tmux: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("tmux split-window failed ({status})"))
    }
}

/// Quote one argument for `sh`.
pub(crate) fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@%+=,".contains(c))
    {
        return s.to_owned();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Run `sh -c command`, optionally in `cwd` and with `stdin` written, with
/// [`TIMEOUT`]; returns stdout, or an error naming stderr / the exit status.
async fn run_sh(command: &str, cwd: Option<&Path>, stdin: Option<&str>) -> Result<String, String> {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let mut child = cmd.spawn().map_err(|e| format!("cannot run `sh`: {e}"))?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // A process that exits without reading gets a broken pipe; that is
        // its business, not an error of ours.
        let _ = pipe.write_all(text.as_bytes()).await;
        drop(pipe);
    }
    let output = tokio::time::timeout(TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| format!("timed out after {} s", TIMEOUT.as_secs()))?
        .map_err(|e| format!("failed: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        return Err(if stderr.is_empty() {
            format!("exited with {}", output.status)
        } else {
            format!("exited with {}: {stderr}", output.status)
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("/srv/a-b_c.txt"), "/srv/a-b_c.txt");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
    }

    #[tokio::test]
    async fn hooks_read_stdin_and_answer_one_line() {
        let out = run_hook(
            "read line; echo \"{\\\"lines\\\":[\\\"got $line\\\"]}\"",
            &serde_json::json!(5),
        )
        .await
        .unwrap();
        assert_eq!(out.lines, vec!["got 5".to_owned()]);
        assert_eq!(out.options, None);
        assert!(run_hook("exit 3", &Value::Null)
            .await
            .unwrap_err()
            .contains("exited"));
        assert!(run_hook("echo nope", &Value::Null)
            .await
            .unwrap_err()
            .contains("bad JSON"));
        assert!(run_hook("true", &Value::Null)
            .await
            .unwrap_err()
            .contains("nothing"));
    }

    #[tokio::test]
    async fn shell_commands_report_stdout() {
        let dir = std::env::temp_dir();
        assert_eq!(run_shell("echo hi", &dir).await.unwrap(), "hi");
        assert!(run_shell("echo bad >&2; exit 1", &dir)
            .await
            .unwrap_err()
            .contains("bad"));
    }

    #[tokio::test]
    async fn editor_needs_tmux() {
        // The test runner is not inside tmux, or we would open a pane; the
        // rule is the same either way, so only assert the message when the
        // variable is absent.
        if std::env::var_os("TMUX").is_none() {
            assert_eq!(
                open_editor_pane("", "/tmp/x").await.unwrap_err(),
                "editor pane needs tmux (S14)"
            );
        }
    }
}
