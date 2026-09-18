//! One connection, from hello to goodbye.
//!
//! A harness process opens exactly one of these and it carries everything: the turn context it
//! asks for, the generation it streams back, the events it sends up, and the tool results it
//! records. The lifetime of the connection is the lifetime of the turn, which buys two things
//! for free. The core learns immediately when a turn process dies, because the socket closes.
//! And it can cancel a turn by closing its end, because the process sees end of file.
//!
//! The read loop never does slow work itself. Anything that waits happens in a spawned task,
//! so a long generation cannot stop the same connection from being cancelled.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use groow_proto::event::{Event, EventName};
use groow_proto::turn::{Epoch, Message, TurnOutcome};
use groow_proto::{Frame, Op, WireError};
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

use crate::brain::Brain;
use crate::codec::{FrameReader, FrameWriter};
use crate::config::Config;
use crate::hub::Handle;
use crate::peer::Peer;

/// How many frames may be queued for one peer before writes start being dropped.
const WRITE_DEPTH: usize = 1024;

/// What one connection knows about itself.
struct Conn {
    peer: Peer,
    hub: Handle,
    brain: Brain,
    cfg: Arc<Config>,
    out: mpsc::Sender<Frame>,
    /// The turn this connection claimed, so it can be cleaned up if the process disappears.
    claimed: Arc<std::sync::Mutex<Option<(String, Epoch)>>>,
    finished: Arc<AtomicBool>,
}

impl Clone for Conn {
    fn clone(&self) -> Self {
        Conn {
            peer: self.peer,
            hub: self.hub.clone(),
            brain: self.brain.clone(),
            cfg: self.cfg.clone(),
            out: self.out.clone(),
            claimed: self.claimed.clone(),
            finished: self.finished.clone(),
        }
    }
}

/// Serve one connection until it closes.
pub async fn serve<R, W>(read: R, write: W, peer: Peer, hub: Handle, brain: Brain, cfg: Arc<Config>)
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (tx, mut rx) = mpsc::channel::<Frame>(WRITE_DEPTH);

    // One task owns the write half. Nothing else ever touches it, so two replies can never
    // interleave halfway through a line.
    let writer = tokio::spawn(async move {
        let mut w = FrameWriter::new(write);
        while let Some(f) = rx.recv().await {
            if w.send(&f).await.is_err() {
                break;
            }
        }
    });

    let conn = Conn {
        peer,
        hub: hub.clone(),
        brain,
        cfg,
        out: tx,
        claimed: Arc::new(std::sync::Mutex::new(None)),
        finished: Arc::new(AtomicBool::new(false)),
    };

    let mut reader = FrameReader::new(read);
    loop {
        match reader.next().await {
            Ok(Some(frame)) => conn.on_frame(frame).await,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(peer = %conn.peer.describe(), "dropping connection: {e}");
                break;
            }
        }
    }

    // The process is gone. If it had a turn and never closed it out, the core must not be left
    // believing a turn is still running, or nothing would ever run again.
    let held = conn.claimed.lock().ok().and_then(|g| g.clone());
    if let Some((turn, _)) = held {
        if !conn.finished.load(Ordering::SeqCst) {
            hub.abandon(&turn, "its process went away before finishing").await;
        }
    }
    drop(conn);
    let _ = writer.await;
}

impl Conn {
    async fn send(&self, f: Frame) {
        // A peer that has stopped reading is not worth waiting for.
        let _ = self.out.try_send(f);
    }

    async fn on_frame(&self, frame: Frame) {
        match frame {
            Frame::Req { id, op, arg } => self.on_request(id, op, arg).await,
            Frame::Ev { name, t, data } => self.on_event(name, t, data).await,
            // A peer has nothing to reply to and nothing to push. Ignoring these keeps a
            // confused client from being able to drive the core through its own answers.
            Frame::Rep { .. } | Frame::Err { .. } | Frame::Part { .. } | Frame::Push { .. } => {}
        }
    }

    async fn on_event(&self, name: String, t: f64, data: Value) {
        // Record the measurable part, then pass it on to whoever is watching.
        if name == EventName::ToolResult.as_str() {
            let turn = data.get("turn").and_then(|v| v.as_str()).unwrap_or("");
            let tool = data.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let actor = data.get("actor").and_then(|v| v.as_str()).unwrap_or("main");
            let ok = data.get("ok").and_then(|v| v.as_bool()).unwrap_or(true);
            let secs = data.get("seconds").and_then(|v| v.as_f64()).unwrap_or(0.0);
            self.hub.tool_ran(turn, tool, actor, ok, secs).await;
        }
        self.hub.emit(Event { name, t, data }).await;
    }

