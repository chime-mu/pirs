//! Phase 4: executables and connected handlers, driven through the real
//! server (S5, S7, S8, S9).
//!
//! Every `run =` here names a real file with the executable bit set — Python
//! at `/usr/bin/python3`, or `sh` — spawned directly by the server, fed one
//! JSON line on stdin and read for one JSON line on stdout. The connected
//! process in the watcher test opens `PIRS_SOCKET` itself and speaks the
//! protocol from Python, as an extension in any language would.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{Client, Harness, WAIT};
use pirs_protocol::{code, PROTOCOL_VERSION};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Write a project policy file, `<project>/.pirs/ext/<name>`.
fn project_policy(h: &Harness, name: &str, body: &str) {
    let dir = h.project.path().join(".pirs").join("ext");
    std::fs::create_dir_all(&dir).expect("ext dir");
    std::fs::write(dir.join(name), body).expect("write policy");
}

/// Write an executable script at `<project>/<rel>` (mode 755).
fn write_exec(h: &Harness, rel: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    let path = h.project.path().join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("script dir");
    }
    std::fs::write(&path, body).expect("write script");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// A Python script that reads the payload line, binds it as `req`, and runs
/// `body`.
fn python(body: &str) -> String {
    format!("#!/usr/bin/env python3\nimport json, os, sys\nreq = json.loads(sys.stdin.readline())\n{body}\n")
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

fn messages_of(entries: &[Value], role: &str) -> Vec<Value> {
    entries
        .iter()
        .filter(|e| e["type"] == "message" && e["message"]["role"] == role)
        .map(|e| e["message"].clone())
        .collect()
}

fn customs_of<'a>(entries: &'a [Value], custom_type: &str) -> Vec<&'a Value> {
    entries.iter().filter(|e| e["type"] == "custom" && e["customType"] == custom_type).collect()
}

fn text_of(message: &Value) -> String {
    match &message["content"] {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join(""),
        _ => String::new(),
    }
}

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

/// The messages of one role among a run's events.
fn role_messages(events: &[(String, Value)], role: &str) -> Vec<Value> {
    events
        .iter()
        .filter(|(m, p)| m == "loop.message" && p["role"] == role && p.get("message").is_some())
        .map(|(_, p)| p["message"].clone())
        .collect()
}

fn warnings(events: &[(String, Value)]) -> Vec<String> {
    events
        .iter()
        .filter(|(m, p)| m == "ui.notify" && p["level"] == "warning")
        .map(|(_, p)| p["text"].as_str().unwrap_or_default().to_owned())
        .collect()
}

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

/// Whether a pid still exists (`kill -0`). D-09's ban is on the *server*
/// spawning outside `process`; this is the test looking at what it spawned.
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

