//! Inner thoughts.
//!
//! Work the mind sets aside for itself and comes back to. Each one is a file holding its own
//! conversation, run by its own process. The main thread spawns them and can pause, resume or
//! kill them; each reaches back once, with the summary it finishes on.
//!
//! Only the process running a thought writes its history. The core patches the few fields it
//! owns, such as the status, so the two never fight over the same file.

use std::fs;
use std::path::PathBuf;

use groow_proto::turn::Message;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThoughtStatus {
    Running,
    Paused,
    Done,
    Killed,
}

impl ThoughtStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ThoughtStatus::Running => "running",
            ThoughtStatus::Paused => "paused",
            ThoughtStatus::Done => "done",
            ThoughtStatus::Killed => "killed",
        }
    }

    /// A finished thought is never restarted.
    pub fn is_final(&self) -> bool {
        matches!(self, ThoughtStatus::Done | ThoughtStatus::Killed)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Thought {
    pub id: String,
    pub goal: String,
    pub status: ThoughtStatus,
    pub created: f64,
    pub updated: f64,
    #[serde(default)]
    pub steps: u32,
    pub max_steps: u32,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub history: Vec<Message>,
    #[serde(default)]
    pub tools_used: Vec<String>,
    #[serde(default)]
    pub interrupted: u32,
    /// The process currently running it, if any.
    #[serde(default)]
    pub pid: Option<u32>,
    /// Consecutive attempts that produced no step. A thought whose process keeps dying is
    /// parked rather than respawned forever.
    #[serde(default)]
    pub attempts: u32,
    /// Unknown fields from a newer version are carried through untouched rather than dropped,
    /// so an older core cannot silently destroy a newer core's data.
    #[serde(flatten, default)]
    pub extra: serde_json::Map<String, Value>,
}

/// The most steps a single thought may take, whatever it asks for.
pub const MAX_STEPS_CAP: u32 = 60;

/// How many times a thought's process may fail to make a step before it is parked.
pub const MAX_ATTEMPTS: u32 = 3;

/// How long to leave a thought alone between steps, so a failing one cannot become a loop.
pub const STEP_GAP: f64 = 3.0;

pub struct Thoughts {
    dir: PathBuf,
}

impl Thoughts {
    pub fn new(dir: impl Into<PathBuf>) -> Thoughts {
        Thoughts { dir: dir.into() }
    }

    fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.json"))
    }

    pub fn get(&self, id: &str) -> std::io::Result<Option<Thought>> {
        match crate::paths::read_opt(&self.path(id))? {
            None => Ok(None),
            Some(s) => Ok(serde_json::from_str(&s).ok()),
        }
    }

    /// Every thought, newest first. A file that will not parse is skipped rather than
    /// bringing down the listing.
    pub fn all(&self) -> std::io::Result<Vec<Thought>> {
        let mut out = Vec::new();
        let rd = match fs::read_dir(&self.dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.extension().map(|x| x == "json").unwrap_or(false) {
                if let Ok(Some(s)) = crate::paths::read_opt(&p) {
                    if let Ok(t) = serde_json::from_str::<Thought>(&s) {
                        out.push(t);
                    }
                }
            }
        }
        out.sort_by(|a, b| b.updated.partial_cmp(&a.updated).unwrap_or(std::cmp::Ordering::Equal));
        Ok(out)
    }

    pub fn live(&self) -> std::io::Result<Vec<Thought>> {
        Ok(self.all()?.into_iter().filter(|t| !t.status.is_final()).collect())
    }

    /// Thoughts actually being worked on. A paused one is set aside, not in flight, and must
    /// not occupy a slot the mind could use for something it wants to do now.
    pub fn running(&self) -> std::io::Result<Vec<Thought>> {
        Ok(self.all()?.into_iter().filter(|t| t.status == ThoughtStatus::Running).collect())
    }

    /// The next thought that should be given a process: running, unclaimed, and left alone
    /// long enough that a failing one cannot spin.
    pub fn ready(&self, now: f64) -> std::io::Result<Option<Thought>> {
        Ok(self.all()?.into_iter().find(|t| {
            t.status == ThoughtStatus::Running
                && t.pid.is_none()
                && t.steps < t.max_steps
                && now - t.updated >= STEP_GAP
        }))
    }

    pub fn save(&self, t: &Thought) -> std::io::Result<()> {
        let mut t = t.clone();
        t.updated = groow_proto::event::now();
        let s = serde_json::to_string_pretty(&t).unwrap_or_default();
        crate::paths::atomic_write(&self.path(&t.id), s.as_bytes())
    }

    /// Change only the fields the core owns, leaving the running process's history alone.
    pub fn patch(&self, id: &str, f: impl FnOnce(&mut Thought)) -> std::io::Result<Option<Thought>> {
        let mut t = match self.get(id)? {
            Some(t) => t,
            None => return Ok(None),
        };
        f(&mut t);
        self.save(&t)?;
        Ok(Some(t))
    }

    /// Start a new thought.
    pub fn spawn(&self, goal: &str, max_steps: u32, system: &str) -> std::io::Result<Thought> {
        let now = groow_proto::event::now();
        let t = Thought {
            id: super::short_id(),
            goal: goal.trim().to_string(),
            status: ThoughtStatus::Running,
            created: now,
            updated: now,
            steps: 0,
            max_steps: max_steps.clamp(1, MAX_STEPS_CAP),
            summary: String::new(),
            history: vec![Message::system(system)],
            tools_used: Vec::new(),
            interrupted: 0,
            pid: None,
            attempts: 0,
            extra: Default::default(),
        };
        self.save(&t)?;
        Ok(t)
    }

    /// Find thoughts whose process is gone and park them, so a crash leaves nothing that
    /// claims to be running but never moves.
    ///
    /// A thought with no process is not an orphan: it takes one step per process, so between
    /// steps it is simply waiting its turn. Only a thought that claims a process which is no
    /// longer there has actually lost anything.
    ///
    /// `alive` answers whether a process id is still running; it is a parameter so this can be
    /// tested without spawning anything.
    pub fn reap(&self, alive: impl Fn(u32) -> bool, now: f64) -> std::io::Result<Vec<String>> {
        let _ = now;
        let mut parked = Vec::new();
        for t in self.all()? {
            if t.status != ThoughtStatus::Running {
                continue;
            }
            let orphan = match t.pid {
                Some(p) => !alive(p),
                None => false,
            };
            if orphan {
                self.patch(&t.id, |t| {
                    t.status = ThoughtStatus::Paused;
                    t.pid = None;
                })?;
                parked.push(t.id);
            }
        }
        Ok(parked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(d: &tempfile::TempDir) -> Thoughts {
        let t = Thoughts::new(d.path().join("thoughts"));
        fs::create_dir_all(d.path().join("thoughts")).unwrap();
        t
    }

    #[test]
    fn a_new_thought_starts_running_with_its_goal_in_the_prompt() {
        let d = tempfile::tempdir().unwrap();
        let s = store(&d);
        let t = s.spawn("write about rivers", 8, "Your goal: write about rivers").unwrap();
        assert_eq!(t.status, ThoughtStatus::Running);
        assert_eq!(t.steps, 0);
        assert_eq!(t.history.len(), 1);
        assert_eq!(t.history[0].role, "system");
        assert_eq!(s.get(&t.id).unwrap().unwrap().goal, "write about rivers");
    }

    #[test]
    fn the_step_budget_is_capped_whatever_is_asked_for() {
        let d = tempfile::tempdir().unwrap();
        let s = store(&d);
        assert_eq!(s.spawn("g", 10_000, "p").unwrap().max_steps, MAX_STEPS_CAP);
        assert_eq!(s.spawn("g", 0, "p").unwrap().max_steps, 1);
        assert_eq!(s.spawn("g", 12, "p").unwrap().max_steps, 12);
    }

    #[test]
    fn patching_leaves_the_history_alone() {
        let d = tempfile::tempdir().unwrap();
        let s = store(&d);
        let t = s.spawn("g", 5, "p").unwrap();
        // The running process writes its own history.
        let mut owned = t.clone();
        owned.history.push(Message::assistant("thinking out loud"));
        owned.steps = 1;
        s.save(&owned).unwrap();
        // The core only touches the status.
        s.patch(&t.id, |t| t.status = ThoughtStatus::Paused).unwrap();
        let back = s.get(&t.id).unwrap().unwrap();
        assert_eq!(back.status, ThoughtStatus::Paused);
        assert_eq!(back.history.len(), 2, "the core must not roll back the history");
        assert_eq!(back.steps, 1);
    }

    #[test]
    fn a_thought_from_a_newer_version_keeps_its_unknown_fields() {
        let d = tempfile::tempdir().unwrap();
        let s = store(&d);
        let t = s.spawn("g", 5, "p").unwrap();
        let p = d.path().join("thoughts").join(format!("{}.json", t.id));
        let mut v: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        v["mood_from_the_future"] = serde_json::json!("curious");
        fs::write(&p, v.to_string()).unwrap();

        s.patch(&t.id, |t| t.status = ThoughtStatus::Done).unwrap();
        let back: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(back["mood_from_the_future"], "curious", "an older core must not destroy newer data");
        assert_eq!(back["status"], "done");
    }

    #[test]
    fn listing_puts_the_freshest_first_and_separates_the_live_ones() {
        let d = tempfile::tempdir().unwrap();
        let s = store(&d);
        let a = s.spawn("first", 5, "p").unwrap();
        let b = s.spawn("second", 5, "p").unwrap();
        s.patch(&a.id, |t| t.status = ThoughtStatus::Done).unwrap();
        let all = s.all().unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, a.id, "the most recently touched sorts first");
        let live = s.live().unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].id, b.id);
    }

    #[test]
    fn a_thought_is_left_alone_between_steps() {
        let d = tempfile::tempdir().unwrap();
        let s = store(&d);
        let t = s.spawn("go", 5, "p").unwrap();
        assert!(s.ready(t.updated).unwrap().is_none(), "it should not be started twice in a breath");
        assert!(s.ready(t.updated + STEP_GAP + 1.0).unwrap().is_some());
    }

    #[test]
    fn a_thought_that_has_run_out_of_steps_is_not_started_again() {
        let d = tempfile::tempdir().unwrap();
        let s = store(&d);
        let t = s.spawn("go", 2, "p").unwrap();
        s.patch(&t.id, |t| t.steps = 2).unwrap();
        let later = groow_proto::event::now() + 100.0;
        assert!(s.ready(later).unwrap().is_none());
    }

    #[test]
    fn a_thought_being_run_by_a_process_is_not_started_again() {
        let d = tempfile::tempdir().unwrap();
        let s = store(&d);
        let t = s.spawn("go", 5, "p").unwrap();
        s.patch(&t.id, |t| t.pid = Some(4242)).unwrap();
        let later = groow_proto::event::now() + 100.0;
        assert!(s.ready(later).unwrap().is_none());
    }

    #[test]
    fn a_thought_whose_process_died_is_parked() {
        let d = tempfile::tempdir().unwrap();
        let s = store(&d);
        let t = s.spawn("orphan", 5, "p").unwrap();
        s.patch(&t.id, |t| t.pid = Some(4242)).unwrap();
        let parked = s.reap(|_| false, groow_proto::event::now()).unwrap();
        assert_eq!(parked, vec![t.id.clone()]);
        let back = s.get(&t.id).unwrap().unwrap();
        assert_eq!(back.status, ThoughtStatus::Paused);
        assert_eq!(back.pid, None);
    }

    #[test]
    fn a_running_thought_with_a_live_process_is_left_alone() {
        let d = tempfile::tempdir().unwrap();
        let s = store(&d);
        let t = s.spawn("busy", 5, "p").unwrap();
        s.patch(&t.id, |t| t.pid = Some(4242)).unwrap();
        assert!(s.reap(|_| true, groow_proto::event::now()).unwrap().is_empty());
        assert_eq!(s.get(&t.id).unwrap().unwrap().status, ThoughtStatus::Running);
    }

    #[test]
    fn a_thought_waiting_for_its_next_step_is_not_treated_as_lost() {
        // It takes one step per process, so having no process is the normal state between
        // steps. Parking it for that would stop it after its first step, every time.
        let d = tempfile::tempdir().unwrap();
        let s = store(&d);
        let t = s.spawn("still going", 5, "p").unwrap();
        let much_later = t.created + 10_000.0;
        assert!(s.reap(|_| false, much_later).unwrap().is_empty());
        assert_eq!(s.get(&t.id).unwrap().unwrap().status, ThoughtStatus::Running);
    }

    #[test]
    fn a_paused_thought_does_not_occupy_a_slot() {
        let d = tempfile::tempdir().unwrap();
        let s = store(&d);
        let a = s.spawn("one", 5, "p").unwrap();
        s.spawn("two", 5, "p").unwrap();
        s.patch(&a.id, |t| t.status = ThoughtStatus::Paused).unwrap();
        assert_eq!(s.live().unwrap().len(), 2, "it still exists");
        assert_eq!(s.running().unwrap().len(), 1, "but it is not being worked on");
    }

    #[test]
    fn finished_thoughts_are_never_parked() {
        let d = tempfile::tempdir().unwrap();
        let s = store(&d);
        let t = s.spawn("done already", 5, "p").unwrap();
        s.patch(&t.id, |t| { t.status = ThoughtStatus::Done; t.pid = Some(4242); }).unwrap();
        assert!(s.reap(|_| false, groow_proto::event::now() + 10_000.0).unwrap().is_empty());
        assert!(ThoughtStatus::Done.is_final());
        assert!(!ThoughtStatus::Paused.is_final());
    }

    #[test]
    fn an_unreadable_file_does_not_break_the_listing() {
        let d = tempfile::tempdir().unwrap();
        let s = store(&d);
        s.spawn("good", 5, "p").unwrap();
        fs::write(d.path().join("thoughts/broken.json"), "{ nope").unwrap();
        assert_eq!(s.all().unwrap().len(), 1);
    }

    #[test]
    fn asking_for_a_thought_that_does_not_exist_is_not_an_error() {
        let d = tempfile::tempdir().unwrap();
        let s = store(&d);
        assert!(s.get("nosuch").unwrap().is_none());
        assert!(s.patch("nosuch", |t| t.steps = 9).unwrap().is_none());
    }
}
