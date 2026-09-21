//! The connection: hello, requests, events and slot requests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures::Stream;
use pirs_protocol::{
    code, compatible, DslCheckParams, DslCheckResult, Empty, Envelope, Event, Frame, FsListParams,
    FsListResult, FsReadParams, FsReadResult, HelloParams, HelloResult, Id, LoopAbortParams,
    LoopAttachParams, LoopAttachResult, LoopCloseParams, LoopCreateParams, LoopInfo, LoopListParams,
    LoopListResult, LoopModelParams, LoopPromptParams, LoopReloadParams, LoopReloadResult,
    LoopSelector, LoopToolsParams, LoopWaitParams, LoopWaitResult, ModelSpec, NotifyLevel,
    PromptWhen, RegisterParams, Request, Response, RpcError, RpcResponse, ServerPath, Slot,
    SlotReply, SlotRequest, SubscribeParams, UiNotifyParams, UiStatusParams, UiWidgetParams,
    UnregisterParams, UnsubscribeParams, PROTOCOL_VERSION,
};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::error::{ClientError, Result};
use crate::socket::socket_path;
use crate::spawn;

/// Call a request and unwrap the one [`Response`] variant it can return.
macro_rules! expect {
    ($self:ident, $request:expr, $variant:ident) => {
        match $self.call($request).await? {
            Response::$variant(result) => Ok(result),
            _ => unreachable!("parse_response returns the request's own variant"),
        }
    };
}
/// How the client reaches a server.
#[derive(Debug, Clone)]
pub struct ConnectOptions {
    /// The socket to connect to; `None` means [`socket_path`].
    pub socket: Option<PathBuf>,
    /// Start a server when nothing is listening. A client that must not start
    /// one (`pirs stop`, a health check) sets this to false and gets
    /// [`ClientError::NoServer`] instead.
    pub auto_start: bool,
    /// This client's name and version, free text for the server's logs
    /// (`"pirs-tui 0.1.0"`).
    pub client_name: String,
    /// The command that starts a server, program first. `None` means
    /// `PIRS_SERVER_COMMAND` split into words, and failing that this
    /// executable with `serve`.
    pub server_command: Option<Vec<String>>,
    /// How long to wait for a started server's socket to appear.
    pub start_timeout: Duration,
}

impl ConnectOptions {
    /// Defaults for a client called `client_name`: the default socket,
    /// auto-start on, the default server command, ten seconds to start.
    pub fn new(client_name: impl Into<String>) -> Self {
        ConnectOptions {
            socket: None,
            auto_start: true,
            client_name: client_name.into(),
            server_command: None,
            start_timeout: Duration::from_secs(10),
        }
    }
}

impl Default for ConnectOptions {
    fn default() -> Self {
        ConnectOptions::new(concat!("pirs-client ", env!("CARGO_PKG_VERSION")))
    }
}

/// One item of the slot stream: the id to answer on, `None` for `on.<event>`,
/// and the request itself.
pub type SlotItem = (Option<Id>, SlotRequest);

/// Requests waiting for their response, by id.
type Pending = Arc<Mutex<HashMap<Id, oneshot::Sender<std::result::Result<Value, RpcError>>>>>;

/// The write half and the one bit of connection state both halves share.
#[derive(Debug)]
struct Conn {
    write: tokio::sync::Mutex<OwnedWriteHalf>,
    closed: AtomicBool,
}

impl Conn {
    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    /// Write one line. Any write failure closes the connection for good.
    async fn send(&self, envelope: &Envelope) -> Result<()> {
        if self.is_closed() {
            return Err(ClientError::Disconnected);
        }
        let line = Frame::encode(envelope)?;
        let mut write = self.write.lock().await;
        if write.write_all(line.as_bytes()).await.is_err() || write.flush().await.is_err() {
            self.close();
            return Err(ClientError::Disconnected);
        }
        Ok(())
    }
}

