//! The unix-socket server: JSON lines in, JSON lines out (D-10).
//!
//! [`serve`] listens on a socket path, runs one task per connection, and
//! answers the requests of `pirs_protocol::Request`. A connection's first
//! message must be `hello`; a different protocol major is refused with
//! `VERSION_REFUSED` and the connection closed. Every write to a connection
//! goes through one writer task fed by an unbounded channel, so a slow
//! observer never blocks a loop: observers are never waited on (D-23).
//! Requests are handled in order on the connection's reader task, except
//! `loop.wait` and `loop.close`, which run in their own task so the
//! connection stays responsive: both wait for a loop to go idle, and the
//! handler replies that let it get there come back over the same
//! connection.
//!
//! Lifecycle: refuse to start when the socket is live (a connect succeeds),
//! replace a stale socket file, write `<socket>.pid`, remove both on exit.
//! SIGTERM and SIGINT close every loop and exit; so does going idle: no
//! working loop and no open connection for [`ServeOptions::idle`].
//!
//! **Faux provider.** `--model faux/scripted` reads `PIRS_FAUX_SCRIPT` from the
//! *server's* environment (not the client's), and its script cursor is
//! process-wide: every loop on one server advances the same script. Start a
//! fresh server per scripted scenario.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _};
use pirs_protocol::{
    code, Empty, Envelope, Frame, FrameError, HelloResult, Id, LoopAttachResult, LoopListResult, LoopSelector,
    LoopState, LoopWaitResult, Request, RpcError, RpcRequest, RpcResponse, Slot, PROTOCOL_VERSION,
};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::{oneshot, Notify};
use tokio_util::sync::CancellationToken;

use crate::agent_loop::{ChildLoops, CreateError, CreateOptions, LoopHandle};
use crate::log::{StarSubscribers, Subscriber};
use crate::session::SessionManager;
use crate::fs;

/// How to run the server.
#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// The unix socket to listen on. Its parent directory is created.
    pub socket: PathBuf,
    /// Exit after this long with no working loop and no open connection.
    pub idle: Duration,
}

/// Run the server until SIGTERM, SIGINT or idle exit.
pub async fn serve(opts: ServeOptions) -> anyhow::Result<()> {
    let shutdown = CancellationToken::new();
    let signal_stop = shutdown.clone();
    tokio::spawn(async move {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = async {
                match term.as_mut() {
                    Some(t) => { t.recv().await; }
                    None => std::future::pending::<()>().await,
                }
            } => {}
        }
        tracing::info!("signal received, shutting down");
        signal_stop.cancel();
    });
    serve_until(opts, shutdown).await
}

/// Run the server until `shutdown` is cancelled or idle exit. What
/// [`serve`] calls after wiring the signals; tests call it directly.
pub async fn serve_until(opts: ServeOptions, shutdown: CancellationToken) -> anyhow::Result<()> {
    // The `<docs>` section must name files that exist, which for an installed
    // or jailed binary means the compiled-in copies under `$PIRS_HOME/docs`.
    crate::system_prompt::install_embedded_docs();
    let socket = opts.socket.clone();
    prepare_socket_path(&socket).await?;
    let listener = UnixListener::bind(&socket).with_context(|| format!("binding {}", socket.display()))?;
    let pid_file = pid_path(&socket);
    tokio::fs::write(&pid_file, format!("{}\n", std::process::id()))
        .await
        .with_context(|| format!("writing {}", pid_file.display()))?;
    tracing::info!(socket = %socket.display(), idle_secs = opts.idle.as_secs_f64(), "pirs server listening");

    let server = Arc::new(Server::new(socket.clone(), opts.idle, shutdown.clone()));
    let idle_watch = tokio::spawn(server.clone().idle_watch());
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let server = server.clone();
                    tokio::spawn(async move { server.run_connection(stream).await });
                }
                Err(e) => {
                    tracing::warn!("accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
            _ = shutdown.cancelled() => break,
        }
    }
    idle_watch.abort();
    server.close_all().await;
    let _ = tokio::fs::remove_file(&socket).await;
    let _ = tokio::fs::remove_file(&pid_file).await;
    tracing::info!("pirs server stopped");
    Ok(())
}

/// The canonical form of a client-supplied directory, or the string itself
/// when it names nothing on this machine.
fn canonical_cwd(cwd: &str) -> String {
    std::fs::canonicalize(cwd).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| cwd.to_owned())
}