/// Wait up to `within` for every pid to be gone.
async fn all_dead(pids: &[i32], within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    while tokio::time::Instant::now() < deadline {
        if pids.iter().all(|pid| !alive(*pid)) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    pids.iter().all(|pid| !alive(*pid))
}

async fn create_and_subscribe(c: &mut Client, cwd: &str) -> String {
    let info = c.create(cwd, json!({})).await;
    let id = info["id"].as_str().expect("loop id").to_owned();
    c.subscribe(&id, None).await;
    id
}

// ---------------------------------------------------------------------------
// 1. An executable tool
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_executable_tool_reads_the_call_on_stdin_and_its_reply_reaches_the_model() {
    let page = "hello from the page\n";
    // One scripted call, then echo mode: the model's next message quotes
    // the tool result, which is how we know it arrived.
    let script = [json!({"text": "fetching", "toolCalls": [{"name": "fetch", "arguments": {"url": "file://PAGE"}}]})];
    let h = Harness::start(WAIT, vec![]).await;
    std::fs::write(h.project.path().join("page.txt"), page).expect("page");
    let url = format!("file://{}", h.project.path().join("page.txt").display());
    let script: Vec<Value> = script.iter().map(|s| serde_json::from_str(&s.to_string().replace("file://PAGE", &url)).unwrap()).collect();
    pi_ai::faux::set_script(script);
    write_exec(
        &h,
        "tools/fetch.py",
        &python(
            "import urllib.request\n\
             with urllib.request.urlopen(req['args']['url']) as r:\n    text = r.read().decode()\n\
             details = {'has_arg': 'PIRS_ARG_url' in os.environ, 'loop': os.environ.get('PIRS_LOOP'), 'slot': os.environ.get('PIRS_SLOT'), 'id': req['id']}\n\
             print(json.dumps({'content': text, 'details': details}))",
        ),
    );
    project_policy(
        &h,
        "fetch.pirs.toml",
        "intent = 'fetch'\n[[tool]]\nname = 'fetch'\ndescription = 'Fetch a URL'\n\
         params.url = { type = 'string' }\nrun = './tools/fetch.py'\ntimeout = 20\n",
    );
    let mut c = h.client().await;
    let id = create_and_subscribe(&mut c, &h.cwd()).await;
    c.call("loop.prompt", json!({"loop": id, "text": "go"})).await;
    let events = c.events_until("loop.run_end").await;

    let results = role_messages(&events, "toolResult");
    assert_eq!(results.len(), 1, "{events:?}");
    assert_eq!(text_of(&results[0]), page, "the executable's `content` is the result");
    assert_eq!(results[0]["isError"], false);
    let details = &results[0]["details"];
    assert_eq!(details["has_arg"], false, "no PIRS_ARG_* for an executable: {details}");
    assert_eq!(details["loop"], id, "PIRS_LOOP is set");
    assert_eq!(details["slot"], "tool.fetch");
    assert!(details["id"].as_str().unwrap_or_default().starts_with("faux_call_"), "the call id is on stdin: {details}");

    let assistant = role_messages(&events, "assistant");
    let last = text_of(assistant.last().expect("a reply after the tool"));
    assert_eq!(last, format!("(faux) Tool fetch returned: {page}"), "the model read the result");
    h.stop().await;
}

// ---------------------------------------------------------------------------
// 2. Executable input, prompt and tool_result handlers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn executable_input_prompt_and_tool_result_handlers_reply_in_json_and_show_in_the_log() {
    let script = vec![json!({"text": "running", "toolCalls": [{"name": "bash", "arguments": {"command": "echo raw"}}]}), json!("done")];
    let h = Harness::start(WAIT, script).await;
    write_exec(&h, "hooks/swallow.py", &python("print(json.dumps({'handled': True}))"));
    write_exec(&h, "hooks/brief.py", &python("print(json.dumps({'text': 'Explain briefly: ' + req['text'][1:]}))"));
    write_exec(
        &h,
        "hooks/append.py",
        &python("saw = '<cwd>' in req['system_prompt']\nprint(json.dumps({'append': 'SAW-PROMPT:' + str(saw)}))"),
    );
    write_exec(
        &h,
        "hooks/upper.py",
        &python(
            "text = ''.join(b['text'] for b in req['result']['content'])\n\
             print(json.dumps({'result': {'content': 'UPPER(' + req['tool'] + '): ' + text.strip().upper()}}))",
        ),
    );
    project_policy(
        &h,
        "hooks.pirs.toml",
        "intent = 'executable hooks'\n\
         [[input]]\nmatch = '^swallow'\nrun = './hooks/swallow.py'\n\
         [[input]]\nmatch = '^\\?'\nrun = './hooks/brief.py'\n\
         [[prompt]]\nrun = './hooks/append.py'\nheader = 'From the hook:'\n\
         [[tool_result]]\ntool = 'bash'\nrun = './hooks/upper.py'\n",
    );
    let mut c = h.client().await;
    let id = create_and_subscribe(&mut c, &h.cwd()).await;
    c.call("loop.prompt", json!({"loop": id, "text": "?why"})).await;
    let events = c.events_until("loop.run_end").await;
    assert!(warnings(&events).is_empty(), "{:?}", warnings(&events));

    let user = role_messages(&events, "user");
    assert_eq!(text_of(&user[0]), "Explain briefly: why", "`{{ text }}` from the executable is what the run started from");

    let results = role_messages(&events, "toolResult");
    assert_eq!(text_of(&results[0]), "UPPER(bash): RAW", "the model reads the executable's `{{ result }}`");

    let entries = log_entries(&h);
    let system = messages_of(&entries, "system");
    assert_eq!(system.len(), 1);
    let text = system_text(&system[0]);
    assert!(text.contains("SAW-PROMPT:True"), "the executable saw the assembled prompt and its `append` is logged: {text}");
    assert!(text.contains("[[prompt]] #1"), "the appended block names its entry (D-21): {text}");
    assert!(
        text.contains("From the hook:\nSAW-PROMPT:True"),
        "`header` is the first line of an executable's block too: {text}"
    );
    let rewrites = customs_of(&entries, "pirs.tool_result_rewrite");
    assert_eq!(rewrites.len(), 1);
    assert!(rewrites[0]["data"]["by"].as_str().unwrap_or_default().contains("[[tool_result]]"));
    assert!(serde_json::to_string(&rewrites[0]["data"]["original"]).unwrap().contains("raw"), "the original is kept");

    // `{ handled: true }` from an executable consumes the input.
    c.call("loop.prompt", json!({"loop": id, "text": "swallow me"})).await;
    let events = c.events_until("loop.run_end").await;
    assert!(role_messages(&events, "user").is_empty(), "consumed: {events:?}");
    let idle = events.iter().find(|(m, p)| m == "loop.status" && p["state"] == "idle").expect("idle");
    assert_eq!(idle.1["detail"], "handled");
    h.stop().await;
}

