//! Bringing the core up: the socket, the scheduler, and the loop that runs turns.
//!
//! The scheduler is the only thing that starts a turn, which is why there is no lock anywhere
//! in this design. One scheduler, one turn at a time, by construction rather than by
//! agreement.

use std::path::PathBuf;
use std::sync::Arc;

use groow_proto::event::{Event, EventName};
use serde_json::json;
use tokio::net::UnixListener;

use crate::brain::Brain;
use crate::config::Config;
use crate::hub::{Duty, Handle};
use crate::peer::Peer;
use crate::spawn::Spawner;

/// Everything the running core holds on to.
pub struct Core {
    pub hub: Handle,
    pub cfg: Arc<Config>,
    pub brain: Brain,
    pub spawner: Spawner,
    pub socket: PathBuf,
    pub agent_uid: Option<u32>,
    /// The interpreter that runs the learning passes.
    pub python: String,
    pub config: PathBuf,
}

impl Core {
    /// Listen on the socket and serve connections until the core stops.
    ///
    /// The socket is removed first if it is stale, and its permissions are set so that only
    /// the owner and the mind can reach it. A socket the world can connect to would undo every
    /// other precaution here.
    pub async fn listen(self: Arc<Self>) -> anyhow::Result<tokio::task::JoinHandle<()>> {
        if self.socket.exists() {
            std::fs::remove_file(&self.socket)?;
        }
        if let Some(d) = self.socket.parent() {
            std::fs::create_dir_all(d)?;
        }
        let listener = UnixListener::bind(&self.socket)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.socket, std::fs::Permissions::from_mode(0o660))?;
            if let Some(uid) = self.agent_uid {
                // The mind must be able to open it; nobody else on the machine should.
                let _ = std::os::unix::fs::chown(&self.socket, Some(0), Some(uid));
            }
        }
        tracing::info!(socket = %self.socket.display(), "listening");

        let me = self.clone();
        Ok(tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let peer = match Peer::of(&stream, me.agent_uid) {
                            Ok(p) => p,
                            Err(e) => {
                                tracing::warn!("refusing a connection: {e}");
                                continue;
                            }
                        };
                        let (r, w) = tokio::io::split(stream);
                        tokio::spawn(crate::conn::serve(
                            r, w, peer, me.hub.clone(), me.brain.clone(), me.cfg.clone(),
                        ));
                    }
                    Err(e) => {
                        tracing::error!("the socket stopped accepting: {e}");
                        return;
                    }
                }
            }
        }))
    }

    /// The loop that decides what happens next and starts the process that does it.
    ///
    /// It never holds state of its own. Every decision comes from the hub, so the scheduler
    /// cannot drift out of step with what is actually recorded.
    pub async fn run_scheduler(self: Arc<Self>) {
        loop {
            let duty = match self.hub.next_duty().await {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!("the core stopped scheduling: {e}");
                    return;
                }
            };
            match duty {
                Duty::Idle(secs) => {
                    tokio::time::sleep(std::time::Duration::from_secs_f64(secs.clamp(0.05, 30.0))).await;
                }
                Duty::Turn(_) => {
                    // The hub has already opened the turn; the process claims it over the
                    // socket. If it cannot even be started, the turn is released at once
                    // rather than being left open forever.
                    match self.spawner.turn() {
                        Ok(child) => self.watch(child, "turn", None).await,
                        Err(e) => {
                            tracing::error!("could not start a turn: {e}");
                            self.hub.emit(Event::log("error", format!("could not start a turn: {e}"))).await;
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        }
                    }
                }
                Duty::Learn(what) => {
                    // A learning pass is slow and must not block a person who starts talking
                    // mid-way, so the scheduler waits on it here rather than in the hub, and
                    // a turn that arrives simply runs next.
                    match self.spawner.learn(what, &self.python, &self.config, &self.spawner.state) {
                        Ok(child) => {
                            self.hub.emit(Event::new(EventName::Learned, json!({
                                "kind": what, "state": "started",
                            }))).await;
                            self.watch(child, "learning pass", None).await;
                            self.hub.emit(Event::new(EventName::Learned, json!({
                                "kind": what, "state": "finished",
                            }))).await;
                        }
                        Err(e) => {
                            tracing::warn!("could not start the {what} pass: {e}");
                            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                        }
                    }
                }
                Duty::Thought(id) => match self.spawner.thought(&id) {
                    Ok(child) => self.watch(child, "thought", Some(id)).await,
                    Err(e) => {
                        tracing::error!("could not start a thought: {e}");
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    }
                },
            }
        }
    }

    /// Wait for a spawned process, with a ceiling on how long it may take.
    ///
    /// A turn that never ends is worse than a turn that fails: nothing else can run. The
    /// timeout kills it, and the connection closing tells the hub to release the turn.
    async fn watch(&self, mut child: tokio::process::Child, what: &str, id: Option<String>) {
        let limit = if what == "turn" { self.cfg.turn_timeout } else { self.cfg.thought_timeout };
        let pid = child.id().unwrap_or(0);
        let stderr = child.stderr.take();

        let waited = tokio::time::timeout(
            std::time::Duration::from_secs_f64(limit.max(1.0)),
            child.wait(),
        ).await;

        match waited {
            Ok(Ok(status)) if status.success() => {}
            Ok(Ok(status)) => {
                let why = drain(stderr).await;
                tracing::warn!(pid, %what, "exited with {status}: {why}");
                self.hub.emit(Event::new(EventName::Log, json!({
                    "level": "error",
                    "text": format!("a {what} exited badly ({status}){}", if why.is_empty() { String::new() } else { format!(": {why}") }),
                    "thought": id,
                }))).await;
            }
            Ok(Err(e)) => tracing::error!(pid, "could not wait for the {what}: {e}"),
            Err(_) => {
                tracing::error!(pid, "the {what} passed its {limit} second limit; stopping it");
                let _ = child.start_kill();
                let _ = child.wait().await;
                self.hub.emit(Event::log("error", format!("a {what} ran past its time limit and was stopped"))).await;
            }
        }
    }
}

