//! Policy at runtime, driven through the real server: every slot of
//! `40-dsl.md` as a loop in a temporary directory actually runs it.
//!
//! Each test writes `.pirs.toml` files into the harness's project directory
//! (or its temporary `PIRS_HOME`), creates a loop there, and asserts on what
//! the model was given, what the session log recorded and what the events
//! carried. Nothing is stubbed: the `run =` strings are real `sh -c`
//! processes.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{Client, Harness, WAIT};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write a project policy file, `<project>/.pirs/ext/<name>`.
fn project_policy(h: &Harness, name: &str, body: &str) -> String {
    let dir = h.project.path().join(".pirs").join("ext");
    std::fs::create_dir_all(&dir).expect("ext dir");
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write policy");
    path.to_string_lossy().into_owned()
}

/// Write a global policy file, `$PIRS_HOME/ext/<name>`.
fn global_policy(h: &Harness, name: &str, body: &str) -> String {
    let dir = h.home.path().join(".pirs").join("ext");
    std::fs::create_dir_all(&dir).expect("ext dir");
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write policy");
    path.to_string_lossy().into_owned()
}

/// Every entry of the loop's session log, in file order.
fn log_entries(h: &Harness) -> Vec<Value> {
    let sessions = h.home.path().join(".pirs").join("sessions");
    let mut files = Vec::new();
    collect_jsonl(&sessions, &mut files);
    assert_eq!(files.len(), 1, "one conversation in {}", sessions.display());
    std::fs::read_to_string(&files[0])
        .expect("read session")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect()
}

fn collect_jsonl(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl(&path, out);
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        }
    }
}

/// The messages of one role in the log.
fn messages_of(entries: &[Value], role: &str) -> Vec<Value> {
    entries
        .iter()
        .filter(|e| e["type"] == "message" && e["message"]["role"] == role)
        .map(|e| e["message"].clone())
        .collect()
}

/// The `custom` entries of one type.
fn customs_of<'a>(entries: &'a [Value], custom_type: &str) -> Vec<&'a Value> {
    entries.iter().filter(|e| e["type"] == "custom" && e["customType"] == custom_type).collect()
}

/// All the text in a message, whatever shape its content has.
fn text_of(message: &Value) -> String {
    match &message["content"] {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join(""),
        _ => String::new(),
    }
}

/// A system message as one string: its content plus every section.
fn system_text(message: &Value) -> String {
    let mut text = text_of(message);
    if let Some(sections) = message["sections"].as_object() {
        for value in sections.values() {
            if let Some(section) = value.as_str() {
                text.push('\n');
                text.push_str(section);
            }
        }
    }
    text
}

