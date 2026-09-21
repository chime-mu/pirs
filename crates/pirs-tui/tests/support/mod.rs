//! A fake pirs server for the TUI tests: a unix socket in a temporary
//! directory speaking the protocol from `pirs-protocol` and nothing else.
//! It answers requests from a scripted `State`, records every request for
//! the test to assert on, and sends whatever events the test pushes.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use pirs_protocol::{
    code, ConversationInfo, Envelope, Event, Frame, HelloResult, LoopAttachResult, LoopInfo,
    LoopListResult, LoopSelector, LoopState, Manifest, ModelSpec, Request, RpcError, RpcRequest,
    RpcResponse, PROTOCOL_VERSION,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

/// A directory removed when the test ends. Unix socket paths are short, so
/// it lives directly under the system temporary directory.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> TempDir {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "pirs-tui-{}-{tag}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("a temporary directory");
        TempDir(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What the fake server answers from.
#[derive(Debug, Default)]
pub struct State {
    pub loops: Vec<LoopInfo>,
    pub conversations: Vec<ConversationInfo>,
    /// Per loop; a loop without one gets an empty manifest.
    pub manifests: HashMap<String, Manifest>,
    /// Per loop: the sequenced events `subscribe { since }` replays.
    pub replay: HashMap<String, Vec<Event>>,
    /// `fs.read` answers, by path.
    pub files: HashMap<String, String>,
    /// Ids handed out by `loop.create`.
    pub created: u32,
}

impl State {
    fn attach_seq(&self, loop_id: &str) -> u64 {
        self.replay
            .get(loop_id)
            .into_iter()
            .flatten()
            .filter_map(Event::seq)
            .max()
            .unwrap_or(0)
    }
}

pub fn loop_info(id: &str, name: &str, state: LoopState) -> LoopInfo {
    LoopInfo {
        id: id.to_owned(),
        name: Some(name.to_owned()),
        cwd: "/srv/project".into(),
        model: ModelSpec {
            model: "faux/scripted".to_owned(),
            thinking: None,
        },
        state,
        since: 1,
        conversation: format!("c-{id}"),
        parent: None,
    }
}

/// The fake server: one listener, any number of connections.
pub struct FakeServer {
    dir: TempDir,
    socket: PathBuf,
    state: Arc<Mutex<State>>,
    requests: tokio::sync::Mutex<mpsc::UnboundedReceiver<RpcRequest>>,
    events: broadcast::Sender<Envelope>,
    _accept: JoinHandle<()>,
}

impl FakeServer {
    pub async fn start(state: State) -> FakeServer {
        let dir = TempDir::new("server");
        let socket = dir.path().join("pirs.sock");
        let listener = UnixListener::bind(&socket).expect("a bindable socket");
        let state = Arc::new(Mutex::new(state));
        let (request_tx, request_rx) = mpsc::unbounded_channel();
        let (events, _) = broadcast::channel(256);
        let accept = {
            let state = Arc::clone(&state);
            let events = events.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        return;
                    };
                    tokio::spawn(serve(
                        stream,
                        Arc::clone(&state),
                        request_tx.clone(),
                        events.subscribe(),
                    ));
                }
            })
        };
        FakeServer {
            dir,
            socket,
            state,
            requests: tokio::sync::Mutex::new(request_rx),
            events,
            _accept: accept,
        }
    }

    pub fn socket(&self) -> PathBuf {
        self.socket.clone()
    }

    /// The temporary directory, for config files and the like.
    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("the state lock is never poisoned")
    }

    /// Send an event to every connection.
    pub fn send(&self, event: Event) {
        let _ = self.events.send(Envelope::Notification(event.into_rpc()));
    }

    /// The next recorded request matching `predicate`, skipping others;
    /// panics after five seconds.
    pub async fn next_request_where(
        &self,
        what: &str,
        predicate: impl Fn(&RpcRequest) -> bool,
    ) -> RpcRequest {
        let mut requests = self.requests.lock().await;
        let wait = async {
            loop {
                let request = requests.recv().await.expect("the server task lives");
                if predicate(&request) {
                    return request;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(5), wait)
            .await
            .unwrap_or_else(|_| panic!("no request `{what}` within 5 s"))
    }

    /// The next recorded request with this method.
    pub async fn next_request(&self, method: &str) -> RpcRequest {
        self.next_request_where(method, |r| r.method == method)
            .await
    }
}

/// Serve one connection until it closes.
async fn serve(
    stream: UnixStream,
    state: Arc<Mutex<State>>,
    requests: mpsc::UnboundedSender<RpcRequest>,
    mut events: broadcast::Receiver<Envelope>,
) {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Ok(Some(line)) = line else { return };
                let envelope = match Frame::decode(&line) {
                    Ok(envelope) => envelope,
                    Err(pirs_protocol::FrameError::Empty) => continue,
                    Err(error) => panic!("the client sent an undecodable line: {error}"),
                };
                let Envelope::Request(request) = envelope else {
                    continue;
                };
                let (response, replay) = answer(&state, &request);
                let line = Frame::encode(&Envelope::Response(response)).expect("encodable");
                write.write_all(line.as_bytes()).await.expect("writable");
                for event in replay {
                    let line = Frame::encode(&Envelope::Notification(event.into_rpc())).expect("encodable");
                    write.write_all(line.as_bytes()).await.expect("writable");
                }
                write.flush().await.expect("writable");
                let _ = requests.send(request);
            }
            event = events.recv() => {
                let Ok(event) = event else { continue };
                let line = Frame::encode(&event).expect("encodable");
                if write.write_all(line.as_bytes()).await.is_err() {
                    return;
                }
                let _ = write.flush().await;
            }
        }
    }
}