// ---------------------------------------------------------------------------
// 3. Bad replies and non-zero exits
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_malformed_reply_is_no_opinion_with_a_warning_and_a_failing_tool_is_an_error_the_model_sees() {
    let script = vec![json!({"text": "calling", "toolCalls": [{"name": "boom", "arguments": {}}]}), json!("done")];
    let h = Harness::start(WAIT, script).await;
    write_exec(&h, "hooks/bad.py", &python("print('this is not json')"));
    write_exec(&h, "tools/boom.py", &python("sys.stderr.write('boom\\n')\nsys.exit(2)"));
    project_policy(
        &h,
        "bad.pirs.toml",
        "intent = 'bad replies'\n\
         [[input]]\nmatch = '^(.*)'\nrun = './hooks/bad.py'\n\
         [[tool]]\nname = 'boom'\ndescription = 'always fails'\nparams = {}\nrun = './tools/boom.py'\n",
    );
    let mut c = h.client().await;
    let id = create_and_subscribe(&mut c, &h.cwd()).await;
    c.call("loop.prompt", json!({"loop": id, "text": "hello"})).await;
    let events = c.events_until("loop.run_end").await;

    let warned = warnings(&events);
    assert_eq!(warned.len(), 1, "one warning, for the input entry: {warned:?}");
    assert!(warned[0].contains("bad.pirs.toml: [[input]] #1"), "names the entry: {warned:?}");
    assert!(warned[0].contains("not one JSON line in the `input` reply shape"), "names the parse error: {warned:?}");
    assert_eq!(text_of(&role_messages(&events, "user")[0]), "hello", "no opinion: the text passed untouched");

    let results = role_messages(&events, "toolResult");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["isError"], true, "a non-zero exit is an error result: {}", results[0]);
    assert_eq!(text_of(&results[0]), "boom", "stderr is the message");
    h.stop().await;
}

