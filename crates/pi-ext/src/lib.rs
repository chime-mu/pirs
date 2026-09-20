//! pi-ext: runs pi extensions (TypeScript modules) inside an embedded QuickJS
//! runtime and bridges them to the Rust agent.
//!
//! Extensions are loaded unchanged: TypeScript types are stripped with swc,
//! `typebox`, `@earendil-works/pi-*`, and common `node:*` imports resolve to
//! embedded shims, and the `pi` API object is provided by `js/runtime.js`.

#![deny(unreachable_pub)]

pub mod host;
pub mod loader;
pub mod strip;
pub mod types;

pub use host::{ExtensionHost, ExtensionTool, HostCallbacks, HostConfig};
pub use types::*;

/// Discover extension entry points the way pi does: `*.ts` files and
/// `*/index.ts` directories under each root.
pub fn discover_extensions(roots: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        let Ok(rd) = std::fs::read_dir(root) else { continue };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            if p.is_file() {
                if matches!(p.extension().and_then(|x| x.to_str()), Some("ts") | Some("js") | Some("mjs")) {
                    out.push(p);
                }
            } else if p.is_dir() {
                for idx in ["index.ts", "index.js", "index.mjs"] {
                    let f = p.join(idx);
                    if f.is_file() {
                        out.push(f);
                        break;
                    }
                }
                // pi package manifest: package.json with "pi": { "extensions": [...] }
                let manifest = p.join("package.json");
                if let Ok(text) = std::fs::read_to_string(&manifest) {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                        if let Some(list) = v["pi"]["extensions"].as_array() {
                            for rel in list.iter().filter_map(|r| r.as_str()) {
                                let f = p.join(rel);
                                if f.is_file() && !out.contains(&f) {
                                    out.push(f);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};
    use tokio_util::sync::CancellationToken;

    #[derive(Default)]
    struct TestCallbacks {
        log: Mutex<Vec<String>>,
        select_answer: Mutex<Option<String>>,
        errors: Mutex<Vec<ExtensionError>>,
    }

    #[async_trait]
    impl HostCallbacks for TestCallbacks {
        fn context_info(&self) -> ContextInfo {
            ContextInfo { cwd: "/tmp".into(), mode: "tui".into(), has_ui: true, model: None, thinking_level: "off".into(), is_idle: true, session_file: None, session_id: "s1".into() }
        }
        async fn ui_select(&self, title: String, options: Vec<String>, _opts: Value) -> Option<String> {
            self.log.lock().unwrap().push(format!("select:{title}:{}", options.join(",")));
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            self.select_answer.lock().unwrap().clone()
        }
        fn ui_notify(&self, message: String, kind: String) {
            self.log.lock().unwrap().push(format!("notify:{kind}:{message}"));
        }
        fn console(&self, level: String, message: String) {
            self.log.lock().unwrap().push(format!("console:{level}:{message}"));
        }
        fn send_message(&self, message: Value, options: Value) {
            self.log.lock().unwrap().push(format!("sendMessage:{}:{}", message["customType"].as_str().unwrap_or(""), options["deliverAs"].as_str().unwrap_or("")));
        }
        fn report_error(&self, err: ExtensionError) {
            self.errors.lock().unwrap().push(err);
        }
    }

    fn examples_dir() -> std::path::PathBuf {
        std::path::PathBuf::from("/private/tmp/claude-501/-Users-chime-Workspace-pirs/425591f1-8df5-454e-8b62-75a3da5c8bd0/scratchpad/pi/packages/coding-agent/examples/extensions")
    }

    async fn host_with(cb: Arc<TestCallbacks>) -> ExtensionHost {
        ExtensionHost::spawn(cb, HostConfig::new("/tmp")).await.expect("host starts")
    }

    #[tokio::test]
    async fn loads_unmodified_pi_examples_and_runs_tool() {
        let dir = examples_dir();
        if !dir.exists() {
            eprintln!("pi checkout not present; skipping");
            return;
        }
        let cb = Arc::new(TestCallbacks::default());
        let host = host_with(cb.clone()).await;
        let loaded = host.load(dir.join("hello.ts").to_string_lossy().to_string()).await.unwrap();
        assert_eq!(loaded.tools.len(), 1);
        assert_eq!(loaded.tools[0].name, "hello");
        assert_eq!(loaded.tools[0].parameters["required"][0], "name");
        let result = host.execute_tool("hello", "c1", json!({"name": "pirs"}), CancellationToken::new(), Arc::new(|_| {})).await.unwrap();
        assert_eq!(result.content[0].as_text().unwrap(), "Hello, pirs!");
        assert_eq!(result.details.unwrap()["greeted"], "pirs");

        // permission-gate: async handler awaiting an async host UI callback
        *cb.select_answer.lock().unwrap() = Some("No".into());
        host.load(dir.join("permission-gate.ts").to_string_lossy().to_string()).await.unwrap();
        let out = host
            .dispatch("tool_call", json!({"type":"tool_call","toolName":"bash","toolCallId":"t1","input":{"command":"sudo rm -rf /"}}), Some(CancellationToken::new()))
            .await
            .unwrap();
        assert_eq!(out.result["block"], true);
        assert_eq!(out.result["reason"], "Blocked by user");
        *cb.select_answer.lock().unwrap() = Some("Yes".into());
        let out = host.dispatch("tool_call", json!({"type":"tool_call","toolName":"bash","toolCallId":"t2","input":{"command":"sudo ls"}}), None).await.unwrap();
        assert!(out.result.is_null());

        // protected-paths blocks writes and notifies
        host.load(dir.join("protected-paths.ts").to_string_lossy().to_string()).await.unwrap();
        let out = host.dispatch("tool_call", json!({"type":"tool_call","toolName":"write","toolCallId":"t3","input":{"path":".env","content":"x"}}), None).await.unwrap();
        assert_eq!(out.result["block"], true);
        assert!(cb.log.lock().unwrap().iter().any(|l| l.starts_with("notify:warning:Blocked write")));

        // dynamic-tools registers a tool during session_start
        host.set_loaded();
        host.load(dir.join("dynamic-tools.ts").to_string_lossy().to_string()).await.unwrap();
        host.dispatch("session_start", json!({"type":"session_start","reason":"startup"}), None).await.unwrap();
        let tools = host.list_tools().await;
        assert!(tools.iter().any(|t| t.name == "echo_session"), "tools: {:?}", tools.iter().map(|t| &t.name).collect::<Vec<_>>());
        let r = host.execute_tool("echo_session", "c2", json!({"message": "hi"}), CancellationToken::new(), Arc::new(|_| {})).await.unwrap();
        assert_eq!(r.content[0].as_text().unwrap(), "[session] hi");
        // and a command that registers more tools
        host.run_command("add-echo-tool", "shout").await.unwrap();
        assert!(host.list_tools().await.iter().any(|t| t.name == "shout"));
        assert!(cb.errors.lock().unwrap().is_empty(), "errors: {:?}", cb.errors.lock().unwrap());
        host.shutdown();
    }

    #[tokio::test]
    async fn typescript_node_shims_timers_and_exec() {
        let cb = Arc::new(TestCallbacks::default());
        let host = host_with(cb.clone()).await;
        let dir = tempfile::tempdir().unwrap();
        let ext = dir.path().join("ext.ts");
        std::fs::write(
            &ext,
            r#"
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import * as fs from "node:fs";
import { join } from "node:path";
import { helper } from "./helper.ts";

interface State { count: number }

export default async function (pi: ExtensionAPI) {
  const state: State = { count: 0 };
  await new Promise((r) => setTimeout(r, 5));
  pi.registerTool({
    name: "fs_tool",
    label: "FS",
    description: "writes a file",
    parameters: Type.Object({ name: Type.String(), n: Type.Optional(Type.Number()) }),
    async execute(_id, params, signal, onUpdate) {
      onUpdate?.({ content: [{ type: "text", text: "working" }] });
      const p = join(process.env.PIRS_TEST_DIR!, params.name);
      fs.writeFileSync(p, helper(params.name));
      const r = await pi.exec("cat", [p]);
      state.count++;
      return { content: [{ type: "text", text: r.stdout }], details: { exists: fs.existsSync(p), count: state.count, aborted: signal?.aborted ?? false } };
    },
  });
  pi.on("tool_result", async (event) => {
    if (event.toolName === "fs_tool") return { details: { ...event.details, patched: true } };
  });
  pi.on("input", async (event) => {
    if (event.text.startsWith("?q ")) return { action: "transform", text: "quick: " + event.text.slice(3) };
    if (event.text === "ping") return { action: "handled" };
  });
  pi.on("before_agent_start", async (event) => ({ message: { customType: "x", content: "ctx", display: false }, systemPrompt: event.systemPrompt + " extra" }));
  pi.on("session_start", async (_e, ctx) => { pi.sendMessage({ customType: "hello", content: "hi", display: true }, { deliverAs: "followUp" }); console.log("started in", ctx.cwd); });
  pi.registerCommand("count", { description: "show count", handler: async (_args, ctx) => { ctx.ui.notify(`count=${state.count}`, "info"); } });
}
"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("helper.ts"), "export function helper(s: string): string { return `content:${s}`; }").unwrap();
        std::env::set_var("PIRS_TEST_DIR", dir.path());
        let loaded = host.load(ext.to_string_lossy().to_string()).await.unwrap();
        assert_eq!(loaded.commands[0].name, "count");
        assert!(loaded.events.contains(&"input".to_string()));
        let updates = Arc::new(Mutex::new(Vec::new()));
        let u2 = updates.clone();
        let r = host.execute_tool("fs_tool", "c1", json!({"name": "a.txt"}), CancellationToken::new(), Arc::new(move |p| u2.lock().unwrap().push(p))).await.unwrap();
        assert_eq!(r.content[0].as_text().unwrap(), "content:a.txt");
        assert_eq!(r.details.as_ref().unwrap()["exists"], true);
        assert_eq!(updates.lock().unwrap().len(), 1);

        let out = host.dispatch("tool_result", json!({"type":"tool_result","toolName":"fs_tool","toolCallId":"c1","input":{},"content":[],"details":{"a":1},"isError":false}), None).await.unwrap();
        assert_eq!(out.result["details"]["patched"], true);
        assert_eq!(out.result["details"]["a"], 1);

        let out = host.dispatch("input", json!({"type":"input","text":"?q hello","source":"interactive"}), None).await.unwrap();
        assert_eq!(out.result["action"], "transform");
        assert_eq!(out.result["text"], "quick: hello");
        let out = host.dispatch("input", json!({"type":"input","text":"ping","source":"interactive"}), None).await.unwrap();
        assert_eq!(out.result["action"], "handled");
        let out = host.dispatch("input", json!({"type":"input","text":"other","source":"interactive"}), None).await.unwrap();
        assert_eq!(out.result["action"], "continue");

        let out = host.dispatch("before_agent_start", json!({"type":"before_agent_start","prompt":"p","systemPrompt":"base"}), None).await.unwrap();
        assert_eq!(out.result["systemPrompt"], "base extra");
        assert_eq!(out.result["messages"][0]["customType"], "x");

        host.dispatch("session_start", json!({"type":"session_start","reason":"startup"}), None).await.unwrap();
        host.run_command("count", "").await.unwrap();
        let log = cb.log.lock().unwrap().clone();
        assert!(log.contains(&"sendMessage:hello:followUp".to_string()), "{log:?}");
        assert!(log.contains(&"console:log:started in /tmp".to_string()), "{log:?}");
        assert!(log.contains(&"notify:info:count=1".to_string()), "{log:?}");
        assert!(cb.errors.lock().unwrap().is_empty(), "errors: {:?}", cb.errors.lock().unwrap());
        host.shutdown();
    }

    #[tokio::test]
    async fn handler_errors_are_reported_not_fatal_and_cancellation_aborts() {
        let cb = Arc::new(TestCallbacks::default());
        let host = host_with(cb.clone()).await;
        let dir = tempfile::tempdir().unwrap();
        let ext = dir.path().join("bad.ts");
        std::fs::write(
            &ext,
            r#"
export default function (pi) {
  pi.on("turn_start", async () => { throw new Error("boom"); });
  pi.registerTool({ name: "slow", description: "", parameters: { type: "object", properties: {} },
    async execute(_id, _p, signal) { await new Promise((res, rej) => { const t = setTimeout(res, 5000); signal?.addEventListener("abort", () => { clearTimeout(t); rej(new Error("aborted!")); }); }); return { content: [{ type: "text", text: "done" }] }; } });
}
"#,
        )
        .unwrap();
        host.load(ext.to_string_lossy().to_string()).await.unwrap();
        host.dispatch("turn_start", json!({"type":"turn_start","turnIndex":0}), None).await.unwrap();
        let errs = cb.errors.lock().unwrap().clone();
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].error, "boom");
        assert_eq!(errs[0].event, "turn_start");

        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            c2.cancel();
        });
        let started = std::time::Instant::now();
        let r = host.execute_tool("slow", "c", json!({}), cancel, Arc::new(|_| {})).await;
        assert!(r.is_err(), "expected abort error, got {r:?}");
        assert!(r.unwrap_err().contains("aborted!"));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));

        // Missing module is a load error, not a crash.
        std::fs::write(dir.path().join("missing.ts"), "import x from 'does-not-exist'; export default function(){}").unwrap();
        let e = host.load(dir.path().join("missing.ts").to_string_lossy().to_string()).await.unwrap_err();
        assert!(e.contains("does-not-exist"), "{e}");
        host.shutdown();
    }
}
