//! A connection that survives losing its link (D-06, S19).
//!
//! An SSH link drops, a container is restarted, a server is upgraded. What a
//! UI wants back is not a new connection but the same one: the same
//! subscriptions, the same registrations, and the events it missed while it
//! was away. [`ReconnectingClient`] is that: a thin wrapper over [`Client`]
//! that remembers what was asked of the connection and re-asks it after
//! [`reconnect`](ReconnectingClient::reconnect), with `since` set to the last
//! `seq` it saw on each loop, so the catch-up arrives exactly once.
//!
//! Exactly once holds per subscription: what a loop's own subscription
//! replays, the wrapper hands the caller once. A `*` subscription is the
//! exception worth knowing. It takes no `since` of its own — `seq` is per
//! loop — yet its events share each loop's `seq` space, so subscribing to a
//! loop that `*` has already been delivering rewinds that loop to the
//! `since` the caller asked for, and events in `(since, last]` already seen
//! on `*` arrive a second time on that first attach. The TUI subscribes `*`
//! for `loop.status` only, where the repeat is the same state said twice.
//!
//! The wrapper's stream is continuous: it does not end when the link does.
//! The drop arrives as [`ServerItem::Disconnected`] and the recovery as
//! [`ServerItem::Reconnected`], so a UI can show a notice and keep the page
//! it was on. Reconnecting is the caller's decision — when, how often, and
//! with what backoff — because only the caller knows whether the user is
//! still there.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures::Stream;
use pirs_protocol::{Event, LoopSelector, Slot};
use tokio::sync::mpsc;

use crate::client::{Client, ConnectOptions, EventStream, SlotItem, SlotStream};
use crate::error::{ClientError, Result};
use crate::seq::SeqTracker;
use crate::servers::ServerConfig;

/// One thing that happened on a server: an event, a slot request, or the
/// connection itself changing state.
#[derive(Debug)]
#[non_exhaustive]
pub enum ServerItem {
    /// An event from a loop this connection is subscribed to. Replays that
    /// the client has already seen are dropped before they get here.
    Event(Event),
    /// A slot request for a slot this connection registered.
    Slot(SlotItem),
    /// The connection is gone. Nothing arrives until
    /// [`reconnect`](ReconnectingClient::reconnect) succeeds.
    Disconnected {
        /// The server that went away.
        server: String,
    },
    /// The connection is back, its subscriptions re-issued from the last
    /// `seq` seen and its registrations renewed. Whatever was missed follows
    /// this item.
    Reconnected {
        /// The server that came back.
        server: String,
    },
}

/// What was asked of the connection and must be asked again after a
/// reconnect.
#[derive(Debug, Clone)]
struct Subscription {
    loop_id: LoopSelector,
    events: Option<Vec<String>>,
}

/// State shared with the pump task: how far each loop has got.
type Seqs = Arc<Mutex<HashMap<String, SeqTracker>>>;

/// Which connection is the current one. Shared with the pump tasks, which
/// each remember the number they were started with: a pump whose number is
/// no longer this one is draining a connection the wrapper has already
/// replaced, and its ending is not a disconnection.
type Generation = Arc<AtomicU64>;

/// The wrapper's own state. Deliberately does not hold the pump task's
/// handle: the pump ends when the connection does, and the connection ends
/// when the last [`Client`] is dropped, which is when this is.
struct Inner {
    config: ServerConfig,
    options: ConnectOptions,
    client: Mutex<Option<Client>>,
    subscriptions: Mutex<Vec<Subscription>>,
    registrations: Mutex<Vec<(String, Slot, u64)>>,
    seqs: Seqs,
    generation: Generation,
    last_error: Mutex<Option<String>>,
    connected_once: AtomicBool,
    tx: mpsc::UnboundedSender<ServerItem>,
    rx: Mutex<Option<mpsc::UnboundedReceiver<ServerItem>>>,
}

