//! The engine without a terminal: a `TestBackend`, driven by a script of
//! JSON lines (`pirs tui --headless WxH`) or by [`Harness`] from tests.
//! The script protocol is documented in the crate's `README.md`.

use std::io::Write;
use std::time::{Duration, Instant};

use ratatui::backend::TestBackend;
use ratatui::Terminal;
use serde::Deserialize;
use serde_json::json;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::engine;
use crate::keys::Key;
use crate::model::Control;
use crate::TuiOptions;

/// How often `wait_until` looks at the screen.
const POLL: Duration = Duration::from_millis(25);

/// The UI running on a test backend, driven from a test.
///
/// Keys and text are queued in order; [`screen`](Self::screen) draws after
/// everything queued before it has been handled. Server traffic is
/// asynchronous, so assertions about it go through
/// [`wait_until`](Self::wait_until).
pub struct Harness {
    control: mpsc::UnboundedSender<Control>,
    task: JoinHandle<anyhow::Result<i32>>,
}

impl Harness {
    /// Connect (auto-starting a server if allowed), issue the start-up
    /// requests, and run the engine on a `size` backend.
    pub async fn start(opts: TuiOptions, size: (u16, u16)) -> anyhow::Result<Harness> {
        let (app, msgs) = crate::boot(&opts).await?;
        let terminal = Terminal::new(TestBackend::new(size.0, size.1))?;
        let (control, control_rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(engine::run(terminal, app, control_rx, msgs));
        Ok(Harness { control, task })
    }

    /// Press a key by name (`"j"`, `"enter"`, `"ctrl-q"`, `"alt-enter"`).
    ///
    /// # Panics
    ///
    /// On a name that is not a key: in a test that is a typo, not a state.
    /// [`try_key`](Self::try_key) reports it instead.
    pub fn key(&self, name: &str) -> &Self {
        self.try_key(name)
            .unwrap_or_else(|e| panic!("Harness::key: {e}"))
    }

    /// Press a key by name, or say why the name is not a key.
    pub fn try_key(&self, name: &str) -> Result<&Self, String> {
        let key = Key::parse(name)?;
        let _ = self.control.send(Control::Key(key));
        Ok(self)
    }

    /// Type text into whatever line is focused.
    pub fn text(&self, text: &str) -> &Self {
        let _ = self.control.send(Control::Text(text.to_owned()));
        self
    }

    /// Give queued input and in-flight server traffic a moment, then let
    /// the engine catch up.
    pub async fn settle(&self) -> &Self {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let _ = self.screen().await;
        self
    }

    /// Draw now and return the screen, one line per row, trailing spaces
    /// trimmed.
    pub async fn screen(&self) -> String {
        let (tx, rx) = oneshot::channel();
        if self.control.send(Control::Screen(tx)).is_err() {
            return "(engine stopped)\n".to_owned();
        }
        rx.await.unwrap_or_else(|_| "(engine stopped)\n".to_owned())
    }

    /// Poll the screen until `predicate` holds; `Ok` is that screen, `Err`
    /// the last one seen before `timeout`.
    pub async fn wait_until(
        &self,
        predicate: impl Fn(&str) -> bool,
        timeout: Duration,
    ) -> Result<String, String> {
        let deadline = Instant::now() + timeout;
        loop {
            let screen = self.screen().await;
            if predicate(&screen) {
                return Ok(screen);
            }
            if Instant::now() >= deadline {
                return Err(screen);
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// [`wait_until`](Self::wait_until) the screen contains `needle`.
    pub async fn wait_for(&self, needle: &str, timeout: Duration) -> Result<String, String> {
        self.wait_until(|s| s.contains(needle), timeout).await
    }

    /// Stop the engine and return its exit code.
    pub async fn quit(self) -> anyhow::Result<i32> {
        let _ = self.control.send(Control::Quit);
        self.task.await?
    }
}

/// One line of a headless script.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScriptLine {
    key: Option<String>,
    text: Option<String>,
    settle: Option<u64>,
    wait: Option<String>,
    timeout: Option<u64>,
    dump: Option<bool>,
    quit: Option<bool>,
}

/// Run the UI headless: `script` is JSON lines, `out` gets the echoes and
/// dumps. Exit code 0 when every command succeeded, 1 otherwise.
pub async fn run_headless(
    opts: TuiOptions,
    size: (u16, u16),
    mut script: impl AsyncBufRead + Unpin,
    mut out: impl Write,
) -> anyhow::Result<i32> {
    let harness = Harness::start(opts, size).await?;
    let mut failed = false;
    let mut line = String::new();
    loop {
        line.clear();
        if script.read_line(&mut line).await? == 0 {
            break;
        }
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        let quit = match execute(&harness, text, &mut out).await {
            Ok(quit) => quit,
            Err(error) => {
                failed = true;
                writeln!(out, "{}", json!({ "error": error }))?;
                false
            }
        };
        out.flush()?;
        if quit {
            break;
        }
    }
    harness.quit().await?;
    out.flush()?;
    Ok(if failed { 1 } else { 0 })
}

/// Execute one script line; `Ok(true)` means quit.
async fn execute(harness: &Harness, text: &str, out: &mut impl Write) -> Result<bool, String> {
    let cmd: ScriptLine = serde_json::from_str(text).map_err(|e| format!("bad line: {e}"))?;
    let ok = |name: &str, out: &mut dyn Write| -> Result<(), String> {
        writeln!(out, "{}", json!({ "ok": name })).map_err(|e| e.to_string())
    };
    if let Some(name) = cmd.key {
        harness.try_key(&name).map_err(|e| format!("key: {e}"))?;
        ok("key", out)?;
    } else if let Some(t) = cmd.text {
        harness.text(&t);
        ok("text", out)?;
    } else if let Some(ms) = cmd.settle {
        tokio::time::sleep(Duration::from_millis(ms)).await;
        let _ = harness.screen().await;
        ok("settle", out)?;
    } else if let Some(needle) = cmd.wait {
        let timeout = Duration::from_millis(cmd.timeout.unwrap_or(5000));
        harness.wait_for(&needle, timeout).await.map_err(|_| {
            format!(
                "wait: timed out after {} ms waiting for {needle:?}",
                timeout.as_millis()
            )
        })?;
        ok("wait", out)?;
    } else if cmd.dump == Some(true) {
        let screen = harness.screen().await;
        ok("dump", out)?;
        write!(out, "=== screen ===\n{screen}=== end ===\n").map_err(|e| e.to_string())?;
    } else if cmd.quit == Some(true) {
        ok("quit", out)?;
        return Ok(true);
    } else {
        return Err("a line needs one of key, text, settle, wait, dump, quit".to_owned());
    }
    Ok(false)
}