fn pid_path(socket: &Path) -> PathBuf {
    let mut s = socket.as_os_str().to_owned();
    s.push(".pid");
    PathBuf::from(s)
}

/// Refuse a live socket, remove a stale one, create the parent directory.
async fn prepare_socket_path(socket: &Path) -> anyhow::Result<()> {
    if let Some(parent) = socket.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await.with_context(|| format!("creating {}", parent.display()))?;
        }
    }
    match tokio::fs::symlink_metadata(socket).await {
        Ok(meta) => {
            use std::os::unix::fs::FileTypeExt;
            if !meta.file_type().is_socket() {
                bail!("{} exists and is not a socket", socket.display());
            }
            if UnixStream::connect(socket).await.is_ok() {
                bail!("a pirs server is already listening on {}", socket.display());
            }
            tracing::info!(socket = %socket.display(), "removing stale socket");
            tokio::fs::remove_file(socket).await.with_context(|| format!("removing stale {}", socket.display()))?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("inspecting {}", socket.display())),
    }
    Ok(())
}

/// Why a slot request got no usable reply.
pub(crate) enum HandlerFailure {
    /// The registered timeout elapsed.
    Timeout,
    /// The handler answered with a JSON-RPC error.
    Error(RpcError),
    /// The handler's connection went away.
    Disconnected,
}

/// One client connection: its writer channel and the slot requests it owes
/// replies to.
pub(crate) struct Connection {
    id: u64,
    client: Mutex<Option<String>>,
    tx: UnboundedSender<String>,
    pending: Mutex<HashMap<Id, oneshot::Sender<Result<Value, RpcError>>>>,
    next_id: AtomicI64,
}

impl Connection {
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// `<client>#<id>`, for log entries and warnings.
    pub(crate) fn label(&self) -> String {
        let client = self.client.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_else(|| "?".into());
        format!("{client}#{}", self.id)
    }

    /// The writer channel, for observer subscriptions.
    pub(crate) fn sender(&self) -> UnboundedSender<String> {
        self.tx.clone()
    }

    fn send(&self, envelope: &Envelope) {
        match Frame::encode(envelope) {
            Ok(line) => {
                let _ = self.tx.send(line);
            }
            Err(e) => tracing::warn!(conn = self.id, "could not encode message: {e}"),
        }
    }

    fn reply(&self, id: Id, result: Result<Value, RpcError>) {
        let response = match result {
            Ok(v) => RpcResponse::ok(id, v),
            Err(e) => RpcResponse::err(id, e),
        };
        self.send(&Envelope::Response(response));
    }

    /// Send a notification (an `on.<event>` slot).
    pub(crate) fn notify(&self, method: String, params: Value) {
        self.send(&Envelope::notification(method, params));
    }

    /// Send a slot request and wait up to `timeout` for the reply.
    pub(crate) async fn call(&self, method: String, params: Value, timeout: Duration) -> Result<Value, HandlerFailure> {
        let id = Id::Number(self.next_id.fetch_add(1, Ordering::SeqCst));
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(id.clone(), tx);
        self.send(&Envelope::request(id.clone(), method, params));
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(error))) => Err(HandlerFailure::Error(error)),
            Ok(Err(_)) => Err(HandlerFailure::Disconnected),
            Err(_) => {
                self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                Err(HandlerFailure::Timeout)
            }
        }
    }

    fn resolve(&self, response: RpcResponse) {
        let waiter = self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&response.id);
        match waiter {
            Some(tx) => {
                let _ = tx.send(response.into_result());
            }
            None => tracing::debug!(conn = self.id, id = %response.id, "response to no outstanding request"),
        }
    }
}

pub(crate) struct Server {
    /// The socket this server listens on; every process a loop calls is told
    /// where it is (`PIRS_SOCKET`).
    socket: PathBuf,
    loops: Mutex<HashMap<String, Arc<LoopHandle>>>,
    connections: Mutex<HashMap<u64, Arc<Connection>>>,
    star: StarSubscribers,
    next_conn: AtomicU64,
    idle: Duration,
    shutdown: CancellationToken,
    activity: Notify,
}

impl Server {
    fn new(socket: PathBuf, idle: Duration, shutdown: CancellationToken) -> Self {
        Server {
            socket,
            loops: Mutex::new(HashMap::new()),
            connections: Mutex::new(HashMap::new()),
            star: StarSubscribers::default(),
            next_conn: AtomicU64::new(1),
            idle,
            shutdown,
            activity: Notify::new(),
        }
    }

