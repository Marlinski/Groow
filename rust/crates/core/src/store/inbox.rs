//! Questions the mind has put to its mentor.
//!
//! Attention is the scarce resource here. A person will not answer twenty questions, so the
//! mind is given a small budget of open ones: asking a twenty-first drops the oldest, and a
//! question nobody answers expires. Both outcomes are fed back as a cost, which is what
//! teaches it to ask less and ask better.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QStatus {
    Open,
    Answered,
    Expired,
    Dropped,
}

impl QStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            QStatus::Open => "open",
            QStatus::Answered => "answered",
            QStatus::Expired => "expired",
            QStatus::Dropped => "dropped",
        }
    }

    /// What this outcome is worth to the mind when the reward finally lands.
    pub fn reward(&self) -> f64 {
        match self {
            QStatus::Answered => 0.6,
            QStatus::Expired => -0.4,
            QStatus::Dropped => -0.6,
            QStatus::Open => 0.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub ts: f64,
    pub question: String,
    #[serde(default)]
    pub context: String,
    pub status: QStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_ts: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

/// What `ask` hands back to the mind: not just an id, but what it cost.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Receipt {
    pub ok: bool,
    pub id: String,
    pub queued: String,
    pub open_questions: usize,
    pub budget: usize,
    pub expires_in_hours: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropped_to_make_room: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

pub struct Inbox {
    path: PathBuf,
    pub budget: usize,
    pub expiry_hours: f64,
}

impl Inbox {
    pub fn new(path: impl Into<PathBuf>, budget: usize, expiry_hours: f64) -> Inbox {
        Inbox { path: path.into(), budget, expiry_hours }
    }

    /// Every question ever asked, in the order it was asked. Unreadable lines are skipped.
    pub fn all(&self) -> std::io::Result<Vec<Question>> {
        let text = crate::paths::read_opt(&self.path)?.unwrap_or_default();
        Ok(text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<Question>(l).ok())
            .collect())
    }

    fn save(&self, qs: &[Question]) -> std::io::Result<()> {
        let mut out = String::new();
        for q in qs {
            out.push_str(&serde_json::to_string(q).unwrap_or_default());
            out.push('\n');
        }
        crate::paths::atomic_write(&self.path, out.as_bytes())
    }

    pub fn open_questions(&self) -> std::io::Result<Vec<Question>> {
        Ok(self.all()?.into_iter().filter(|q| q.status == QStatus::Open).collect())
    }

    /// Ask. If the budget is full the oldest open question is dropped to make room, and the
    /// receipt says so, because the mind should feel that it displaced something.
    pub fn add(&self, question: &str, context: &str) -> std::io::Result<Receipt> {
        let now = groow_proto::event::now();
        let mut qs = self.all()?;
        let mut dropped = None;

        let open: Vec<usize> = qs.iter().enumerate()
            .filter(|(_, q)| q.status == QStatus::Open)
            .map(|(i, _)| i).collect();
        if open.len() >= self.budget {
            if let Some(&i) = open.iter().min_by(|a, b| {
                qs[**a].ts.partial_cmp(&qs[**b].ts).unwrap_or(std::cmp::Ordering::Equal)
            }) {
                qs[i].status = QStatus::Dropped;
                qs[i].resolved_ts = Some(now);
                dropped = Some(serde_json::json!({
                    "id": qs[i].id,
                    "question": truncate(&qs[i].question, 120),
                    "waited": ago(now - qs[i].ts),
                }));
            }
        }

        let q = Question {
            id: short_id(),
            ts: now,
            question: question.trim().to_string(),
            context: context.trim().to_string(),
            status: QStatus::Open,
            resolved_ts: None,
            answer: None,
        };
        let id = q.id.clone();
        let queued = truncate(&q.question, 200);
        qs.push(q);
        self.save(&qs)?;

        let open_now = qs.iter().filter(|q| q.status == QStatus::Open).count();
        Ok(Receipt {
            ok: true,
            id,
            queued,
            open_questions: open_now,
            budget: self.budget,
            expires_in_hours: self.expiry_hours,
            note: dropped.as_ref().map(|_| {
                "your mentor's attention is limited; an older question was dropped to make room".to_string()
            }),
            dropped_to_make_room: dropped,
        })
    }

    /// Close a question out. Only an open question can be resolved, so a late answer to an
    /// already expired question does not silently rewrite history.
    pub fn resolve(&self, id: &str, status: QStatus, answer: &str) -> std::io::Result<Option<Question>> {
        let mut qs = self.all()?;
        let mut hit = None;
        for q in qs.iter_mut() {
            if q.id == id && q.status == QStatus::Open {
                q.status = status;
                q.resolved_ts = Some(groow_proto::event::now());
                if !answer.trim().is_empty() {
                    q.answer = Some(truncate(answer, 500));
                }
                hit = Some(q.clone());
            }
        }
        if hit.is_some() {
            self.save(&qs)?;
        }
        Ok(hit)
    }

    /// Expire everything that has waited too long. Returns what expired, so each one can be
    /// charged for and shown to the mind together rather than one interruption at a time.
    pub fn expire_due(&self) -> std::io::Result<Vec<Question>> {
        let now = groow_proto::event::now();
        let cutoff = now - self.expiry_hours * 3600.0;
        let mut qs = self.all()?;
        let mut gone = Vec::new();
        for q in qs.iter_mut() {
            if q.status == QStatus::Open && q.ts < cutoff {
                q.status = QStatus::Expired;
                q.resolved_ts = Some(now);
                gone.push(q.clone());
            }
        }
        if !gone.is_empty() {
            self.save(&qs)?;
        }
        Ok(gone)
    }

    /// Mark every open question answered, for when a person has dealt with them out of band.
    pub fn clear(&self) -> std::io::Result<usize> {
        let now = groow_proto::event::now();
        let mut qs = self.all()?;
        let mut n = 0;
        for q in qs.iter_mut() {
            if q.status == QStatus::Open {
                q.status = QStatus::Answered;
                q.resolved_ts = Some(now);
                n += 1;
            }
        }
        if n > 0 {
            self.save(&qs)?;
        }
        Ok(n)
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect()
    }
}

fn ago(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    if s >= 86400 { format!("{}d", s / 86400) }
    else if s >= 3600 { format!("{}h", s / 3600) }
    else if s >= 60 { format!("{}m", s / 60) }
    else { format!("{s}s") }
}

/// A short, human-quotable id. Collisions are checked by the caller's storage, not here.
fn short_id() -> String {
    use rand::Rng;
    let mut r = rand::rng();
    (0..6).map(|_| {
        let n: u8 = r.random_range(0..16);
        std::char::from_digit(n as u32, 16).unwrap_or('0')
    }).collect()
}

/// The id generator, shared with the schedule so ids look alike everywhere.
pub fn short_id_pub() -> String { short_id() }

/// A convenience for tests and for the status endpoint.
pub fn path_in(state: &Path) -> PathBuf {
    state.join("mentor_inbox.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inbox(d: &tempfile::TempDir) -> Inbox {
        Inbox::new(d.path().join("inbox.jsonl"), 3, 48.0)
    }

    #[test]
    fn a_question_is_open_until_it_is_resolved() {
        let d = tempfile::tempdir().unwrap();
        let ib = inbox(&d);
        let r = ib.add("what should I read?", "").unwrap();
        assert!(r.ok);
        assert_eq!(ib.open_questions().unwrap().len(), 1);
        ib.resolve(&r.id, QStatus::Answered, "try the river book").unwrap();
        assert!(ib.open_questions().unwrap().is_empty());
        let q = &ib.all().unwrap()[0];
        assert_eq!(q.status, QStatus::Answered);
        assert_eq!(q.answer.as_deref(), Some("try the river book"));
        assert!(q.resolved_ts.is_some());
    }

    #[test]
    fn the_budget_drops_the_oldest_and_says_so() {
        let d = tempfile::tempdir().unwrap();
        let ib = inbox(&d);
        let mut ids = Vec::new();
        for i in 0..3 {
            ids.push(ib.add(&format!("q{i}"), "").unwrap().id);
        }
        let r = ib.add("one too many", "").unwrap();
        assert!(r.dropped_to_make_room.is_some(), "the receipt must admit the cost");
        assert!(r.note.is_some());
        assert_eq!(r.open_questions, 3, "the budget still holds");
        let all = ib.all().unwrap();
        assert_eq!(all[0].status, QStatus::Dropped, "the oldest went");
        assert_eq!(all[1].status, QStatus::Open);
    }

    #[test]
    fn a_dropped_question_is_the_oldest_not_the_first_in_the_file() {
        let d = tempfile::tempdir().unwrap();
        let ib = inbox(&d);
        // Write them out of chronological order on purpose.
        let mut qs = Vec::new();
        for (i, ts) in [("new", 3000.0), ("oldest", 1000.0), ("middle", 2000.0)] {
            qs.push(Question {
                id: format!("id{}", qs.len()), ts, question: i.to_string(),
                context: String::new(), status: QStatus::Open, resolved_ts: None, answer: None,
            });
        }
        ib.save(&qs).unwrap();
        let r = ib.add("pushes one out", "").unwrap();
        let dropped = r.dropped_to_make_room.unwrap();
        assert_eq!(dropped["question"], "oldest");
    }

    #[test]
    fn questions_expire_when_nobody_answers() {
        let d = tempfile::tempdir().unwrap();
        let ib = Inbox::new(d.path().join("i.jsonl"), 5, 1.0);
        let stale = Question {
            id: "aaa111".into(), ts: groow_proto::event::now() - 7200.0,
            question: "anyone?".into(), context: String::new(),
            status: QStatus::Open, resolved_ts: None, answer: None,
        };
        let fresh = Question { id: "bbb222".into(), ts: groow_proto::event::now(), ..stale.clone() };
        ib.save(&[stale, fresh]).unwrap();
        let gone = ib.expire_due().unwrap();
        assert_eq!(gone.len(), 1);
        assert_eq!(gone[0].id, "aaa111");
        assert_eq!(ib.open_questions().unwrap().len(), 1, "the fresh one is untouched");
    }

    #[test]
    fn expiring_twice_charges_once() {
        let d = tempfile::tempdir().unwrap();
        let ib = Inbox::new(d.path().join("i.jsonl"), 5, 1.0);
        ib.save(&[Question {
            id: "aaa111".into(), ts: 0.0, question: "old".into(), context: String::new(),
            status: QStatus::Open, resolved_ts: None, answer: None,
        }]).unwrap();
        assert_eq!(ib.expire_due().unwrap().len(), 1);
        assert_eq!(ib.expire_due().unwrap().len(), 0, "an expired question must not expire again");
    }

    #[test]
    fn a_late_answer_to_a_closed_question_changes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let ib = inbox(&d);
        let r = ib.add("q", "").unwrap();
        ib.resolve(&r.id, QStatus::Expired, "").unwrap();
        assert!(ib.resolve(&r.id, QStatus::Answered, "too late").unwrap().is_none());
        assert_eq!(ib.all().unwrap()[0].status, QStatus::Expired);
    }

    #[test]
    fn each_outcome_carries_its_own_price() {
        assert!(QStatus::Answered.reward() > 0.0);
        assert!(QStatus::Expired.reward() < 0.0);
        assert!(QStatus::Dropped.reward() < QStatus::Expired.reward(), "being displaced is worse than being ignored");
    }

    #[test]
    fn clearing_closes_everything_open() {
        let d = tempfile::tempdir().unwrap();
        let ib = inbox(&d);
        ib.add("a", "").unwrap();
        ib.add("b", "").unwrap();
        assert_eq!(ib.clear().unwrap(), 2);
        assert!(ib.open_questions().unwrap().is_empty());
        assert_eq!(ib.clear().unwrap(), 0);
    }

    #[test]
    fn a_missing_file_is_an_empty_inbox() {
        let d = tempfile::tempdir().unwrap();
        let ib = Inbox::new(d.path().join("never-written.jsonl"), 3, 48.0);
        assert!(ib.all().unwrap().is_empty());
        assert!(ib.expire_due().unwrap().is_empty());
    }

    #[test]
    fn ids_are_distinct_across_many_questions() {
        let d = tempfile::tempdir().unwrap();
        let ib = Inbox::new(d.path().join("i.jsonl"), 1000, 48.0);
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            assert!(seen.insert(ib.add("q", "").unwrap().id), "duplicate id");
        }
    }
}
