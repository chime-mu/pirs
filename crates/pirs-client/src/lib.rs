//! The pirs client library: how anything talks to a pirs loop server.
//!
//! Print mode, the TUI and long-lived extensions are all clients, and the
//! plumbing they share lives here (D-03): finding the socket, starting a
//! server that is not running, saying `hello` and checking the protocol
//! version, matching responses to requests, and handing the caller two
//! streams — the loop events it subscribed to, and the slot requests the
//! server sends to slots it registered.
//!
//! On top of one connection there are three more pieces, for the clients
//! that need them:
//!
//! - [`servers`] reads `~/.pirs/servers.toml`, which names each server and
//!   the bridge command that reaches it (D-05), and
//!   [`Transport::Command`] is that bridge: a program whose stdio carries
//!   the protocol, `ssh build pirs proxy` or `docker exec -i jail pirs
//!   proxy`. Auto-start is for the local socket and nothing else.
//! - [`ReconnectingClient`] survives a dropped link: it remembers the
//!   subscriptions and registrations, re-issues them with `since` set to the
//!   last `seq` it saw, and keeps one continuous stream across the gap
//!   (D-06, S19).
//! - [`Pool`] holds one of those per server, so a loop is `(server, loop)`
//!   and there is one stream to read (S18, S21).
//!
//! The crate speaks nothing but [`pirs_protocol`]. It has no dependency on
//! `pi-ai` or `pi-agent`, and that absent edge is a test: a client that needs
//! a type from either means the protocol is missing something, and the fix
//! goes in `pirs-protocol`, not in this crate's `Cargo.toml`.
//!
//! ```no_run
//! # async fn example() -> Result<(), pirs_client::ClientError> {
//! use pirs_client::{Client, ConnectOptions};
//! use pirs_protocol::{LoopCreateParams, PromptWhen};
//!
//! // Connects to `$PIRS_SOCKET`, `$XDG_RUNTIME_DIR/pirs.sock` or
//! // `~/.pirs/pirs.sock`, starting a server if none is listening.
//! let client = Client::connect(ConnectOptions::new("example 0.1.0")).await?;
//! let info = client
//!     .loop_create(LoopCreateParams {
//!         cwd: "/srv/project".into(),
//!         model: None,
//!         name: None,
//!         session: None,
//!     })
//!     .await?;
//! client.loop_prompt(&info.id, "hello", PromptWhen::Now).await?;
//! let mut events = client.events().expect("nobody has taken the stream yet");
//! while let Some(event) = events.recv().await {
//!     println!("{}", event.method());
//! }
//! # Ok(())
//! # }
//! ```

#![deny(unreachable_pub)]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod client;
mod error;
mod pool;
mod reconnect;
mod seq;
mod servers;
mod socket;
mod spawn;
mod transport;

pub use client::{
    open_server_socket, Client, ConnectOptions, EventStream, SlotItem, SlotStream,
};
pub use error::{ClientError, Result};
pub use pool::{Pool, PoolStream};
pub use reconnect::{ReconnectingClient, ServerItem, ServerStream};
pub use seq::SeqTracker;
pub use servers::{parse_servers, servers, servers_path, ServerConfig, LOCAL};
pub use socket::{pirs_home, socket_path};
pub use spawn::{split_command, SERVER_COMMAND_ENV};
pub use transport::Transport;