// ---------------------------------------------------------------------------
// 4. A declared tool served by a connected handler
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_declared_tool_is_in_the_manifest_and_is_served_by_whoever_registers_it() {
    let call = json!({"text": "fetching", "toolCalls": [{"name": "fetch", "arguments": {"url": "https://x"}}]});
    let h = Harness::start(WAIT, vec![call.clone(), json!("first"), call, json!("second")]).await;
    project_policy(
        &h,
        "fetch.pirs.toml",
        "intent = 'a declared fetch'\n[[tool]]\nname = 'fetch'\ndescription = 'Fetch a URL'\n\
         params.url = { type = 'string', description = 'Absolute URL' }\n",
    );
    let mut c = h.client().await;
    let id = create_and_subscribe(&mut c, &h.cwd()).await;
    let manifest_fetch = |attach: &Value| {
        let tools = attach["manifest"]["tools"].as_array().expect("tools").clone();
        let found: Vec<&Value> = tools.iter().filter(|t| t["name"] == "fetch").collect();
        assert_eq!(found.len(), 1, "one fetch in the manifest: {tools:?}");
        found[0].clone()
    };
    let attach = c.call("loop.attach", json!({"loop": id})).await;
    let fetch = manifest_fetch(&attach);
    assert_eq!(fetch["description"], "Fetch a URL");
    assert_eq!(fetch["parameters"]["required"], json!(["url"]), "the declaration's schema: {fetch}");

    // Nobody has registered: the model sees an error result.
    c.call("loop.prompt", json!({"loop": id, "text": "go"})).await;
    let events = c.events_until("loop.run_end").await;
    let results = role_messages(&events, "toolResult");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["isError"], true);
    assert_eq!(text_of(&results[0]), "no handler registered for tool `fetch`");

    // A raw client registers and answers; the declaration keeps its schema.
    let mut handler = h.client().await;
    handler.call("register", json!({"loop": &id, "slot": "tool.fetch", "timeout": 5000})).await;
    let attach = c.call("loop.attach", json!({"loop": id})).await;
    let fetch = manifest_fetch(&attach);
    assert_eq!(fetch["parameters"]["required"], json!(["url"]), "a registration takes the declaration, not the placeholder: {fetch}");
    assert_eq!(fetch["description"], "Fetch a URL");

    c.call("loop.prompt", json!({"loop": id, "text": "again"})).await;
    let req = handler.slot_request().await;
    assert_eq!(req.method, "tool.fetch");
    assert_eq!(req.params["args"]["url"], "https://x");
    handler.reply(req.id, json!({"content": "served by the handler", "details": {"status": 200}})).await;
    let events = c.events_until("loop.run_end").await;
    let results = role_messages(&events, "toolResult");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["isError"], false);
    assert_eq!(text_of(&results[0]), "served by the handler", "the answer reaches the model");
    assert_eq!(results[0]["details"]["status"], 200);
    h.stop().await;
}

// ---------------------------------------------------------------------------
// 4b. A registration never takes a name the file already defines (D-22)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn registering_a_tool_the_file_defines_or_a_built_in_is_refused() {
    let h = Harness::start(WAIT, vec![]).await;
    write_exec(&h, "tools/fetch.py", &python("print(json.dumps({'content': 'from the script'}))"));
    project_policy(
        &h,
        "fetch.pirs.toml",
        "intent = 'a fetch that runs'\n[[tool]]\nname = 'fetch'\ndescription = 'Fetch a URL'\n\
         params.url = { type = 'string' }\nrun = './tools/fetch.py'\n",
    );
    let mut c = h.client().await;
    let id = create_and_subscribe(&mut c, &h.cwd()).await;

    let refused = |slot: &str| json!({"loop": &id, "slot": slot, "timeout": 5000});
    let error = c.request("register", refused("tool.fetch")).await.expect_err("a defined tool is not free");
    assert_eq!(error.code, code::INVALID_PARAMS);
    assert!(error.message.contains("fetch.pirs.toml"), "the message names the file: {}", error.message);
    assert!(error.message.contains("defines tool `fetch`"), "{}", error.message);

    let error = c.request("register", refused("tool.bash")).await.expect_err("a built-in is not free");
    assert_eq!(error.code, code::INVALID_PARAMS);
    assert!(error.message.contains("built-in"), "{}", error.message);

    // An undeclared name is still free, with the placeholder schema.
    c.call("register", refused("tool.anything")).await;
    let attach = c.call("loop.attach", json!({"loop": &id})).await;
    let tools = attach["manifest"]["tools"].as_array().expect("tools").clone();
    let named = |name: &str| tools.iter().filter(|t| t["name"] == name).count();
    assert_eq!(named("anything"), 1, "an undeclared registration registers: {tools:?}");
    assert_eq!(named("bash"), 1, "the built-in is untouched: {tools:?}");
    assert_eq!(named("fetch"), 1, "the file's fetch is the only fetch: {tools:?}");
    h.stop().await;
}

