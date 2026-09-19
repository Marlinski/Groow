//! The queue of things waiting to wake the mind.
//!
//! A directory, not a data structure in memory, so a restart loses nothing and a message left
//! while the core was down is still there when it comes back. The name of a file is its
//! priority and its arrival time, so the next thing to handle is simply the first name in
//! sorted order.
//!
//! Delivery is at least once: a signal is moved aside while it is being handled and moved back
//! if the handler died. Handling the same alarm twice is survivable; losing a person's message
//! is not.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use groow_proto::turn::SignalKind;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

static SEQ: AtomicU64 = AtomicU64::new(1);

/// One thing waiting to be handled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Signal {
    pub priority: u8,
    pub kind: SignalKind,
    pub text: String,
    pub ts: f64,
    #[serde(default)]
    pub meta: Value,
    /// How many times a process has taken this signal and failed to finish it.
    #[serde(default)]
    pub attempts: u32,
    /// The file this came from, so it can be acknowledged. Not part of the stored record.
    #[serde(skip)]
    pub token: Option<PathBuf>,
}

impl Signal {
    pub fn new(kind: SignalKind, text: impl Into<String>) -> Signal {
        Signal {
            priority: kind.priority(),
            kind,
            text: text.into(),
            ts: groow_proto::event::now(),
            meta: json!({}),
            attempts: 0,
            token: None,
        }
    }

    pub fn with_meta(mut self, meta: Value) -> Signal {
        self.meta = meta;
        self
    }
}

pub struct Mailbox {
    new: PathBuf,
    cur: PathBuf,
}

impl Mailbox {
    /// Open the mailbox, returning anything a previous life left half-handled to the queue.
    pub fn open(dir: impl AsRef<Path>) -> std::io::Result<Mailbox> {
        let dir = dir.as_ref();
        let mb = Mailbox { new: dir.join("new"), cur: dir.join("cur") };
        fs::create_dir_all(&mb.new)?;
        fs::create_dir_all(&mb.cur)?;
        mb.recover()?;
        Ok(mb)
    }

    /// Anything sitting in `cur` belonged to a process that is gone. Put it back.
    fn recover(&self) -> std::io::Result<usize> {
        let mut n = 0;
        for e in fs::read_dir(&self.cur)? {
            let p = e?.path();
            if p.extension().map(|x| x == "json").unwrap_or(false) {
                if let Some(name) = p.file_name() {
                    fs::rename(&p, self.new.join(name))?;
                    n += 1;
                }
            }
        }
        Ok(n)
    }

