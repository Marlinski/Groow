//! The harness side of the connection.
//!
//! One socket, opened at startup and held for the life of the process. Everything the harness
//! needs goes through it, and when it closes the turn is over one way or another.
//!
//! Requests are made one at a time, which is all a single turn ever needs, so there is no
//! bookkeeping of outstanding calls and no way for two answers to be confused. Frames that
//! arrive while waiting and are not the answer are handled on the way past.

use std::path::Path;

use groow_proto::event::Event;
use groow_proto::Frame;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("cannot reach the core at {0}: {1}")]
    Unreachable(String, String),
    #[error("the connection ended")]
    Closed,
    #[error("{code}: {msg}")]
    Refused { code: String, msg: String },
    #[error("the core sent something unreadable: {0}")]
    Garbled(String),
}

impl ClientError {
    /// Whether this means the turn is over rather than that one request failed.
    pub fn is_fatal(&self) -> bool {
        match self {
            ClientError::Closed | ClientError::Unreachable(_, _) => true,
            ClientError::Refused { code, .. } => code == "stale_epoch" || code == "cancelled",
            ClientError::Garbled(_) => false,
        }
    }
}

pub struct Client {
    write: tokio::io::WriteHalf<UnixStream>,
    read: BufReader<tokio::io::ReadHalf<UnixStream>>,
    next_id: u64,
    /// Set when the core asks for the turn to stop.
    cancelled: bool,
}

impl Client {
    pub async fn connect(path: &Path) -> Result<Client, ClientError> {
        let s = UnixStream::connect(path)
            .await
            .map_err(|e| ClientError::Unreachable(path.display().to_string(), e.to_string()))?;
        let (r, w) = tokio::io::split(s);
        Ok(Client { write: w, read: BufReader::new(r), next_id: 1, cancelled: false })
    }

