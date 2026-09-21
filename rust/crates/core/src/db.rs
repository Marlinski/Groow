//! The statistics database.
//!
//! Everything measurable about a life: how long turns take, which tools fail, what each turn
//! felt like, how long the learning passes take. It lives beside the state but is readable only
//! by the core, so the mind cannot see or edit how it is being scored.
//!
//! Files remain the source of truth for the conversation. Nothing here is irreplaceable: if
//! the database were deleted it could be rebuilt by replaying the journal, which is why it is
//! safe to keep it fast and simple. There is exactly one writer, so there is no contention and
//! no locking discipline to get wrong.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

pub struct Db {
    conn: Connection,
}

/// How a turn finished. Named rather than passed as six values in a row, because six scalars
/// of the same shape are easy to put in the wrong order and nothing would ever complain.
#[derive(Debug, Clone, Default)]
pub struct Ended {
    pub at: f64,
    pub seconds: f64,
    pub tools: u32,
    pub rounds: u32,
    pub flags: Vec<String>,
    pub outcome: String,
    /// Everything generated during the turn, and how much of the wall clock was spent waiting
    /// for the brain rather than running commands. The difference between them is the answer
    /// to "why was that slow".
    pub tokens: u32,
    pub thinking: f64,
    /// Which creature answered: the weights as they were, not as they are now.
    pub version: String,
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Db> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let conn = Connection::open(path)?;
        Self::prepare(&conn)?;
        keep_private(path);
        Ok(Db { conn })
    }

    /// An in-memory database, for tests and for a dry run.
    pub fn memory() -> anyhow::Result<Db> {
        let conn = Connection::open_in_memory()?;
        Self::prepare(&conn)?;
        Ok(Db { conn })
    }

    /// The path of the database, so its permissions can be reasserted after a checkpoint.
    pub fn keep_private(path: &Path) {
        keep_private(path)
    }

    fn prepare(conn: &Connection) -> anyhow::Result<()> {
        // Write-ahead logging so a reader never blocks the single writer, and a normal sync:
        // losing the last few statistics in a power cut costs nothing.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS turns (
                id        TEXT PRIMARY KEY,
                kind      TEXT NOT NULL,
                started   REAL NOT NULL,
                ended     REAL,
                seconds   REAL,
                tools     INTEGER NOT NULL DEFAULT 0,
                rounds    INTEGER NOT NULL DEFAULT 0,
                flags     TEXT NOT NULL DEFAULT '',
                outcome   TEXT,
                tokens    INTEGER,
                thinking  REAL,
                version   TEXT
            );
            CREATE INDEX IF NOT EXISTS turns_started ON turns(started);

            CREATE TABLE IF NOT EXISTS tool_calls (
                id      INTEGER PRIMARY KEY AUTOINCREMENT,
                turn    TEXT,
                ts      REAL NOT NULL,
                name    TEXT NOT NULL,
                actor   TEXT NOT NULL DEFAULT 'main',
                ok      INTEGER NOT NULL,
                seconds REAL
            );
            CREATE INDEX IF NOT EXISTS tool_calls_name ON tool_calls(name);
            CREATE INDEX IF NOT EXISTS tool_calls_turn ON tool_calls(turn);

            CREATE TABLE IF NOT EXISTS feelings (
                id       INTEGER PRIMARY KEY AUTOINCREMENT,
                turn     TEXT,
                ts       REAL NOT NULL,
                sensors  REAL NOT NULL,
                approval REAL,
                valence  REAL NOT NULL
            );
            CREATE INDEX IF NOT EXISTS feelings_ts ON feelings(ts);

            CREATE TABLE IF NOT EXISTS learning (
                id      INTEGER PRIMARY KEY AUTOINCREMENT,
                ts      REAL NOT NULL,
                kind    TEXT NOT NULL,
                samples INTEGER NOT NULL DEFAULT 0,
                loss    REAL,
                seconds REAL,
                note    TEXT,
                version TEXT
            );

            CREATE TABLE IF NOT EXISTS counters (
                key   TEXT PRIMARY KEY,
                value REAL NOT NULL
            );
            "#,
        )?;
        // A creature older than this column keeps its history and gains the column empty.
        // Sqlite has no ADD COLUMN IF NOT EXISTS, and the error for one that is already there
        // is the expected outcome rather than a problem.
        let _ = conn.execute("ALTER TABLE learning ADD COLUMN seconds REAL", []);
        let _ = conn.execute("ALTER TABLE turns ADD COLUMN tokens INTEGER", []);
        let _ = conn.execute("ALTER TABLE turns ADD COLUMN thinking REAL", []);
        let _ = conn.execute("ALTER TABLE turns ADD COLUMN version TEXT", []);
        let _ = conn.execute("ALTER TABLE learning ADD COLUMN version TEXT", []);
        Ok(())
    }

    // ---------------------------------------------------------------- turns
    pub fn turn_started(&self, id: &str, kind: &str, started: f64) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO turns (id, kind, started) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO NOTHING",
            params![id, kind, started],
        )?;
        Ok(())
    }

    /// Close a turn out. Writing the same outcome twice leaves one row, because a retried
    /// report must not double-count a turn in the statistics.
    pub fn turn_ended(&self, id: &str, e: &Ended) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE turns SET ended=?2, seconds=?3, tools=?4, rounds=?5, flags=?6, outcome=?7, \
             tokens=?8, thinking=?9, version=?10 WHERE id=?1",
            params![id, e.at, e.seconds, e.tools, e.rounds, e.flags.join(","), e.outcome,
                    e.tokens, e.thinking, e.version],
        )?;
        Ok(())
    }

    pub fn turn_count(&self) -> anyhow::Result<i64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM turns", [], |r| r.get(0))?)
    }

    /// Turns not worth showing the mind again.
    ///
    /// A window is not just a record, it is the pattern the model reads before it answers, and
    /// it copies what it sees there. Replaying a turn that went round in circles, ran out of
    /// room, or that a person reacted badly to is an instruction to do it again. This is the
    /// list of turns to leave out.
    pub fn turns_to_forget(&self) -> anyhow::Result<std::collections::HashSet<String>> {
        let mut st = self.conn.prepare(
            "SELECT t.id FROM turns t LEFT JOIN feelings f ON f.turn = t.id
             WHERE t.outcome IS NOT 'ok'
                OR t.flags LIKE '%repeat%'
                OR t.flags LIKE '%exhausted%'
                OR f.valence < -0.5",
        )?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    // ---------------------------------------------------------------- tools
    pub fn tool_call(&self, turn: &str, ts: f64, name: &str, actor: &str, ok: bool, seconds: f64) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO tool_calls (turn, ts, name, actor, ok, seconds) VALUES (?1,?2,?3,?4,?5,?6)",
            params![turn, ts, name, actor, ok as i32, seconds],
        )?;
        Ok(())
    }

    /// How often each tool is used and how often it fails, worst first, and when each was last
    /// reached for. This is the table that says which part of the toolset the mind has not
    /// learned to drive.
    ///
    /// `since` keeps it about the present. Counting since birth means a tool that was removed
    /// months ago still sits at the top of the list with its old failures, which reads as a
    /// problem with something that no longer exists.
    pub fn tool_health(&self, since: f64) -> anyhow::Result<Vec<(String, i64, f64, f64)>> {
        let mut st = self.conn.prepare(
            "SELECT name, COUNT(*) AS n, AVG(CASE WHEN ok THEN 0.0 ELSE 1.0 END) AS fail, MAX(ts)
             FROM tool_calls WHERE ts >= ?1 GROUP BY name ORDER BY fail DESC, n DESC",
        )?;
        let rows = st.query_map([since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    // ---------------------------------------------------------------- feeling
    pub fn felt(&self, turn: &str, ts: f64, sensors: f64, approval: Option<f64>, valence: f64) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO feelings (turn, ts, sensors, approval, valence) VALUES (?1,?2,?3,?4,?5)",
            params![turn, ts, sensors, approval, valence],
        )?;
        Ok(())
    }

    /// Accumulated pain and pleasure, decayed toward the present. Old hurt fades; it does not
    /// sit in the totals forever.
    pub fn mood(&self, now: f64, halflife: f64) -> anyhow::Result<(f64, f64)> {
        let mut st = self.conn.prepare("SELECT ts, valence FROM feelings WHERE ts > ?1")?;
        let cutoff = now - halflife * 10.0;
        let rows = st.query_map(params![cutoff], |r| Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?)))?;
        let (mut pain, mut pleasure) = (0.0, 0.0);
        for (ts, v) in rows.filter_map(|r| r.ok()) {
            let k = 0.5_f64.powf((now - ts).max(0.0) / halflife);
            if v < 0.0 { pain += -v * k } else { pleasure += v * k }
        }
        Ok((pain, pleasure))
    }

    // ---------------------------------------------------------------- learning
    pub fn learned(&self, ts: f64, kind: &str, samples: u32, loss: Option<f64>, note: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO learning (ts, kind, samples, loss, note) VALUES (?1,?2,?3,?4,?5)",
            params![ts, kind, samples, loss, note],
        )?;
        Ok(())
    }

    // ---------------------------------------------------------------- counters
    /// The learning passes, most recent first. What ran, how much it practised, what it cost.
    pub fn recent_learning(&self, n: usize) -> anyhow::Result<Vec<Value>> {
        let mut st = self.conn.prepare(
            "SELECT ts, kind, samples, loss, seconds, note, version \
             FROM learning ORDER BY ts DESC LIMIT ?1",
        )?;
        let rows = st.query_map([n as i64], |r| {
            Ok(json!({
                "ts": r.get::<_, f64>(0)?,
                "kind": r.get::<_, String>(1)?,
                "samples": r.get::<_, i64>(2)?,
                "loss": r.get::<_, Option<f64>>(3)?,
                "seconds": r.get::<_, Option<f64>>(4)?,
                "note": r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                "version": r.get::<_, Option<String>>(6)?.unwrap_or_default(),
            }))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// The turns, most recent first, with how long each took and how it ended.
    pub fn recent_turns(&self, n: usize) -> anyhow::Result<Vec<Value>> {
        let mut st = self.conn.prepare(
            "SELECT id, kind, started, seconds, tools, rounds, flags, outcome, tokens, thinking, \
             version FROM turns ORDER BY started DESC LIMIT ?1",
        )?;
        let rows = st.query_map([n as i64], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "kind": r.get::<_, String>(1)?,
                "started": r.get::<_, f64>(2)?,
                "seconds": r.get::<_, Option<f64>>(3)?,
                "tools": r.get::<_, i64>(4)?,
                "rounds": r.get::<_, i64>(5)?,
                "flags": r.get::<_, String>(6)?,
                "outcome": r.get::<_, Option<String>>(7)?.unwrap_or_default(),
                "tokens": r.get::<_, Option<i64>>(8)?,
                "thinking": r.get::<_, Option<f64>>(9)?,
                "version": r.get::<_, Option<String>>(10)?.unwrap_or_default(),
            }))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// How turns felt, oldest first, which is the order a line is drawn in.
    pub fn recent_feelings(&self, n: usize) -> anyhow::Result<Vec<Value>> {
        let mut st = self.conn.prepare(
            "SELECT ts, sensors, approval, valence FROM feelings ORDER BY ts DESC LIMIT ?1",
        )?;
        let rows = st.query_map([n as i64], |r| {
            Ok(json!({
                "ts": r.get::<_, f64>(0)?,
                "sensors": r.get::<_, f64>(1)?,
                "approval": r.get::<_, Option<f64>>(2)?,
                "valence": r.get::<_, f64>(3)?,
            }))
        })?;
        let mut v: Vec<Value> = rows.filter_map(|r| r.ok()).collect();
        v.reverse();
        Ok(v)
    }

    /// Everything counted since it was born.
    pub fn counters(&self) -> anyhow::Result<Value> {
        let mut st = self.conn.prepare("SELECT key, value FROM counters ORDER BY key")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))?;
        let mut out = serde_json::Map::new();
        for (k, v) in rows.flatten() {
            out.insert(k, json!(v));
        }
        Ok(Value::Object(out))
    }

    pub fn bump(&self, key: &str, by: f64) -> anyhow::Result<f64> {
        self.conn.execute(
            "INSERT INTO counters (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = value + ?2",
            params![key, by],
        )?;
        self.counter(key).map(|v| v.unwrap_or(0.0))
    }

    pub fn set_counter(&self, key: &str, v: f64) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO counters (key, value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=?2",
            params![key, v],
        )?;
        Ok(())
    }

    pub fn counter(&self, key: &str) -> anyhow::Result<Option<f64>> {
        Ok(self.conn
            .query_row("SELECT value FROM counters WHERE key=?1", params![key], |r| r.get(0))
            .optional()?)
    }
}

