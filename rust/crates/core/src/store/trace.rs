//! Exactly what was sent, and exactly what came back.
//!
//! The conversation says what was said. This says what the model was actually given to say it
//! from: the whole request, system prompt and window and tool schemas and sampling settings,
//! as it went over the wire, one line per exchange.
//!
//! It is the only place that answers "why did it say that". A window rebuilt from the journal
//! is a reconstruction; a trace is the thing itself. They differ in exactly the cases anyone
//! is investigating — a prompt that was not what you thought, a window missing the message you
//! expected, a tool schema it never saw.
//!
//! One file per run, so reading one costs nothing and deleting old ones is a file at a time.
//! It is off by default in nothing: it is on, because the cost is disk and the alternative is
//! guessing, but `trace` in the settings turns it off and `trace_keep` says how many runs are
//! worth keeping.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

pub struct Traces {
    dir: PathBuf,
    keep: usize,
}

impl Traces {
    pub fn new(dir: impl Into<PathBuf>, keep: usize) -> Traces {
        Traces { dir: dir.into(), keep }
    }

    fn path(&self, run: &str) -> PathBuf {
        self.dir.join(format!("{run}.jsonl"))
    }

    /// Write down one thing that happened during a run.
    ///
    /// Failures are swallowed: a trace is for reading afterwards, and a creature that stops
    /// working because it could not write one would be a poor trade.
    pub fn put(&self, run: &str, what: &str, at: f64, body: Value) {
        if run.is_empty() {
            return;
        }
        let _ = fs::create_dir_all(&self.dir);
        let line = json!({"at": at, "what": what, "body": body});
        let _ = crate::paths::append_line(&self.path(run), &line.to_string());
    }

    /// Everything traced for one run, in order.
    pub fn of(&self, run: &str) -> Vec<Value> {
        let Ok(text) = fs::read_to_string(self.path(run)) else { return Vec::new() };
        text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    /// Which runs have a trace, oldest first.
    fn runs(&self) -> Vec<PathBuf> {
        let Ok(rd) = fs::read_dir(&self.dir) else { return Vec::new() };
        let mut v: Vec<PathBuf> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
            .collect();
        // The names are run ids, which begin with the time they started, so sorting them is
        // sorting by age.
        v.sort();
        v
    }

    /// Keep the most recent runs and forget the rest. Called when a run ends, so the oldest
    /// goes at about the rate new ones arrive.
    pub fn prune(&self) -> usize {
        let all = self.runs();
        let over = all.len().saturating_sub(self.keep);
        for p in all.into_iter().take(over) {
            let _ = fs::remove_file(p);
        }
        over
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_was_sent_comes_back_in_the_order_it_was_sent() {
        let d = tempfile::tempdir().unwrap();
        let t = Traces::new(d.path(), 10);
        t.put("t1", "request", 1.0, json!({"messages": [{"role": "system", "content": "you are"}]}));
        t.put("t1", "answer", 2.0, json!({"text": "hello", "tokens": 2}));
        t.put("t2", "request", 3.0, json!({"messages": []}));

        let of_it = t.of("t1");
        assert_eq!(of_it.len(), 2, "one run's trace is only that run's");
        assert_eq!(of_it[0]["what"], "request");
        assert_eq!(of_it[0]["body"]["messages"][0]["content"], "you are");
        assert_eq!(of_it[1]["what"], "answer");
        assert!(t.of("nothing at all").is_empty());
    }

    #[test]
    fn only_the_last_so_many_runs_are_kept() {
        let d = tempfile::tempdir().unwrap();
        let t = Traces::new(d.path(), 2);
        for run in ["01", "02", "03", "04"] {
            t.put(run, "request", 1.0, json!({}));
        }
        assert_eq!(t.prune(), 2, "two were over the limit");
        assert!(t.of("01").is_empty() && t.of("02").is_empty(), "the oldest went");
        assert!(!t.of("03").is_empty() && !t.of("04").is_empty(), "the newest stayed");
    }

    #[test]
    fn a_trace_without_a_run_to_belong_to_is_dropped_rather_than_written_somewhere() {
        let d = tempfile::tempdir().unwrap();
        let t = Traces::new(d.path().join("traces"), 10);
        t.put("", "request", 1.0, json!({}));
        assert!(t.runs().is_empty());
    }
}