    async fn on_request(&self, id: u64, op_name: String, arg: Value) {
        let Some(op) = Op::parse(&op_name) else {
            self.send(Frame::err(id, &WireError::UnknownOp(op_name))).await;
            return;
        };
        if !op.allowed_for(self.peer.role) {
            tracing::warn!(peer = %self.peer.describe(), op = op.name(), "refused");
            self.send(Frame::err(id, &WireError::Denied(op.name().to_string()))).await;
            return;
        }
        // Generation is the only op that can take minutes. It runs on its own task so that a
        // cancel arriving on this same connection is still read while it is in flight.
        if op == Op::Complete {
            let me = self.clone();
            tokio::spawn(async move { me.complete(id, arg).await });
            return;
        }
        let me = self.clone();
        tokio::spawn(async move {
            let result = me.dispatch(op, arg).await;
            match result {
                Ok(v) => me.send(Frame::rep(id, v)).await,
                Err(e) => me.send(Frame::err(id, &e)).await,
            }
        });
    }

    async fn dispatch(&self, op: Op, arg: Value) -> Result<Value, WireError> {
        let s = |k: &str| arg.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        match op {
            Op::Hello => {
                let mut v = self.hub.hello(&self.peer.describe()).await?;
                v["role"] = json!(match self.peer.role {
                    groow_proto::ops::Role::Agent => "agent",
                    groow_proto::ops::Role::Viewer => "viewer",
                    groow_proto::ops::Role::Mentor => "mentor",
                });
                Ok(v)
            }
            Op::Status => self.hub.status().await,
            Op::TurnClaim => {
                let ctx = self.hub.claim(self.peer.pid).await?;
                if let Ok(mut g) = self.claimed.lock() {
                    *g = Some((ctx.turn.clone(), ctx.epoch));
                }
                serde_json::to_value(ctx).map_err(|e| WireError::Internal(e.to_string()))
            }
            Op::TurnAppend => {
                let msg: Message = serde_json::from_value(
                    arg.get("message").cloned().unwrap_or(Value::Null),
                ).map_err(|e| WireError::BadArg(format!("message: {e}")))?;
                let epoch = Epoch(arg.get("epoch").and_then(|v| v.as_u64()).unwrap_or(0));
                self.hub.append(&s("turn"), epoch, msg).await
            }
            Op::TurnEnd => {
                let out: TurnOutcome = serde_json::from_value(arg.clone())
                    .map_err(|e| WireError::BadArg(format!("outcome: {e}")))?;
                let r = self.hub.finish(out).await;
                if r.is_ok() {
                    self.finished.store(true, Ordering::SeqCst);
                }
                r
            }
            Op::Ask => self.hub.ask_mentor(&s("question"), &s("context")).await,
            Op::Think => {
                let steps = arg.get("max_steps").and_then(|v| v.as_u64()).unwrap_or(8) as u32;
                self.hub.think(&s("goal"), steps).await
            }
            Op::Thought => self.hub.thought(&s("action"), &s("id"), &s("text")).await,
            Op::Recall => {
                let n = arg.get("n").and_then(|v| v.as_u64()).unwrap_or(40) as usize;
                self.hub.recall(n).await
            }
            Op::Schedule => {
                let action = if s("action").is_empty() { "list".into() } else { s("action") };
                self.hub.schedule(&action, &s("text"), &s("when"), &s("every"), &s("id"), &s("by")).await
            }
            Op::Inbox => {
                let action = if s("action").is_empty() { "list".into() } else { s("action") };
                self.hub.inbox(&action, &s("id"), &s("answer")).await
            }
            Op::Say => {
                let kind = if s("kind") == "note" {
                    groow_proto::turn::SignalKind::Note
                } else {
                    groow_proto::turn::SignalKind::User
                };
                self.hub.say(&s("text"), kind, json!({})).await
            }
            Op::Command => {
                self.hub.say(&s("text"), groow_proto::turn::SignalKind::Command, json!({})).await
            }
            Op::Quit => self.hub.shutdown().await,
            // Handled elsewhere or not yet wired to the Python side.
            Op::Complete => Err(WireError::Internal("generation is handled separately".into())),
            Op::Consolidate | Op::Train => Err(WireError::BadArg(
                "lifecycle work runs through the learning side; ask it directly".into(),
            )),
        }
    }