/// A connection to one server that can be re-opened without the caller
/// losing its place.
///
/// Cheap to clone; every clone is the same connection.
#[derive(Clone)]
pub struct ReconnectingClient {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for ReconnectingClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReconnectingClient")
            .field("server", &self.inner.config.name)
            .field("connected", &self.is_connected())
            .finish_non_exhaustive()
    }
}

impl ReconnectingClient {
    /// A wrapper that has not connected yet. Nothing happens until
    /// [`reconnect`](Self::reconnect).
    pub fn new(config: ServerConfig, client_name: impl Into<String>) -> ReconnectingClient {
        let options = ConnectOptions::for_server(&config, client_name);
        ReconnectingClient::with_options(config, options)
    }

    /// The same, for a caller that has something to say about how the
    /// connection is made: a server command to auto-start, or no auto-start
    /// at all.
    ///
    /// The transport comes from `options`, so a caller that changes it
    /// changes where this connects; `config` stays the answer to
    /// [`server`](Self::server) and [`config`](Self::config).
    pub fn with_options(config: ServerConfig, options: ConnectOptions) -> ReconnectingClient {
        let (tx, rx) = mpsc::unbounded_channel();
        ReconnectingClient {
            inner: Arc::new(Inner {
                config,
                options,
                client: Mutex::new(None),
                subscriptions: Mutex::new(Vec::new()),
                registrations: Mutex::new(Vec::new()),
                seqs: Arc::new(Mutex::new(HashMap::new())),
                generation: Arc::new(AtomicU64::new(0)),
                last_error: Mutex::new(None),
                connected_once: AtomicBool::new(false),
                tx,
                rx: Mutex::new(Some(rx)),
            }),
        }
    }

    /// A wrapper that is connected, or the error that stopped it.
    pub async fn connect(
        config: ServerConfig,
        client_name: impl Into<String>,
    ) -> Result<ReconnectingClient> {
        let client = ReconnectingClient::new(config, client_name);
        client.reconnect().await?;
        Ok(client)
    }

    /// The server's name, as `servers.toml` spells it.
    pub fn server(&self) -> &str {
        &self.inner.config.name
    }

    /// Everything the configuration says about this server, including its
    /// [`editor_prefix`](ServerConfig::editor_prefix).
    pub fn config(&self) -> &ServerConfig {
        &self.inner.config
    }

    /// Whether there is a live connection right now.
    pub fn is_connected(&self) -> bool {
        self.current().is_some()
    }

    /// Why the last connection attempt failed, if one did. Cleared by a
    /// successful [`reconnect`](Self::reconnect).
    pub fn last_error(&self) -> Option<String> {
        self.inner
            .last_error
            .lock()
            .expect("the error lock is never poisoned")
            .clone()
    }

    /// The live connection, for the requests this wrapper does not wrap
    /// (`loop.create`, `fs.read`, everything else). `None` while the link is
    /// down; a request made on a client kept across a drop fails with
    /// [`ClientError::Disconnected`].
    ///
    /// Its [`events`](Client::events) and
    /// [`slot_requests`](Client::slot_requests) are already taken: they are
    /// what feeds [`items`](Self::items).
    pub fn client(&self) -> Option<Client> {
        self.current()
    }

    /// The live connection or [`ClientError::Disconnected`], for callers that
    /// would only write that `match` themselves.
    pub fn connected(&self) -> Result<Client> {
        self.current().ok_or(ClientError::Disconnected)
    }

    /// The stream of everything this server sends, continuous across
    /// reconnects. The first caller takes it; later callers get `None`.
    pub fn items(&self) -> Option<ServerStream> {
        self.inner
            .rx
            .lock()
            .expect("the stream lock is never poisoned")
            .take()
            .map(ServerStream)
    }

    /// The last `seq` seen on a loop, which is where a resumed subscription
    /// starts.
    pub fn last_seq(&self, loop_id: &str) -> Option<u64> {
        self.inner
            .seqs
            .lock()
            .expect("the seq lock is never poisoned")
            .get(loop_id)
            .and_then(SeqTracker::last_seq)
    }