/// Make the database unreadable to anyone but its owner.
///
/// Write-ahead logging means there are three files, not one, and the two extra ones are
/// created by SQLite with the default permissions. Locking only the main file would leave
/// everything recent in a file the mind can read, which is precisely the part that matters.
fn keep_private(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let p = if suffix.is_empty() {
                path.to_path_buf()
            } else {
                let mut name = path.as_os_str().to_os_string();
                name.push(suffix);
                std::path::PathBuf::from(name)
            };
            if p.exists() {
                let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
            }
        }
    }
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_turn_is_recorded_from_start_to_finish() {
        let db = Db::memory().unwrap();
        db.turn_started("t1", "user", 100.0).unwrap();
        db.turn_ended("t1", &Ended { at: 104.5, seconds: 4.5, tools: 3, rounds: 2,
                                     flags: vec!["completed".into()], outcome: "ok".into() , ..Default::default()}).unwrap();
        assert_eq!(db.turn_count().unwrap(), 1);
    }

    #[test]
    fn reporting_the_same_turn_twice_does_not_double_count_it() {
        let db = Db::memory().unwrap();
        db.turn_started("t1", "user", 100.0).unwrap();
        db.turn_started("t1", "user", 100.0).unwrap();
        let done = Ended { at: 104.0, seconds: 4.0, tools: 1, rounds: 1, flags: vec![], outcome: "ok".into() , ..Default::default()};
        db.turn_ended("t1", &done).unwrap();
        db.turn_ended("t1", &done).unwrap();
        assert_eq!(db.turn_count().unwrap(), 1, "a retried report must not invent a second turn");
    }

    #[test]
    fn turns_that_went_badly_are_not_shown_again() {
        let db = Db::memory().unwrap();
        for (id, flags, outcome) in [
            ("good", "completed", "ok"),
            ("looping", "completed,repeat,repeat", "ok"),
            ("ran_out", "exhausted", "ok"),
            ("died", "abandoned", "its process went away"),
        ] {
            db.turn_started(id, "user", 1.0).unwrap();
            db.turn_ended(id, &Ended { at: 2.0, seconds: 1.0, tools: 0, rounds: 1,
                                       flags: vec![flags.to_string()], outcome: outcome.into() , ..Default::default()}).unwrap();
        }
        let forget = db.turns_to_forget().unwrap();
        assert!(!forget.contains("good"), "a turn that worked should stay in the window");
        for bad in ["looping", "ran_out", "died"] {
            assert!(forget.contains(bad), "{bad} should not be replayed");
        }
    }

    #[test]
    fn a_turn_a_person_reacted_badly_to_is_not_shown_again() {
        let db = Db::memory().unwrap();
        db.turn_started("sour", "user", 1.0).unwrap();
        db.turn_ended("sour", &Ended { at: 2.0, seconds: 1.0, tools: 0, rounds: 1,
                                       flags: vec!["completed".into()], outcome: "ok".into() , ..Default::default()}).unwrap();
        assert!(!db.turns_to_forget().unwrap().contains("sour"), "nothing is wrong with it yet");
        db.felt("sour", 3.0, 0.15, Some(-0.9), -0.75).unwrap();
        assert!(db.turns_to_forget().unwrap().contains("sour"));
    }

    #[test]
    fn a_turn_nobody_has_reacted_to_is_still_shown() {
        let db = Db::memory().unwrap();
        db.turn_started("fresh", "user", 1.0).unwrap();
        db.turn_ended("fresh", &Ended { at: 2.0, seconds: 1.0, tools: 0, rounds: 1,
                                        flags: vec!["completed".into()], outcome: "ok".into() , ..Default::default()}).unwrap();
        db.felt("fresh", 3.0, 0.15, None, 0.15).unwrap();
        assert!(!db.turns_to_forget().unwrap().contains("fresh"));
    }

    #[test]
    fn tool_health_puts_the_least_reliable_first() {
        let db = Db::memory().unwrap();
        for _ in 0..8 {
            db.tool_call("t1", 1.0, "shell", "main", true, 0.1).unwrap();
        }
        db.tool_call("t1", 1.0, "shell", "main", false, 0.1).unwrap();
        for _ in 0..3 {
            db.tool_call("t1", 1.0, "think", "main", false, 0.1).unwrap();
        }
        let h = db.tool_health(0.0).unwrap();
        assert_eq!(h[0].0, "think");
        assert!((h[0].2 - 1.0).abs() < 1e-9, "think fails every time");
        assert_eq!(h[1].0, "shell");
        assert!(h[1].2 < 0.2);
    }

    #[test]
    fn a_tool_nobody_has_touched_lately_falls_out_of_the_table() {
        // Counting since birth means a tool removed months ago still sits at the top with its
        // old failures, which reads as a problem with something that does not exist.
        let db = Db::memory().unwrap();
        db.tool_call("t1", 100.0, "focus", "main", false, 0.1).unwrap();
        db.tool_call("t2", 900.0, "shell", "main", true, 0.1).unwrap();

        let recent: Vec<String> = db.tool_health(500.0).unwrap().into_iter().map(|t| t.0).collect();
        assert_eq!(recent, vec!["shell".to_string()], "only what has been used lately");

        let (_, _, _, last) = db.tool_health(0.0).unwrap().into_iter().find(|t| t.0 == "focus").unwrap();
        assert_eq!(last, 100.0, "and when it was last reached for is remembered");
    }

    #[test]
    fn old_feelings_fade_out_of_the_mood() {
        let db = Db::memory().unwrap();
        let now = 1_000_000.0;
        db.felt("t1", now - 3600.0, -1.0, None, -1.0).unwrap();
        db.felt("t2", now, 1.0, None, 1.0).unwrap();
        let (pain, pleasure) = db.mood(now, 1800.0).unwrap();
        assert!((pleasure - 1.0).abs() < 1e-6, "the fresh one counts fully");
        assert!((pain - 0.25).abs() < 1e-6, "two half-lives on, a quarter is left");
    }

    #[test]
    fn a_mood_with_no_history_is_flat() {
        let db = Db::memory().unwrap();
        assert_eq!(db.mood(1000.0, 1800.0).unwrap(), (0.0, 0.0));
    }

    #[test]
    fn counters_accumulate_and_can_be_set() {
        let db = Db::memory().unwrap();
        assert_eq!(db.counter("steps").unwrap(), None);
        assert_eq!(db.bump("steps", 1.0).unwrap(), 1.0);
        assert_eq!(db.bump("steps", 2.0).unwrap(), 3.0);
        db.set_counter("steps", 10.0).unwrap();
        assert_eq!(db.counter("steps").unwrap(), Some(10.0));
    }

    #[test]
    fn it_survives_being_closed_and_reopened() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("groow.db");
        {
            let db = Db::open(&p).unwrap();
            db.bump("nights", 3.0).unwrap();
        }
        let db = Db::open(&p).unwrap();
        assert_eq!(db.counter("nights").unwrap(), Some(3.0));
    }

    #[test]
    fn every_database_file_is_private_to_the_core() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("groow.db");
        let db = Db::open(&p).unwrap();
        // Force the write-ahead files into existence, which is where the recent scores live.
        db.bump("turns", 1.0).unwrap();
        db.felt("t1", 1.0, 0.0, None, -0.5).unwrap();
        Db::keep_private(&p);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for suffix in ["", "-wal", "-shm"] {
                let f = std::path::PathBuf::from(format!("{}{suffix}", p.display()));
                if !f.exists() {
                    continue;
                }
                let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o077;
                assert_eq!(mode, 0, "{} can be read by the mind", f.display());
            }
        }
    }
}
