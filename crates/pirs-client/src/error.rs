//! What can go wrong between a client and a server.

use std::path::PathBuf;
use std::time::Duration;

use pirs_protocol::{FrameError, RpcError};

/// The result of every fallible operation in this crate.
pub type Result<T> = std::result::Result<T, ClientError>;

/// Everything that can fail while reaching or talking to a pirs server.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClientError {
    /// No server is listening on the socket and [`ConnectOptions::auto_start`]
    /// was false, so none was started.
    ///
    /// [`ConnectOptions::auto_start`]: crate::ConnectOptions::auto_start
    #[error("no pirs server on {socket} (auto-start disabled): {source}")]
    NoServer {
        /// The socket that was tried.
        socket: PathBuf,
        /// Why the connection failed.
        source: std::io::Error,
    },

    /// The socket exists but could not be connected to for a reason other than
    /// "nothing is listening".
    #[error("cannot connect to {socket}: {source}")]
    Connect {
        /// The socket that was tried.
        socket: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },

    /// The server command could not be spawned.
    #[error("cannot start the pirs server ({}): {source}", command.join(" "))]
    StartFailed {
        /// The command that was attempted, program first.
        command: Vec<String>,
        /// The underlying error.
        source: std::io::Error,
    },

    /// `PIRS_SERVER_COMMAND` was set but empty, or an empty command was passed
    /// in [`ConnectOptions::server_command`].
    ///
    /// [`ConnectOptions::server_command`]: crate::ConnectOptions::server_command
    #[error("the pirs server command is empty")]
    EmptyServerCommand,

    /// The current executable could not be found, so there is no default
    /// server command.
    #[error("cannot determine the current executable to start a server: {source}")]
    NoServerCommand {
        /// The underlying error.
        source: std::io::Error,
    },

    /// A server was started but its socket never appeared.
    #[error("the pirs server did not create {socket} within {waited:?}")]
    StartTimeout {
        /// The socket that was waited for.
        socket: PathBuf,
        /// How long it was waited for.
        waited: Duration,
    },

    /// The server refused `hello` because its protocol major differs from
    /// ours ([`code::VERSION_REFUSED`](pirs_protocol::code::VERSION_REFUSED)).
    #[error("server refused hello: it speaks protocol {server}, this client speaks {protocol_version}")]
    VersionRefused {
        /// The server's protocol version, from the refusal's
        /// `data: { "server": "<version>" }`; the refusal's message when that
        /// field is absent.
        server: String,
        /// The protocol version this client offered, i.e.
        /// [`PROTOCOL_VERSION`](pirs_protocol::PROTOCOL_VERSION).
        protocol_version: String,
    },

    /// The server answered a request with an error response.
    #[error("{method} failed: {source}")]
    Rpc {
        /// The method that failed.
        method: &'static str,
        /// The server's error.
        source: RpcError,
    },

    /// A response arrived but its `result` is not the method's result type.
    #[error("{method} returned a result this client cannot read: {source}")]
    Decode {
        /// The method whose result could not be read.
        method: &'static str,
        /// The deserialisation error.
        source: serde_json::Error,
    },

    /// A line could not be encoded or decoded.
    #[error("frame: {0}")]
    Frame(#[from] FrameError),

    /// The connection is gone: the server closed it, or writing to it failed.
    /// Both [`events`] and [`slot_requests`] end at the same moment.
    ///
    /// [`events`]: crate::Client::events
    /// [`slot_requests`]: crate::Client::slot_requests
    #[error("the connection to the pirs server is closed")]
    Disconnected,
}