    fn loops(&self) -> Vec<Arc<LoopHandle>> {
        self.loops.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect()
    }

    fn is_quiet(&self) -> bool {
        self.connections.lock().unwrap_or_else(|e| e.into_inner()).is_empty() && !self.loops().iter().any(|l| l.is_working())
    }

    /// Cancel `shutdown` once the server has been quiet for `idle`.
    async fn idle_watch(self: Arc<Self>) {
        let tick = (self.idle / 10).clamp(Duration::from_millis(20), Duration::from_secs(1));
        let mut quiet_since: Option<Instant> = None;
        loop {
            tokio::select! {
                _ = tokio::time::sleep(tick) => {}
                _ = self.activity.notified() => {}
                _ = self.shutdown.cancelled() => return,
            }
            if self.is_quiet() {
                let since = *quiet_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= self.idle {
                    tracing::info!(idle_secs = self.idle.as_secs_f64(), "idle, exiting");
                    self.shutdown.cancel();
                    return;
                }
            } else {
                quiet_since = None;
            }
        }
    }

    /// Close one loop and every loop it started (D-28: a loop started by a
    /// `[[tool]] loop` call outlives the call, but not its parent). Returns
    /// false when there was no such loop.
    async fn close_loop(&self, id: &str) -> bool {
        let closing: Vec<Arc<LoopHandle>> = {
            let mut loops = self.loops.lock().unwrap_or_else(|e| e.into_inner());
            let Some(handle) = loops.remove(id) else {
                return false;
            };
            let mut closing = vec![handle];
            let mut frontier = vec![id.to_owned()];
            while let Some(parent) = frontier.pop() {
                let children: Vec<String> = loops
                    .values()
                    .filter(|l| l.parent.as_deref() == Some(parent.as_str()))
                    .map(|l| l.id.clone())
                    .collect();
                for child in children {
                    if let Some(handle) = loops.remove(&child) {
                        frontier.push(child);
                        closing.push(handle);
                    }
                }
            }
            closing
        };
        // The parent first: aborting its run is what ends a `[[tool]] loop`
        // call still waiting on a child.
        for handle in closing {
            tracing::info!(loop_id = handle.id, "loop closed");
            handle.close().await;
        }
        true
    }

    async fn close_all(&self) {
        let loops: Vec<Arc<LoopHandle>> = self.loops.lock().unwrap_or_else(|e| e.into_inner()).drain().map(|(_, l)| l).collect();
        for l in loops {
            l.close().await;
        }
    }

    fn get_loop(&self, id: &str) -> Result<Arc<LoopHandle>, RpcError> {
        self.loops
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
            .ok_or_else(|| RpcError::new(code::UNKNOWN_LOOP, format!("no loop {id:?}")))
    }

