//! `pirs tui`: the reference UI, as a client of the loop server.
//!
//! Nothing of the UI lives here. This is the glue between the command line
//! and `pirs_tui`: which servers, which directory, which config file, and
//! whether a terminal or a script drives it. The old in-process interactive
//! mode and the `pi-cli` crate it lived in are gone (D-04).
//!
//! Which servers: every one `~/.pirs/servers.toml` names (D-05), so the
//! sidebar spans them all, or the one `--server <name>` picks. `--socket`
//! overrides the local server's socket, as it does for every other command,
//! and means nothing for a server reached through a bridge command.

use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{bail, Result};
use pirs_client::ServerConfig;
use pirs_tui::TuiOptions;
use tokio::io::BufReader;

use crate::cli::GlobalArgs;
use crate::connect::{all_servers, resolve_cwd, resolve_server};

/// Start the UI and return its exit code.
///
/// `headless` is `Some((columns, rows))` for the scriptable mode: the same
/// engine on a test backend, driven by JSON lines on stdin, drawing to
/// stdout (`crates/pirs-tui/README.md`).
pub(crate) async fn run(
    headless: Option<(u16, u16)>,
    config: Option<PathBuf>,
    global: &GlobalArgs,
) -> Result<i32> {
    let options = TuiOptions {
        servers: servers(global)?,
        socket: None,
        cwd: PathBuf::from(resolve_cwd(global.cwd.as_deref())?),
        config_path: config,
        client_name: concat!("pirs-tui ", env!("CARGO_PKG_VERSION")).to_owned(),
        auto_start: !global.no_start,
    };
    match headless {
        Some(size) => {
            let script = BufReader::new(tokio::io::stdin());
            pirs_tui::run_headless(options, size, script, std::io::stdout()).await
        }
        None => {
            if !std::io::stdout().is_terminal() {
                bail!("pirs tui needs a terminal; --headless WxH drives it from a script");
            }
            pirs_tui::run(options).await
        }
    }
}

/// The servers the sidebar spans, with `--socket` applied where it means
/// something.
///
/// `--server <name>` is one server and `resolve_socket` has already refused
/// a `--socket` that would do nothing there. Without it the list is every
/// configured server, and `--socket` names the local one's socket — the
/// same rule `pirs --list` follows, so the two commands look at the same
/// place.
fn servers(global: &GlobalArgs) -> Result<Vec<ServerConfig>> {
    if let Some(name) = &global.server {
        return Ok(vec![resolve_server(Some(name), global.socket.as_deref())?]);
    }
    let mut all = all_servers()?;
    if let Some(socket) = global.socket.as_deref() {
        let single = all.len() == 1;
        for config in all.iter_mut() {
            if !config.is_remote() && (single || config.name == pirs_client::LOCAL) {
                config.socket = Some(socket.to_path_buf());
            }
        }
    }
    Ok(all)
}
