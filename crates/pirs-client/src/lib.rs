//! The pirs client library: how anything talks to a pirs loop server.
//!
//! Print mode, the TUI and long-lived extensions are all clients, and the
//! plumbing they share lives here (D-03): finding the socket, starting a
//! server that is not running, saying `hello` and checking the protocol
//! version, matching responses to requests, and handing the caller two
//! streams — the loop events it subscribed to, and the slot requests the
//! server sends to slots it registered.
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
mod seq;
mod socket;
mod spawn;

pub use client::{Client, ConnectOptions, EventStream, SlotItem, SlotStream};
pub use error::{ClientError, Result};
pub use seq::SeqTracker;
pub use socket::{pirs_home, socket_path};
pub use spawn::{split_command, SERVER_COMMAND_ENV};