/// Wait for a file to appear (an `[[on]]` entry is fire and forget, so its
/// effect lands shortly after the event, not before it).
async fn wait_for(path: &Path) -> bool {
    let deadline = tokio::time::Instant::now() + WAIT;
    while tokio::time::Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// Create a loop, subscribe, run one prompt to its end, return the events.
async fn one_run(c: &mut Client, cwd: &str, text: &str) -> (String, Vec<(String, Value)>) {
    let info = c.create(cwd, json!({})).await;
    let id = info["id"].as_str().expect("loop id").to_owned();
    c.subscribe(&id, None).await;
    c.call("loop.prompt", json!({"loop": id, "text": text})).await;
    let events = c.events_until("loop.run_end").await;
    (id, events)
}

// ---------------------------------------------------------------------------
// input
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_input_entry_rewrites_the_text_the_model_receives() {
    let h = Harness::start(WAIT, vec![]).await;
    project_policy(
        &h,
        "shortcuts.pirs.toml",
        "intent = 'brief answers'\n[[input]]\nmatch = '^\\?(.*)'\nreplace = 'Explain briefly: $1'\n",
    );
    let mut c = h.client().await;
    let (_, events) = one_run(&mut c, &h.cwd(), "?why").await;

    let user: Vec<String> = events
        .iter()
        .filter(|(m, p)| m == "loop.message" && p["role"] == "user" && p.get("message").is_some())
        .map(|(_, p)| text_of(&p["message"]))
        .collect();
    assert_eq!(user, ["Explain briefly: why"], "the rewritten text is what the run started from");

    // The faux provider echoes its last user message, so the answer proves
    // the rewrite reached the model and not just the log.
    let assistant: String = events
        .iter()
        .filter(|(m, p)| m == "loop.message" && p["role"] == "assistant" && p.get("message").is_some())
        .map(|(_, p)| text_of(&p["message"]))
        .collect();
    assert_eq!(assistant, "(faux) Explain briefly: why");
    h.stop().await;
}

#[tokio::test]
async fn a_handled_input_runs_a_command_and_never_reaches_the_model() {
    let h = Harness::start(WAIT, vec![]).await;
    project_policy(&h, "bang.pirs.toml", "intent = 'shell shortcut'\n[[input]]\nmatch = '^!(.*)'\nhandled = true\nrun = '$1'\n");
    let mut c = h.client().await;
    let (_, events) = one_run(&mut c, &h.cwd(), "!echo hi").await;

    assert!(
        !events.iter().any(|(m, p)| m == "loop.message" && p["role"] == "assistant"),
        "no model call for a consumed input: {events:?}"
    );
    let entries = log_entries(&h);
    let bash = messages_of(&entries, "bashExecution");
    assert_eq!(bash.len(), 1, "one bashExecution: {entries:#?}");
    assert_eq!(bash[0]["command"], "echo hi", "the interpolated command, not the pattern");
    assert_eq!(bash[0]["output"], "hi");
    assert_eq!(bash[0]["exitCode"], 0);
    h.stop().await;
}

#[tokio::test]
async fn a_command_is_an_input_entry_with_args_and_a_manifest_row() {
    let h = Harness::start(WAIT, vec![]).await;
    project_policy(
        &h,
        "handoff.pirs.toml",
        "intent = 'handoff'\n[[command]]\nname = 'handoff'\ndescription = 'Start a fresh session'\nrun = 'echo handoff-ran $args'\n",
    );
    let mut c = h.client().await;
    let info = c.create(&h.cwd(), json!({})).await;
    let id = info["id"].as_str().unwrap().to_owned();
    let attach = c.call("loop.attach", json!({"loop": id})).await;
    assert_eq!(attach["manifest"]["commands"], json!([{"name": "handoff", "description": "Start a fresh session"}]));

    c.subscribe(&id, None).await;
    c.call("loop.prompt", json!({"loop": id, "text": "/handoff notes.md"})).await;
    c.events_until("loop.run_end").await;

    let entries = log_entries(&h);
    let bash = messages_of(&entries, "bashExecution");
    assert_eq!(bash.len(), 1);
    assert_eq!(bash[0]["output"], "handoff-ran notes.md", "$args is the text after the command name");
    assert!(messages_of(&entries, "assistant").is_empty(), "a command never reaches the model");
    h.stop().await;
}

#[tokio::test]
async fn a_slow_input_run_is_skipped_with_a_warning_and_the_turn_goes_on() {
    let h = Harness::start(Duration::from_secs(30), vec![]).await;
    project_policy(&h, "slow.pirs.toml", "intent = 'slow'\n[[input]]\nmatch = '^(.*)'\nrun = 'sleep 6; echo too-late'\n");
    let mut c = h.client().await;
    let started = tokio::time::Instant::now();
    let (_, events) = one_run(&mut c, &h.cwd(), "hello").await;
    let elapsed = started.elapsed();

    assert!(elapsed < Duration::from_secs(10), "the 5 s timeout bit, not the process: {elapsed:?}");
    let warnings: Vec<String> = events
        .iter()
        .filter(|(m, p)| m == "ui.notify" && p["level"] == "warning")
        .map(|(_, p)| p["text"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(warnings.iter().any(|w| w.contains("timed out")), "a warning naming the timeout: {warnings:?}");
    let assistant: String = events
        .iter()
        .filter(|(m, p)| m == "loop.message" && p["role"] == "assistant" && p.get("message").is_some())
        .map(|(_, p)| text_of(&p["message"]))
        .collect();
    assert_eq!(assistant, "(faux) hello", "the text the entry had no opinion about");
    h.stop().await;
}

// ---------------------------------------------------------------------------
// prompt
// ---------------------------------------------------------------------------

#[tokio::test]
async fn prompt_entries_are_visible_in_dsl_check_and_in_the_logged_system_message() {
    let h = Harness::start(WAIT, vec![]).await;
    std::fs::create_dir_all(h.project.path().join("rules")).expect("rules dir");
    std::fs::write(h.project.path().join("rules").join("style.md"), "Prefer small commits.\n").expect("rule");
    project_policy(
        &h,
        "prompt.pirs.toml",
        "intent = 'standing instructions'\n\
         [[prompt]]\ntext = 'Never rewrite history.'\n\
         [[prompt]]\nfiles = 'rules/*.md'\n\
         [[prompt]]\nrun = 'echo on-branch-main'\nheader = 'Branch:'\n",
    );
    let mut c = h.client().await;

    let check = c.call("dsl.check", json!({"cwd": h.cwd()})).await;
    let prompt = check["system_prompt"].as_str().expect("a prompt").to_owned();
    assert!(prompt.contains("<policy>"), "the section is tagged so it can be seen: {prompt}");
    assert!(prompt.contains("Never rewrite history."));
    assert!(prompt.contains("<file path=") && prompt.contains("Prefer small commits."));
    assert!(prompt.contains("Branch:\non-branch-main"), "the header sits above the output: {prompt}");
    assert_eq!(check["conflicts"], json!([]));
    assert_eq!(check["files"].as_array().map(Vec::len), Some(1));

    let (_, _) = one_run(&mut c, &h.cwd(), "hi").await;
    let entries = log_entries(&h);
    let system = messages_of(&entries, "system");
    assert_eq!(system.len(), 1, "one system message for the run");
    let text = system_text(&system[0]);
    assert!(text.contains("Never rewrite history."), "the log shows what was injected (D-21): {text}");
    assert!(text.contains("on-branch-main"));
    h.stop().await;
}

// ---------------------------------------------------------------------------
// tool_result
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_tool_result_entry_rewrites_the_result_and_the_original_stays_in_the_log() {
    let script = vec![json!({"text": "looking", "toolCalls": [{"name": "bash", "arguments": {"command": "echo raw-output"}}]}), json!("done")];
    let h = Harness::start(WAIT, script).await;
    project_policy(
        &h,
        "trim.pirs.toml",
        "intent = 'trim bash output'\n[[tool_result]]\ntool = 'bash'\nrun = 'echo \"rewritten: $PIRS_ARG_tool\"'\n",
    );
    let mut c = h.client().await;
    let (_, events) = one_run(&mut c, &h.cwd(), "go").await;

    let results: Vec<String> = events
        .iter()
        .filter(|(m, p)| m == "loop.message" && p["role"] == "toolResult" && p.get("message").is_some())
        .map(|(_, p)| text_of(&p["message"]))
        .collect();
    assert_eq!(results, ["rewritten: bash"], "the model reads the rewrite");

    let entries = log_entries(&h);
    let rewrites = customs_of(&entries, "pirs.tool_result_rewrite");
    assert_eq!(rewrites.len(), 1, "one rewrite recorded: {entries:#?}");
    let data = &rewrites[0]["data"];
    assert_eq!(data["tool"], "bash");
    assert!(data["by"].as_str().unwrap_or_default().contains("[[tool_result]]"), "named by origin: {data}");
    assert!(
        serde_json::to_string(&data["original"]).unwrap().contains("raw-output"),
        "the original is next to it (D-21): {data}"
    );
    h.stop().await;
}

// ---------------------------------------------------------------------------
// on, status, widget
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_on_turn_end_entry_runs_after_the_turn() {
    let h = Harness::start(WAIT, vec![]).await;
    let touched = h.project.path().join("touched");
    project_policy(&h, "hook.pirs.toml", "intent = 'checkpoint'\n[[on]]\nevent = 'turn_end'\nrun = 'touch touched'\nquiet = true\n");
    let mut c = h.client().await;
    one_run(&mut c, &h.cwd(), "hi").await;
    assert!(wait_for(&touched).await, "the turn_end entry ran in the loop's cwd");
    h.stop().await;
}

#[tokio::test]
async fn status_and_widget_entries_emit_their_keys_and_are_in_the_manifest() {
    let h = Harness::start(WAIT, vec![]).await;
    std::fs::write(h.project.path().join("todo.md"), "one\ntwo\n").expect("todo");
    project_policy(
        &h,
        "ui.pirs.toml",
        "intent = 'status and widget'\n\
         [[status]]\nkey = 'branch'\nrun = 'echo main'\non = ['start', 'turn_end']\n\
         [[widget]]\nkey = 'todo'\nfile = 'todo.md'\non = ['start']\n",
    );
    let mut c = h.client().await;
    let info = c.create(&h.cwd(), json!({})).await;
    let id = info["id"].as_str().unwrap().to_owned();
    let attach = c.call("loop.attach", json!({"loop": id})).await;
    assert_eq!(attach["manifest"]["status_keys"], json!(["branch"]));
    assert_eq!(attach["manifest"]["widget_keys"], json!(["todo"]));

    c.subscribe(&id, Some(0)).await;
    c.call("loop.prompt", json!({"loop": id, "text": "hi"})).await;
    let events = c.events_until("loop.run_end").await;
    let status: Vec<&Value> = events.iter().filter(|(m, _)| m == "ui.status").map(|(_, p)| p).collect();
    assert!(status.iter().any(|p| p["key"] == "branch" && p["text"] == "main"), "a status event: {events:?}");
    let widget: Vec<&Value> = events.iter().filter(|(m, _)| m == "ui.widget").map(|(_, p)| p).collect();
    assert!(widget.iter().any(|p| p["key"] == "todo" && p["lines"] == json!(["one", "two"])), "a widget: {events:?}");
    h.stop().await;
}

#[tokio::test]
async fn closing_a_loop_kills_the_process_group_an_on_start_entry_left_behind() {
    let h = Harness::start(WAIT, vec![]).await;
    let pidfile = h.project.path().join("child.pid");
    // The shell exits at once; its background grandchild is what has to die.
    // Single quotes inside, so `$$` is the *inner* shell's pid: the one that
    // outlives the `sh -c` the server started.
    project_policy(
        &h,
        "watch.pirs.toml",
        "intent = 'a watcher'\n[[on]]\nevent = 'start'\nrun = \"\"\"sh -c 'echo $$ > child.pid; sleep 300' &\"\"\"\nquiet = true\n",
    );
    let mut c = h.client().await;
    let info = c.create(&h.cwd(), json!({})).await;
    let id = info["id"].as_str().unwrap().to_owned();
    assert!(wait_for(&pidfile).await, "the start entry ran");
    let pid: i32 = std::fs::read_to_string(&pidfile).expect("pid").trim().parse().expect("a pid");
    assert!(alive(pid), "the grandchild is running");

    c.call("loop.close", json!({"loop": id})).await;
    let deadline = tokio::time::Instant::now() + WAIT;
    while alive(pid) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!alive(pid), "loop.close killed the whole process group (D-23)");
    h.stop().await;
}

/// Whether a pid still exists (`kill -0`).
///
/// D-09's ban is on the *server* spawning children outside `process`; this is
/// the test looking at one it already spawned.
#[allow(clippy::disallowed_methods)]
fn alive(pid: i32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// tool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_declared_tool_is_called_with_its_arguments_as_variables_and_environment() {
    let script = vec![
        json!({"text": "fetching", "toolCalls": [{"name": "fetch", "arguments": {"url": "https://example.test/page"}}]}),
        json!("done"),
    ];
    let h = Harness::start(WAIT, script).await;
    project_policy(
        &h,
        "fetch.pirs.toml",
        "intent = 'a fetch tool'\n[[tool]]\nname = 'fetch'\ndescription = 'Fetch a URL'\n\
         params.url = { type = 'string', description = 'Absolute URL' }\n\
         run = 'echo \"got $url via $PIRS_ARG_url in $PIRS_SLOT\"'\ntimeout = 10\n",
    );
    let mut c = h.client().await;
    let info = c.create(&h.cwd(), json!({})).await;
    let id = info["id"].as_str().unwrap().to_owned();
    let attach = c.call("loop.attach", json!({"loop": id})).await;
    let tools = attach["manifest"]["tools"].as_array().expect("tools").clone();
    let fetch = tools.iter().find(|t| t["name"] == "fetch").expect("fetch is in the manifest");
    assert_eq!(fetch["description"], "Fetch a URL");
    assert_eq!(fetch["parameters"]["required"], json!(["url"]));

    c.subscribe(&id, None).await;
    c.call("loop.prompt", json!({"loop": id, "text": "go"})).await;
    let events = c.events_until("loop.run_end").await;
    let results: Vec<String> = events
        .iter()
        .filter(|(m, p)| m == "loop.message" && p["role"] == "toolResult" && p.get("message").is_some())
        .map(|(_, p)| text_of(&p["message"]))
        .collect();
    assert_eq!(
        results,
        ["got https://example.test/page via https://example.test/page in tool.fetch"],
        "both spellings of an argument reach the shell (D-24)"
    );
    h.stop().await;
}

#[tokio::test]
async fn disabled_removes_a_builtin_and_wrap_replaces_what_it_runs() {
    let script = vec![json!({"text": "running", "toolCalls": [{"name": "bash", "arguments": {"command": "echo real"}}]}), json!("done")];
    let h = Harness::start(WAIT, script).await;
    project_policy(&h, "off.pirs.toml", "intent = 'no grep'\n[[tool]]\nname = 'grep'\ndisabled = true\n");
    project_policy(
        &h,
        "wrap.pirs.toml",
        "intent = 'sandboxed bash'\n[[tool]]\nname = 'bash'\nwrap = 'echo \"wrapped: $command\"'\n",
    );
    let mut c = h.client().await;
    let info = c.create(&h.cwd(), json!({})).await;
    let id = info["id"].as_str().unwrap().to_owned();
    let attach = c.call("loop.attach", json!({"loop": id})).await;
    let tools = attach["manifest"]["tools"].as_array().expect("tools").clone();
    assert!(!tools.iter().any(|t| t["name"] == "grep"), "a disabled built-in is gone");
    let bash = tools.iter().find(|t| t["name"] == "bash").expect("bash is still offered");
    assert!(bash["description"].as_str().unwrap_or_default().contains("command"), "a wrap keeps the built-in's own description: {bash}");

    c.subscribe(&id, None).await;
    c.call("loop.prompt", json!({"loop": id, "text": "go"})).await;
    let events = c.events_until("loop.run_end").await;
    let results: Vec<String> = events
        .iter()
        .filter(|(m, p)| m == "loop.message" && p["role"] == "toolResult" && p.get("message").is_some())
        .map(|(_, p)| text_of(&p["message"]))
        .collect();
    assert_eq!(results, ["wrapped: echo real"], "the wrapper ran instead of the built-in");
    h.stop().await;
}

// ---------------------------------------------------------------------------
// reload
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_loops_own_write_of_a_policy_file_shapes_its_very_next_request() {
    let body = "intent = 'a marker'\\n[[prompt]]\\ntext = \"MARKER-42\"\\n";
    let script = vec![
        json!({"text": "writing", "toolCalls": [{"name": "write", "arguments": {"path": ".pirs/ext/new.pirs.toml", "content": body.replace("\\n", "\n")}}]}),
        json!("written"),
    ];
    let h = Harness::start(WAIT, script).await;
    let mut c = h.client().await;
    let (_, events) = one_run(&mut c, &h.cwd(), "give yourself a marker").await;
    assert!(events.iter().any(|(m, _)| m == "fs.changed"), "the write was seen");

    let entries = log_entries(&h);
    let system = messages_of(&entries, "system");
    assert!(
        system.iter().any(|m| system_text(m).contains("MARKER-42")),
        "the policy the loop wrote is in the prompt of its next request (D-33): {system:#?}"
    );
    h.stop().await;
}

#[tokio::test]
async fn loop_reload_returns_the_files_and_starts_only_the_new_on_start_entries() {
    let h = Harness::start(WAIT, vec![]).await;
    let first = h.project.path().join("first");
    let second = h.project.path().join("second");
    project_policy(
        &h,
        "a.pirs.toml",
        "intent = 'first'\n[[on]]\nevent = 'start'\nrun = 'echo x >> first'\nquiet = true\n",
    );
    let mut c = h.client().await;
    let info = c.create(&h.cwd(), json!({})).await;
    let id = info["id"].as_str().unwrap().to_owned();
    assert!(wait_for(&first).await, "the start entry ran at create");

    project_policy(
        &h,
        "b.pirs.toml",
        "intent = 'second'\n[[on]]\nevent = 'start'\nrun = 'echo x >> second'\nquiet = true\n",
    );
    let reloaded = c.call("loop.reload", json!({"loop": id})).await;
    let files: Vec<String> =
        reloaded["files"].as_array().expect("files").iter().map(|f| f.as_str().unwrap_or_default().to_owned()).collect();
    assert_eq!(files.len(), 2, "both policy files: {files:?}");
    assert!(files.iter().any(|f| f.ends_with("a.pirs.toml")) && files.iter().any(|f| f.ends_with("b.pirs.toml")));

    assert!(wait_for(&second).await, "the new start entry ran");
    // Give the old one every chance to run a second time before saying it did not.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(std::fs::read_to_string(&first).expect("first").lines().count(), 1, "an entry that is not new is not restarted");
    h.stop().await;
}

// ---------------------------------------------------------------------------
// errors
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_broken_file_warns_and_leaves_the_others_working() {
    let h = Harness::start(WAIT, vec![]).await;
    global_policy(&h, "broken.pirs.toml", "this is not toml at all [[[\n");
    project_policy(
        &h,
        "good.pirs.toml",
        "intent = 'brief answers'\n[[input]]\nmatch = '^\\?(.*)'\nreplace = 'Explain briefly: $1'\n",
    );
    let mut c = h.client().await;

    let check = c.call("dsl.check", json!({"cwd": h.cwd()})).await;
    let conflicts = check["conflicts"].as_array().expect("conflicts").clone();
    assert!(!conflicts.is_empty(), "the broken file is reported");
    assert!(conflicts[0]["files"][0].as_str().unwrap_or_default().ends_with("broken.pirs.toml"));

    let (_, events) = one_run(&mut c, &h.cwd(), "?why").await;
    let warnings: Vec<String> = events
        .iter()
        .filter(|(m, p)| m == "ui.notify" && p["level"] == "warning")
        .map(|(_, p)| p["text"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert!(warnings.iter().any(|w| w.contains("broken.pirs.toml")), "one warning per problem: {warnings:?}");
    let assistant: String = events
        .iter()
        .filter(|(m, p)| m == "loop.message" && p["role"] == "assistant" && p.get("message").is_some())
        .map(|(_, p)| text_of(&p["message"]))
        .collect();
    assert_eq!(assistant, "(faux) Explain briefly: why", "the file that did load still works");
    h.stop().await;
}

#[tokio::test]
async fn a_duplicate_key_across_two_files_is_a_conflict_naming_both() {
    let h = Harness::start(WAIT, vec![]).await;
    let a = global_policy(&h, "a.pirs.toml", "intent = 'a'\n[[status]]\nkey = 'branch'\nrun = 'echo a'\non = ['start']\n");
    let b = project_policy(&h, "b.pirs.toml", "intent = 'b'\n[[status]]\nkey = 'branch'\nrun = 'echo b'\non = ['start']\n");
    let mut c = h.client().await;
    let check = c.call("dsl.check", json!({"cwd": h.cwd()})).await;
    let conflicts = check["conflicts"].as_array().expect("conflicts").clone();
    assert_eq!(conflicts.len(), 1, "{conflicts:?}");
    assert!(conflicts[0]["message"].as_str().unwrap_or_default().contains("duplicate status key `branch`"));
    let files: Vec<&str> = conflicts[0]["files"].as_array().unwrap().iter().map(|f| f.as_str().unwrap()).collect();
    assert_eq!(files, [a.as_str(), b.as_str()], "both files are named");
    h.stop().await;
}

// ---------------------------------------------------------------------------
// settings
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_settings_table_picks_the_model_and_the_tool_set() {
    let h = Harness::start(WAIT, vec![]).await;
    project_policy(
        &h,
        "settings.pirs.toml",
        "intent = 'defaults'\n[settings]\nmodel = 'faux/scripted'\ntools = ['read']\ntool_execution = 'sequential'\n",
    );
    let mut c = h.client().await;
    // No `model` in `loop.create`: the policy's is what the loop starts on.
    let info = c.call("loop.create", json!({"cwd": h.cwd()})).await;
    assert_eq!(info["model"]["model"], "faux/scripted");
    let id = info["id"].as_str().unwrap().to_owned();
    let attach = c.call("loop.attach", json!({"loop": id})).await;
    let tools: Vec<&str> =
        attach["manifest"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(tools.contains(&"bash"), "the manifest lists what exists, not what is selected");

    let check = c.call("dsl.check", json!({"cwd": h.cwd()})).await;
    let prompt = check["system_prompt"].as_str().unwrap();
    assert!(prompt.contains("- read:"), "the prompt's tool list follows `[settings] tools`: {prompt}");
    assert!(!prompt.contains("- bash:"));
    h.stop().await;
}
