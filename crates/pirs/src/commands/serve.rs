//! `pirs serve`: the loop server in this process.
//!
//! This is also what a client auto-starts (`<this executable> serve --socket
//! <path>`), so its arguments must stay compatible with what
//! `pirs_client::ConnectOptions` spawns.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use pirs_server::{serve, ServeOptions};

use crate::connect::resolve_socket;

/// Listen until SIGTERM, SIGINT, or `idle` seconds with nothing to do.
pub(crate) async fn run(idle: u64, socket: Option<&Path>) -> Result<i32> {
    let socket = resolve_socket(socket);
    serve(ServeOptions {
        socket,
        idle: Duration::from_secs(idle),
    })
    .await?;
    Ok(0)
}