    /// Subscribe, and keep subscribing across reconnects.
    ///
    /// `since` is the first subscription's replay point; after that the
    /// wrapper knows where it got to and uses that instead. A `*`
    /// subscription takes no `since` — the protocol refuses one, because
    /// `seq` is per loop — so a client that wants to catch up on a loop
    /// subscribes to that loop.
    pub async fn subscribe(
        &self,
        loop_id: LoopSelector,
        events: Option<Vec<String>>,
        since: Option<u64>,
    ) -> Result<()> {
        if let (LoopSelector::Loop(id), Some(since)) = (&loop_id, since) {
            // The caller is authoritative about where it wants this loop to
            // resume, so an explicit `since` replaces whatever the tracker
            // held. It may hold something already: a `*` subscription
            // delivers this loop's `loop.status` events, and those share the
            // loop's `seq` space, so without the replacement a status event
            // seen before the loop was subscribed to would make the loop's
            // own replay look like something already seen and drop it.
            self.inner
                .seqs
                .lock()
                .expect("the seq lock is never poisoned")
                .insert(id.clone(), SeqTracker::resuming_from(since));
        }
        let subscription = Subscription {
            loop_id: loop_id.clone(),
            events: events.clone(),
        };
        {
            let mut subscriptions = self
                .inner
                .subscriptions
                .lock()
                .expect("the subscription lock is never poisoned");
            subscriptions.retain(|s| s.loop_id != subscription.loop_id);
            subscriptions.push(subscription);
        }
        self.connected()?
            .subscribe_with_replay(loop_id, events, since)
            .await
            .map(drop)
    }

    /// Stop subscribing, now and after a reconnect.
    pub async fn unsubscribe(&self, loop_id: LoopSelector) -> Result<()> {
        self.inner
            .subscriptions
            .lock()
            .expect("the subscription lock is never poisoned")
            .retain(|s| s.loop_id != loop_id);
        self.connected()?.unsubscribe(loop_id).await.map(drop)
    }

    /// Register a slot, and register it again across reconnects.
    pub async fn register(&self, loop_id: &str, slot: Slot, timeout_ms: u64) -> Result<()> {
        {
            let mut registrations = self
                .inner
                .registrations
                .lock()
                .expect("the registration lock is never poisoned");
            registrations.retain(|(id, s, _)| !(id == loop_id && *s == slot));
            registrations.push((loop_id.to_owned(), slot.clone(), timeout_ms));
        }
        self.connected()?
            .register(loop_id, slot, timeout_ms)
            .await
            .map(drop)
    }

    /// Stop handling a slot, now and after a reconnect.
    pub async fn unregister(&self, loop_id: &str, slot: Slot) -> Result<()> {
        self.inner
            .registrations
            .lock()
            .expect("the registration lock is never poisoned")
            .retain(|(id, s, _)| !(id == loop_id && *s == slot));
        self.connected()?.unregister(loop_id, slot).await.map(drop)
    }

