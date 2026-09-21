//! The pirs reference UI, as a client of the loop server.
//!
//! A sidebar of agents with their state and an attention flag, an agent page
//! per selected agent (the conversation from `loop.message` events, tool
//! calls and results, widgets, a status line, the files touched this run),
//! read-only file pages fed by `fs.read` and refreshed on `fs.changed`, and
//! one merged command list from the attach manifest and `~/.pirs/tui.toml`.
//! Everything it knows arrives over the protocol; it depends on
//! `pirs-protocol` and `pirs-client` and nothing of the server.
//!
//! The sidebar spans every server `~/.pirs/servers.toml` names (D-05):
//! an agent is `(server, loop)`, labelled `server:id` when there is more
//! than one server, and every request about it goes to that server's
//! connection. A link that drops is a notice and a retry, not the end of the
//! session (D-06, S19).
//!
//! [`run`] drives a real terminal; [`run_headless`] drives the same engine on
//! a test backend from a script of JSON lines (see `README.md`); [`Harness`]
//! is that engine for tests.

#![deny(unreachable_pub)]
#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod app;
mod config;
mod engine;
mod headless;
mod keys;
mod model;
mod process;
mod render;
mod status;
mod terminal;

use std::path::PathBuf;

use anyhow::Context;
use pirs_client::{ConnectOptions, Pool, ReconnectingClient, ServerConfig, ServerItem, LOCAL};
use tokio::sync::mpsc;

pub use headless::{run_headless, Harness};

use crate::app::{App, Io};
use crate::config::ConfigFile;
use crate::model::Msg;

/// What the UI needs to start.
#[derive(Debug, Clone)]
pub struct TuiOptions {
    /// The servers the sidebar spans, in the order they are listed (D-05).
    /// Empty means every server `~/.pirs/servers.toml` names, which is just
    /// the local one when there is no such file.
    pub servers: Vec<ServerConfig>,
    /// The local server's socket; `None` means the default
    /// (`$PIRS_SOCKET`, `$XDG_RUNTIME_DIR/pirs.sock`, `~/.pirs/pirs.sock`).
    /// It replaces the socket of the server called `local`, or of the only
    /// server when there is one, and means nothing for a server reached
    /// through a bridge command. A missing local server is started.
    pub socket: Option<PathBuf>,
    /// The directory whose stored conversations the sidebar lists, and the
    /// default for `new`; normally the process's cwd.
    ///
    /// It is this machine's directory, and it is sent to every server as
    /// written, because a path belongs to the server that produced it and
    /// no client rewrites one (D-31). On a server somewhere else it usually
    /// means nothing, and that server's list of conversations is then
    /// empty; its running agents are listed all the same.
    pub cwd: PathBuf,
    /// The config file; `None` means `~/.pirs/tui.toml` (`PIRS_HOME`
    /// honoured).
    pub config_path: Option<PathBuf>,
    /// This client's name for the server's logs, e.g. `pirs-tui 0.1.0`.
    pub client_name: String,
    /// Start a server when nothing is listening on a local server's socket;
    /// `false` is `pirs --no-start`, which fails instead. A bridge command
    /// is the whole transport and starts nothing on this machine.
    pub auto_start: bool,
}

impl TuiOptions {
    /// Defaults for a UI started in `cwd`: every configured server, the
    /// default socket, auto-start on.
    pub fn new(cwd: impl Into<PathBuf>) -> TuiOptions {
        TuiOptions {
            servers: Vec::new(),
            socket: None,
            cwd: cwd.into(),
            config_path: None,
            client_name: concat!("pirs-tui ", env!("CARGO_PKG_VERSION")).to_owned(),
            auto_start: true,
        }
    }
}

/// The servers to connect to: the ones named, or the configured ones, with
/// `--socket` applied to the local one.
fn configured(opts: &TuiOptions) -> anyhow::Result<Vec<ServerConfig>> {
    let mut servers = match opts.servers.is_empty() {
        true => pirs_client::servers().context("reading servers.toml")?,
        false => opts.servers.clone(),
    };
    if let Some(socket) = &opts.socket {
        let single = servers.len() == 1;
        for config in servers.iter_mut() {
            if !config.is_remote() && (single || config.name == LOCAL) {
                config.socket = Some(socket.clone());
            }
        }
    }
    Ok(servers)
}

/// Connect to every server, take the pool's one stream, and build the app
/// with its start-up requests issued.
///
/// A server that cannot be reached is not a failure: the others are still
/// there, it is drawn as disconnected, and the UI keeps trying with backoff
/// (D-06, S19). Every server failing is still not a failure — a UI with no
/// server is a UI that says so and waits — but a `servers.toml` that does
/// not parse is, because a typo in it would otherwise silently lose a
/// machine.
pub(crate) async fn boot(opts: &TuiOptions) -> anyhow::Result<(App, mpsc::UnboundedReceiver<Msg>)> {
    let servers = configured(opts)?;
    let mut clients = Vec::with_capacity(servers.len());
    for config in servers {
        let mut options = ConnectOptions::for_server(&config, opts.client_name.clone());
        if !config.is_remote() {
            options.auto_start = opts.auto_start;
        }
        clients.push(ReconnectingClient::with_options(config, options));
    }
    // In parallel: one unreachable server must not hold up the others, and
    // a bridge over SSH takes as long as SSH takes.
    futures::future::join_all(clients.iter().map(|client| async move {
        if let Err(error) = client.reconnect().await {
            tracing::warn!(server = client.server(), %error, "cannot reach a server");
        }
    }))
    .await;
    let pool = Pool::from_clients(clients);

    let (tx, rx) = mpsc::unbounded_channel();
    if let Some(mut items) = pool.events() {
        let tx = tx.clone();
        tokio::spawn(async move {
            while let Some((server, item)) = items.recv().await {
                let msg = match item {
                    ServerItem::Event(event) => Msg::Event { server, event },
                    ServerItem::Disconnected { server } => Msg::ServerGone { server },
                    ServerItem::Reconnected { server } => Msg::ServerBack { server },
                    // The UI registers no slots: everything it answers, it
                    // answers as a prompt (D-32).
                    ServerItem::Slot(_) => continue,
                    _ => continue,
                };
                if tx.send(msg).is_err() {
                    return;
                }
            }
        });
    }
    let io = Io::new(pool, tx, opts.cwd.clone());
    let config_path = opts.config_path.clone().unwrap_or_else(|| {
        // `~/.pirs/tui.toml`, a local file of the UI's own, not a path from
        // a server (D-31).
        #[allow(clippy::disallowed_methods)]
        pirs_client::pirs_home().join("tui.toml")
    });
    let cwd = opts.cwd.to_string_lossy().into_owned();
    let app = App::new(ConfigFile::new(config_path), io, cwd);
    Ok((app, rx))
}

/// Run the UI on the terminal until the user quits. Returns the exit code.
pub async fn run(opts: TuiOptions) -> anyhow::Result<i32> {
    let (app, msgs) = boot(&opts).await?;
    terminal::run(app, msgs).await
}
