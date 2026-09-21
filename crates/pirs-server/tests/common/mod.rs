//! The harness the server tests share: a server on a temporary socket with a
//! temporary `PIRS_HOME` and the faux provider, and a raw JSON-lines client.
//!
//! `PIRS_HOME` and the faux script cursor are process-wide, so every test in
//! one binary takes [`Harness::start`]'s lock and runs alone.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use pirs_protocol::{Envelope, Frame, FrameError, Id, RpcError, RpcResponse, PROTOCOL_VERSION};
use pirs_server::{serve_until, ServeOptions};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use tokio::sync::{Mutex, MutexGuard};
use tokio_util::sync::CancellationToken;

pub const WAIT: Duration = Duration::from_secs(10);

fn lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

pub struct Harness {
    pub _guard: MutexGuard<'static, ()>,
    pub home: tempfile::TempDir,
    pub project: tempfile::TempDir,
    pub socket: PathBuf,
    pub shutdown: CancellationToken,
    pub task: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Harness {
    pub async fn start(idle: Duration, script: Vec<Value>) -> Harness {
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

    pub fn cwd(&self) -> String {
        self.project.path().to_string_lossy().into_owned()
    }

    pub async fn client(&self) -> Client {
        let mut client = Client::connect(&self.socket).await;
        client.hello().await;
        client
    }

    pub async fn stop(self) {
        self.shutdown.cancel();
        tokio::time::timeout(WAIT, self.task).await.expect("server stops").expect("join").expect("serve ok");
        assert!(!self.socket.exists(), "socket removed on exit");
        assert!(!pid_path(&self.socket).exists(), "pid file removed on exit");
        drop(self.home);
    }
}

pub fn pid_path(socket: &Path) -> PathBuf {
    let mut s = socket.as_os_str().to_owned();
    s.push(".pid");
    PathBuf::from(s)
}

/// A raw protocol client: one request at a time, everything else queued.
pub struct Client {
    lines: Lines<BufReader<OwnedReadHalf>>,
    writer: OwnedWriteHalf,
    next_id: i64,
    pub inbox: VecDeque<Envelope>,
}

impl Client {
    pub async fn connect(socket: &Path) -> Client {
        let stream = UnixStream::connect(socket).await.expect("connect");
        let (read, writer) = stream.into_split();
        Client { lines: BufReader::new(read).lines(), writer, next_id: 1, inbox: VecDeque::new() }
    }

    pub async fn send(&mut self, envelope: &Envelope) {
        let line = Frame::encode(envelope).expect("encode");
        self.writer.write_all(line.as_bytes()).await.expect("write");
    }

    pub async fn send_raw(&mut self, line: &str) {
        self.writer.write_all(line.as_bytes()).await.expect("write");
    }

    /// The next non-empty raw line, or `None` at end of stream.
    pub async fn read_raw(&mut self) -> Option<String> {
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
    pub async fn read(&mut self) -> Option<Envelope> {
        let line = self.read_raw().await?;
        match Frame::decode::<Envelope>(&line) {
            Ok(e) => Some(e),
            Err(FrameError::Empty) => unreachable!("read_raw skips empty lines"),
            Err(e) => panic!("undecodable line {line:?}: {e}"),
        }
    }

    pub async fn request(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
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

    pub async fn call(&mut self, method: &str, params: Value) -> Value {
        self.request(method, params.clone()).await.unwrap_or_else(|e| panic!("{method} {params} failed: {e}"))
    }

    pub async fn hello(&mut self) -> Value {
        self.call("hello", json!({"client": "test 0", "protocol_version": PROTOCOL_VERSION})).await
    }

    /// The next message the server sent on its own (an event or a slot request).
    pub async fn next(&mut self) -> Envelope {
        match self.inbox.pop_front() {
            Some(e) => e,
            None => self.read().await.expect("a message, not end of stream"),
        }
    }

    /// Events until (and including) the first named `until`, as (method, params).
    pub async fn events_until(&mut self, until: &str) -> Vec<(String, Value)> {
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
    pub async fn event(&mut self, method: &str) -> Value {
        self.events_until(method).await.pop().expect("at least the event itself").1
    }

    /// The next slot request from the server.
    pub async fn slot_request(&mut self) -> pirs_protocol::RpcRequest {
        loop {
            if let Envelope::Request(r) = self.next().await {
                return r;
            }
        }
    }

    pub async fn reply(&mut self, id: Id, result: Value) {
        self.send(&Envelope::Response(RpcResponse::ok(id, result))).await;
    }

    pub async fn create(&mut self, cwd: &str, extra: Value) -> Value {
        let mut params = json!({"cwd": cwd, "model": {"model": "faux/scripted"}});
        if let Some(map) = extra.as_object() {
            for (k, v) in map {
                params[k] = v.clone();
            }
        }
        self.call("loop.create", params).await
    }

    pub async fn subscribe(&mut self, loop_id: &str, since: Option<u64>) {
        let mut params = json!({"loop": loop_id});
        if let Some(since) = since {
            params["since"] = json!(since);
        }
        self.call("subscribe", params).await;
    }

    pub async fn wait(&mut self, loop_id: &str) -> Value {
        self.call("loop.wait", json!({"loop": loop_id})).await
    }
}

pub fn methods(events: &[(String, Value)]) -> Vec<&str> {
    events.iter().map(|(m, _)| m.as_str()).collect()
}

pub fn seqs(events: &[(String, Value)]) -> Vec<u64> {
    events.iter().filter_map(|(_, p)| p["seq"].as_u64()).collect()
}

pub fn message_events<'a>(events: &'a [(String, Value)], role: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|(m, p)| m == "loop.message" && p["role"] == role && p.get("message").is_some())
        .map(|(_, p)| &p["message"])
        .collect()
}

pub fn text_of(message: &Value) -> String {
    match &message["content"] {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join(""),
        _ => String::new(),
    }
}

pub async fn run_to_end(client: &mut Client, loop_id: &str, text: &str) -> Vec<(String, Value)> {
    client.call("loop.prompt", json!({"loop": loop_id, "text": text})).await;
    client.events_until("loop.run_end").await
}