    /// Open the transport again, say `hello`, and restore what was asked of
    /// the old connection: every subscription with `since` set to the last
    /// `seq` seen on that loop, then every registration.
    ///
    /// A server whose protocol major has changed under us refuses `hello` and
    /// this returns [`ClientError::VersionRefused`], whose message says both
    /// versions. The wrapper stays disconnected; reconnecting again will fail
    /// the same way until one side is upgraded.
    ///
    /// Asking for a connection that is already there is not an error and not
    /// a second connection: it succeeds and changes nothing. A UI that hears
    /// about a drop from more than one place — a stale pump, a failed
    /// request, its own backoff timer — would otherwise churn through
    /// connections it does not need.
    pub async fn reconnect(&self) -> Result<()> {
        if self.is_connected() {
            return Ok(());
        }
        let client = match Client::connect(self.inner.options.clone()).await {
            Ok(client) => client,
            Err(error) => {
                *self
                    .inner
                    .last_error
                    .lock()
                    .expect("the error lock is never poisoned") = Some(error.to_string());
                return Err(error);
            }
        };
        *self
            .inner
            .last_error
            .lock()
            .expect("the error lock is never poisoned") = None;

        let events = client.events().expect("a fresh client's events");
        let slots = client
            .slot_requests()
            .expect("a fresh client's slot requests");
        *self
            .inner
            .client
            .lock()
            .expect("the client lock is never poisoned") = Some(client.clone());

        // The marker goes in before the pump starts, so a UI sees "back" and
        // then the catch-up, in that order. The first connection is not a
        // reconnection and gets no marker.
        if self.inner.connected_once.swap(true, Ordering::AcqRel) {
            let _ = self.inner.tx.send(ServerItem::Reconnected {
                server: self.inner.config.name.clone(),
            });
        }
        // This connection's number. The pump quotes it back when it ends, so
        // a pump still draining a connection we have already replaced cannot
        // tell the caller the link is down when it is not.
        let generation = self.inner.generation.fetch_add(1, Ordering::AcqRel) + 1;
        tokio::spawn(pump(
            self.inner.config.name.clone(),
            events,
            slots,
            Arc::clone(&self.inner.seqs),
            self.inner.tx.clone(),
            Arc::clone(&self.inner.generation),
            generation,
        ));

        self.restore(&client).await
    }

    /// Re-issue the subscriptions and registrations on a fresh connection.
    ///
    /// A subscription the server refuses — the loop was closed while we were
    /// away — is dropped with a warning rather than failing the reconnect;
    /// the connection is good and the other loops are still there.
    async fn restore(&self, client: &Client) -> Result<()> {
        let subscriptions = self
            .inner
            .subscriptions
            .lock()
            .expect("the subscription lock is never poisoned")
            .clone();
        // Every loop's own subscription goes out before the `*` one. A `*`
        // subscription starts delivering live events the moment it is
        // answered, and those share the loops' `seq` space, so a loop still
        // waiting for its replay would have its tracker carried past it and
        // the replay dropped as something already seen.
        let (loops, all): (Vec<Subscription>, Vec<Subscription>) = subscriptions
            .into_iter()
            .partition(|subscription| matches!(subscription.loop_id, LoopSelector::Loop(_)));
        let mut gone = Vec::new();
        for subscription in loops.into_iter().chain(all) {
            let since = match &subscription.loop_id {
                // `since` needs one loop: `seq` is per loop, so a `*`
                // subscription cannot replay (see docs/protocol.md).
                LoopSelector::All => None,
                LoopSelector::Loop(id) => {
                    // Read and rewind together, immediately before the
                    // subscribe and under one lock, exactly as `subscribe`
                    // does: this loop resumes from here and nothing that
                    // arrived in between may move it on.
                    let mut seqs = self
                        .inner
                        .seqs
                        .lock()
                        .expect("the seq lock is never poisoned");
                    let since = seqs.get(id).map_or(0, SeqTracker::since);
                    seqs.insert(id.clone(), SeqTracker::resuming_from(since));
                    Some(since)
                }
            };
            match client
                .subscribe_with_replay(
                    subscription.loop_id.clone(),
                    subscription.events.clone(),
                    since,
                )
                .await
            {
                Ok(_) => {}
                Err(ClientError::Rpc { source, .. }) => {
                    tracing::warn!(
                        server = %self.inner.config.name,
                        error = %source,
                        "a subscription did not survive the reconnect"
                    );
                    gone.push(subscription.loop_id.clone());
                }
                Err(error) => return Err(error),
            }
        }
        if !gone.is_empty() {
            self.inner
                .subscriptions
                .lock()
                .expect("the subscription lock is never poisoned")
                .retain(|s| !gone.contains(&s.loop_id));
        }

        let registrations = self
            .inner
            .registrations
            .lock()
            .expect("the registration lock is never poisoned")
            .clone();
        let mut dead = Vec::new();
        for (loop_id, slot, timeout) in registrations {
            match client.register(&loop_id, slot.clone(), timeout).await {
                Ok(_) => {}
                Err(ClientError::Rpc { source, .. }) => {
                    tracing::warn!(
                        server = %self.inner.config.name,
                        error = %source,
                        "a registration did not survive the reconnect"
                    );
                    dead.push((loop_id, slot));
                }
                Err(error) => return Err(error),
            }
        }
        if !dead.is_empty() {
            self.inner
                .registrations
                .lock()
                .expect("the registration lock is never poisoned")
                .retain(|(id, slot, _)| !dead.iter().any(|(i, s)| i == id && s == slot));
        }
        Ok(())
    }