/// The last words of a process that failed, for the log.
async fn drain(stderr: Option<tokio::process::ChildStderr>) -> String {
    use tokio::io::AsyncReadExt;
    let Some(mut e) = stderr else { return String::new() };
    let mut s = String::new();
    let _ = e.read_to_string(&mut s).await;
    let s = s.trim();
    // The tail is where the reason usually is.
    s.chars().rev().take(400).collect::<Vec<_>>().into_iter().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::hub_for_test;

    fn core_for(d: &tempfile::TempDir, exe: &str) -> Arc<Core> {
        let hub = hub_for_test(d.path()).unwrap();
        let (handle, _join) = hub.spawn();
        let mut cfg = Config::default();
        cfg.turn_timeout = 2.0;
        cfg.thought_timeout = 2.0;
        Arc::new(Core {
            hub: handle,
            cfg: Arc::new(cfg),
            brain: Brain::new("http://127.0.0.1:1"),
            spawner: Spawner {
                exe: PathBuf::from(exe),
                uid: None,
                gid: None,
                home: d.path().to_path_buf(),
                socket: d.path().join("core.sock"),
                state: d.path().to_path_buf(),
            },
            socket: d.path().join("core.sock"),
            agent_uid: None,
            python: "/bin/true".into(),
            config: d.path().join("groow.json"),
        })
    }

    #[tokio::test]
    async fn the_socket_is_not_open_to_the_whole_machine() {
        let d = tempfile::tempdir().unwrap();
        let core = core_for(&d, "/bin/true");
        let _h = core.clone().listen().await.unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&core.socket).unwrap().permissions().mode() & 0o007;
            assert_eq!(mode, 0, "anyone on the machine could talk to the core");
        }
    }

    #[tokio::test]
    async fn a_stale_socket_does_not_stop_the_core_starting() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("core.sock"), "left over from last time").unwrap();
        let core = core_for(&d, "/bin/true");
        assert!(core.listen().await.is_ok());
    }

    #[tokio::test]
    async fn a_process_that_never_ends_is_stopped_rather_than_blocking_forever() {
        let d = tempfile::tempdir().unwrap();
        let core = core_for(&d, "/bin/sleep");
        // A child that would outlast the limit several times over.
        let child = tokio::process::Command::new("/bin/sleep")
            .arg("120")
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();
        core.watch(child, "turn", None).await;
        assert!(started.elapsed().as_secs() < 10, "the watchdog did not fire");
    }

    #[tokio::test]
    async fn a_failing_process_is_reported_with_its_last_words() {
        let d = tempfile::tempdir().unwrap();
        let core = core_for(&d, "/bin/sh");
        let mut rx = core.hub.subscribe().await.unwrap();
        let child = tokio::process::Command::new("/bin/sh")
            .args(["-c", "echo 'it went wrong in the usual place' >&2; exit 3"])
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        core.watch(child, "turn", None).await;
        let ev = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("no event")
            .expect("channel closed");
        let text = ev.data["text"].as_str().unwrap_or("");
        assert!(text.contains("exited badly"), "unhelpful: {text}");
        assert!(text.contains("usual place"), "the reason was lost: {text}");
    }

    #[tokio::test]
    async fn a_turn_that_cannot_be_started_does_not_wedge_the_scheduler() {
        let d = tempfile::tempdir().unwrap();
        let core = core_for(&d, "/definitely/not/a/program");
        core.hub.say("hello", groow_proto::turn::SignalKind::User, json!({})).await.unwrap();
        let sched = tokio::spawn(core.clone().run_scheduler());
        // It should keep going rather than dying on the failed spawn.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(!sched.is_finished(), "the scheduler gave up on one bad spawn");
        sched.abort();
    }
}
