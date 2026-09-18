//! The interface's connection to the core.
//!
//! Unlike a turn, an interface has to read and write at the same time: events arrive while a
//! person is typing. So the socket is split, a reader task pushes everything it sees into a
//! channel, and the drawing loop never blocks on the network.

use std::path::{Path, PathBuf};

use groow_proto::Frame;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

/// What the drawing loop hears from the core.
#[derive(Debug, Clone)]
pub enum FromCore {
    /// An event: its name and its payload.
    Event(String, Value),
    /// The answer to something the interface asked.
    Reply(u64, Value),
    Failed(u64, String),
    /// The connection ended.
    Gone,
}

pub struct Link {
    write: tokio::io::WriteHalf<UnixStream>,
    next_id: u64,
}

impl Link {
    /// Open a connection and start listening on it.
    pub async fn open(path: &Path) -> std::io::Result<(Link, mpsc::Receiver<FromCore>)> {
        let s = UnixStream::connect(path).await?;
        let (r, w) = tokio::io::split(s);
        let (tx, rx) = mpsc::channel(1024);

        tokio::spawn(async move {
            let mut r = BufReader::new(r);
            loop {
                let mut line = String::new();
                match r.read_line(&mut line).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                let Ok(f) = Frame::decode(&line) else { continue };
                let msg = match f {
                    // Events reach an interface as pushes, carrying the event name.
                    Frame::Push { name, data } => {
                        FromCore::Event(name, data.get("data").cloned().unwrap_or(Value::Null))
                    }
                    Frame::Rep { id, ok } => FromCore::Reply(id, ok),
                    Frame::Err { id, code, msg } => FromCore::Failed(id, format!("{code}: {msg}")),
                    _ => continue,
                };
                if tx.send(msg).await.is_err() {
                    return;
                }
            }
            let _ = tx.send(FromCore::Gone).await;
        });

        Ok((Link { write: w, next_id: 1 }, rx))
    }

    /// Where the core is listening. The owner's own state directory by default.
    pub fn socket_path() -> PathBuf {
        if let Some(p) = std::env::var_os("GROOW_SOCKET") {
            return PathBuf::from(p);
        }
        let state = std::env::var_os("GROOW_STATE")
            .map(PathBuf::from)
            .unwrap_or_else(default_state);
        state.join("core.sock")
    }

    /// Send a request. The answer arrives on the channel, tagged with the id returned here.
    pub async fn send(&mut self, op: &str, arg: Value) -> std::io::Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        self.write.write_all(Frame::req(id, op, arg).encode().as_bytes()).await?;
        self.write.flush().await?;
        Ok(id)
    }

    pub async fn say(&mut self, text: &str) -> std::io::Result<u64> {
        self.send("say", json!({"text": text})).await
    }

    pub async fn command(&mut self, text: &str) -> std::io::Result<u64> {
        self.send("command", json!({"text": text})).await
    }
}

/// The state directory for a person running the command by hand: the one in the project if
/// there is one, otherwise the one in their home.
pub fn default_state() -> PathBuf {
    let here = PathBuf::from("state");
    if here.join("birth.json").exists() || PathBuf::from("groow.json").exists() {
        return here;
    }
    match std::env::var_os("HOME") {
        Some(h) => PathBuf::from(h).join(".groow/state"),
        None => here,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    async fn fake_core(path: PathBuf, script: Vec<Frame>) {
        let l = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let (_r, mut w) = tokio::io::split(s);
            for f in script {
                if w.write_all(f.encode().as_bytes()).await.is_err() {
                    return;
                }
            }
            let _ = w.flush().await;
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        });
    }

    #[tokio::test]
    async fn events_arrive_with_their_name_and_payload() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("core.sock");
        fake_core(p.clone(), vec![Frame::push("turn_end", json!({"t": 1.0, "data": {"final": "hello"}}))]).await;
        let (_link, mut rx) = Link::open(&p).await.unwrap();
        match rx.recv().await.unwrap() {
            FromCore::Event(name, data) => {
                assert_eq!(name, "turn_end");
                assert_eq!(data["final"], "hello");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_closed_connection_is_announced_rather_than_going_quiet() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("core.sock");
        fake_core(p.clone(), vec![]).await;
        let (_link, mut rx) = Link::open(&p).await.unwrap();
        let mut last = None;
        while let Some(m) = rx.recv().await {
            last = Some(m);
        }
        assert!(matches!(last, Some(FromCore::Gone)), "the interface must learn the core went away");
    }

    #[tokio::test]
    async fn replies_and_refusals_are_told_apart() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("core.sock");
        fake_core(p.clone(), vec![
            Frame::rep(1, json!({"ok": true})),
            Frame::err(2, &groow_proto::WireError::Denied("quit".into())),
        ]).await;
        let (_link, mut rx) = Link::open(&p).await.unwrap();
        assert!(matches!(rx.recv().await.unwrap(), FromCore::Reply(1, _)));
        match rx.recv().await.unwrap() {
            FromCore::Failed(2, msg) => assert!(msg.starts_with("denied:")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_missing_core_is_an_error_the_interface_can_report() {
        let d = tempfile::tempdir().unwrap();
        assert!(Link::open(&d.path().join("nothing.sock")).await.is_err());
    }

    #[test]
    fn the_socket_path_can_be_told_explicitly() {
        // Set and read in one test to avoid two tests racing on the same variable.
        std::env::set_var("GROOW_SOCKET", "/tmp/somewhere/core.sock");
        assert_eq!(Link::socket_path(), PathBuf::from("/tmp/somewhere/core.sock"));
        std::env::remove_var("GROOW_SOCKET");
    }
}