    /// The current client, forgetting one whose connection has closed.
    fn current(&self) -> Option<Client> {
        let mut held = self
            .inner
            .client
            .lock()
            .expect("the client lock is never poisoned");
        match held.as_ref() {
            Some(client) if client.is_connected() => Some(client.clone()),
            Some(_) => {
                *held = None;
                None
            }
            None => None,
        }
    }
}

/// Forward one connection's events and slot requests into the wrapper's
/// stream until the connection ends, then say so — unless the wrapper has
/// moved on to a later connection, in which case this one's ending is old
/// news and saying it would tell the caller a live link is down.
async fn pump(
    server: String,
    mut events: EventStream,
    mut slots: SlotStream,
    seqs: Seqs,
    tx: mpsc::UnboundedSender<ServerItem>,
    generation: Generation,
    mine: u64,
) {
    let (mut events_done, mut slots_done) = (false, false);
    while !(events_done && slots_done) {
        tokio::select! {
            event = events.recv(), if !events_done => match event {
                Some(event) => {
                    let fresh = seqs
                        .lock()
                        .expect("the seq lock is never poisoned")
                        .entry(event.loop_id().to_owned())
                        .or_default()
                        .observe(&event);
                    // A replay overlapping what we already have is dropped
                    // here, so the caller sees every event once (D-06).
                    if fresh && tx.send(ServerItem::Event(event)).is_err() {
                        return;
                    }
                }
                None => events_done = true,
            },
            slot = slots.recv(), if !slots_done => match slot {
                Some(slot) => {
                    if tx.send(ServerItem::Slot(slot)).is_err() {
                        return;
                    }
                }
                None => slots_done = true,
            },
        }
    }
    if generation.load(Ordering::Acquire) == mine {
        let _ = tx.send(ServerItem::Disconnected { server });
    }
}

/// Everything one server sends, across reconnects.
#[derive(Debug)]
pub struct ServerStream(mpsc::UnboundedReceiver<ServerItem>);

impl ServerStream {
    /// The next item. `None` only when the wrapper itself is gone; a lost
    /// link is [`ServerItem::Disconnected`], not the end of the stream.
    pub async fn recv(&mut self) -> Option<ServerItem> {
        self.0.recv().await
    }
}

impl Stream for ServerStream {
    type Item = ServerItem;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<ServerItem>> {
        self.0.poll_recv(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run a pump over two streams that are already at their end, and say
    /// what it put on the wrapper's stream.
    async fn drained_pump(current: u64, mine: u64) -> Option<ServerItem> {
        let (events_tx, events_rx) = mpsc::unbounded_channel();
        let (slots_tx, slots_rx) = mpsc::unbounded_channel();
        let (tx, mut rx) = mpsc::unbounded_channel();
        drop(events_tx);
        drop(slots_tx);
        pump(
            "local".to_owned(),
            EventStream::from_receiver(events_rx),
            SlotStream::from_receiver(slots_rx),
            Arc::new(Mutex::new(HashMap::new())),
            tx,
            Arc::new(AtomicU64::new(current)),
            mine,
        )
        .await;
        rx.recv().await
    }

    #[tokio::test]
    async fn a_pump_for_a_superseded_connection_says_nothing_when_it_ends() {
        // The connection this pump was started for is still the current one.
        assert!(matches!(
            drained_pump(1, 1).await,
            Some(ServerItem::Disconnected { .. })
        ));
        // The wrapper has reconnected since; this pump is draining a link
        // nobody is waiting on, and the live one is not down.
        assert!(drained_pump(2, 1).await.is_none());
    }
}