    /// Where the core is listening, as the core told this process when it started it.
    pub fn socket_path() -> std::path::PathBuf {
        std::env::var_os("GROOW_SOCKET")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("state/core.sock"))
    }

    pub fn was_cancelled(&self) -> bool {
        self.cancelled
    }

    async fn write_frame(&mut self, f: &Frame) -> Result<(), ClientError> {
        self.write
            .write_all(f.encode().as_bytes())
            .await
            .map_err(|_| ClientError::Closed)?;
        self.write.flush().await.map_err(|_| ClientError::Closed)
    }

    /// Ask the core for something and wait for the answer.
    pub async fn call(&mut self, op: &str, arg: Value) -> Result<Value, ClientError> {
        self.call_streaming(op, arg, |_| {}).await
    }

    /// The same, calling `on_part` for each piece of an answer that arrives in pieces.
    pub async fn call_streaming(
        &mut self,
        op: &str,
        arg: Value,
        mut on_part: impl FnMut(&Value),
    ) -> Result<Value, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_frame(&Frame::req(id, op, arg)).await?;

        loop {
            let frame = self.next_frame().await?;
            match frame {
                Frame::Rep { id: got, ok } if got == id => return Ok(ok),
                Frame::Err { id: got, code, msg } if got == id => {
                    return Err(ClientError::Refused { code, msg })
                }
                Frame::Part { id: got, data } if got == id => on_part(&data),
                Frame::Push { name, .. } => {
                    if name == "cancel" {
                        self.cancelled = true;
                    }
                }
                // An answer to something else, or a frame that makes no sense here. Ignoring
                // it is safe: only one request is ever outstanding.
                _ => {}
            }
        }
    }

    async fn next_frame(&mut self) -> Result<Frame, ClientError> {
        let mut line = String::new();
        let n = self.read.read_line(&mut line).await.map_err(|_| ClientError::Closed)?;
        if n == 0 {
            return Err(ClientError::Closed);
        }
        Frame::decode(&line).map_err(|e| ClientError::Garbled(e.to_string()))
    }

    /// Tell the core something happened. Nothing waits on this.
    pub async fn emit(&mut self, ev: Event) -> Result<(), ClientError> {
        self.write_frame(&Frame::Ev { name: ev.name, t: ev.t, data: ev.data }).await
    }

    /// The conscious turn's contract.
    pub async fn claim(&mut self) -> Result<groow_proto::turn::TurnContext, ClientError> {
        let v = self.call("turn.claim", json!({})).await?;
        serde_json::from_value(v).map_err(|e| ClientError::Garbled(e.to_string()))
    }

    pub async fn append(
        &mut self,
        turn: &str,
        epoch: groow_proto::turn::Epoch,
        msg: &groow_proto::turn::Message,
    ) -> Result<(), ClientError> {
        self.call("turn.append", json!({"turn": turn, "epoch": epoch, "message": msg})).await?;
        Ok(())
    }

    pub async fn end(&mut self, out: &groow_proto::turn::TurnOutcome) -> Result<(), ClientError> {
        let arg = serde_json::to_value(out).map_err(|e| ClientError::Garbled(e.to_string()))?;
        self.call("turn.end", arg).await?;
        Ok(())
    }

    /// An inner thought's contract. Its trace is its own, so it is addressed by id rather
    /// than by whatever turn the conscious mind happens to be taking.
    pub async fn claim_thought(&mut self, id: &str) -> Result<groow_proto::turn::TurnContext, ClientError> {
        let v = self.call("thought.claim", json!({"id": id})).await?;
        serde_json::from_value(v).map_err(|e| ClientError::Garbled(e.to_string()))
    }

    pub async fn append_thought(&mut self, id: &str, msg: &groow_proto::turn::Message) -> Result<(), ClientError> {
        self.call("thought.append", json!({"id": id, "message": msg})).await?;
        Ok(())
    }

    pub async fn end_thought(&mut self, id: &str, final_text: &str, flags: &[String]) -> Result<(), ClientError> {
        self.call("thought.end", json!({"id": id, "final_text": final_text, "flags": flags})).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use groow_proto::WireError;
    use tokio::net::UnixListener;

    /// A stand-in core that answers from a script, so the client can be tested on its own.
    async fn fake_core(path: std::path::PathBuf, script: Vec<Frame>) {
        let l = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let (r, mut w) = tokio::io::split(s);
            let mut r = BufReader::new(r);
            let mut line = String::new();
            // Wait for one request, then play the script back.
            let _ = r.read_line(&mut line).await;
            for f in script {
                if w.write_all(f.encode().as_bytes()).await.is_err() {
                    return;
                }
            }
            let _ = w.flush().await;
            // Hold the connection open so the client does not see a close it did not expect.
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        });
    }

    #[tokio::test]
    async fn an_answer_comes_back() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.sock");
        fake_core(p.clone(), vec![Frame::rep(1, json!({"ok": true}))]).await;
        let mut c = Client::connect(&p).await.unwrap();
        assert_eq!(c.call("status", json!({})).await.unwrap()["ok"], true);
    }

    #[tokio::test]
    async fn a_refusal_carries_its_code() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.sock");
        fake_core(p.clone(), vec![Frame::err(1, &WireError::StaleEpoch("t1".into()))]).await;
        let mut c = Client::connect(&p).await.unwrap();
        match c.call("turn.append", json!({})).await.unwrap_err() {
            ClientError::Refused { code, .. } => assert_eq!(code, "stale_epoch"),
            other => panic!("unexpected: {other}"),
        }
    }

    #[tokio::test]
    async fn pieces_arrive_before_the_answer() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.sock");
        fake_core(p.clone(), vec![
            Frame::Part { id: 1, data: json!({"delta": "he"}) },
            Frame::Part { id: 1, data: json!({"delta": "llo"}) },
            Frame::rep(1, json!({"text": "hello"})),
        ]).await;
        let mut c = Client::connect(&p).await.unwrap();
        let mut seen = String::new();
        let out = c.call_streaming("complete", json!({}), |d| {
            seen.push_str(d["delta"].as_str().unwrap_or(""));
        }).await.unwrap();
        assert_eq!(seen, "hello");
        assert_eq!(out["text"], "hello");
    }

    #[tokio::test]
    async fn a_cancel_arriving_mid_call_is_noticed() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.sock");
        fake_core(p.clone(), vec![
            Frame::push("cancel", json!({"why": "a person spoke"})),
            Frame::rep(1, json!({"ok": true})),
        ]).await;
        let mut c = Client::connect(&p).await.unwrap();
        c.call("status", json!({})).await.unwrap();
        assert!(c.was_cancelled(), "the turn should know it was asked to stop");
    }

    #[tokio::test]
    async fn an_answer_to_something_else_does_not_satisfy_this_request() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.sock");
        fake_core(p.clone(), vec![
            Frame::rep(99, json!({"wrong": true})),
            Frame::rep(1, json!({"right": true})),
        ]).await;
        let mut c = Client::connect(&p).await.unwrap();
        let v = c.call("status", json!({})).await.unwrap();
        assert_eq!(v["right"], true, "the client took an answer meant for another request");
    }

    #[tokio::test]
    async fn a_connection_that_ends_is_reported_rather_than_hanging() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("s.sock");
        fake_core(p.clone(), vec![]).await;
        let mut c = Client::connect(&p).await.unwrap();
        let e = tokio::time::timeout(std::time::Duration::from_secs(8), c.call("status", json!({})))
            .await
            .expect("the client hung waiting for an answer that will never come")
            .unwrap_err();
        assert!(e.is_fatal());
    }

    #[tokio::test]
    async fn a_missing_core_is_a_clear_error() {
        let d = tempfile::tempdir().unwrap();
        let e = match Client::connect(&d.path().join("nothing.sock")).await {
            Err(e) => e,
            Ok(_) => panic!("connected to a socket that does not exist"),
        };
        assert!(matches!(e, ClientError::Unreachable(_, _)));
        assert!(e.is_fatal());
    }

    #[test]
    fn a_stale_turn_ends_the_process_but_a_bad_argument_does_not() {
        assert!(ClientError::Refused { code: "stale_epoch".into(), msg: String::new() }.is_fatal());
        assert!(!ClientError::Refused { code: "bad_arg".into(), msg: String::new() }.is_fatal());
    }
}