    /// File name, which is also the sort key: priority first, then arrival, then a tiebreak.
    fn name_for(sig: &Signal) -> String {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed) % 100_000;
        let micros = (sig.ts * 1e6) as u128;
        let kind = sig.kind.as_str();
        format!("{}-{:016}-{:05}{:05}-{}.json", sig.priority, micros, std::process::id() % 100_000, seq, kind)
    }

    /// Put a signal in the queue. Written to a temporary name first, so a reader scanning the
    /// directory never picks up a half-written message.
    pub fn push(&self, sig: &Signal) -> std::io::Result<PathBuf> {
        let name = Self::name_for(sig);
        let target = self.new.join(&name);
        let body = serde_json::to_string(sig).unwrap_or_else(|_| "{}".into());
        crate::paths::atomic_write(&target, body.as_bytes())?;
        Ok(target)
    }

    /// Everything waiting, most urgent first.
    fn pending(&self) -> std::io::Result<Vec<PathBuf>> {
        let mut v: Vec<PathBuf> = match fs::read_dir(&self.new) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false))
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        v.sort();
        Ok(v)
    }

    /// Whether anything at least this urgent is waiting.
    pub fn has(&self, max_priority: u8) -> std::io::Result<bool> {
        for p in self.pending()? {
            if let Some(pr) = priority_of(&p) {
                if pr <= max_priority {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub fn len(&self) -> std::io::Result<usize> {
        Ok(self.pending()?.len())
    }

    pub fn is_empty(&self) -> std::io::Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Take the most urgent signal, moving it aside so a crash returns it to the queue.
    ///
    /// A file that cannot be parsed is moved aside and skipped rather than retried forever,
    /// because a poison message that blocks the queue would silence the mind completely.
    pub fn pop(&self) -> std::io::Result<Option<Signal>> {
        for p in self.pending()? {
            let name = match p.file_name() {
                Some(n) => n.to_owned(),
                None => continue,
            };
            let held = self.cur.join(&name);
            // Another process may have taken it between the listing and here. That is fine.
            if fs::rename(&p, &held).is_err() {
                continue;
            }
            let text = match fs::read_to_string(&held) {
                Ok(t) => t,
                Err(_) => continue,
            };
            match serde_json::from_str::<Signal>(&text) {
                Ok(mut sig) => {
                    sig.token = Some(held);
                    return Ok(Some(sig));
                }
                Err(_) => {
                    // Poison. Keep it out of the way for a person to look at later.
                    let _ = fs::rename(&held, self.cur.join(format!("{}.bad", name.to_string_lossy())));
                    continue;
                }
            }
        }
        Ok(None)
    }

    /// Confirm a signal was handled. Until this is called it comes back after a restart.
    pub fn ack(&self, sig: &Signal) -> std::io::Result<()> {
        if let Some(t) = &sig.token {
            match fs::remove_file(t) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Put a claimed signal back in the queue for another attempt, counting the failure.
    ///
    /// The count is written back, so a message that keeps killing its process is eventually
    /// given up on rather than retried until the end of time.
    pub fn requeue(&self, sig: &Signal) -> std::io::Result<()> {
        let mut again = sig.clone();
        again.attempts += 1;
        again.token = None;
        self.push(&again)?;
        self.ack(sig)
    }

    /// Remove every waiting signal of one kind. Used to collapse repeated idle nudges.
    pub fn drop_kind(&self, kind: SignalKind) -> std::io::Result<usize> {
        let suffix = format!("-{}.json", kind.as_str());
        let mut n = 0;
        for p in self.pending()? {
            let matches = p.file_name().and_then(|s| s.to_str()).map(|s| s.ends_with(&suffix)).unwrap_or(false);
            if matches && fs::remove_file(&p).is_ok() {
                n += 1;
            }
        }
        Ok(n)
    }
}

/// The priority encoded in a file name.
fn priority_of(p: &Path) -> Option<u8> {
    p.file_name()?.to_str()?.split('-').next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_most_urgent_signal_comes_out_first() {
        let d = tempfile::tempdir().unwrap();
        let mb = Mailbox::open(d.path()).unwrap();
        mb.push(&Signal::new(SignalKind::SignalIdle, "idle thought")).unwrap();
        mb.push(&Signal::new(SignalKind::SignalAlarm, "an alarm")).unwrap();
        mb.push(&Signal::new(SignalKind::SignalUser, "a person")).unwrap();
        assert_eq!(mb.pop().unwrap().unwrap().text, "a person");
        assert_eq!(mb.pop().unwrap().unwrap().text, "an alarm");
        assert_eq!(mb.pop().unwrap().unwrap().text, "idle thought");
        assert!(mb.pop().unwrap().is_none());
    }

    #[test]
    fn signals_of_equal_priority_keep_their_order() {
        let d = tempfile::tempdir().unwrap();
        let mb = Mailbox::open(d.path()).unwrap();
        for i in 0..5 {
            let mut s = Signal::new(SignalKind::SignalUser, format!("m{i}"));
            s.ts = 1000.0 + i as f64;
            mb.push(&s).unwrap();
        }
        for i in 0..5 {
            assert_eq!(mb.pop().unwrap().unwrap().text, format!("m{i}"));
        }
    }

    #[test]
    fn an_unacknowledged_signal_comes_back_after_a_restart() {
        let d = tempfile::tempdir().unwrap();
        {
            let mb = Mailbox::open(d.path()).unwrap();
            mb.push(&Signal::new(SignalKind::SignalUser, "important")).unwrap();
            let s = mb.pop().unwrap().unwrap();
            assert_eq!(s.text, "important");
            // The process dies here without acknowledging.
        }
        let mb = Mailbox::open(d.path()).unwrap();
        assert_eq!(mb.pop().unwrap().unwrap().text, "important", "a lost turn must not lose the message");
    }

    #[test]
    fn an_acknowledged_signal_stays_gone() {
        let d = tempfile::tempdir().unwrap();
        {
            let mb = Mailbox::open(d.path()).unwrap();
            mb.push(&Signal::new(SignalKind::SignalUser, "done with this")).unwrap();
            let s = mb.pop().unwrap().unwrap();
            mb.ack(&s).unwrap();
        }
        let mb = Mailbox::open(d.path()).unwrap();
        assert!(mb.pop().unwrap().is_none());
    }

    #[test]
    fn a_poison_message_does_not_block_the_queue() {
        let d = tempfile::tempdir().unwrap();
        let mb = Mailbox::open(d.path()).unwrap();
        fs::write(d.path().join("new/0-0000000000000001-0000000001-user.json"), "{ not json").unwrap();
        mb.push(&Signal::new(SignalKind::SignalUser, "the real one")).unwrap();
        assert_eq!(mb.pop().unwrap().unwrap().text, "the real one", "the bad file must be stepped over");
    }

    #[test]
    fn meta_survives_the_round_trip() {
        let d = tempfile::tempdir().unwrap();
        let mb = Mailbox::open(d.path()).unwrap();
        mb.push(&Signal::new(SignalKind::SignalFocus, "look at this").with_meta(json!({"thought": "ab12cd"}))).unwrap();
        let s = mb.pop().unwrap().unwrap();
        assert_eq!(s.meta["thought"], "ab12cd");
        assert_eq!(s.kind, SignalKind::SignalFocus);
    }

    #[test]
    fn urgency_can_be_asked_about_without_consuming() {
        let d = tempfile::tempdir().unwrap();
        let mb = Mailbox::open(d.path()).unwrap();
        mb.push(&Signal::new(SignalKind::SignalIdle, "later")).unwrap();
        assert!(!mb.has(0).unwrap(), "an idle nudge is not urgent");
        assert!(mb.has(3).unwrap());
        mb.push(&Signal::new(SignalKind::SignalUser, "now")).unwrap();
        assert!(mb.has(0).unwrap());
        assert_eq!(mb.len().unwrap(), 2, "asking must not consume");
    }

    #[test]
    fn repeated_nudges_of_one_kind_can_be_collapsed() {
        let d = tempfile::tempdir().unwrap();
        let mb = Mailbox::open(d.path()).unwrap();
        for _ in 0..4 {
            mb.push(&Signal::new(SignalKind::SignalIdle, "curious")).unwrap();
        }
        mb.push(&Signal::new(SignalKind::SignalUser, "keep me")).unwrap();
        assert_eq!(mb.drop_kind(SignalKind::SignalIdle).unwrap(), 4);
        assert_eq!(mb.len().unwrap(), 1);
        assert_eq!(mb.pop().unwrap().unwrap().text, "keep me");
    }

    #[test]
    fn a_requeued_signal_is_handed_out_again_and_remembers_it_failed() {
        let d = tempfile::tempdir().unwrap();
        let mb = Mailbox::open(d.path()).unwrap();
        mb.push(&Signal::new(SignalKind::SignalUser, "retry me")).unwrap();
        let s = mb.pop().unwrap().unwrap();
        assert_eq!(s.attempts, 0);
        mb.requeue(&s).unwrap();
        let again = mb.pop().unwrap().unwrap();
        assert_eq!(again.text, "retry me");
        assert_eq!(again.attempts, 1, "a failure that is not counted is a failure repeated forever");
        mb.requeue(&again).unwrap();
        assert_eq!(mb.pop().unwrap().unwrap().attempts, 2);
    }

    #[test]
    fn requeueing_does_not_leave_the_old_copy_behind() {
        let d = tempfile::tempdir().unwrap();
        let mb = Mailbox::open(d.path()).unwrap();
        mb.push(&Signal::new(SignalKind::SignalUser, "once")).unwrap();
        let s = mb.pop().unwrap().unwrap();
        mb.requeue(&s).unwrap();
        assert_eq!(mb.len().unwrap(), 1, "a retry must not multiply the message");
    }

    #[test]
    fn a_half_written_file_is_never_read() {
        let d = tempfile::tempdir().unwrap();
        let mb = Mailbox::open(d.path()).unwrap();
        // A temporary from an interrupted write, which must not be picked up.
        fs::write(d.path().join("new/.0-x.json.99.tmp"), "{\"half\":").unwrap();
        mb.push(&Signal::new(SignalKind::SignalUser, "whole")).unwrap();
        assert_eq!(mb.pop().unwrap().unwrap().text, "whole");
        assert!(mb.pop().unwrap().is_none());
    }
}
