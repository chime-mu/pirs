//! Minimal Server-Sent Events parser over a byte stream.

use bytes::Bytes;
use futures::{Stream, StreamExt};
use std::pin::Pin;
use std::task::{Context, Poll};

#[derive(Debug, Clone, Default)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

pub struct SseStream<S> {
    inner: S,
    buffer: Vec<u8>,
    pending: std::collections::VecDeque<SseEvent>,
    done: bool,
}

impl<S> SseStream<S> {
    pub fn new(inner: S) -> Self {
        SseStream { inner, buffer: Vec::new(), pending: Default::default(), done: false }
    }

    fn drain_buffer(&mut self) {
        // Events are separated by a blank line. Handle both \n\n and \r\n\r\n.
        loop {
            let Some(pos) = find_event_boundary(&self.buffer) else { break };
            let (raw, sep_len) = pos;
            let chunk = self.buffer.drain(..raw + sep_len).collect::<Vec<u8>>();
            let text = String::from_utf8_lossy(&chunk[..raw]).to_string();
            let mut ev = SseEvent::default();
            let mut data_lines = Vec::new();
            for line in text.lines() {
                if let Some(rest) = line.strip_prefix("event:") {
                    ev.event = Some(rest.trim().to_string());
                } else if let Some(rest) = line.strip_prefix("data:") {
                    data_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
                }
            }
            if data_lines.is_empty() && ev.event.is_none() {
                continue;
            }
            ev.data = data_lines.join("\n");
            self.pending.push_back(ev);
        }
    }
}

fn find_event_boundary(buf: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i + 1 < buf.len() {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some((i, 2));
        }
        if i + 3 < buf.len() && &buf[i..i + 4] == b"\r\n\r\n" {
            return Some((i, 4));
        }
        i += 1;
    }
    None
}

impl<S, E> Stream for SseStream<S>
where
    S: Stream<Item = Result<Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    type Item = Result<SseEvent, String>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if let Some(ev) = self.pending.pop_front() {
                return Poll::Ready(Some(Ok(ev)));
            }
            if self.done {
                return Poll::Ready(None);
            }
            match self.inner.poll_next_unpin(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    self.buffer.extend_from_slice(&bytes);
                    self.drain_buffer();
                }
                Poll::Ready(Some(Err(e))) => {
                    self.done = true;
                    return Poll::Ready(Some(Err(e.to_string())));
                }
                Poll::Ready(None) => {
                    self.done = true;
                    // Flush a trailing event without a terminating blank line.
                    if !self.buffer.is_empty() {
                        self.buffer.extend_from_slice(b"\n\n");
                        self.drain_buffer();
                        self.buffer.clear();
                    }
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}
