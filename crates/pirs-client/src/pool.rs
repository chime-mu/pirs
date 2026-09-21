//! Several servers at once (S18, S21).
//!
//! A client that holds more than one connection needs two things the single
//! connection does not have: a way to name a loop — `(server, loop)`, because
//! two servers can hand out the same loop id — and one stream to read, so a
//! UI has a single place where things happen. [`Pool`] is both: one
//! [`ReconnectingClient`] per configured server, and a merged stream of
//! `(server, item)`.
//!
//! Nothing here interprets anything a server said. A path from `build` goes
//! back to `build` and nowhere else (D-31), and the pool's only job is
//! remembering which server that was.

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::Stream;
use tokio::sync::mpsc;

use crate::reconnect::{ReconnectingClient, ServerItem};
use crate::servers::ServerConfig;

/// One connection per server, and one stream for all of them.
#[derive(Debug)]
pub struct Pool {
    order: Vec<String>,
    clients: BTreeMap<String, Arc<ReconnectingClient>>,
    rx: std::sync::Mutex<Option<mpsc::UnboundedReceiver<(String, ServerItem)>>>,
}

impl Pool {
    /// Connect to every server in the list, in parallel, and keep the ones
    /// that failed as disconnected entries.
    ///
    /// A server that is not reachable is not an error for the pool: the other
    /// servers are still there, and the UI shows the one that is down with
    /// [`ReconnectingClient::last_error`] and can
    /// [`reconnect`](ReconnectingClient::reconnect) it later. Auto-start
    /// applies to local servers only; a bridge command is the whole transport
    /// and nothing is started on this machine for it.
    pub async fn from_config(
        servers: Vec<ServerConfig>,
        client_name: impl Into<String>,
    ) -> Pool {
        let client_name = client_name.into();
        let attempts = servers.into_iter().map(|config| {
            let client_name = client_name.clone();
            async move {
                let client = ReconnectingClient::new(config, client_name);
                if let Err(error) = client.reconnect().await {
                    tracing::warn!(server = client.server(), %error, "cannot reach a server");
                }
                client
            }
        });
        Pool::from_clients(futures::future::join_all(attempts).await)
    }

    /// A pool over connections the caller made itself.
    ///
    /// Each client's [`items`](ReconnectingClient::items) stream is taken
    /// here, which is what [`events`](Self::events) merges; a client whose
    /// stream was already taken contributes nothing to it.
    pub fn from_clients(clients: Vec<ReconnectingClient>) -> Pool {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut order = Vec::with_capacity(clients.len());
        let mut held = BTreeMap::new();
        for client in clients {
            let name = client.server().to_owned();
            if let Some(mut items) = client.items() {
                let tx = tx.clone();
                let name = name.clone();
                tokio::spawn(async move {
                    while let Some(item) = items.recv().await {
                        if tx.send((name.clone(), item)).is_err() {
                            return;
                        }
                    }
                });
            }
            order.push(name.clone());
            held.insert(name, Arc::new(client));
        }
        Pool {
            order,
            clients: held,
            rx: std::sync::Mutex::new(Some(rx)),
        }
    }

    /// The server names, in the order the configuration listed them.
    pub fn servers(&self) -> Vec<&str> {
        self.order.iter().map(String::as_str).collect()
    }

    /// One server's connection.
    pub fn client(&self, server: &str) -> Option<Arc<ReconnectingClient>> {
        self.clients.get(server).map(Arc::clone)
    }

    /// Every server's connection, in configuration order.
    pub fn clients(&self) -> Vec<Arc<ReconnectingClient>> {
        self.order
            .iter()
            .filter_map(|name| self.clients.get(name).map(Arc::clone))
            .collect()
    }

    /// One server's configuration.
    pub fn config(&self, server: &str) -> Option<&ServerConfig> {
        self.clients.get(server).map(|client| client.config())
    }

    /// Everything every server sends, tagged with the server it came from.
    /// The first caller takes it; later callers get `None`.
    pub fn events(&self) -> Option<PoolStream> {
        self.rx
            .lock()
            .expect("the stream lock is never poisoned")
            .take()
            .map(PoolStream)
    }
}

/// The merged stream: `(server, item)`.
#[derive(Debug)]
pub struct PoolStream(mpsc::UnboundedReceiver<(String, ServerItem)>);

impl PoolStream {
    /// The next item from any server, or `None` when the pool is gone.
    pub async fn recv(&mut self) -> Option<(String, ServerItem)> {
        self.0.recv().await
    }
}

impl Stream for PoolStream {
    type Item = (String, ServerItem);

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}