/// Everything one connection owns. Held behind an `Arc` by [`Client`]; the
/// reader task deliberately holds only `Conn` and `Pending`, so dropping the
/// last `Client` really does run [`Drop`] and stop the task.
#[derive(Debug)]
struct Inner {
    conn: Arc<Conn>,
    pending: Pending,
    next_id: AtomicI64,
    hello: HelloResult,
    socket: PathBuf,
    events: Mutex<Option<mpsc::UnboundedReceiver<Event>>>,
    slots: Mutex<Option<mpsc::UnboundedReceiver<SlotItem>>>,
    cancel: CancellationToken,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// A connection to one pirs server.
///
/// Cheap to clone: every clone shares the connection, the id counter and the
/// pending-request table. The connection closes when the last clone is
/// dropped.
///
/// ```no_run
/// # async fn example() -> Result<(), pirs_client::ClientError> {
/// use pirs_client::{Client, ConnectOptions};
/// use pirs_protocol::{LoopListParams, LoopSelector, PromptWhen};
///
/// let client = Client::connect(ConnectOptions::new("my-client 0.1.0")).await?;
/// let loops = client.loop_list(LoopListParams::default()).await?;
/// if let Some(first) = loops.loops.first() {
///     client.subscribe_with_replay(LoopSelector::Loop(first.id.clone()), None, None).await?;
///     client.loop_prompt(&first.id, "hello", PromptWhen::Now).await?;
/// }
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

impl Client {
    /// Connect, starting a server first if one is needed and allowed.
    ///
    /// The socket is [`ConnectOptions::socket`] or [`socket_path`]. When
    /// nothing is listening there and [`auto_start`](ConnectOptions::auto_start)
    /// is set, the server command is spawned detached (see
    /// [`split_command`](crate::split_command) for how
    /// `PIRS_SERVER_COMMAND` is read) and the socket is polled with backoff
    /// for up to [`start_timeout`](ConnectOptions::start_timeout). Then
    /// `hello` is exchanged; a server whose protocol major differs refuses it
    /// and this returns [`ClientError::VersionRefused`].
    pub async fn connect(options: ConnectOptions) -> Result<Client> {
        let socket = options.socket.clone().unwrap_or_else(socket_path);
        let stream = match UnixStream::connect(&socket).await {
            Ok(stream) => stream,
            Err(error) if nothing_listening(&error) => {
                if !options.auto_start {
                    return Err(ClientError::NoServer {
                        socket,
                        source: error,
                    });
                }
                let command = spawn::server_command(options.server_command.clone())?;
                spawn::spawn_detached(&command)?;
                wait_for_socket(&socket, options.start_timeout).await?
            }
            Err(error) => {
                return Err(ClientError::Connect {
                    socket,
                    source: error,
                })
            }
        };
        Client::handshake(stream, socket, &options.client_name).await
    }

