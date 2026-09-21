//! A fake pirs server: a unix socket in a temporary directory that speaks the
//! protocol from `pirs-protocol` and nothing else.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use pirs_protocol::{Envelope, Frame, RpcRequest};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};

/// A directory removed when the test ends. Unix socket paths are short, so it
/// lives directly under the system temporary directory.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> TempDir {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "pirs-client-{}-{tag}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).expect("a temporary directory");
        TempDir(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// The socket path inside it. Not created; the test decides when.
    pub fn socket(&self) -> PathBuf {
        self.0.join("pirs.sock")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One accepted connection, read as lines and written as lines.
pub struct Peer {
    lines: Lines<BufReader<OwnedReadHalf>>,
    write: OwnedWriteHalf,
}

impl Peer {
    /// Accept exactly one connection on `listener`.
    pub async fn accept(listener: &UnixListener) -> Peer {
        let (stream, _) = listener.accept().await.expect("a client connects");
        Peer::from_stream(stream)
    }

    pub fn from_stream(stream: UnixStream) -> Peer {
        let (read, write) = stream.into_split();
        Peer {
            lines: BufReader::new(read).lines(),
            write,
        }
    }

    /// The next message, or `None` at end of stream.
    pub async fn recv(&mut self) -> Option<Envelope> {
        loop {
            let line = self.lines.next_line().await.expect("a readable line")?;
            match Frame::decode(&line) {
                Ok(envelope) => return Some(envelope),
                Err(pirs_protocol::FrameError::Empty) => continue,
                Err(error) => panic!("the client sent an undecodable line: {error}"),
            }
        }
    }

    /// The next message, which must be a request.
    pub async fn recv_request(&mut self) -> RpcRequest {
        match self.recv().await.expect("a request") {
            Envelope::Request(request) => request,
            other => panic!("expected a request, got {other:?}"),
        }
    }

    pub async fn send(&mut self, envelope: &Envelope) {
        let line = Frame::encode(envelope).expect("an encodable message");
        self.write
            .write_all(line.as_bytes())
            .await
            .expect("a writable socket");
        self.write.flush().await.expect("a writable socket");
    }
}
