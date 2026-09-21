//! `pirs tui`: the old in-process interactive mode, for phases 1-3 only.
//!
//! It does not speak the protocol and does not use the loop server: it is the
//! pre-protocol agent, kept working while the client, the server and the new
//! TUI are built around it (D-04). Phase 3 replaces this with `pirs-tui`, a
//! client like any other, and deletes `pi-cli`.

use anyhow::Result;

/// Hand every argument to the old CLI and return its exit code.
pub(crate) async fn run(args: Vec<String>) -> Result<i32> {
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push("pirs".to_owned());
    argv.extend(args);
    Ok(pi_cli::run(argv).await)
}
