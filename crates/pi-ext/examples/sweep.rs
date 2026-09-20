//! Load every extension in the given directories and report what happens.
//! Usage: cargo run -p pi-ext --example sweep -- <dir-or-file>...
use pi_ext::*;
use std::sync::Arc;

struct Quiet;
#[async_trait::async_trait]
impl HostCallbacks for Quiet {
    fn context_info(&self) -> ContextInfo {
        ContextInfo { cwd: std::env::current_dir().unwrap().to_string_lossy().to_string(), mode: "print".into(), has_ui: false, model: None, thinking_level: "off".into(), is_idle: true, session_file: None, session_id: "sweep".into() }
    }
    fn ui_notify(&self, _m: String, _k: String) {}
    fn console(&self, _l: String, _m: String) {}
    fn report_error(&self, err: ExtensionError) { println!("      handler error [{}]: {}", err.event, err.error.lines().next().unwrap_or("")); }
}

#[tokio::main]
async fn main() {
    let roots: Vec<std::path::PathBuf> = std::env::args().skip(1).map(Into::into).collect();
    let mut files = Vec::new();
    for r in &roots { if r.is_file() { files.push(r.clone()); } else { files.extend(discover_extensions(std::slice::from_ref(r))); } }
    let mut ok = 0; let mut failed = Vec::new();
    for f in &files {
        let host = ExtensionHost::spawn(Arc::new(Quiet), HostConfig::new(std::env::current_dir().unwrap())).await.unwrap();
        let name = f.strip_prefix(roots.first().unwrap()).unwrap_or(f).display().to_string();
        match host.load(f.to_string_lossy().to_string()).await {
            Ok(ext) => {
                ok += 1;
                host.set_loaded();
                let _ = host.dispatch("session_start", serde_json::json!({"type":"session_start","reason":"startup"}), None).await;
                println!("  ok   {name}  tools={} commands={} events={}", ext.tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(","), ext.commands.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(","), ext.events.len());
            }
            Err(e) => { let first = e.lines().find(|l| !l.trim().is_empty()).unwrap_or("").to_string(); println!("  FAIL {name}: {}", first.replace("/private/tmp/claude-501/-Users-chime-Workspace-pirs/425591f1-8df5-454e-8b62-75a3da5c8bd0/scratchpad/pi/packages/coding-agent/examples/extensions/", "").chars().take(230).collect::<String>()); failed.push(name); }
        }
        host.shutdown();
    }
    println!("\n{ok}/{} loaded", files.len());
}
