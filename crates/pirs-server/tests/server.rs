//! The loop server, driven over its socket by a raw JSON-lines client.
//!
//! Each test starts its own server on a temporary socket with a temporary
//! `PIRS_HOME` and the faux provider (`pi_ai::faux::set_script`). The faux
//! script cursor and `PIRS_HOME` are process-wide, so the tests run one at a
//! time behind a lock.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use pirs_protocol::{code, Envelope, Frame, FrameError, Id, RpcError, RpcResponse, PROTOCOL_VERSION};
use pirs_server::{serve_until, ServeOptions, REF_THRESHOLD};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use tokio::sync::{Mutex, MutexGuard};
use tokio_util::sync::CancellationToken;

const WAIT: Duration = Duration::from_secs(10);

fn lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct Harness {
    _guard: MutexGuard<'static, ()>,
    home: tempfile::TempDir,
    project: tempfile::TempDir,
    socket: PathBuf,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Harness {
    async fn start(idle: Duration, script: Vec<Value>) -> Harness {
        let guard = lock().lock().await;
        let home = tempfile::tempdir().expect("home");
        let project = tempfile::tempdir().expect("project");
        std::env::set_var("PIRS_HOME", home.path().join(".pirs"));
        pi_ai::faux::set_script(script);
        let socket = home.path().join("run").join("pirs.sock");
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(serve_until(ServeOptions { socket: socket.clone(), idle }, shutdown.clone()));
        let started = tokio::time::Instant::now();
        while UnixStream::connect(&socket).await.is_err() {
            assert!(started.elapsed() < WAIT, "server did not come up");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Harness { _guard: guard, home, project, socket, shutdown, task }
    }

    fn cwd(&self) -> String {
        self.project.path().to_string_lossy().into_owned()
    }

    async fn client(&self) -> Client {
        let mut client = Client::connect(&self.socket).await;
        client.hello().await;
        client
    }

    async fn stop(self) {
        self.shutdown.cancel();
        tokio::time::timeout(WAIT, self.task).await.expect("server stops").expect("join").expect("serve ok");
        assert!(!self.socket.exists(), "socket removed on exit");
        assert!(!pid_path(&self.socket).exists(), "pid file removed on exit");
        drop(self.home);
    }
}

fn pid_path(socket: &Path) -> PathBuf {
    let mut s = socket.as_os_str().to_owned();
    s.push(".pid");
    PathBuf::from(s)
}

/// A raw protocol client: one request at a time, everything else queued.
struct Client {
    lines: Lines<BufReader<OwnedReadHalf>>,
    writer: OwnedWriteHalf,
    next_id: i64,
    inbox: VecDeque<Envelope>,
}

impl Client {
    async fn connect(socket: &Path) -> Client {
        let stream = UnixStream::connect(socket).await.expect("connect");
        let (read, writer) = stream.into_split();
        Client { lines: BufReader::new(read).lines(), writer, next_id: 1, inbox: VecDeque::new() }
    }

    async fn send(&mut self, envelope: &Envelope) {
        let line = Frame::encode(envelope).expect("encode");
        self.writer.write_all(line.as_bytes()).await.expect("write");
    }

    async fn send_raw(&mut self, line: &str) {
        self.writer.write_all(line.as_bytes()).await.expect("write");
    }

    /// The next non-empty raw line, or `None` at end of stream.
    async fn read_raw(&mut self) -> Option<String> {
        loop {
            let line = tokio::time::timeout(WAIT, self.lines.next_line()).await.expect("a line in time").expect("read");
            match line {
                None => return None,
                Some(line) if line.trim().is_empty() => continue,
                Some(line) => return Some(line),
            }
        }
    }

    /// The next message, or `None` at end of stream.
    async fn read(&mut self) -> Option<Envelope> {
        let line = self.read_raw().await?;
        match Frame::decode::<Envelope>(&line) {
            Ok(e) => Some(e),
            Err(FrameError::Empty) => unreachable!("read_raw skips empty lines"),
            Err(e) => panic!("undecodable line {line:?}: {e}"),
        }
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
        let id = Id::Number(self.next_id);
        self.next_id += 1;
        self.send(&Envelope::request(id.clone(), method, params)).await;
        loop {
            match self.read().await.expect("a response, not end of stream") {
                Envelope::Response(r) if r.id == id => return r.into_result(),
                other => self.inbox.push_back(other),
            }
        }
    }

    async fn call(&mut self, method: &str, params: Value) -> Value {
        self.request(method, params.clone()).await.unwrap_or_else(|e| panic!("{method} {params} failed: {e}"))
    }

    async fn hello(&mut self) -> Value {
        self.call("hello", json!({"client": "test 0", "protocol_version": PROTOCOL_VERSION})).await
    }

    /// The next message the server sent on its own (an event or a slot request).
    async fn next(&mut self) -> Envelope {
        match self.inbox.pop_front() {
            Some(e) => e,
            None => self.read().await.expect("a message, not end of stream"),
        }
    }

    /// Events until (and including) the first named `until`, as (method, params).
    async fn events_until(&mut self, until: &str) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        loop {
            let Envelope::Notification(n) = self.next().await else { continue };
            let done = n.method == until;
            out.push((n.method, n.params));
            if done {
                return out;
            }
        }
    }

    /// Skip to the next event named `method` and return its params.
    async fn event(&mut self, method: &str) -> Value {
        self.events_until(method).await.pop().expect("at least the event itself").1
    }

    /// The next slot request from the server.
    async fn slot_request(&mut self) -> pirs_protocol::RpcRequest {
        loop {
            if let Envelope::Request(r) = self.next().await {
                return r;
            }
        }
    }

    async fn reply(&mut self, id: Id, result: Value) {
        self.send(&Envelope::Response(RpcResponse::ok(id, result))).await;
    }

    async fn create(&mut self, cwd: &str, extra: Value) -> Value {
        let mut params = json!({"cwd": cwd, "model": {"model": "faux/scripted"}});
        if let Some(map) = extra.as_object() {
            for (k, v) in map {
                params[k] = v.clone();
            }
        }
        self.call("loop.create", params).await
    }

    async fn subscribe(&mut self, loop_id: &str, since: Option<u64>) {
        let mut params = json!({"loop": loop_id});
        if let Some(since) = since {
            params["since"] = json!(since);
        }
        self.call("subscribe", params).await;
    }

    async fn wait(&mut self, loop_id: &str) -> Value {
        self.call("loop.wait", json!({"loop": loop_id})).await
    }
}

fn methods(events: &[(String, Value)]) -> Vec<&str> {
    events.iter().map(|(m, _)| m.as_str()).collect()
}

fn seqs(events: &[(String, Value)]) -> Vec<u64> {
    events.iter().filter_map(|(_, p)| p["seq"].as_u64()).collect()
}

fn message_events<'a>(events: &'a [(String, Value)], role: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|(m, p)| m == "loop.message" && p["role"] == role && p.get("message").is_some())
        .map(|(_, p)| &p["message"])
        .collect()
}

