//! `pirs proxy`: the bridge a remote or contained server is reached through.
//!
//! One job: move protocol lines between this process's stdin/stdout and the
//! server's unix socket, in both directions, until either side closes. It
//! does not parse a line beyond finding its end, knows nothing about loops,
//! holds no state and opens no port (D-36). Everything about getting here —
//! authentication, encryption, the container boundary — belongs to whatever
//! spawned it:
//!
//! ```text
//! [[server]]
//! name = "build"
//! command = "ssh build pirs proxy"
//!
//! [[server]]
//! name = "jail"
//! command = "docker exec -i jail pirs proxy"
//! ```
//!
//! So it makes no assumptions about its stdio: no terminal, no prompt,
//! nothing on stdout that is not a protocol line, and a flush after every
//! one, because the other end is a program waiting for an answer and not a
//! screen that will catch up later.
//!
//! Nothing is running on the socket when the bridge arrives? Then it starts
//! a server, exactly as a local client would: every client auto-starts a
//! missing server, and over SSH or `docker exec` the proxy *is* the client's
//! stand-in on that machine, so the rule holds there too. `--no-start` turns
//! that off and the bridge fails instead. A container image still names
//! `pirs serve` as its entrypoint: the server there is the point of the
//! image, not an accident of someone attaching.

use std::path::Path;

use anyhow::Result;
use pirs_client::open_server_socket;
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::connect::{connect_options, resolve_socket};

/// Exit code when the socket is not there.
const EXIT_NO_SERVER: i32 = 1;

/// Forward until one side closes. 0 on a clean close, 1 when the socket
/// cannot be reached and no server could be started.
pub(crate) async fn run(socket: Option<&Path>, no_start: bool) -> Result<i32> {
    let socket = resolve_socket(socket);
    // The socket, and a server started on it when nothing is listening: the
    // same auto-start any local client does, and the same server command,
    // pointed at this socket (`connect_options`). No `hello` is said on it
    // -- a bridge speaks no protocol of its own.
    let stream = match open_server_socket(&connect_options(&socket, !no_start)).await {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!(
                "pirs proxy: cannot reach a pirs server on {}: {error}",
                socket.display()
            );
            return Ok(EXIT_NO_SERVER);
        }
    };
    let (from_server, to_server) = stream.into_split();

    // stdin to the server, in its own task: when stdin ends, the server is
    // told so and the answer to whatever was in flight still comes back
    // through the other half.
    let upstream = tokio::spawn(async move {
        let _ = forward(BufReader::new(tokio::io::stdin()), to_server).await;
    });

    // The server to stdout, awaited here: this is the half that ends when
    // the server closes, which is the end of the bridge.
    let downstream = forward(BufReader::new(from_server), tokio::io::stdout()).await;
    upstream.abort();
    downstream?;
    Ok(0)
}

/// Copy whole lines from one side to the other, flushing each one.
///
/// `read_until` rather than `lines()`: a line is bytes, and a bridge that
/// decoded them would be a bridge that could refuse one. The newline travels
/// with the line, so what arrives is byte for byte what was sent.
async fn forward(
    mut from: impl AsyncBufReadExt + Unpin,
    mut to: impl AsyncWrite + Unpin,
) -> std::io::Result<()> {
    let mut line = Vec::new();
    loop {
        line.clear();
        if from.read_until(b'\n', &mut line).await? == 0 {
            break;
        }
        to.write_all(&line).await?;
        to.flush().await?;
    }
    // Tell the other side we are done; on stdout this is a flush and
    // nothing more.
    let _ = to.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn whole_lines_are_forwarded_byte_for_byte_and_the_last_one_needs_no_newline() {
        let input: &[u8] = b"{\"a\":1}\n{\"b\":[2,3]}\nnot json either\n";
        let mut out = Vec::new();
        forward(BufReader::new(input), &mut out).await.expect("forwarded");
        assert_eq!(out, input);

        let mut out = Vec::new();
        forward(BufReader::new(&b"{\"a\":1}"[..]), &mut out)
            .await
            .expect("forwarded");
        assert_eq!(out, b"{\"a\":1}");
    }
}