    /// Say hello on an already-open stream and start the reader task.
    async fn handshake(stream: UnixStream, socket: PathBuf, client_name: &str) -> Result<Client> {
        let (read, write) = stream.into_split();
        let conn = Arc::new(Conn {
            write: tokio::sync::Mutex::new(write),
            closed: AtomicBool::new(false),
        });
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (slot_tx, slot_rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();

        tokio::spawn(read_loop(
            read,
            Arc::clone(&conn),
            Arc::clone(&pending),
            event_tx,
            slot_tx,
            cancel.clone(),
        ));

        let request = Request::Hello(HelloParams {
            client: client_name.to_owned(),
            protocol_version: PROTOCOL_VERSION.to_owned(),
        });
        let hello = match send_request(&conn, &pending, Id::Number(0), &request).await {
            Ok(value) => match request.parse_response(value) {
                Ok(Response::Hello(hello)) => hello,
                Ok(_) => unreachable!("parse_response returns the request's own variant"),
                Err(source) => {
                    cancel.cancel();
                    return Err(ClientError::Decode {
                        method: "hello",
                        source,
                    });
                }
            },
            Err(ClientError::Rpc { source, .. }) if source.code == code::VERSION_REFUSED => {
                cancel.cancel();
                return Err(version_refused(source));
            }
            Err(error) => {
                cancel.cancel();
                return Err(error);
            }
        };

        if !compatible(&hello.protocol_version, PROTOCOL_VERSION) {
            // A server that answers rather than refusing is still not one we
            // can talk to; treat it exactly like a refusal.
            cancel.cancel();
            return Err(ClientError::VersionRefused {
                server: hello.protocol_version,
                protocol_version: PROTOCOL_VERSION.to_owned(),
            });
        }

        Ok(Client {
            inner: Arc::new(Inner {
                conn,
                pending,
                next_id: AtomicI64::new(1),
                hello,
                socket,
                events: Mutex::new(Some(event_rx)),
                slots: Mutex::new(Some(slot_rx)),
                cancel,
            }),
        })
    }

    /// What the server said in its `hello` reply.
    pub fn hello(&self) -> &HelloResult {
        &self.inner.hello
    }

    /// The socket this client is connected to.
    pub fn socket(&self) -> &Path {
        &self.inner.socket
    }

    /// Whether the connection is still open. Once false, always false.
    pub fn is_connected(&self) -> bool {
        !self.inner.conn.is_closed()
    }

    /// The stream of events from every loop this connection subscribed to.
    ///
    /// The first caller takes it; later callers get `None`, because a single
    /// receiver cannot be shared. It ends when the connection closes.
    ///
    /// Dropping the stream is not a disconnect: the connection stays open and
    /// further events are discarded as they arrive. A stream that is never
    /// taken buffers them instead, so take it only if you read it.
    pub fn events(&self) -> Option<EventStream> {
        self.inner
            .events
            .lock()
            .expect("the events lock is never poisoned")
            .take()
            .map(EventStream)
    }

    /// The stream of slot requests the server sends to slots this connection
    /// registered, as `(id, request)`.
    ///
    /// `id` is `Some` for the slots that expect a reply — answer with
    /// [`reply_slot`](Self::reply_slot) or [`reply_error`](Self::reply_error)
    /// within the timeout given to `register`, or the server treats the slot
    /// as "no opinion". It is `None` for `on.<event>`, which is fire and
    /// forget (D-23). The first caller takes the stream; later callers get
    /// `None`. It ends when the connection closes.
    ///
    /// Dropping the stream is not a disconnect: the connection stays open and
    /// further slot requests are discarded (the server sees them as "no
    /// opinion"). A stream that is never taken buffers them instead.
    pub fn slot_requests(&self) -> Option<SlotStream> {
        self.inner
            .slots
            .lock()
            .expect("the slots lock is never poisoned")
            .take()
            .map(SlotStream)
    }

    /// Answer a slot request. A [`SlotReply::None`] sends nothing, because
    /// `on.<event>` is not answered.
    pub async fn reply_slot(&self, id: Id, reply: SlotReply) -> Result<()> {
        match reply.to_value() {
            Some(result) => {
                self.inner
                    .conn
                    .send(&Envelope::Response(RpcResponse::ok(id, result)))
                    .await
            }
            None => Ok(()),
        }
    }

    /// Answer a slot request with an error. The server reads it as a handler
    /// error and applies the slot's default outcome.
    pub async fn reply_error(&self, id: Id, error: RpcError) -> Result<()> {
        self.inner
            .conn
            .send(&Envelope::Response(RpcResponse::err(id, error)))
            .await
    }

    /// Send any request and get its raw `result`.
    ///
    /// The typed helpers below are this plus the method's result type; use
    /// this one to speak to a newer server whose result carries fields this
    /// client does not know.
    pub async fn request(&self, request: Request) -> Result<Value> {
        let id = Id::Number(self.inner.next_id.fetch_add(1, Ordering::Relaxed));
        send_request(&self.inner.conn, &self.inner.pending, id, &request).await
    }

    /// Send a request and parse its result as the method's own type.
    async fn call(&self, request: Request) -> Result<Response> {
        let method = request.method();
        let value = self.request(request.clone()).await?;
        request
            .parse_response(value)
            .map_err(|source| ClientError::Decode { method, source })
    }

    /// Start a loop.
    pub async fn loop_create(&self, params: LoopCreateParams) -> Result<LoopInfo> {
        expect!(self, Request::LoopCreate(params), LoopCreate)
    }

    /// List running loops and, when `params.cwd` is set, that directory's
    /// conversations.
    pub async fn loop_list(&self, params: LoopListParams) -> Result<LoopListResult> {
        expect!(self, Request::LoopList(params), LoopList)
    }

    /// Attach to a loop: its info, its manifest and the latest `seq`, which is
    /// where [`subscribe_with_replay`](Self::subscribe_with_replay) should
    /// resume from.
    pub async fn loop_attach(&self, loop_id: &str) -> Result<LoopAttachResult> {
        let params = LoopAttachParams {
            loop_id: loop_id.to_owned(),
        };
        expect!(self, Request::LoopAttach(params), LoopAttach)
    }

    /// Close a loop and kill its processes. Its conversation stays on disk.
    pub async fn loop_close(&self, loop_id: &str) -> Result<Empty> {
        let params = LoopCloseParams {
            loop_id: loop_id.to_owned(),
        };
        expect!(self, Request::LoopClose(params), LoopClose)
    }

    /// Send a prompt. The answer arrives as events, not as this result.
    pub async fn loop_prompt(
        &self,
        loop_id: &str,
        text: impl Into<String>,
        when: PromptWhen,
    ) -> Result<Empty> {
        let params = LoopPromptParams {
            loop_id: loop_id.to_owned(),
            text: text.into(),
            when,
        };
        expect!(self, Request::LoopPrompt(params), LoopPrompt)
    }

    /// Abort the loop's current turn. An idle loop is unaffected.
    pub async fn loop_abort(&self, loop_id: &str) -> Result<Empty> {
        let params = LoopAbortParams {
            loop_id: loop_id.to_owned(),
        };
        expect!(self, Request::LoopAbort(params), LoopAbort)
    }

    /// Block until the loop is idle.
    pub async fn loop_wait(&self, loop_id: &str) -> Result<LoopWaitResult> {
        let params = LoopWaitParams {
            loop_id: loop_id.to_owned(),
        };
        expect!(self, Request::LoopWait(params), LoopWait)
    }

    /// Observe a loop's events; they arrive on [`events`](Self::events).
    pub async fn subscribe(&self, params: SubscribeParams) -> Result<Empty> {
        expect!(self, Request::Subscribe(params), Subscribe)
    }

    /// Observe a loop, replaying what happened after `since` first.
    ///
    /// This is [`subscribe`](Self::subscribe) with `since` spelled out,
    /// because resuming is the normal case, not an option. Every event except
    /// a streaming delta carries a `seq` — its index in the loop's session
    /// log, which is the event stream (D-06) — and a client resumes by passing
    /// the last `seq` it saw: `0` replays the whole log, `None` replays
    /// nothing. Deltas are not logged and never replayed, so a resumed client
    /// sees the complete messages it missed rather than the typing.
    /// [`SeqTracker`](crate::SeqTracker) keeps the bookkeeping.
    pub async fn subscribe_with_replay(
        &self,
        loop_id: LoopSelector,
        events: Option<Vec<String>>,
        since: Option<u64>,
    ) -> Result<Empty> {
        self.subscribe(SubscribeParams {
            loop_id,
            events,
            since,
        })
        .await
    }

    /// Stop observing.
    pub async fn unsubscribe(&self, loop_id: LoopSelector) -> Result<Empty> {
        let params = UnsubscribeParams { loop_id };
        expect!(self, Request::Unsubscribe(params), Unsubscribe)
    }

    /// Handle a slot on a loop. Firings arrive on
    /// [`slot_requests`](Self::slot_requests); `timeout` is how long the
    /// server waits for each reply, in milliseconds.
    pub async fn register(&self, loop_id: &str, slot: Slot, timeout_ms: u64) -> Result<Empty> {
        let params = RegisterParams {
            loop_id: loop_id.to_owned(),
            slot,
            timeout: timeout_ms,
        };
        expect!(self, Request::Register(params), Register)
    }

    /// Stop handling a slot.
    pub async fn unregister(&self, loop_id: &str, slot: Slot) -> Result<Empty> {
        let params = UnregisterParams {
            loop_id: loop_id.to_owned(),
            slot,
        };
        expect!(self, Request::Unregister(params), Unregister)
    }

    /// Set a status key; the server emits a `ui.status` event. An empty
    /// `text` clears the key.
    pub async fn ui_status(&self, loop_id: &str, key: &str, text: &str) -> Result<Empty> {
        let params = UiStatusParams {
            loop_id: loop_id.to_owned(),
            key: key.to_owned(),
            text: text.to_owned(),
        };
        expect!(self, Request::UiStatus(params), UiStatus)
    }

    /// Set a widget's lines; the server emits a `ui.widget` event. Empty
    /// `lines` remove the widget.
    pub async fn ui_widget(&self, loop_id: &str, key: &str, lines: Vec<String>) -> Result<Empty> {
        let params = UiWidgetParams {
            loop_id: loop_id.to_owned(),
            key: key.to_owned(),
            lines,
        };
        expect!(self, Request::UiWidget(params), UiWidget)
    }

    /// Send a one-off notice; the server emits a `ui.notify` event.
    pub async fn ui_notify(&self, loop_id: &str, level: NotifyLevel, text: &str) -> Result<Empty> {
        let params = UiNotifyParams {
            loop_id: loop_id.to_owned(),
            level,
            text: text.to_owned(),
        };
        expect!(self, Request::UiNotify(params), UiNotify)
    }

    /// List a directory on the loop's server. `path` is a label the server
    /// produced; never build one (D-31).
    pub async fn fs_list(&self, loop_id: &str, path: ServerPath) -> Result<FsListResult> {
        let params = FsListParams {
            loop_id: loop_id.to_owned(),
            path,
        };
        expect!(self, Request::FsList(params), FsList)
    }

    /// Read a file on the loop's server. Large content comes back as a `ref`,
    /// which this same request serves in full when handed back (D-11).
    pub async fn fs_read(&self, loop_id: &str, path: ServerPath) -> Result<FsReadResult> {
        let params = FsReadParams {
            loop_id: loop_id.to_owned(),
            path,
        };
        expect!(self, Request::FsRead(params), FsRead)
    }

    /// Set the loop's active tool set; tools not named are disabled.
    pub async fn loop_tools(&self, loop_id: &str, names: Vec<String>) -> Result<Empty> {
        let params = LoopToolsParams {
            loop_id: loop_id.to_owned(),
            names,
        };
        expect!(self, Request::LoopTools(params), LoopTools)
    }

    /// Change the loop's model between turns.
    pub async fn loop_model(&self, loop_id: &str, spec: ModelSpec) -> Result<Empty> {
        let params = LoopModelParams {
            loop_id: loop_id.to_owned(),
            spec,
        };
        expect!(self, Request::LoopModel(params), LoopModel)
    }

    /// Re-read the loop's policy files now.
    pub async fn loop_reload(&self, loop_id: &str) -> Result<LoopReloadResult> {
        let params = LoopReloadParams {
            loop_id: loop_id.to_owned(),
        };
        expect!(self, Request::LoopReload(params), LoopReload)
    }

    /// Check the policy files a loop in `cwd` would start with.
    pub async fn dsl_check(&self, cwd: ServerPath) -> Result<DslCheckResult> {
        let params = DslCheckParams { cwd };
        expect!(self, Request::DslCheck(params), DslCheck)
    }
}

/// Turn a `VERSION_REFUSED` error into the typed refusal.
fn version_refused(error: RpcError) -> ClientError {
    let server = error
        .data
        .as_ref()
        .and_then(|data| data.get("server"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| error.message.clone());
    ClientError::VersionRefused {
        server,
        protocol_version: PROTOCOL_VERSION.to_owned(),
    }
}

/// Whether the error means "the socket is not there, or nobody is listening".
fn nothing_listening(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
    )
}

/// Poll the socket until a server answers, or `timeout` runs out.
async fn wait_for_socket(socket: &Path, timeout: Duration) -> Result<UnixStream> {
    let deadline = Instant::now() + timeout;
    let mut delay = Duration::from_millis(10);
    loop {
        match UnixStream::connect(socket).await {
            Ok(stream) => return Ok(stream),
            Err(error) if nothing_listening(&error) => {}
            Err(error) => {
                return Err(ClientError::Connect {
                    socket: socket.to_path_buf(),
                    source: error,
                })
            }
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(ClientError::StartTimeout {
                socket: socket.to_path_buf(),
                waited: timeout,
            });
        }
        tokio::time::sleep(delay.min(deadline - now)).await;
        delay = (delay * 2).min(Duration::from_millis(200));
    }
}

/// Send one request and wait for its response.
async fn send_request(
    conn: &Conn,
    pending: &Pending,
    id: Id,
    request: &Request,
) -> Result<Value> {
    let method = request.method();
    let (tx, rx) = oneshot::channel();
    pending
        .lock()
        .expect("the pending lock is never poisoned")
        .insert(id.clone(), tx);
    let envelope = Envelope::Request(request.clone().into_rpc(id.clone()));
    if let Err(error) = conn.send(&envelope).await {
        pending
            .lock()
            .expect("the pending lock is never poisoned")
            .remove(&id);
        return Err(error);
    }
    match rx.await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(source)) => Err(ClientError::Rpc { method, source }),
        Err(_) => Err(ClientError::Disconnected),
    }
}

