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

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Context;
use pirs_client::{Client, ConnectOptions};
use tokio::sync::mpsc;

pub use headless::{run_headless, Harness};

use crate::app::{App, Io};
use crate::config::ConfigFile;
use crate::model::Msg;

/// What the UI needs to start.
#[derive(Debug, Clone)]
pub struct TuiOptions {
    /// The local server's socket; `None` means the default
    /// (`$PIRS_SOCKET`, `$XDG_RUNTIME_DIR/pirs.sock`, `~/.pirs/pirs.sock`).
    /// A missing server is started.
    pub socket: Option<PathBuf>,
    /// The directory whose stored conversations the sidebar lists, and the
    /// default for `new`; normally the process's cwd.
    pub cwd: PathBuf,
    /// The config file; `None` means `~/.pirs/tui.toml` (`PIRS_HOME`
    /// honoured).
    pub config_path: Option<PathBuf>,
    /// This client's name for the server's logs, e.g. `pirs-tui 0.1.0`.
    pub client_name: String,
    /// Start a server when nothing is listening on the socket; `false` is
    /// `pirs --no-start`, which fails instead.
    pub auto_start: bool,
}

impl TuiOptions {
    /// Defaults for a UI started in `cwd`.
    pub fn new(cwd: impl Into<PathBuf>) -> TuiOptions {
        TuiOptions {
            socket: None,
            cwd: cwd.into(),
            config_path: None,
            client_name: concat!("pirs-tui ", env!("CARGO_PKG_VERSION")).to_owned(),
            auto_start: true,
        }
    }
}

/// The name of the one server this phase knows.
const LOCAL: &str = "local";

/// Connect to every server, take their event streams, and build the app
/// with its start-up requests issued.
pub(crate) async fn boot(opts: &TuiOptions) -> anyhow::Result<(App, mpsc::UnboundedReceiver<Msg>)> {
    let mut connect = ConnectOptions::new(opts.client_name.clone());
    connect.socket = opts.socket.clone();
    connect.auto_start = opts.auto_start;
    let client = Client::connect(connect)
        .await
        .context("connecting to the pirs server")?;
    let (tx, rx) = mpsc::unbounded_channel();
    let mut servers = BTreeMap::new();
    if let Some(mut events) = client.events() {
        let tx = tx.clone();
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if tx
                    .send(Msg::Event {
                        server: LOCAL.to_owned(),
                        event,
                    })
                    .is_err()
                {
                    return;
                }
            }
            let _ = tx.send(Msg::ServerGone {
                server: LOCAL.to_owned(),
            });
        });
    }
    servers.insert(LOCAL.to_owned(), client);
    let io = Io::new(servers, tx, opts.cwd.clone());
    let config_path = opts
        .config_path
        .clone()
        .unwrap_or_else(|| pirs_client::pirs_home().join("tui.toml"));
    let cwd = opts.cwd.to_string_lossy().into_owned();
    let app = App::new(ConfigFile::new(config_path), io, cwd);
    Ok((app, rx))
}

/// Run the UI on the terminal until the user quits. Returns the exit code.
pub async fn run(opts: TuiOptions) -> anyhow::Result<i32> {
    let (app, msgs) = boot(&opts).await?;
    terminal::run(app, msgs).await
}