fn ok<T: serde::Serialize>(id: &pirs_protocol::Id, value: T) -> RpcResponse {
    RpcResponse::ok(
        id.clone(),
        serde_json::to_value(value).expect("serialisable"),
    )
}

/// The response to one request, plus events to send right after it.
fn answer(state: &Arc<Mutex<State>>, request: &RpcRequest) -> (RpcResponse, Vec<Event>) {
    let mut state = state.lock().expect("the state lock is never poisoned");
    let typed = match Request::from_rpc(request) {
        Ok(typed) => typed,
        Err(e) => {
            return (
                RpcResponse::err(
                    request.id.clone(),
                    RpcError::new(code::INVALID_PARAMS, e.to_string()),
                ),
                Vec::new(),
            )
        }
    };
    let id = &request.id;
    match typed {
        Request::Hello(_) => (
            ok(
                id,
                HelloResult {
                    server: "fake-pirs 0.1.0".to_owned(),
                    protocol_version: PROTOCOL_VERSION.to_owned(),
                },
            ),
            Vec::new(),
        ),
        Request::LoopList(_) => (
            ok(
                id,
                LoopListResult {
                    loops: state.loops.clone(),
                    conversations: state.conversations.clone(),
                },
            ),
            Vec::new(),
        ),
        Request::LoopAttach(p) => match state.loops.iter().find(|l| l.id == p.loop_id) {
            Some(info) => (
                ok(
                    id,
                    LoopAttachResult {
                        info: info.clone(),
                        manifest: state.manifests.get(&p.loop_id).cloned().unwrap_or_default(),
                        seq: state.attach_seq(&p.loop_id),
                    },
                ),
                Vec::new(),
            ),
            None => (
                RpcResponse::err(
                    id.clone(),
                    RpcError::new(code::UNKNOWN_LOOP, format!("no loop `{}`", p.loop_id)),
                ),
                Vec::new(),
            ),
        },
        Request::Subscribe(p) => {
            let replay = match (&p.loop_id, p.since) {
                (LoopSelector::Loop(loop_id), Some(since)) => state
                    .replay
                    .get(loop_id)
                    .into_iter()
                    .flatten()
                    .filter(|e| e.seq().is_some_and(|s| s > since))
                    .cloned()
                    .collect(),
                _ => Vec::new(),
            };
            (ok(id, json!({})), replay)
        }
        Request::FsRead(p) => match state.files.get(p.path.as_str()) {
            Some(content) => (ok(id, json!({ "content": content })), Vec::new()),
            None => (
                RpcResponse::err(
                    id.clone(),
                    RpcError::new(code::NOT_FOUND, format!("no file {}", p.path)),
                ),
                Vec::new(),
            ),
        },
        Request::LoopCreate(p) => {
            state.created += 1;
            let info = LoopInfo {
                id: format!("n{}", state.created),
                name: p.name,
                cwd: p.cwd,
                model: p.model.unwrap_or(ModelSpec {
                    model: "faux/scripted".to_owned(),
                    thinking: None,
                }),
                state: LoopState::Idle,
                since: 2,
                conversation: p.session.unwrap_or_else(|| format!("c-n{}", state.created)),
                parent: None,
            };
            state.loops.push(info.clone());
            (ok(id, info), Vec::new())
        }
        _ => (ok(id, Value::Object(Default::default())), Vec::new()),
    }
}