/// Own the read half: route responses to their requests, events and slot
/// requests to their streams, and end everything when the connection does.
async fn read_loop(
    read: OwnedReadHalf,
    conn: Arc<Conn>,
    pending: Pending,
    events: mpsc::UnboundedSender<Event>,
    slots: mpsc::UnboundedSender<SlotItem>,
    cancel: CancellationToken,
) {
    let mut lines = BufReader::new(read).lines();
    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => break,
            line = lines.next_line() => line,
        };
        let line = match next {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(error) => {
                tracing::debug!(%error, "reading from the pirs server failed");
                break;
            }
        };
        let envelope: Envelope = match Frame::decode(&line) {
            Ok(envelope) => envelope,
            Err(pirs_protocol::FrameError::Empty) => continue,
            Err(error) => {
                tracing::warn!(%error, "undecodable line from the pirs server");
                continue;
            }
        };
        if !dispatch(envelope, &conn, &pending, &events, &slots).await {
            break;
        }
    }

    // The connection is over: fail every waiting request and end both streams
    // (their senders are dropped when this task returns).
    conn.close();
    pending
        .lock()
        .expect("the pending lock is never poisoned")
        .clear();
}

/// Handle one incoming message. Returns false only when the connection
/// itself is finished; a dropped [`EventStream`] or [`SlotStream`] discards
/// what was addressed to it and the read loop carries on.
async fn dispatch(
    envelope: Envelope,
    conn: &Conn,
    pending: &Pending,
    events: &mpsc::UnboundedSender<Event>,
    slots: &mpsc::UnboundedSender<SlotItem>,
) -> bool {
    match envelope {
        Envelope::Response(response) => {
            let waiting = pending
                .lock()
                .expect("the pending lock is never poisoned")
                .remove(&response.id);
            match waiting {
                Some(tx) => {
                    let _ = tx.send(response.into_result());
                }
                None => tracing::warn!(id = %response.id, "response to no request"),
            }
            true
        }
        Envelope::Notification(notification) => {
            // `on.<event>` is a slot firing, fire and forget; everything else
            // a server sends without an id is an event.
            if let Ok(slot) = notification.method.parse::<Slot>() {
                match SlotRequest::from_parts(slot, notification.params) {
                    Ok(request) => {
                        let _ = slots.send((None, request));
                        true
                    }
                    Err(error) => {
                        tracing::warn!(
                            method = %notification.method,
                            %error,
                            "slot notification with unreadable params"
                        );
                        true
                    }
                }
            } else {
                match Event::from_rpc(&notification) {
                    Ok(event) => {
                        let _ = events.send(event);
                        true
                    }
                    Err(error) => {
                        tracing::warn!(
                            method = %notification.method,
                            %error,
                            "unknown notification from the pirs server"
                        );
                        true
                    }
                }
            }
        }
        Envelope::Request(request) => {
            let Ok(slot) = request.method.parse::<Slot>() else {
                tracing::warn!(
                    method = %request.method,
                    "unknown request from the pirs server"
                );
                return true;
            };
            match SlotRequest::from_parts(slot, request.params) {
                Ok(slot_request) => {
                    let _ = slots.send((Some(request.id), slot_request));
                    true
                }
                Err(error) => {
                    let message = format!("cannot read {} params: {error}", request.method);
                    tracing::warn!(method = %request.method, %error, "slot request with unreadable params");
                    let reply = RpcResponse::err(
                        request.id,
                        RpcError::new(code::INVALID_PARAMS, message),
                    );
                    conn.send(&Envelope::Response(reply)).await.is_ok()
                }
            }
        }
    }
}

/// The events this connection is subscribed to, in arrival order.
///
/// Ends when the connection closes. Also usable as a plain receiver with
/// [`recv`](Self::recv).
#[derive(Debug)]
pub struct EventStream(mpsc::UnboundedReceiver<Event>);

impl EventStream {
    /// The next event, or `None` when the connection has closed.
    pub async fn recv(&mut self) -> Option<Event> {
        self.0.recv().await
    }
}

impl Stream for EventStream {
    type Item = Event;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Event>> {
        self.0.poll_recv(cx)
    }
}

/// The slot requests the server sends to this connection's registered slots.
///
/// Ends when the connection closes.
#[derive(Debug)]
pub struct SlotStream(mpsc::UnboundedReceiver<SlotItem>);

impl SlotStream {
    /// The next slot request, or `None` when the connection has closed.
    pub async fn recv(&mut self) -> Option<SlotItem> {
        self.0.recv().await
    }
}

impl Stream for SlotStream {
    type Item = SlotItem;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}
