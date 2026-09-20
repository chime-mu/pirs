//! Newline-delimited JSON framing.
//!
//! One message is one line: compact JSON followed by `\n`, with no raw newline
//! inside (serde_json escapes them as `\n` in strings). The same framing is
//! used on the socket and on a called process's stdin/stdout, so
//! `echo '{...}' | ./tool` and `socat - UNIX:pirs.sock` both work (D-10).

use serde::de::DeserializeOwned;
use serde::Serialize;
use std::io::{BufRead, Write};

/// Why a line could not be encoded or decoded.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The line was empty (or only whitespace); there is no message on it.
    #[error("empty line")]
    Empty,
    /// The line was not valid JSON for the expected type.
    #[error("invalid JSON line: {0}")]
    Json(#[from] serde_json::Error),
    /// Reading or writing the underlying stream failed.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Encode and decode one message per line. Synchronous; an async transport
/// splits on `\n` itself and calls [`Frame::decode`] per line.
pub struct Frame;

impl Frame {
    /// Encode `message` as one `\n`-terminated line.
    ///
    /// The result contains exactly one newline, the last byte: serde_json
    /// escapes control characters inside strings and emits no whitespace
    /// between tokens.
    pub fn encode<T: Serialize + ?Sized>(message: &T) -> Result<String, FrameError> {
        let mut line = serde_json::to_string(message)?;
        debug_assert!(!line.contains('\n'), "serde_json emitted a raw newline");
        line.push('\n');
        Ok(line)
    }

    /// Decode one line. A trailing `\n` or `\r\n` is ignored; an empty line is
    /// [`FrameError::Empty`], which a reader may skip.
    pub fn decode<T: DeserializeOwned>(line: &str) -> Result<T, FrameError> {
        let line = line.trim_end_matches(['\n', '\r']);
        if line.trim().is_empty() {
            return Err(FrameError::Empty);
        }
        Ok(serde_json::from_str(line)?)
    }

    /// Encode `message` and write it, followed by a flush.
    pub fn write_to<W: Write, T: Serialize + ?Sized>(
        writer: &mut W,
        message: &T,
    ) -> Result<(), FrameError> {
        writer.write_all(Frame::encode(message)?.as_bytes())?;
        writer.flush()?;
        Ok(())
    }

    /// Read the next non-empty line and decode it. `None` at end of stream.
    pub fn read_from<R: BufRead, T: DeserializeOwned>(
        reader: &mut R,
    ) -> Option<Result<T, FrameError>> {
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => return None,
                Ok(_) => match Frame::decode(&line) {
                    Err(FrameError::Empty) => continue,
                    other => return Some(other),
                },
                Err(e) => return Some(Err(e.into())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn newlines_inside_strings_are_escaped() {
        let line = Frame::encode(&json!({"text": "a\nb\r\nc"})).unwrap();
        assert_eq!(line.matches('\n').count(), 1);
        assert!(line.ends_with('\n'));
        let back: serde_json::Value = Frame::decode(&line).unwrap();
        assert_eq!(back["text"], "a\nb\r\nc");
    }

    #[test]
    fn empty_lines_are_reported_and_skipped() {
        assert!(matches!(
            Frame::decode::<serde_json::Value>("\n"),
            Err(FrameError::Empty)
        ));
        let mut input = std::io::Cursor::new("\n\n{\"a\":1}\r\n\n{\"a\":2}");
        let a: serde_json::Value = Frame::read_from(&mut input).unwrap().unwrap();
        let b: serde_json::Value = Frame::read_from(&mut input).unwrap().unwrap();
        assert_eq!((a["a"].as_i64(), b["a"].as_i64()), (Some(1), Some(2)));
        assert!(Frame::read_from::<_, serde_json::Value>(&mut input).is_none());
    }

    #[test]
    fn write_to_flushes_one_line() {
        let mut out = Vec::new();
        Frame::write_to(&mut out, &json!({"k": "v"})).unwrap();
        assert_eq!(out, b"{\"k\":\"v\"}\n");
    }
}