    fn new_loop_id(&self) -> String {
        let loops = self.loops.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let id = uuid::Uuid::new_v4().simple().to_string()[..6].to_owned();
            if !loops.contains_key(&id) {
                return id;
            }
        }
    }

    // ----- connections -------------------------------------------------------

    async fn run_connection(self: Arc<Self>, stream: UnixStream) {
        let id = self.next_conn.fetch_add(1, Ordering::SeqCst);
        let (read_half, mut write_half) = stream.into_split();
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let conn = Arc::new(Connection {
            id,
            client: Mutex::new(None),
            tx,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicI64::new(1),
        });
        self.connections.lock().unwrap_or_else(|e| e.into_inner()).insert(id, conn.clone());
        self.activity.notify_one();
        tracing::debug!(conn = id, "connection opened");

        let writer = tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                if write_half.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
            let _ = write_half.shutdown().await;
        });

        let mut lines = BufReader::new(read_half).lines();
        let mut greeted = false;
        loop {
            let line = tokio::select! {
                line = lines.next_line() => match line {
                    Ok(Some(line)) => line,
                    Ok(None) => break,
                    Err(e) => {
                        tracing::debug!(conn = id, "read failed: {e}");
                        break;
                    }
                },
                _ = self.shutdown.cancelled() => break,
            };
            let envelope: Envelope = match Frame::decode(&line) {
                Ok(e) => e,
                Err(FrameError::Empty) => continue,
                Err(e) => {
                    // The typed `Id` has no null; JSON-RPC's parse error reply
                    // does, so this one line is written by hand.
                    let error = RpcError::new(code::PARSE_ERROR, e.to_string());
                    let _ = conn.tx.send(format!("{}\n", json!({"jsonrpc": "2.0", "id": null, "error": error})));
                    continue;
                }
            };
            match envelope {
                Envelope::Response(response) => conn.resolve(response),
                Envelope::Notification(n) => tracing::debug!(conn = id, method = n.method, "ignoring notification from client"),
                Envelope::Request(rpc) => {
                    let rpc_id = rpc.id.clone();
                    if !greeted && rpc.method != "hello" {
                        conn.reply(rpc_id, Err(RpcError::new(code::INVALID_REQUEST, "the first message on a connection must be hello")));
                        continue;
                    }
                    match self.dispatch(&conn, rpc).await {
                        Dispatch::Reply(result) => conn.reply(rpc_id, result),
                        Dispatch::Greeted(result) => {
                            greeted = true;
                            conn.reply(rpc_id, Ok(result));
                        }
                        Dispatch::Refused(error) => {
                            conn.reply(rpc_id, Err(error));
                            break;
                        }
                        Dispatch::Spawned => {}
                    }
                }
            }
        }

        tracing::debug!(conn = id, "connection closed");
        self.connections.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
        for l in self.loops() {
            l.unregister_connection(id);
            l.log.lock().unwrap_or_else(|e| e.into_inner()).unsubscribe(id);
        }
        self.star.lock().unwrap_or_else(|e| e.into_inner()).retain(|s| s.conn != id);
        conn.pending.lock().unwrap_or_else(|e| e.into_inner()).clear();
        // Every sender is gone once the connection and its subscriptions are
        // dropped, so the writer drains what is queued (a version refusal,
        // say) and exits; a peer that stopped reading is given up on.
        drop(conn);
        let abort = writer.abort_handle();
        if tokio::time::timeout(Duration::from_secs(2), writer).await.is_err() {
            tracing::debug!(conn = id, "writer did not drain; abandoning");
            abort.abort();
        }
        self.activity.notify_one();
    }

    async fn dispatch(self: &Arc<Self>, conn: &Arc<Connection>, rpc: RpcRequest) -> Dispatch {
        let request = match Request::from_rpc(&rpc) {
            Ok(r) => r,
            Err(e) => {
                if !Request::METHODS.contains(&rpc.method.as_str()) {
                    return Dispatch::Reply(Err(RpcError::new(code::METHOD_NOT_FOUND, format!("unknown method {:?}", rpc.method))));
                }
                if matches!(rpc.method.as_str(), "register" | "unregister") {
                    if let Some(slot) = rpc.params.get("slot").and_then(Value::as_str) {
                        if let Err(e) = slot.parse::<Slot>() {
                            return Dispatch::Reply(Err(RpcError::new(code::UNKNOWN_SLOT, e.to_string())));
                        }
                    }
                }
                return Dispatch::Reply(Err(RpcError::new(code::INVALID_PARAMS, format!("invalid params for {}", rpc.method))
                    .with_data(Value::String(e.to_string()))));
            }
        };
        match request {
            Request::Hello(p) => {
                if !pirs_protocol::compatible(&p.protocol_version, PROTOCOL_VERSION) {
                    return Dispatch::Refused(
                        RpcError::new(
                            code::VERSION_REFUSED,
                            format!("protocol {} is not compatible with this server's {PROTOCOL_VERSION}", p.protocol_version),
                        )
                        .with_data(json!({ "server": PROTOCOL_VERSION })),
                    );
                }
                *conn.client.lock().unwrap_or_else(|e| e.into_inner()) = Some(p.client);
                Dispatch::Greeted(
                    serde_json::to_value(HelloResult {
                        server: format!("pirs-server {}", env!("CARGO_PKG_VERSION")),
                        protocol_version: PROTOCOL_VERSION.to_owned(),
                    })
                    .unwrap_or_default(),
                )
            }
            Request::LoopClose(p) => {
                // Like `loop.wait`: a client that is both a handler and the
                // closer would stall, because closing aborts the run and
                // waits for idle, and the run may still be calling that
                // client's handlers. Its replies arrive on the reader task
                // this would otherwise be blocking.
                let server = self.clone();
                let conn = conn.clone();
                let id = rpc.id;
                tokio::spawn(async move {
                    let result = if server.close_loop(&p.loop_id).await {
                        server.activity.notify_one();
                        serde_json::to_value(Empty {}).map_err(|e| RpcError::new(code::INTERNAL_ERROR, e.to_string()))
                    } else {
                        Err(RpcError::new(code::UNKNOWN_LOOP, format!("no loop {:?}", p.loop_id)))
                    };
                    conn.reply(id, result);
                });
                Dispatch::Spawned
            }
            Request::LoopWait(p) => {
                let server = self.clone();
                let conn = conn.clone();
                let id = rpc.id;
                tokio::spawn(async move {
                    let result = match server.get_loop(&p.loop_id) {
                        Ok(handle) => {
                            handle.wait_idle().await;
                            Ok(json!(LoopWaitResult { state: LoopState::Idle }))
                        }
                        Err(e) => Err(e),
                    };
                    conn.reply(id, result);
                });
                Dispatch::Spawned
            }
            other => Dispatch::Reply(self.handle(conn, other).await),
        }
    }

    async fn handle(self: &Arc<Self>, conn: &Arc<Connection>, request: Request) -> Result<Value, RpcError> {
        fn ok<T: serde::Serialize>(value: T) -> Result<Value, RpcError> {
            serde_json::to_value(value).map_err(|e| RpcError::new(code::INTERNAL_ERROR, e.to_string()))
        }
        let empty = || ok(Empty {});
        match request {
            Request::Hello(_) | Request::LoopWait(_) | Request::LoopClose(_) => {
                Err(RpcError::new(code::INTERNAL_ERROR, "handled elsewhere"))
            }
            Request::LoopCreate(p) => {
                let opts = CreateOptions {
                    cwd: PathBuf::from(p.cwd.as_str()),
                    model: p.model,
                    name: p.name,
                    session: p.session,
                    parent: None,
                    socket: self.socket.clone(),
                };
                let handle = self.clone().create_child(opts).map_err(|e| match e {
                    CreateError::NotFound(m) => RpcError::new(code::NOT_FOUND, m),
                    CreateError::Invalid(m) => RpcError::new(code::INVALID_PARAMS, m),
                    CreateError::Internal(e) => RpcError::new(code::INTERNAL_ERROR, format!("{e:#}")),
                })?;
                ok(handle.info())
            }
            Request::LoopList(p) => {
                let mut loops: Vec<_> = self.loops().iter().map(|l| l.info()).collect();
                loops.sort_by(|a, b| a.id.cmp(&b.id));
                let conversations = match p.cwd {
                    // The conversations of a directory are keyed by the
                    // canonical path `loop.create` stored them under, so the
                    // client's spelling of it (a symlink, `.`, a trailing
                    // slash) is resolved the same way here. A directory that
                    // does not exist keeps the string it was given and simply
                    // lists nothing.
                    Some(cwd) => SessionManager::list(&canonical_cwd(cwd.as_str()), None)
                        .map_err(|e| RpcError::new(code::INTERNAL_ERROR, format!("{e:#}")))?
                        .iter()
                        .map(|s| s.to_conversation_info())
                        .collect(),
                    None => Vec::new(),
                };
                ok(LoopListResult { loops, conversations })
            }
            Request::LoopAttach(p) => {
                let handle = self.get_loop(&p.loop_id)?;
                let seq = handle.log.lock().unwrap_or_else(|e| e.into_inner()).latest_seq();
                ok(LoopAttachResult { info: handle.info(), manifest: handle.manifest(), seq })
            }
            Request::LoopPrompt(p) => {
                let handle = self.get_loop(&p.loop_id)?;
                handle.prompt(p.text, p.when);
                self.activity.notify_one();
                empty()
            }
            Request::LoopAbort(p) => {
                self.get_loop(&p.loop_id)?.abort();
                empty()
            }
            Request::Subscribe(p) => {
                let events = match p.events {
                    Some(names) => {
                        if let Some(bad) = names.iter().find(|n| !pirs_protocol::Event::METHODS.contains(&n.as_str())) {
                            return Err(RpcError::new(code::INVALID_PARAMS, format!("unknown event {bad:?}")));
                        }
                        Some(names.into_iter().collect())
                    }
                    None => None,
                };
                let subscriber = Subscriber { conn: conn.id(), tx: conn.sender(), events };
                match p.loop_id {
                    LoopSelector::All => {
                        if p.since.is_some() {
                            return Err(RpcError::new(code::INVALID_PARAMS, "since needs one loop, not \"*\""));
                        }
                        let mut star = self.star.lock().unwrap_or_else(|e| e.into_inner());
                        star.retain(|s| s.conn != conn.id());
                        star.push(subscriber);
                    }
                    LoopSelector::Loop(id) => {
                        let handle = self.get_loop(&id)?;
                        handle.log.lock().unwrap_or_else(|e| e.into_inner()).subscribe(subscriber, p.since);
                    }
                }
                empty()
            }
            Request::Unsubscribe(p) => {
                match p.loop_id {
                    LoopSelector::All => self.star.lock().unwrap_or_else(|e| e.into_inner()).retain(|s| s.conn != conn.id()),
                    LoopSelector::Loop(id) => {
                        self.get_loop(&id)?.log.lock().unwrap_or_else(|e| e.into_inner()).unsubscribe(conn.id());
                    }
                }
                empty()
            }
            Request::Register(p) => {
                let handle = self.get_loop(&p.loop_id)?;
                if let Some(why) = handle.registration_refusal(&p.slot) {
                    return Err(RpcError::new(code::INVALID_PARAMS, why));
                }
                handle.register(conn.clone(), p.slot, Duration::from_millis(p.timeout));
                empty()
            }
            Request::Unregister(p) => {
                let handle = self.get_loop(&p.loop_id)?;
                if !handle.unregister(conn.id(), &p.slot) {
                    return Err(RpcError::new(code::UNKNOWN_SLOT, format!("this connection did not register {}", p.slot)));
                }
                empty()
            }
            Request::UiStatus(p) => {
                self.get_loop(&p.loop_id)?.ui_status(p.key, p.text);
                empty()
            }
            Request::UiWidget(p) => {
                self.get_loop(&p.loop_id)?.ui_widget(p.key, p.lines);
                empty()
            }
            Request::UiNotify(p) => {
                self.get_loop(&p.loop_id)?.ui_notify(p.level, p.text);
                empty()
            }
            Request::FsList(p) => {
                let handle = self.get_loop(&p.loop_id)?;
                ok(fs::list(&handle.cwd, &p.path).await?)
            }
            Request::FsRead(p) => {
                let handle = self.get_loop(&p.loop_id)?;
                ok(fs::read(&handle.cwd, &p.path).await?)
            }
            Request::LoopTools(p) => {
                self.get_loop(&p.loop_id)?.set_tools(p.names).map_err(|m| RpcError::new(code::INVALID_PARAMS, m))?;
                empty()
            }
            Request::LoopModel(p) => {
                self.get_loop(&p.loop_id)?.set_model(&p.spec).map_err(|m| RpcError::new(code::INVALID_PARAMS, m))?;
                empty()
            }
            Request::LoopReload(p) => {
                let files =
                    self.get_loop(&p.loop_id)?.reload().await.map_err(|m| RpcError::new(code::BUSY, m))?;
                ok(pirs_protocol::LoopReloadResult { files })
            }
            Request::DslCheck(p) => {
                let cwd = std::fs::canonicalize(p.cwd.as_str())
                    .map_err(|e| RpcError::new(code::INVALID_PARAMS, format!("cwd {}: {e}", p.cwd)))?;
                ok(crate::policy::check(&cwd, &self.socket).await)
            }
        }
    }
}

