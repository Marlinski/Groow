//! The conversation, on disk.
//!
//! One directory of rotating JSON-lines files, oldest first by name. This is the source of
//! truth for what was said: the window a turn works from is rebuilt from here, the learning
//! passes read it, and a person can read it with `cat`. Only the core appends to it, so the
//! mind cannot rewrite its own history.
//!
//! The format is the one the Python side already reads, because the hippocampus and the
//! trainer still parse these files.

use std::fs;
use std::path::{Path, PathBuf};

use groow_proto::turn::Message;
use serde_json::{json, Value};

/// At most this many records per file before a new one is opened.
pub const MAX_LINES: usize = 1000;

pub struct Journal {
    dir: PathBuf,
    max_lines: usize,
    /// The file currently being appended to, and how many records are in it.
    current: Option<(PathBuf, usize)>,
    /// The local day the current file belongs to, so a file never straddles midnight.
    day: String,
}

impl Journal {
    /// Open the journal, resuming the newest file when it still has room and belongs to today.
    pub fn open(dir: impl Into<PathBuf>) -> std::io::Result<Journal> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        let day = today();
        let mut j = Journal { dir, max_lines: MAX_LINES, current: None, day };
        j.resume()?;
        Ok(j)
    }

    fn resume(&mut self) -> std::io::Result<()> {
        if let Some(last) = self.files()?.pop() {
            let name = last.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string();
            if name.starts_with(&self.day) {
                let n = count_lines(&last)?;
                if n < self.max_lines {
                    self.current = Some((last, n));
                }
            }
        }
        Ok(())
    }

    /// Every journal file, oldest first.
    ///
    /// Byte order of the names is chronological because the timestamp prefix is fixed width,
    /// and `.` sorts before `_`, so a rotation suffix stays next to its own second.
    pub fn files(&self) -> std::io::Result<Vec<PathBuf>> {
        let mut out: Vec<PathBuf> = match fs::read_dir(&self.dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().map(|x| x == "jsonl").unwrap_or(false))
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        out.sort();
        Ok(out)
    }

    /// A fresh file name, unique even if several are opened within the same second.
    fn new_file(&self) -> PathBuf {
        let stamp = stamp_now();
        let first = self.dir.join(format!("{stamp}.jsonl"));
        if !first.exists() {
            return first;
        }
        // Count up rather than hashing the clock, so a third rotation in the same second
        // cannot land on a name that is already in use.
        for n in 1..1000 {
            let p = self.dir.join(format!("{stamp}_{n:03}.jsonl"));
            if !p.exists() {
                return p;
            }
        }
        self.dir.join(format!("{stamp}_overflow.jsonl"))
    }

    /// Append one record, durably. Returns the file it landed in.
    pub fn append(&mut self, rec: &Value) -> std::io::Result<PathBuf> {
        let day = today();
        let rotate = match &self.current {
            None => true,
            Some((_, n)) => *n >= self.max_lines || day != self.day,
        };
        if rotate {
            self.day = day;
            self.current = Some((self.new_file(), 0));
        }
        let (path, n) = self.current.as_mut().expect("a file is open after rotation");
        // `ts` first so a human scanning the file sees the time at the start of the line.
        let mut obj = serde_json::Map::new();
        obj.insert("ts".into(), rec.get("ts").cloned().unwrap_or_else(|| json!(groow_proto::event::now())));
        if let Some(m) = rec.as_object() {
            for (k, v) in m {
                if k != "ts" {
                    obj.insert(k.clone(), v.clone());
                }
            }
        }
        let line = serde_json::to_string(&Value::Object(obj)).unwrap_or_else(|_| "{}".into());
        crate::paths::append_line(path, &line)?;
        *n += 1;
        Ok(path.clone())
    }

    /// Append a message as the conversation record it is.
    pub fn append_message(&mut self, m: &Message) -> std::io::Result<()> {
        let v = serde_json::to_value(m).unwrap_or_else(|_| json!({}));
        self.append(&v)?;
        Ok(())
    }

    /// The last `n` records, oldest first. Unreadable lines are skipped; a corrupt line in the
    /// middle of a long history must not make the whole conversation unreadable.
    pub fn tail(&self, n: usize) -> std::io::Result<Vec<Value>> {
        let mut out: Vec<Value> = Vec::new();
        for f in self.files()?.into_iter().rev() {
            let text = fs::read_to_string(&f)?;
            for line in text.lines().rev() {
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<Value>(line) {
                    out.push(v);
                    if out.len() >= n {
                        out.reverse();
                        return Ok(out);
                    }
                }
            }
        }
        out.reverse();
        Ok(out)
    }

    /// The last `n` records written strictly before `ts`, oldest first.
    ///
    /// This is how a window reads further back than it has: it asks for what came before the
    /// oldest line it holds, and keeps asking until nothing comes back. Reading the files
    /// newest first means a long history costs only the pages actually looked at.
    pub fn before(&self, ts: f64, n: usize) -> std::io::Result<Vec<Value>> {
        let mut out: Vec<Value> = Vec::new();
        for f in self.files()?.into_iter().rev() {
            let text = fs::read_to_string(&f)?;
            for line in text.lines().rev() {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
                // A record without a time cannot be placed, so it is not offered as history.
                let Some(at) = v.get("ts").and_then(|t| t.as_f64()) else { continue };
                if at >= ts {
                    continue;
                }
                out.push(v);
                if out.len() >= n {
                    out.reverse();
                    return Ok(out);
                }
            }
        }
        out.reverse();
        Ok(out)
    }

    pub fn count(&self) -> std::io::Result<usize> {
        let mut total = 0;
        for f in self.files()? {
            total += count_lines(&f)?;
        }
        Ok(total)
    }
}