// ---------------------------------------------------------------------------
// 5. A connected process started from `on start`
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_on_start_executable_connects_registers_serves_and_dies_with_the_loop() {
    let h = Harness::start(WAIT, vec![]).await;
    // Built line by line: a `\` continuation would strip Python's indentation.
    let watcher = [
        "#!/usr/bin/env python3",
        "import json, os, socket",
        "sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)",
        "sock.connect(os.environ['PIRS_SOCKET'])",
        "f = sock.makefile('rw')",
        "def send(o):",
        "    f.write(json.dumps(o) + '\\n'); f.flush()",
        &format!("send({{'jsonrpc': '2.0', 'id': 1, 'method': 'hello', 'params': {{'client': 'watch', 'protocol_version': '{PROTOCOL_VERSION}'}}}})"),
        "assert 'result' in json.loads(f.readline())",
        "send({'jsonrpc': '2.0', 'id': 2, 'method': 'register', 'params': {'loop': os.environ['PIRS_LOOP'], 'slot': 'on.turn_end', 'timeout': 1000}})",
        "assert 'result' in json.loads(f.readline())",
        "with open('watch.pid', 'w') as p:",
        "    p.write(str(os.getpid()))",
        "for line in f:",
        "    msg = json.loads(line)",
        "    if msg.get('method') == 'on.turn_end':",
        "        with open('watch.log', 'a') as log:",
        "            log.write(json.dumps(msg['params']) + '\\n')",
        "",
    ]
    .join("\n");
    write_exec(&h, "watch.py", &watcher);
    project_policy(&h, "watch.pirs.toml", "intent = 'a watcher'\n[[on]]\nevent = 'start'\nrun = './watch.py'\n");
    let pidfile = h.project.path().join("watch.pid");
    let logfile = h.project.path().join("watch.log");
    let mut c = h.client().await;
    let id = create_and_subscribe(&mut c, &h.cwd()).await;
    assert!(wait_for(&pidfile).await, "the watcher connected and registered (D-16)");
    let pid: i32 = std::fs::read_to_string(&pidfile).expect("pid").trim().parse().expect("a pid");
    assert!(alive(pid));

    c.call("loop.prompt", json!({"loop": id, "text": "hi"})).await;
    c.events_until("loop.run_end").await;
    assert!(wait_for(&logfile).await, "the watcher received on.turn_end");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let log = std::fs::read_to_string(&logfile).expect("log");
    assert_eq!(log.lines().count(), 1, "one turn, one line: {log}");
    let params: Value = serde_json::from_str(log.lines().next().unwrap()).expect("the event payload");
    assert_eq!(params["loop"], id);
    assert!(params["seq"].is_number(), "the same payload as the event: {params}");

    c.call("loop.close", json!({"loop": id})).await;
    assert!(all_dead(&[pid], Duration::from_secs(3)).await, "loop.close kills the process it started (D-23)");
    h.stop().await;
}

// ---------------------------------------------------------------------------
// 7. Abort kills a running executable's process group
// ---------------------------------------------------------------------------

#[tokio::test]
async fn aborting_a_run_kills_the_executable_tools_process_group() {
    let script = vec![json!({"text": "sleeping", "toolCalls": [{"name": "sleepy", "arguments": {}}]}), json!("done")];
    let h = Harness::start(WAIT, script).await;
    write_exec(
        &h,
        "tools/sleepy.py",
        &python(
            "import subprocess, time\n\
             child = subprocess.Popen(['sleep', '300'])\n\
             with open('sleepy.pid', 'w') as p:\n    p.write(f'{os.getpid()} {child.pid}')\n\
             time.sleep(300)",
        ),
    );
    project_policy(
        &h,
        "sleepy.pirs.toml",
        "intent = 'a slow tool'\n[[tool]]\nname = 'sleepy'\ndescription = 'sleeps'\nparams = {}\nrun = './tools/sleepy.py'\ntimeout = 120\n",
    );
    let pidfile = h.project.path().join("sleepy.pid");
    let mut c = h.client().await;
    let id = create_and_subscribe(&mut c, &h.cwd()).await;
    c.call("loop.prompt", json!({"loop": id, "text": "go"})).await;
    assert!(wait_for(&pidfile).await, "the tool started");
    let pids: Vec<i32> =
        std::fs::read_to_string(&pidfile).expect("pids").split_whitespace().map(|p| p.parse().expect("a pid")).collect();
    assert_eq!(pids.len(), 2);
    assert!(pids.iter().all(|pid| alive(*pid)), "the script and its child are running");

    let started = tokio::time::Instant::now();
    c.call("loop.abort", json!({"loop": id})).await;
    assert_eq!(c.wait(&id).await["state"], "idle");
    assert!(started.elapsed() < Duration::from_secs(5), "the abort did not wait out the tool's timeout");
    assert!(all_dead(&pids, Duration::from_secs(3)).await, "the whole process group is gone: {pids:?}");
    h.stop().await;
}
