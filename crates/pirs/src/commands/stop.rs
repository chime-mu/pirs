//! `pirs stop`: close the running agents and stop the server.
//!
//! A server is stopped the way anything else stops it — SIGTERM to the pid in
//! `<socket>.pid`, which makes it close every loop, remove the socket and the
//! pid file, and exit. The protocol has no "shut down" request and should not
//! have one: a client that can end the server for everybody is a different
//! kind of authority from a client that can close its own loop.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use pirs_client::{Client, ClientError};
use pirs_protocol::{LoopListParams, LoopState};

use crate::connect::{connect_options, resolve_server, resolve_socket};

/// How long to wait for the socket to disappear after SIGTERM.
const GONE: Duration = Duration::from_secs(5);

/// Report the running loops, then stop the server. Always exits 0: a server
/// that is not there is the state `pirs stop` is asked for.
///
/// Only a local server can be stopped from here. A server reached through a
/// bridge command runs somewhere this process has no signal to send —
/// another machine, a container — and is stopped where it runs (D-05); the
/// refusal says so rather than stopping the local one by accident.
pub(crate) async fn run(server: Option<&str>, socket: Option<&Path>) -> Result<i32> {
    let config = resolve_server(server, socket)?;
    if config.is_remote() {
        let command = config.command.as_deref().unwrap_or_default().join(" ");
        eprintln!(
            "pirs: {:?} is reached with `command = {command}`; stop it where it runs",
            config.name
        );
        return Ok(1);
    }
    let socket = resolve_socket(config.socket.as_deref());
    let client = match Client::connect(connect_options(&socket, false)).await {
        Ok(client) => client,
        // `NoServer` is "nothing is listening there", which is what
        // `pirs stop` is for. Any other connect failure (a socket that is
        // not readable, a path the kernel refuses) is a real error and says
        // so rather than pretending the server is gone.
        Err(ClientError::NoServer { .. }) => {
            println!("no server");
            return Ok(0);
        }
        Err(error) => return Err(error.into()),
    };

    let listed = client.loop_list(LoopListParams::default()).await?;
    if listed.loops.is_empty() {
        println!("no running loop");
    } else {
        for info in &listed.loops {
            let state = match info.state {
                LoopState::Working => "working",
                LoopState::Idle => "idle",
            };
            println!("stopping {} ({state})", info.id);
        }
    }
    drop(client);

    match read_pid(&socket) {
        Some(pid) => {
            if let Err(error) = kill(Pid::from_raw(pid), Signal::SIGTERM) {
                eprintln!("could not signal the server (pid {pid}): {error}");
                return Ok(0);
            }
            wait_until_gone(&socket).await;
        }
        None => eprintln!(
            "no pid file at {}: the server was not stopped",
            pid_path(&socket).display()
        ),
    }
    Ok(0)
}

/// `<socket>.pid`, as the server writes it.
fn pid_path(socket: &Path) -> std::path::PathBuf {
    let mut path = socket.as_os_str().to_owned();
    path.push(".pid");
    std::path::PathBuf::from(path)
}

/// The pid the server recorded, if the file is there and holds one.
fn read_pid(socket: &Path) -> Option<i32> {
    std::fs::read_to_string(pid_path(socket))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Poll until the socket file is gone, or [`GONE`] has passed.
async fn wait_until_gone(socket: &Path) {
    let deadline = Instant::now() + GONE;
    while socket.exists() {
        if Instant::now() >= deadline {
            eprintln!(
                "the server did not remove {} within {}s",
                socket.display(),
                GONE.as_secs()
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
