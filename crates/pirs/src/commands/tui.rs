//! `pirs tui`: the reference UI, as a client of the loop server.
//!
//! Nothing of the UI lives here. This is the three lines of glue between the
//! command line and `pirs_tui`: which socket, which directory, which config
//! file, and whether a terminal or a script drives it. The old in-process
//! interactive mode and the `pi-cli` crate it lived in are gone (D-04).

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use pirs_tui::TuiOptions;
use tokio::io::BufReader;

use crate::connect::resolve_cwd;

/// Start the UI and return its exit code.
///
/// `headless` is `Some((columns, rows))` for the scriptable mode: the same
/// engine on a test backend, driven by JSON lines on stdin, drawing to
/// stdout (`crates/pirs-tui/README.md`).
pub(crate) async fn run(
    headless: Option<(u16, u16)>,
    config: Option<PathBuf>,
    cwd: Option<&Path>,
    socket: Option<&Path>,
    no_start: bool,
) -> Result<i32> {
    let options = TuiOptions {
        socket: socket.map(Path::to_path_buf),
        cwd: PathBuf::from(resolve_cwd(cwd)?),
        config_path: config,
        client_name: concat!("pirs-tui ", env!("CARGO_PKG_VERSION")).to_owned(),
        auto_start: !no_start,
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