impl ChildLoops for Server {
    fn create_child(self: Arc<Self>, opts: CreateOptions) -> Result<Arc<LoopHandle>, CreateError> {
        let id = self.new_loop_id();
        let weak = {
            let strong: Arc<dyn ChildLoops> = self.clone();
            Arc::downgrade(&strong)
        };
        let parent = opts.parent.clone();
        let handle = LoopHandle::create(id.clone(), opts, self.star.clone(), weak)?;
        tracing::info!(
            loop_id = id,
            cwd = %handle.cwd.display(),
            conversation = handle.conversation,
            parent = parent.unwrap_or_default(),
            "loop created"
        );
        self.loops.lock().unwrap_or_else(|e| e.into_inner()).insert(id, handle.clone());
        self.activity.notify_one();
        Ok(handle)
    }

    fn find_loop(&self, id: &str) -> Option<Arc<LoopHandle>> {
        self.loops.lock().unwrap_or_else(|e| e.into_inner()).get(id).cloned()
    }
}

/// What a request produced on the reader task.
enum Dispatch {
    Reply(Result<Value, RpcError>),
    /// `hello` succeeded; the connection may now send anything.
    Greeted(Value),
    /// `hello` named an incompatible major; reply and close.
    Refused(RpcError),
    /// The reply is sent by another task (`loop.wait`).
    Spawned,
}