    /// Generate, streaming the pieces back on this connection as they arrive.
    async fn complete(&self, id: u64, arg: Value) {
        let messages: Vec<Message> = match serde_json::from_value(arg.get("messages").cloned().unwrap_or(json!([]))) {
            Ok(m) => m,
            Err(e) => {
                self.send(Frame::err(id, &WireError::BadArg(format!("messages: {e}")))).await;
                return;
            }
        };
        let tools = serde_json::from_value(arg.get("tools").cloned().unwrap_or(json!([]))).unwrap_or_default();
        let priority = arg.get("priority").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
        let stream_out = arg.get("stream").and_then(|v| v.as_bool()).unwrap_or(true);

        let req = crate::brain::request_from(&self.cfg, messages, tools, priority);
        let out = self.out.clone();
        let hub = self.hub.clone();
        let turn = arg.get("turn").and_then(|v| v.as_str()).unwrap_or("").to_string();

        // Deltas go two ways: back to the process that asked, so it can act on the answer, and
        // out to every watching interface, so a person sees it appear.
        let (dtx, mut drx) = mpsc::unbounded_channel::<String>();
        let pump = tokio::spawn(async move {
            while let Some(d) = drx.recv().await {
                if stream_out {
                    let _ = out.try_send(Frame::Part { id, data: json!({"delta": d}) });
                }
                hub.emit(Event::new(EventName::Token, json!({"delta": d, "turn": turn}))).await;
            }
        });

        let result = self.brain.complete(&req, |d| { let _ = dtx.send(d.to_string()); }).await;
        drop(dtx);
        let _ = pump.await;

        match result {
            Ok(g) => {
                self.send(Frame::rep(id, json!({
                    "text": g.text, "tokens": g.tokens, "seconds": g.seconds,
                }))).await;
            }
            Err(e) => {
                self.hub.emit(Event::log("error", format!("generation failed: {e}"))).await;
                self.send(Frame::err(id, &WireError::Internal(e.to_string()))).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::{hub_for_test, Hub};
    use groow_proto::ops::Role;
    use tokio::io::{duplex, AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// A client end of a connection served by a real hub, with a brain that is not there.
    ///
    /// The reader is kept across calls: a fresh buffered reader each time would swallow
    /// whatever had already arrived and make the test lie about what the core sent.
    struct Client {
        write: tokio::io::WriteHalf<tokio::io::DuplexStream>,
        read: BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
    }

    impl Client {
        async fn send(&mut self, id: u64, op: &str, arg: Value) {
            self.write.write_all(Frame::req(id, op, arg).encode().as_bytes()).await.unwrap();
            self.write.flush().await.unwrap();
        }

        async fn recv(&mut self) -> Frame {
            let mut line = String::new();
            tokio::time::timeout(std::time::Duration::from_secs(5), self.read.read_line(&mut line))
                .await
                .expect("the core should answer")
                .unwrap();
            Frame::decode(&line).unwrap()
        }

        async fn call(&mut self, id: u64, op: &str, arg: Value) -> Frame {
            self.send(id, op, arg).await;
            self.recv().await
        }
    }

    async fn rig(role: Role) -> (Client, Handle, tempfile::TempDir) {
        let d = tempfile::tempdir().unwrap();
        let hub: Hub = hub_for_test(d.path()).unwrap();
        let (handle, _join) = hub.spawn();
        let (mine, theirs) = duplex(64 * 1024);
        let (r, w) = tokio::io::split(theirs);
        let peer = Peer { uid: 1000, pid: 42, role };
        let cfg = Arc::new(Config::default());
        tokio::spawn(serve(r, w, peer, handle.clone(), Brain::new("http://127.0.0.1:1"), cfg));
        let (cr, cw) = tokio::io::split(mine);
        (Client { write: cw, read: BufReader::new(cr) }, handle, d)
    }

    #[tokio::test]
    async fn hello_says_which_role_the_core_assigned() {
        let (mut s, _h, _d) = rig(Role::Agent).await;
        let f = s.call(1, "hello", json!({})).await;
        match f {
            Frame::Rep { ok, .. } => assert_eq!(ok["role"], "agent"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_unknown_op_is_named_back() {
        let (mut s, _h, _d) = rig(Role::Agent).await;
        match s.call(1, "delete_everything", json!({})).await {
            Frame::Err { code, msg, .. } => {
                assert_eq!(code, "unknown_op");
                assert!(msg.contains("delete_everything"));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[tokio::test]
    async fn the_mind_cannot_shut_the_core_down() {
        let (mut s, _h, _d) = rig(Role::Agent).await;
        match s.call(1, "quit", json!({})).await {
            Frame::Err { code, .. } => assert_eq!(code, "denied"),
            other => panic!("the mind was allowed to quit: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_viewer_cannot_claim_a_turn_but_can_speak() {
        let (mut s, _h, _d) = rig(Role::Viewer).await;
        match s.call(1, "turn.claim", json!({})).await {
            Frame::Err { code, .. } => assert_eq!(code, "denied"),
            other => panic!("a terminal claimed a turn: {other:?}"),
        }
        match s.call(2, "say", json!({"text": "hello"})).await {
            Frame::Rep { ok, .. } => assert_eq!(ok["ok"], true),
            other => panic!("a viewer could not speak: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_turn_runs_end_to_end_over_one_connection() {
        let (mut s, hub, _d) = rig(Role::Agent).await;
        hub.say("what is a river", groow_proto::turn::SignalKind::User, json!({})).await.unwrap();
        hub.next_duty(groow_proto::event::now()).await.unwrap();

        let ctx = match s.call(1, "turn.claim", json!({})).await {
            Frame::Rep { ok, .. } => ok,
            other => panic!("could not claim: {other:?}"),
        };
        let (turn, epoch) = (ctx["turn"].as_str().unwrap().to_string(), ctx["epoch"].as_u64().unwrap());
        assert_eq!(ctx["framed"], "what is a river");

        let r = s.call(2, "turn.append", json!({
            "turn": turn, "epoch": epoch,
            "message": {"role": "assistant", "content": "water going downhill"},
        })).await;
        assert!(matches!(r, Frame::Rep { .. }), "append refused: {r:?}");

        let r = s.call(3, "turn.end", json!({
            "turn": turn, "epoch": epoch, "final_text": "water going downhill",
            "flags": [], "tools_used": 0, "seconds": 0.4,
        })).await;
        assert!(matches!(r, Frame::Rep { .. }), "end refused: {r:?}");
    }

    #[tokio::test]
    async fn a_turn_whose_process_vanishes_does_not_block_the_next_one() {
        let (mut s, hub, _d) = rig(Role::Agent).await;
        hub.say("first", groow_proto::turn::SignalKind::User, json!({})).await.unwrap();
        hub.next_duty(groow_proto::event::now()).await.unwrap();
        s.call(1, "turn.claim", json!({})).await;

        // The process dies without ending its turn.
        drop(s);
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;

        // The message was a person's, so it comes back rather than being lost.
        let duty = hub.next_duty(groow_proto::event::now()).await.unwrap();
        match duty {
            crate::hub::Duty::Turn(sig) => assert_eq!(sig.text, "first"),
            other => panic!("the core stayed stuck on the dead turn: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_generation_failure_comes_back_as_an_error_not_a_hang() {
        let (mut s, _h, _d) = rig(Role::Agent).await;
        // The brain in this rig is not listening.
        match s.call(1, "complete", json!({"messages": [{"role":"user","content":"hi"}]})).await {
            Frame::Err { code, msg, .. } => {
                assert_eq!(code, "internal");
                assert!(msg.contains("not answering"), "unhelpful: {msg}");
            }
            other => panic!("expected an error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn requests_can_overtake_each_other_without_being_confused() {
        let (mut s, _h, _d) = rig(Role::Agent).await;
        // Two requests in flight at once; the answers must carry the right ids.
        s.send(7, "status", json!({})).await;
        s.send(9, "hello", json!({})).await;
        let mut ids = vec![s.recv().await.id().unwrap(), s.recv().await.id().unwrap()];
        ids.sort();
        assert_eq!(ids, vec![7, 9]);
    }

    #[tokio::test]
    async fn a_malformed_argument_is_refused_without_dropping_the_connection() {
        let (mut s, _h, _d) = rig(Role::Agent).await;
        match s.call(1, "turn.append", json!({"turn": "x", "epoch": 1, "message": "not a message"})).await {
            Frame::Err { code, .. } => assert_eq!(code, "bad_arg"),
            other => panic!("unexpected: {other:?}"),
        }
        // Still usable afterwards.
        assert!(matches!(s.call(2, "status", json!({})).await, Frame::Rep { .. }));
    }
}