/// Rebuild the conversation window: complete exchanges only, no tool traffic.
///
/// Framed signals such as alarms and idle nudges are left out on purpose. They were prompts to
/// act, not things a person said, and replaying them as user turns teaches the mind to talk
/// to itself.
pub fn restore_window(journal: &Journal, n: usize) -> std::io::Result<Vec<Message>> {
    restore_window_without(journal, n, &Default::default())
}

/// The same, leaving out any exchange that belongs to a turn in `forget`.
///
/// The window is not a transcript, it is the example the model reads before it answers. A turn
/// that went badly does not belong in it, because showing it is the same as recommending it.
pub fn restore_window_without(
    journal: &Journal,
    n: usize,
    forget: &std::collections::HashSet<String>,
) -> std::io::Result<Vec<Message>> {
    let recs = journal.tail(n * 4)?;
    let mut msgs: Vec<Message> = Vec::new();
    for r in recs {
        let role = r.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let content = r.get("content").and_then(|v| v.as_str()).unwrap_or("");
        let kind = r.get("kind").and_then(|v| v.as_str()).unwrap_or("user");
        let has_calls = r.get("tool_calls").map(|v| !v.is_null()).unwrap_or(false);
        let turn = r.get("turn").and_then(|v| v.as_str()).unwrap_or("");
        if !turn.is_empty() && forget.contains(turn) {
            continue;
        }
        if role == "user" && (kind == "user" || kind == "command") {
            msgs.push(Message::user(content));
        } else if role == "assistant" && !content.is_empty() && !has_calls {
            msgs.push(Message::assistant(content));
        }
    }
    // Keep only whole exchanges, so the window never opens on a dangling answer. An exchange
    // whose answer repeats the one before it is left out: a window holding the same reply ten
    // times over is an instruction to give it an eleventh, and a mind that has got stuck would
    // never get unstuck while its own history keeps telling it to repeat itself.
    let mut pairs: Vec<Message> = Vec::new();
    let mut last_answer: Option<String> = None;
    let mut i = 0;
    while i + 1 < msgs.len() {
        if msgs[i].role == "user" && msgs[i + 1].role == "assistant" {
            let answer = msgs[i + 1].text().trim().to_string();
            let repeat = last_answer.as_deref() == Some(answer.as_str()) && !answer.is_empty();
            if !repeat {
                pairs.push(msgs[i].clone());
                pairs.push(msgs[i + 1].clone());
                last_answer = Some(answer);
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    let keep = n - n % 2;
    if pairs.len() > keep {
        pairs.drain(..pairs.len() - keep);
    }
    Ok(pairs)
}

fn count_lines(p: &Path) -> std::io::Result<usize> {
    match fs::read_to_string(p) {
        Ok(s) => Ok(s.lines().filter(|l| !l.trim().is_empty()).count()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e),
    }
}

fn today() -> String {
    local_now().map(|t| format!("{:04}-{:02}-{:02}", t.year(), t.month() as u8, t.day()))
        .unwrap_or_else(|| "0000-00-00".into())
}

fn stamp_now() -> String {
    match local_now() {
        Some(t) => format!(
            "{:04}-{:02}-{:02}_{:02}{:02}{:02}",
            t.year(), t.month() as u8, t.day(), t.hour(), t.minute(), t.second()
        ),
        None => format!("0000-00-00_{:06}", std::process::id() % 1_000_000),
    }
}

/// Local time, or `None` when the offset cannot be determined. Falling back to UTC silently
/// would put a file in yesterday and confuse the rotation.
fn local_now() -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::now_local().ok().or_else(|| Some(time::OffsetDateTime::now_utc()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(t: &str) -> Value { json!({"role": "user", "content": t, "kind": "user"}) }
    fn bot(t: &str) -> Value { json!({"role": "assistant", "content": t}) }

    #[test]
    fn records_come_back_in_order() {
        let d = tempfile::tempdir().unwrap();
        let mut j = Journal::open(d.path()).unwrap();
        for i in 0..5 {
            j.append(&user(&format!("m{i}"))).unwrap();
        }
        let got = j.tail(10).unwrap();
        assert_eq!(got.len(), 5);
        assert_eq!(got[0]["content"], "m0");
        assert_eq!(got[4]["content"], "m4");
    }

    #[test]
    fn every_record_starts_with_its_timestamp() {
        let d = tempfile::tempdir().unwrap();
        let mut j = Journal::open(d.path()).unwrap();
        let p = j.append(&user("hi")).unwrap();
        let line = fs::read_to_string(p).unwrap();
        assert!(line.starts_with("{\"ts\":"), "line did not lead with ts: {line}");
    }

    #[test]
    fn it_rotates_and_keeps_reading_across_files() {
        let d = tempfile::tempdir().unwrap();
        let mut j = Journal::open(d.path()).unwrap();
        j.max_lines = 3;
        for i in 0..7 {
            j.append(&user(&format!("m{i}"))).unwrap();
        }
        assert!(j.files().unwrap().len() >= 3, "should have rotated");
        assert_eq!(j.count().unwrap(), 7);
        let got = j.tail(7).unwrap();
        assert_eq!(got.len(), 7);
        assert_eq!(got[0]["content"], "m0", "the oldest record survived rotation");
        assert_eq!(got[6]["content"], "m6");
    }

    #[test]
    fn reopening_continues_the_same_file() {
        let d = tempfile::tempdir().unwrap();
        {
            let mut j = Journal::open(d.path()).unwrap();
            j.append(&user("before")).unwrap();
        }
        let mut j = Journal::open(d.path()).unwrap();
        j.append(&user("after")).unwrap();
        assert_eq!(j.files().unwrap().len(), 1, "a restart should not start a new file");
        assert_eq!(j.count().unwrap(), 2);
    }

    #[test]
    fn a_corrupt_line_does_not_hide_the_rest() {
        let d = tempfile::tempdir().unwrap();
        let mut j = Journal::open(d.path()).unwrap();
        let p = j.append(&user("good one")).unwrap();
        crate::paths::append_line(&p, "{ this is not json").unwrap();
        j.append(&user("good two")).unwrap();
        let got = j.tail(10).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[1]["content"], "good two");
    }

    #[test]
    fn the_window_keeps_only_whole_exchanges() {
        let d = tempfile::tempdir().unwrap();
        let mut j = Journal::open(d.path()).unwrap();
        j.append(&user("one")).unwrap();
        j.append(&bot("answer one")).unwrap();
        j.append(&user("two")).unwrap();
        // no answer yet
        let w = restore_window(&j, 30).unwrap();
        assert_eq!(w.len(), 2, "the dangling question must be left out");
        assert_eq!(w[0].text(), "one");
        assert_eq!(w[1].text(), "answer one");
    }

    #[test]
    fn the_window_leaves_out_tool_traffic_and_framed_signals() {
        let d = tempfile::tempdir().unwrap();
        let mut j = Journal::open(d.path()).unwrap();
        j.append(&json!({"role":"user","content":"[an alarm you set earlier] read","kind":"alarm"})).unwrap();
        j.append(&bot("reading now")).unwrap();
        j.append(&json!({"role":"assistant","content":"","tool_calls":[{"name":"shell"}]})).unwrap();
        j.append(&json!({"role":"tool","content":"output","name":"shell"})).unwrap();
        j.append(&user("hello")).unwrap();
        j.append(&bot("hi")).unwrap();
        let w = restore_window(&j, 30).unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].text(), "hello");
        assert_eq!(w[1].text(), "hi");
    }

    #[test]
    fn the_window_is_capped_to_an_even_number_of_messages() {
        let d = tempfile::tempdir().unwrap();
        let mut j = Journal::open(d.path()).unwrap();
        for i in 0..20 {
            j.append(&user(&format!("q{i}"))).unwrap();
            j.append(&bot(&format!("a{i}"))).unwrap();
        }
        let w = restore_window(&j, 7).unwrap();
        assert_eq!(w.len(), 6, "an odd budget rounds down to whole exchanges");
        assert_eq!(w[0].role, "user", "a window always opens on a question");
        assert_eq!(w[5].text(), "a19");
    }

    #[test]
    fn a_turn_that_went_badly_is_left_out_of_the_window() {
        let d = tempfile::tempdir().unwrap();
        let mut j = Journal::open(d.path()).unwrap();
        j.append(&json!({"role":"user","content":"good question","kind":"user","turn":"t1"})).unwrap();
        j.append(&json!({"role":"assistant","content":"a good answer","turn":"t1"})).unwrap();
        j.append(&json!({"role":"user","content":"bad question","kind":"user","turn":"t2"})).unwrap();
        j.append(&json!({"role":"assistant","content":"a stuck answer","turn":"t2"})).unwrap();

        let forget: std::collections::HashSet<String> = ["t2".to_string()].into_iter().collect();
        let w = restore_window_without(&j, 30, &forget).unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(w[1].text(), "a good answer");
        assert!(!w.iter().any(|m| m.text().contains("stuck")), "a bad turn was recommended back to it");
    }

    #[test]
    fn records_from_before_turns_were_stamped_are_still_shown() {
        let d = tempfile::tempdir().unwrap();
        let mut j = Journal::open(d.path()).unwrap();
        j.append(&user("old question")).unwrap();
        j.append(&bot("old answer")).unwrap();
        let forget: std::collections::HashSet<String> = ["t1".to_string()].into_iter().collect();
        assert_eq!(restore_window_without(&j, 30, &forget).unwrap().len(), 2);
    }

    #[test]
    fn a_run_of_identical_answers_collapses_to_one() {
        let d = tempfile::tempdir().unwrap();
        let mut j = Journal::open(d.path()).unwrap();
        for i in 0..10 {
            j.append(&user(&format!("q{i}"))).unwrap();
            j.append(&bot("I have started a new thought about rivers.")).unwrap();
        }
        j.append(&user("how many files are here?")).unwrap();
        j.append(&bot("Four.")).unwrap();

        let w = restore_window(&j, 30).unwrap();
        let stuck = w.iter().filter(|m| m.text().contains("rivers")).count();
        assert_eq!(stuck, 1, "the window told it to repeat itself {stuck} times");
        assert_eq!(w.last().unwrap().text(), "Four.");
    }

    #[test]
    fn different_answers_are_all_kept_even_when_they_are_short() {
        let d = tempfile::tempdir().unwrap();
        let mut j = Journal::open(d.path()).unwrap();
        for (q, a) in [("one?", "yes"), ("two?", "no"), ("three?", "yes")] {
            j.append(&user(q)).unwrap();
            j.append(&bot(a)).unwrap();
        }
        let w = restore_window(&j, 30).unwrap();
        assert_eq!(w.len(), 6, "only a run of the same answer collapses, not a repeated word");
    }

    #[test]
    fn blank_answers_do_not_collapse_into_each_other() {
        // Two questions that were never really answered are two separate exchanges, not one
        // repeat of the other. A turn that produced nothing is not a habit to prune.
        let d = tempfile::tempdir().unwrap();
        let mut j = Journal::open(d.path()).unwrap();
        j.append(&user("a")).unwrap();
        j.append(&json!({"role": "assistant", "content": "  "})).unwrap();
        j.append(&user("b")).unwrap();
        j.append(&json!({"role": "assistant", "content": " "})).unwrap();
        let w = restore_window(&j, 30).unwrap();
        assert_eq!(w.iter().filter(|m| m.role == "user").count(), 2);
    }

    #[test]
    fn an_empty_journal_gives_an_empty_window() {
        let d = tempfile::tempdir().unwrap();
        let j = Journal::open(d.path()).unwrap();
        assert!(restore_window(&j, 30).unwrap().is_empty());
        assert_eq!(j.count().unwrap(), 0);
    }
}