fn text_of(message: &Value) -> String {
    match &message["content"] {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join(""),
        _ => String::new(),
    }
}

async fn run_to_end(client: &mut Client, loop_id: &str, text: &str) -> Vec<(String, Value)> {
    client.call("loop.prompt", json!({"loop": loop_id, "text": text})).await;
    client.events_until("loop.run_end").await
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn hello_comes_first_and_majors_must_agree() {
    let h = Harness::start(WAIT, vec![]).await;

    let mut c = Client::connect(&h.socket).await;
    let err = c.request("loop.list", json!({})).await.unwrap_err();
    assert_eq!(err.code, code::INVALID_REQUEST);
    let err = c.request("hello", json!({"client": "old", "protocol_version": "1.0"})).await.unwrap_err();
    assert_eq!(err.code, code::VERSION_REFUSED);
    assert_eq!(err.data.unwrap()["server"], PROTOCOL_VERSION);
    assert!(c.read().await.is_none(), "the connection is closed after a refusal");

    let mut c = Client::connect(&h.socket).await;
    let hello = c.hello().await;
    assert!(hello["server"].as_str().unwrap().starts_with("pirs-server "));
    assert_eq!(hello["protocol_version"], PROTOCOL_VERSION);
    let err = c.request("loop.dance", json!({})).await.unwrap_err();
    assert_eq!(err.code, code::METHOD_NOT_FOUND);
    let err = c.request("loop.create", json!({"cwd": 7})).await.unwrap_err();
    assert_eq!(err.code, code::INVALID_PARAMS);
    let err = c.request("loop.attach", json!({"loop": "nope"})).await.unwrap_err();
    assert_eq!(err.code, code::UNKNOWN_LOOP);
    let err = c.request("register", json!({"loop": "x", "slot": "guard", "timeout": 1})).await.unwrap_err();
    assert_eq!(err.code, code::UNKNOWN_SLOT);
    // A parse error is answered per JSON-RPC with `id: null`, which the typed
    // `Id` cannot carry, so this one is read raw.
    c.send_raw("this is not json\n").await;
    let line: Value = serde_json::from_str(&c.read_raw().await.expect("a parse error reply")).unwrap();
    assert!(line["id"].is_null());
    assert_eq!(line["error"]["code"], code::PARSE_ERROR);
    c.call("loop.list", json!({})).await;

    assert!(pid_path(&h.socket).exists());
    h.stop().await;
}

#[tokio::test]
async fn a_prompt_streams_its_run_then_the_loop_closes() {
    let h = Harness::start(WAIT, vec![json!("hello there from faux")]).await;
    let mut c = h.client().await;
    let info = c.create(&h.cwd(), json!({"name": "first"})).await;
    let id = info["id"].as_str().unwrap().to_owned();
    assert_eq!(id.len(), 6);
    assert_eq!(info["state"], "idle");
    assert_eq!(info["name"], "first");
    assert_eq!(info["model"]["model"], "faux/scripted");
    assert_eq!(info["cwd"], h.cwd());

    c.subscribe(&id, None).await;
    let events = run_to_end(&mut c, &id, "hi").await;
    let m = methods(&events);
    assert_eq!(m[0], "loop.status");
    assert_eq!(events[0].1["state"], "working");
    assert!(m.iter().filter(|x| **x == "loop.status").count() == 2, "{m:?}");
    let idle_at = events.iter().position(|(m, p)| m == "loop.status" && p["state"] == "idle").unwrap();
    assert_eq!(m.last(), Some(&"loop.run_end"));
    assert_eq!(idle_at, events.len() - 2, "idle status right before run_end: {m:?}");
    let turn_end_at = m.iter().position(|x| *x == "loop.turn_end").unwrap();
    assert!(turn_end_at < idle_at);

    // Deltas: unsequenced, before the complete assistant message.
    let deltas: Vec<usize> = events.iter().enumerate().filter(|(_, (m, p))| m == "loop.message" && p.get("delta").is_some()).map(|(i, _)| i).collect();
    assert!(!deltas.is_empty());
    assert!(events[deltas[0]].1["seq"].is_null());
    let assistant_at = events.iter().position(|(m, p)| m == "loop.message" && p["role"] == "assistant" && p.get("message").is_some()).unwrap();
    assert!(deltas.iter().all(|d| *d < assistant_at));
    let joined: String = deltas.iter().map(|d| events[*d].1["delta"]["text"].as_str().unwrap()).collect();
    assert_eq!(joined, "hello there from faux");

    // Complete messages: system (the logged prompt), user, assistant.
    assert_eq!(text_of(message_events(&events, "user")[0]), "hi");
    assert_eq!(text_of(message_events(&events, "assistant")[0]), "hello there from faux");
    let system = message_events(&events, "system")[0];
    assert!(system["sections"]["cwd"].as_str().unwrap().contains(&h.cwd()));
    assert!(system["toolsAdded"].as_array().unwrap().iter().any(|t| t["name"] == "read"));

    // Sequenced events carry strictly increasing seqs; turn_end covers the turn.
    let s = seqs(&events);
    assert!(s.windows(2).all(|w| w[0] < w[1]), "{s:?}");
    let turn_end = &events[turn_end_at].1;
    let roles: Vec<&str> = turn_end["messages"].as_array().unwrap().iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["user", "assistant"]);
    let run_end = &events.last().unwrap().1;
    let roles: Vec<&str> = run_end["messages"].as_array().unwrap().iter().map(|m| m["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["system", "user", "assistant"]);

    assert_eq!(c.wait(&id).await["state"], "idle");
    let attach = c.call("loop.attach", json!({"loop": &id})).await;
    assert_eq!(attach["seq"], *s.last().unwrap());
    assert_eq!(attach["loop"]["state"], "idle");
    assert!(attach["manifest"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == "bash"));

    c.call("loop.close", json!({"loop": &id})).await;
    let list = c.call("loop.list", json!({"cwd": h.cwd()})).await;
    assert!(list["loops"].as_array().unwrap().is_empty());
    let conversations = list["conversations"].as_array().unwrap();
    assert_eq!(conversations.len(), 1);
    assert_eq!(conversations[0]["id"], info["conversation"]);
    assert_eq!(conversations[0]["name"], "first");
    let err = c.request("loop.prompt", json!({"loop": &id, "text": "x"})).await.unwrap_err();
    assert_eq!(err.code, code::UNKNOWN_LOOP);
    h.stop().await;
}

#[tokio::test]
async fn two_loops_in_one_directory_are_two_conversations() {
    let h = Harness::start(WAIT, vec![json!("a"), json!("b")]).await;
    let mut c = h.client().await;
    let one = c.create(&h.cwd(), json!({"name": "one"})).await;
    let two = c.create(&h.cwd(), json!({"name": "two"})).await;
    assert_ne!(one["id"], two["id"]);
    assert_ne!(one["conversation"], two["conversation"]);
    for l in [&one, &two] {
        c.call("loop.prompt", json!({"loop": l["id"], "text": "go"})).await;
    }
    for l in [&one, &two] {
        c.wait(l["id"].as_str().unwrap()).await;
    }
    let list = c.call("loop.list", json!({"cwd": h.cwd()})).await;
    assert_eq!(list["loops"].as_array().unwrap().len(), 2);
    let mut names: Vec<&str> = list["conversations"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
    names.sort();
    assert_eq!(names, ["one", "two"]);
    let without_cwd = c.call("loop.list", json!({})).await;
    assert!(without_cwd["conversations"].as_array().unwrap().is_empty());
    assert_eq!(without_cwd["loops"].as_array().unwrap().len(), 2);
    h.stop().await;
}

#[tokio::test]
async fn continuing_by_id_or_name_restores_the_transcript() {
    let h = Harness::start(WAIT, vec![json!("first answer"), json!("second answer")]).await;
    let mut c = h.client().await;
    let first = c.create(&h.cwd(), json!({"name": "named"})).await;
    let first_id = first["id"].as_str().unwrap().to_owned();
    c.call("loop.prompt", json!({"loop": &first_id, "text": "q1"})).await;
    c.wait(&first_id).await;
    c.call("loop.close", json!({"loop": &first_id})).await;
    let conversation = first["conversation"].as_str().unwrap().to_owned();

    let err = c.request("loop.create", json!({"cwd": h.cwd(), "model": {"model": "faux/scripted"}, "session": "nope"})).await.unwrap_err();
    assert_eq!(err.code, code::NOT_FOUND);

    let by_id = c.create(&h.cwd(), json!({"session": &conversation})).await;
    assert_eq!(by_id["conversation"], conversation);
    assert_eq!(by_id["name"], "named");
    let by_id_id = by_id["id"].as_str().unwrap().to_owned();
    let attach = c.call("loop.attach", json!({"loop": &by_id_id})).await;
    assert!(attach["seq"].as_u64().unwrap() > 0);
    c.subscribe(&by_id_id, Some(0)).await;
    let replayed = c.events_until("loop.run_end").await;
    assert_eq!(text_of(message_events(&replayed, "user")[0]), "q1");
    assert_eq!(text_of(message_events(&replayed, "assistant")[0]), "first answer");
    assert!(replayed.iter().all(|(_, p)| p.get("delta").is_none()));

    // A second turn on the continued conversation sees the whole transcript.
    let events = run_to_end(&mut c, &by_id_id, "q2").await;
    assert_eq!(text_of(message_events(&events, "assistant")[0]), "second answer");
    // No new system message: prompt and tools are unchanged.
    assert!(message_events(&events, "system").is_empty(), "{:?}", methods(&events));
    c.call("loop.close", json!({"loop": &by_id_id})).await;

    let by_name = c.create(&h.cwd(), json!({"session": "named"})).await;
    assert_eq!(by_name["conversation"], conversation);
    let by_name_id = by_name["id"].as_str().unwrap();
    c.subscribe(by_name_id, Some(0)).await;
    let replayed = c.events_until("loop.run_end").await;
    let users: Vec<String> = message_events(&replayed, "user").iter().map(|m| text_of(m)).collect();
    assert_eq!(users, ["q1"]);
    let turn_ends = replayed.iter().filter(|(m, _)| m == "loop.turn_end").count();
    assert_eq!(turn_ends, 1);
    h.stop().await;
}

#[tokio::test]
async fn subscribe_since_replays_the_live_sequence_without_deltas() {
    let h = Harness::start(WAIT, vec![json!("streamed words here")]).await;
    let mut live = h.client().await;
    let id = live.create(&h.cwd(), json!({})).await["id"].as_str().unwrap().to_owned();
    live.subscribe(&id, None).await;
    let events = run_to_end(&mut live, &id, "hi").await;
    let live_sequenced: Vec<(String, Value)> = events.into_iter().filter(|(_, p)| p["seq"].is_u64()).collect();

    let mut late = h.client().await;
    late.subscribe(&id, Some(0)).await;
    // A `ui.status` after subscribing must follow the replay with no gap.
    live.call("ui.status", json!({"loop": &id, "key": "branch", "text": "main"})).await;
    let replayed = late.events_until("ui.status").await;
    let (tail, replay) = replayed.split_last().unwrap();
    assert_eq!(tail.0, "ui.status");
    // The live observer subscribed after `loop.create`, so the replay has one
    // event it never saw: the loop's first status.
    let (created, replay) = replay.split_first().unwrap();
    assert_eq!(created.1["detail"], "created");
    assert_eq!(created.1["seq"], 1);
    assert_eq!(tail.1["seq"].as_u64().unwrap(), live_sequenced.last().unwrap().1["seq"].as_u64().unwrap() + 1);
    assert_eq!(methods(replay), methods(&live_sequenced));
    assert_eq!(seqs(replay), seqs(&live_sequenced));
    for ((_, a), (_, b)) in replay.iter().zip(&live_sequenced) {
        assert_eq!(a, b);
    }

    // `since` from the attach point yields only what came after; "*" refuses since.
    let mut third = h.client().await;
    let seq = live_sequenced.last().unwrap().1["seq"].as_u64().unwrap();
    third.subscribe(&id, Some(seq)).await;
    let after = third.events_until("ui.status").await;
    assert_eq!(methods(&after), ["ui.status"]);
    let err = third.request("subscribe", json!({"loop": "*", "since": 0})).await.unwrap_err();
    assert_eq!(err.code, code::INVALID_PARAMS);
    let err = third.request("subscribe", json!({"loop": &id, "events": ["loop.bogus"]})).await.unwrap_err();
    assert_eq!(err.code, code::INVALID_PARAMS);

    // "*" sees loops created later; a filter narrows it.
    third.call("subscribe", json!({"loop": "*", "events": ["loop.status"]})).await;
    let other = live.create(&h.cwd(), json!({})).await;
    let created = third.event("loop.status").await;
    assert_eq!(created["loop"], other["id"]);
    assert_eq!(created["detail"], "created");
    third.call("unsubscribe", json!({"loop": "*"})).await;
    h.stop().await;
}

#[tokio::test]
async fn the_write_tool_reports_fs_changed_and_fs_read_serves_it() {
    let script = vec![json!({"text": "writing", "toolCalls": [{"name": "write", "arguments": {"path": "out.txt", "content": "written by the loop"}}]}), json!("done")];
    let h = Harness::start(WAIT, script).await;
    let mut c = h.client().await;
    let id = c.create(&h.cwd(), json!({})).await["id"].as_str().unwrap().to_owned();
    c.subscribe(&id, None).await;
    let events = run_to_end(&mut c, &id, "write it").await;
    let changed = events.iter().find(|(m, _)| m == "fs.changed").map(|(_, p)| p).expect("fs.changed");
    assert_eq!(changed["by"], "tool");
    let path = changed["path"].as_str().unwrap().to_owned();
    assert!(path.ends_with("/out.txt"), "{path}");
    let tool_result = message_events(&events, "toolResult")[0];
    assert_eq!(tool_result["toolName"], "write");
    // fs.changed follows the tool result it belongs to.
    let result_seq = tool_result["timestamp"].as_u64().map(|_| ()).and(events.iter().find(|(m, p)| m == "loop.message" && p["role"] == "toolResult").map(|(_, p)| p["seq"].as_u64().unwrap())).unwrap();
    assert!(changed["seq"].as_u64().unwrap() > result_seq);
    assert_eq!(methods(&events).iter().filter(|m| **m == "loop.turn_end").count(), 2, "two turns: tool call, then done");

    let read = c.call("fs.read", json!({"loop": &id, "path": &path})).await;
    assert_eq!(read["content"], "written by the loop");
    let relative = c.call("fs.read", json!({"loop": &id, "path": "out.txt"})).await;
    assert_eq!(relative["content"], "written by the loop");
    let listed = c.call("fs.list", json!({"loop": &id, "path": "."})).await;
    let entry = listed["entries"].as_array().unwrap().iter().find(|e| e["name"] == "out.txt").expect("listed");
    assert_eq!(entry["kind"], "file");
    assert_eq!(entry["bytes"], 19);
    assert_eq!(entry["path"], path);
    let err = c.request("fs.read", json!({"loop": &id, "path": "missing.txt"})).await.unwrap_err();
    assert_eq!(err.code, code::NOT_FOUND);
    h.stop().await;
}

#[tokio::test]
async fn oversized_tool_results_arrive_by_reference() {
    let script = vec![json!({"toolCalls": [{"name": "big", "arguments": {}}]}), json!("done")];
    let h = Harness::start(WAIT, script).await;
    let mut observer = h.client().await;
    let id = observer.create(&h.cwd(), json!({})).await["id"].as_str().unwrap().to_owned();
    observer.subscribe(&id, None).await;

    let mut handler = h.client().await;
    handler.call("register", json!({"loop": &id, "slot": "tool.big", "timeout": 5000})).await;
    let manifest = observer.call("loop.attach", json!({"loop": &id})).await["manifest"].clone();
    assert!(manifest["tools"].as_array().unwrap().iter().any(|t| t["name"] == "big" && t["parameters"]["type"] == "object"));

    let big = "y".repeat(REF_THRESHOLD + 1000);
    observer.call("loop.prompt", json!({"loop": &id, "text": "go"})).await;
    let req = handler.slot_request().await;
    assert_eq!(req.method, "tool.big");
    handler.reply(req.id, json!({"content": big})).await;

    let events = observer.events_until("loop.run_end").await;
    let result = message_events(&events, "toolResult")[0];
    assert_eq!(result["content"][0]["text"], "[by reference]");
    assert_eq!(result["details"]["ref"]["bytes"], big.len());
    assert_eq!(result["details"]["refs"][0]["index"], 0);
    let reference = result["details"]["ref"]["ref"].as_str().unwrap();
    let read = observer.call("fs.read", json!({"loop": &id, "path": reference})).await;
    assert_eq!(read["content"].as_str().unwrap(), big);
    // The run_end copy of the message is by reference too.
    let run_end = &events.last().unwrap().1;
    let copy = run_end["messages"].as_array().unwrap().iter().find(|m| m["role"] == "toolResult").unwrap();
    assert_eq!(copy["content"][0]["text"], "[by reference]");
    // The model got the whole thing: the faux echo of a tool result would be
    // the next script step, but here the log is the proof.
    let list = observer.call("loop.list", json!({"cwd": h.cwd()})).await;
    let log = std::fs::read_to_string(list["conversations"][0]["path"].as_str().unwrap()).unwrap();
    assert!(log.contains(&big));
    h.stop().await;
}

#[tokio::test]
async fn an_input_handler_rewrites_or_consumes_the_prompt() {
    let h = Harness::start(WAIT, vec![]).await; // echo mode
    let mut observer = h.client().await;
    let id = observer.create(&h.cwd(), json!({})).await["id"].as_str().unwrap().to_owned();
    observer.subscribe(&id, None).await;
    let mut handler = h.client().await;
    handler.call("register", json!({"loop": &id, "slot": "input", "timeout": 5000})).await;

    observer.call("loop.prompt", json!({"loop": &id, "text": "original"})).await;
    let req = handler.slot_request().await;
    assert_eq!(req.method, "input");
    assert_eq!(req.params["text"], "original");
    handler.reply(req.id, json!({"text": "rewritten"})).await;
    let events = observer.events_until("loop.run_end").await;
    assert_eq!(text_of(message_events(&events, "user")[0]), "rewritten");
    assert_eq!(text_of(message_events(&events, "assistant")[0]), "(faux) rewritten");

    observer.call("loop.prompt", json!({"loop": &id, "text": "swallow me"})).await;
    let req = handler.slot_request().await;
    handler.reply(req.id, json!({"handled": true})).await;
    let events = observer.events_until("loop.run_end").await;
    assert!(message_events(&events, "user").is_empty(), "{:?}", methods(&events));
    let idle = events.iter().find(|(m, p)| m == "loop.status" && p["state"] == "idle").unwrap();
    assert_eq!(idle.1["detail"], "handled");

    // Unregister, then the text passes untouched.
    handler.call("unregister", json!({"loop": &id, "slot": "input"})).await;
    let err = handler.request("unregister", json!({"loop": &id, "slot": "input"})).await.unwrap_err();
    assert_eq!(err.code, code::UNKNOWN_SLOT);
    let events = run_to_end(&mut observer, &id, "plain").await;
    assert_eq!(text_of(message_events(&events, "user")[0]), "plain");
    h.stop().await;
}

#[tokio::test]
async fn a_tool_handler_answers_calls_and_rewrites_are_logged() {
    let script = vec![json!({"toolCalls": [{"name": "echo", "arguments": {"text": "x"}}]}), json!("done")];
    let h = Harness::start(WAIT, script).await;
    let mut observer = h.client().await;
    let id = observer.create(&h.cwd(), json!({})).await["id"].as_str().unwrap().to_owned();
    observer.subscribe(&id, None).await;
    let mut handler = h.client().await;
    for slot in ["tool.echo", "tool_result", "on.tool_result", "on.turn_end", "prompt"] {
        handler.call("register", json!({"loop": &id, "slot": slot, "timeout": 5000})).await;
    }

    observer.call("loop.prompt", json!({"loop": &id, "text": "call echo"})).await;
    let req = handler.slot_request().await;
    assert_eq!(req.method, "prompt");
    assert!(req.params["system_prompt"].as_str().unwrap().contains("<cwd>"));
    handler.reply(req.id, json!({"append": "Always say please."})).await;

    let req = handler.slot_request().await;
    assert_eq!(req.method, "tool.echo");
    assert_eq!(req.params["args"]["text"], "x");
    assert!(req.params["id"].as_str().unwrap().starts_with("faux_call_"));
    handler.reply(req.id, json!({"content": "echo: x", "details": {"k": 1}})).await;

    let req = handler.slot_request().await;
    assert_eq!(req.method, "tool_result");
    assert_eq!(req.params["tool"], "echo");
    assert_eq!(req.params["args"]["text"], "x");
    assert_eq!(req.params["result"]["content"][0]["text"], "echo: x");
    handler.reply(req.id, json!({"result": {"content": "ECHO: X"}})).await;

    // Fire-and-forget notifications arrive with no id.
    let mut notified = Vec::new();
    while notified.len() < 2 {
        if let Envelope::Notification(n) = handler.next().await {
            notified.push(n);
        }
    }
    let on_result = notified.iter().find(|n| n.method == "on.tool_result").expect("on.tool_result");
    assert_eq!(on_result.params["result"]["content"][0]["text"], "ECHO: X");
    assert_eq!(on_result.params["args"]["text"], "x");
    assert!(notified.iter().any(|n| n.method == "on.turn_end"));

    let events = observer.events_until("loop.run_end").await;
    let result = message_events(&events, "toolResult")[0];
    assert_eq!(result["content"][0]["text"], "ECHO: X", "the model reads the rewrite");
    assert_eq!(result["toolName"], "echo");
    let system = message_events(&events, "system")[0];
    assert_eq!(system["sections"]["handlers"], "Always say please.");

    // D-21: the log holds the rewrite as the message and the original beside it.
    let list = observer.call("loop.list", json!({"cwd": h.cwd()})).await;
    let log = std::fs::read_to_string(list["conversations"][0]["path"].as_str().unwrap()).unwrap();
    let entries: Vec<Value> = log.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let at = entries.iter().position(|e| e["message"]["role"] == "toolResult").unwrap();
    assert_eq!(entries[at]["message"]["content"][0]["text"], "ECHO: X");
    let rewrite = &entries[at + 1];
    assert_eq!(rewrite["customType"], "pirs.tool_result_rewrite");
    assert_eq!(rewrite["data"]["original"]["content"][0]["text"], "echo: x");
    assert_eq!(rewrite["data"]["messageSeq"], entries[at]["seq"]);
    assert!(rewrite["data"]["by"].as_str().unwrap().starts_with("test 0#"));
    h.stop().await;
}

#[tokio::test]
async fn a_slow_handler_is_skipped_with_a_warning() {
    let h = Harness::start(WAIT, vec![json!("answer")]).await;
    let mut observer = h.client().await;
    let id = observer.create(&h.cwd(), json!({})).await["id"].as_str().unwrap().to_owned();
    observer.subscribe(&id, None).await;
    let mut sleeper = h.client().await;
    sleeper.call("register", json!({"loop": &id, "slot": "input", "timeout": 100})).await;
    let mut failing = h.client().await;
    failing.call("register", json!({"loop": &id, "slot": "prompt", "timeout": 5000})).await;

    observer.call("loop.prompt", json!({"loop": &id, "text": "slow"})).await;
    let slow_req = sleeper.slot_request().await;
    assert_eq!(slow_req.method, "input");
    // No reply: the server gives up after 100 ms and continues.
    let req = failing.slot_request().await;
    assert_eq!(req.method, "prompt");
    failing.send(&Envelope::Response(RpcResponse::err(req.id, RpcError::new(code::INTERNAL_ERROR, "broken")))).await;

    let events = observer.events_until("loop.run_end").await;
    let warnings: Vec<&Value> = events.iter().filter(|(m, p)| m == "ui.notify" && p["level"] == "warning").map(|(_, p)| p).collect();
    assert_eq!(warnings.len(), 2, "{:?}", methods(&events));
    assert!(warnings[0]["text"].as_str().unwrap().contains("did not reply within 100 ms"));
    assert!(warnings[1]["text"].as_str().unwrap().contains("broken"));
    assert_eq!(text_of(message_events(&events, "user")[0]), "slow");
    assert_eq!(text_of(message_events(&events, "assistant")[0]), "answer");
    // A late reply to the expired request is ignored, not an error.
    sleeper.reply(slow_req.id, json!({"text": "too late"})).await;
    sleeper.call("loop.list", json!({})).await;
    h.stop().await;
}

#[tokio::test]
async fn aborting_a_handler_tool_call_does_not_wait_out_its_timeout() {
    let script = vec![json!({"toolCalls": [{"name": "slow", "arguments": {}}]}), json!("done")];
    let h = Harness::start(WAIT, script).await;
    let mut observer = h.client().await;
    let id = observer.create(&h.cwd(), json!({})).await["id"].as_str().unwrap().to_owned();
    observer.subscribe(&id, None).await;
    let mut handler = h.client().await;
    // Thirty seconds of patience the abort must not spend.
    handler.call("register", json!({"loop": &id, "slot": "tool.slow", "timeout": 30_000})).await;

    observer.call("loop.prompt", json!({"loop": &id, "text": "call slow"})).await;
    let req = handler.slot_request().await;
    assert_eq!(req.method, "tool.slow");
    // No reply, ever.
    let started = tokio::time::Instant::now();
    observer.call("loop.abort", json!({"loop": &id})).await;
    assert_eq!(observer.wait(&id).await["state"], "idle");
    assert!(started.elapsed() < Duration::from_secs(5), "the abort waited {:?}", started.elapsed());
    h.stop().await;
}

#[tokio::test]
async fn an_abort_during_the_input_handlers_skips_the_model_call() {
    let h = Harness::start(WAIT, vec![json!("never asked for")]).await;
    let mut observer = h.client().await;
    let id = observer.create(&h.cwd(), json!({})).await["id"].as_str().unwrap().to_owned();
    observer.subscribe(&id, None).await;
    let mut handler = h.client().await;
    handler.call("register", json!({"loop": &id, "slot": "input", "timeout": 2000})).await;

    observer.call("loop.prompt", json!({"loop": &id, "text": "go"})).await;
    let started = tokio::time::Instant::now();
    let req = handler.slot_request().await;
    assert_eq!(req.method, "input");
    observer.call("loop.abort", json!({"loop": &id})).await;
    assert!(started.elapsed() < Duration::from_millis(100), "the abort lands while the handler is still holding the run");
    // The handler never replies, so the run reaches the model call the long
    // way; the abort it carries stops it there.
    assert_eq!(observer.wait(&id).await["state"], "idle");
    let events = observer.events_until("loop.run_end").await;
    let idle = events.iter().find(|(m, p)| m == "loop.status" && p["state"] == "idle").expect("idle");
    assert_eq!(idle.1["detail"], "aborted");
    assert!(message_events(&events, "assistant").is_empty(), "{:?}", methods(&events));
    assert!(message_events(&events, "system").is_empty(), "nothing was logged for a run that never ran");

    // The script's first step is still there: nothing was asked of the model.
    handler.call("unregister", json!({"loop": &id, "slot": "input"})).await;
    let events = run_to_end(&mut observer, &id, "again").await;
    assert_eq!(text_of(message_events(&events, "assistant")[0]), "never asked for");
    h.stop().await;
}

#[tokio::test]
async fn a_handler_can_close_its_own_loop_while_the_run_waits_on_it() {
    let h = Harness::start(WAIT, vec![json!("answer")]).await;
    let mut c = h.client().await;
    let id = c.create(&h.cwd(), json!({})).await["id"].as_str().unwrap().to_owned();
    c.call("register", json!({"loop": &id, "slot": "input", "timeout": 5000})).await;
    c.call("loop.prompt", json!({"loop": &id, "text": "go"})).await;
    let req = c.slot_request().await;
    assert_eq!(req.method, "input");

    // Close from the same connection the run is waiting on, without waiting
    // for the reply: the server must keep reading, or our handler reply
    // never arrives and the close costs the handler's whole timeout.
    let close_id = Id::Number(9001);
    c.send(&Envelope::request(close_id.clone(), "loop.close", json!({"loop": &id}))).await;
    let started = tokio::time::Instant::now();
    tokio::time::sleep(Duration::from_millis(50)).await;
    c.reply(req.id, json!({"text": "go"})).await;
    loop {
        if let Envelope::Response(r) = c.next().await {
            if r.id == close_id {
                r.into_result().expect("loop.close replies when the loop is closed");
                break;
            }
        }
    }
    assert!(started.elapsed() < Duration::from_secs(2), "the close waited {:?}", started.elapsed());
    let err = c.request("loop.attach", json!({"loop": &id})).await.unwrap_err();
    assert_eq!(err.code, code::UNKNOWN_LOOP);
    h.stop().await;
}

#[tokio::test]
async fn loop_list_resolves_the_cwd_it_is_given() {
    let h = Harness::start(WAIT, vec![]).await;
    let mut c = h.client().await;
    let created = c.create(&h.cwd(), json!({})).await;
    let canonical = created["cwd"].as_str().unwrap().to_owned();
    let loop_id = created["id"].as_str().unwrap().to_owned();
    // A conversation reaches the disk once it has something in it.
    c.call("loop.prompt", json!({"loop": &loop_id, "text": "hello"})).await;
    c.wait(&loop_id).await;

    // A symlink to the project is the project: `loop.create` canonicalised
    // the cwd the conversation was stored under, and so does `loop.list`.
    let link = h.home.path().join("link-to-project");
    std::os::unix::fs::symlink(h.project.path(), &link).unwrap();
    let listed = c.call("loop.list", json!({"cwd": link.to_string_lossy()})).await;
    assert_eq!(listed["conversations"].as_array().unwrap().len(), 1, "{listed}");
    assert_eq!(listed["conversations"][0]["cwd"], canonical);

    // A directory that is not there is not an error; it simply has none.
    let missing = c.call("loop.list", json!({"cwd": "/nope/not/here"})).await;
    assert_eq!(missing["conversations"], json!([]));
    h.stop().await;
}

#[tokio::test]
async fn the_server_exits_when_idle() {
    let h = Harness::start(Duration::from_millis(300), vec![]).await;
    let socket = h.socket.clone();
    {
        let mut c = h.client().await;
        c.call("loop.list", json!({})).await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        // Still up: a connection is open.
        c.call("loop.list", json!({})).await;
    }
    let Harness { task, home, _guard, .. } = h;
    tokio::time::timeout(Duration::from_secs(5), task).await.expect("exits when idle").expect("join").expect("serve ok");
    assert!(!socket.exists());
    assert!(!pid_path(&socket).exists());
    drop(home);
}

#[tokio::test]
async fn next_input_waits_for_the_next_run() {
    let h = Harness::start(WAIT, vec![]).await;
    let mut c = h.client().await;
    let id = c.create(&h.cwd(), json!({})).await["id"].as_str().unwrap().to_owned();
    c.subscribe(&id, None).await;
    c.call("loop.prompt", json!({"loop": &id, "text": "held", "when": "next_input"})).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(c.call("loop.attach", json!({"loop": &id})).await["loop"]["state"], "idle");
    assert!(c.inbox.is_empty(), "nothing happened yet");

    let events = run_to_end(&mut c, &id, "go").await;
    let users: Vec<String> = message_events(&events, "user").iter().map(|m| text_of(m)).collect();
    assert_eq!(users, ["held", "go"]);
    assert_eq!(text_of(message_events(&events, "assistant")[0]), "(faux) go");
    h.stop().await;
}

#[tokio::test]
async fn abort_while_working_ends_idle() {
    let long = (0..400).map(|i| format!("w{i}")).collect::<Vec<_>>().join(" ");
    let h = Harness::start(WAIT, vec![json!(long)]).await;
    let mut c = h.client().await;
    let id = c.create(&h.cwd(), json!({})).await["id"].as_str().unwrap().to_owned();
    c.subscribe(&id, None).await;
    c.call("loop.prompt", json!({"loop": &id, "text": "talk"})).await;
    let working = c.event("loop.status").await;
    assert_eq!(working["state"], "working");
    loop {
        if let Envelope::Notification(n) = c.next().await {
            if n.params.get("delta").is_some() {
                break;
            }
        }
    }
    c.call("loop.abort", json!({"loop": &id})).await;
    assert_eq!(c.wait(&id).await["state"], "idle");
    let events = c.events_until("loop.run_end").await;
    let idle = events.iter().find(|(m, p)| m == "loop.status" && p["state"] == "idle").expect("idle");
    assert_eq!(idle.1["detail"], "aborted");
    let assistant = message_events(&events, "assistant");
    assert!(assistant.is_empty() || assistant[0]["stopReason"] == "aborted");
    h.stop().await;
}

#[tokio::test]
async fn control_requests_and_ui_events() {
    let h = Harness::start(WAIT, vec![]).await;
    let mut c = h.client().await;
    let id = c.create(&h.cwd(), json!({})).await["id"].as_str().unwrap().to_owned();
    c.subscribe(&id, None).await;

    c.call("ui.status", json!({"loop": &id, "key": "branch", "text": "main"})).await;
    c.call("ui.widget", json!({"loop": &id, "key": "todo", "lines": ["a", "b"]})).await;
    c.call("ui.notify", json!({"loop": &id, "level": "info", "text": "hello"})).await;
    let events = c.events_until("ui.notify").await;
    assert_eq!(methods(&events), ["ui.status", "ui.widget", "ui.notify"]);
    assert_eq!(events[1].1["lines"], json!(["a", "b"]));
    let manifest = c.call("loop.attach", json!({"loop": &id})).await["manifest"].clone();
    assert_eq!(manifest["status_keys"], json!(["branch"]));
    assert_eq!(manifest["widget_keys"], json!(["todo"]));
    assert_eq!(manifest["commands"], json!([]));

    let err = c.request("loop.tools", json!({"loop": &id, "names": ["read", "teleport"]})).await.unwrap_err();
    assert_eq!(err.code, code::INVALID_PARAMS);
    c.call("loop.tools", json!({"loop": &id, "names": ["read"]})).await;
    let err = c.request("loop.model", json!({"loop": &id, "spec": {"model": "nope/none"}})).await.unwrap_err();
    assert_eq!(err.code, code::INVALID_PARAMS);
    c.call("loop.model", json!({"loop": &id, "spec": {"model": "faux/scripted", "thinking": "high"}})).await;
    assert_eq!(c.call("loop.attach", json!({"loop": &id})).await["loop"]["model"]["thinking"], "high");

    std::fs::write(h.project.path().join("AGENTS.md"), "always be kind").unwrap();
    let reload = c.call("loop.reload", json!({"loop": &id})).await;
    assert_eq!(reload["files"], json!([]));
    let check = c.call("dsl.check", json!({"cwd": h.cwd()})).await;
    assert!(check["system_prompt"].as_str().unwrap().contains("always be kind"));
    assert!(check["system_prompt"].as_str().unwrap().contains(&h.cwd()));
    assert_eq!(check["files"], json!([]));
    assert_eq!(check["conflicts"], json!([]));
    assert!(check["manifest"]["tools"].as_array().unwrap().len() >= 7);

    // The run after the reload sees the new context and only `read`.
    let events = run_to_end(&mut c, &id, "x").await;
    let system = message_events(&events, "system")[0];
    assert!(system["sections"]["project_context"].as_str().unwrap().contains("always be kind"));
    let tools: Vec<&str> = system["toolsAdded"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(tools, ["read"]);
    // Thinking level and model changes were logged.
    let list = c.call("loop.list", json!({"cwd": h.cwd()})).await;
    let log = std::fs::read_to_string(list["conversations"][0]["path"].as_str().unwrap()).unwrap();
    assert!(log.contains("\"type\":\"thinking_level_change\""));
    h.stop().await;
}
