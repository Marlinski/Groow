//! Line-delimited frames over any byte stream.
//!
//! Two halves that never share a lock: a reader task owns the read half, a writer task owns
//! the write half, and everything else reaches the writer through a channel. Nothing can hold
//! the write half while waiting for a read, which is the shape most connection deadlocks take.

use std::io;

use groow_proto::{Frame, ProtoError};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

/// Longest line accepted. A frame larger than this is a bug or an attack, not a message.
pub const MAX_LINE: usize = 8 * 1024 * 1024;

/// Reads frames off a stream until it ends.
pub struct FrameReader<R> {
    inner: BufReader<R>,
    line: String,
}

impl<R: tokio::io::AsyncRead + Unpin> FrameReader<R> {
    pub fn new(read: R) -> Self {
        FrameReader { inner: BufReader::new(read), line: String::new() }
    }

    /// The next frame, or `None` at clean end of stream.
    ///
    /// A malformed line is reported and the caller decides; it does not silently skip, because
    /// a skipped frame is a lost reply and a hung waiter.
    pub async fn next(&mut self) -> Result<Option<Frame>, CodecError> {
        loop {
            self.line.clear();
            let n = self.inner.read_line(&mut self.line).await?;
            if n == 0 {
                return Ok(None);
            }
            if n > MAX_LINE {
                return Err(CodecError::TooLong(n));
            }
            match Frame::decode(&self.line) {
                Ok(f) => return Ok(Some(f)),
                // Blank lines are keepalives, not frames.
                Err(ProtoError::Empty) => continue,
                Err(e) => return Err(CodecError::Proto(e)),
            }
        }
    }
}

/// Writes frames to a stream.
pub struct FrameWriter<W> {
    inner: W,
}

impl<W: AsyncWrite + Unpin> FrameWriter<W> {
    pub fn new(write: W) -> Self {
        FrameWriter { inner: write }
    }

    pub async fn send(&mut self, frame: &Frame) -> Result<(), CodecError> {
        self.inner.write_all(frame.encode().as_bytes()).await?;
        self.inner.flush().await?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("frame of {0} bytes exceeds the limit")]
    TooLong(usize),
    #[error(transparent)]
    Proto(#[from] ProtoError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use groow_proto::WireError;
    use serde_json::json;

    #[tokio::test]
    async fn reads_back_what_it_wrote() {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut w = FrameWriter::new(&mut buf);
            w.send(&Frame::req(1, "hello", json!({}))).await.unwrap();
            w.send(&Frame::ev("token", 1.0, json!({"delta": "hi"}))).await.unwrap();
            w.send(&Frame::err(1, &WireError::Idle)).await.unwrap();
        }
        let mut r = FrameReader::new(&buf[..]);
        assert!(matches!(r.next().await.unwrap(), Some(Frame::Req { .. })));
        assert!(matches!(r.next().await.unwrap(), Some(Frame::Ev { .. })));
        assert!(matches!(r.next().await.unwrap(), Some(Frame::Err { .. })));
        assert!(r.next().await.unwrap().is_none(), "stream should end cleanly");
    }

    #[tokio::test]
    async fn blank_lines_are_skipped_but_garbage_is_not() {
        let input = "\n\n{\"f\":\"ev\",\"name\":\"log\",\"t\":0.0}\n";
        let mut r = FrameReader::new(input.as_bytes());
        assert!(matches!(r.next().await.unwrap(), Some(Frame::Ev { .. })));

        let mut bad = FrameReader::new(&b"{not json}\n"[..]);
        assert!(bad.next().await.is_err(), "garbage must surface, never be skipped");
    }

    #[tokio::test]
    async fn a_truncated_final_line_is_an_error_not_a_frame() {
        let mut r = FrameReader::new(&b"{\"f\":\"req\",\"id\":1"[..]);
        assert!(r.next().await.is_err());
    }

    #[tokio::test]
    async fn unicode_survives_the_round_trip() {
        let mut buf: Vec<u8> = Vec::new();
        let f = Frame::ev("message", 0.0, json!({"content": "héllo 🌱 \n embedded"}));
        FrameWriter::new(&mut buf).send(&f).await.unwrap();
        assert_eq!(buf.iter().filter(|b| **b == b'\n').count(), 1, "newlines must be escaped");
        let mut r = FrameReader::new(&buf[..]);
        assert_eq!(r.next().await.unwrap().unwrap(), f);
    }
}
