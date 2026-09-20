//! Print mode (`-p`): run prompts non-interactively. `text` prints the final
//! assistant text; `json` prints every agent/UI event as a JSON line.

use crate::agent_session::{AgentSession, UiBackend, UiEvent};
use async_trait::async_trait;
use pi_agent::{AgentEvent, AgentMessage};
use serde_json::json;
use std::io::Write;
use std::sync::Mutex;

pub(crate) struct PrintUi {
    pub json: bool,
    pub last_assistant_text: Mutex<Option<String>>,
    pub errors: Mutex<Vec<String>>,
}

impl PrintUi {
    pub(crate) fn new(json: bool) -> Self {
        PrintUi { json, last_assistant_text: Mutex::new(None), errors: Mutex::new(Vec::new()) }
    }
}

#[async_trait]
impl UiBackend for PrintUi {
    fn mode(&self) -> &'static str {
        if self.json { "json" } else { "print" }
    }
    fn has_ui(&self) -> bool {
        false
    }
    fn emit(&self, event: UiEvent) {
        match &event {
            UiEvent::Agent(ev) => {
                if self.json {
                    if let Ok(v) = serde_json::to_value(ev) {
                        let mut out = std::io::stdout().lock();
                        let _ = writeln!(out, "{v}");
                    }
                }
                if let AgentEvent::MessageEnd { message: AgentMessage::Assistant(a) } = &**ev {
                    if let Some(err) = &a.error_message {
                        self.errors.lock().unwrap().push(err.clone());
                    }
                    let text = a.text();
                    if !text.is_empty() {
                        *self.last_assistant_text.lock().unwrap() = Some(text);
                    }
                }
            }
            UiEvent::Notify { message, kind } => {
                if self.json {
                    println!("{}", json!({"type": "notify", "message": message, "kind": kind}));
                } else {
                    eprintln!("[{kind}] {message}");
                }
            }
            UiEvent::ExtensionError(e) => {
                if self.json {
                    println!("{}", json!({"type": "extension_error", "extensionPath": e.extension_path, "event": e.event, "error": e.error}));
                } else {
                    eprintln!("[extension error] {} ({}): {}", e.extension_path, e.event, e.error);
                }
            }
            UiEvent::Console { level, message } => {
                if self.json {
                    println!("{}", json!({"type": "console", "level": level, "message": message}));
                } else {
                    eprintln!("{message}");
                }
            }
            UiEvent::BashOutput { output, .. } => {
                if self.json {
                    println!("{}", json!({"type": "bash_output", "output": output}));
                } else {
                    print!("{output}");
                }
            }
            _ => {}
        }
    }
}

/// Run one or more prompts and print the result. Returns the process exit code.
pub(crate) async fn run(session: &AgentSession, ui: &PrintUi, prompts: Vec<String>) -> i32 {
    for p in prompts {
        if let Err(e) = session.submit(p, Vec::new(), None).await {
            eprintln!("error: {e}");
            return 1;
        }
        session.wait_for_idle().await;
    }
    if !ui.json {
        if let Some(text) = ui.last_assistant_text.lock().unwrap().as_ref() {
            println!("{text}");
        }
    }
    let errors = ui.errors.lock().unwrap();
    if let Some(e) = errors.last() {
        eprintln!("error: {e}");
        return 1;
    }
    0
}
