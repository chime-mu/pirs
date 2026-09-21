//! How a client reaches a server: a local socket, or a bridge command.
//!
//! The protocol is the same newline-delimited JSON either way (D-10), so the
//! only difference is which pair of pipes the lines travel on. A bridge is a
//! program that forwards those lines between its stdio and a server's socket
//! — `ssh build pirs proxy`, `docker exec -i jail pirs proxy` — and pirs
//! knows nothing else about it (D-05, D-36).

use std::path::PathBuf;
use std::process::Stdio;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};

use crate::error::{ClientError, Result};

/// The read half of a connection, whatever it is made of.
pub(crate) type Reader = Box<dyn AsyncRead + Send + Unpin>;

/// The write half of a connection, whatever it is made of.
pub(crate) type Writer = Box<dyn AsyncWrite + Send + Unpin>;

/// Where a server is and how to get to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    /// A unix socket on this machine.
    Socket(PathBuf),
    /// A command, program first, whose stdin and stdout carry the protocol.
    Command(Vec<String>),
}

impl Transport {
    /// How this transport reads in a message to the user: the socket path, or
    /// the command line.
    pub fn describe(&self) -> String {
        match self {
            Transport::Socket(path) => path.display().to_string(),
            Transport::Command(command) => command.join(" "),
        }
    }

    /// The socket, for a socket transport; `None` for a bridge.
    pub fn socket(&self) -> Option<&std::path::Path> {
        match self {
            Transport::Socket(path) => Some(path),
            Transport::Command(_) => None,
        }
    }
}

/// An open connection: the two halves, and the child process when the
/// transport is a bridge.
pub(crate) struct Channel {
    pub(crate) read: Reader,
    pub(crate) write: Writer,
    /// The bridge, killed when it is dropped, so a client that goes away
    /// takes its `ssh` with it.
    pub(crate) child: Option<Child>,
}

impl Channel {
    /// An accepted unix socket, split in two.
    pub(crate) fn from_socket(stream: UnixStream) -> Channel {
        let (read, write) = stream.into_split();
        Channel {
            read: Box::new(read),
            write: Box::new(write),
            child: None,
        }
    }

    /// Spawn a bridge and talk to its stdio.
    ///
    /// stdin and stdout are pipes; stderr is inherited, because a bridge's
    /// diagnostics (`ssh: Could not resolve hostname`) are the user's only
    /// clue about what went wrong, and they are not protocol.
    pub(crate) fn spawn_bridge(command: &[String]) -> Result<Channel> {
        let Some((program, args)) = command.split_first() else {
            return Err(ClientError::EmptyBridgeCommand);
        };
        let mut spawned = Command::new(program);
        spawned
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        let mut child = spawned.spawn().map_err(|source| ClientError::Bridge {
            command: command.to_vec(),
            source,
        })?;
        let write = child.stdin.take().expect("stdin was piped");
        let read = child.stdout.take().expect("stdout was piped");
        tracing::debug!(pid = child.id(), command = ?command, "started a bridge");
        Ok(Channel {
            read: Box::new(read),
            write: Box::new(write),
            child: Some(child),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_transport_describes_itself() {
        assert_eq!(
            Transport::Socket(PathBuf::from("/run/pirs.sock")).describe(),
            "/run/pirs.sock"
        );
        let bridge = Transport::Command(vec!["ssh".to_owned(), "build".to_owned()]);
        assert_eq!(bridge.describe(), "ssh build");
        assert!(bridge.socket().is_none());
    }

    #[test]
    fn an_empty_bridge_command_is_refused() {
        assert!(matches!(
            Channel::spawn_bridge(&[]),
            Err(ClientError::EmptyBridgeCommand)
        ));
    }
}
