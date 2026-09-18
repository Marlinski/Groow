//! Alarms.
//!
//! The mind has no clock of its own: it exists only when something wakes it. This is the file
//! that says when that should happen. It can set its own alarms, which is the difference
//! between a thing that answers and a thing that comes back to something later.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::{Duration, OffsetDateTime, Time};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alarm {
    pub id: String,
    pub text: String,
    /// When it next goes off, as seconds since the epoch.
    pub when: f64,
    #[serde(default)]
    pub repeat_s: Option<f64>,
    /// A wall-clock time of day, `HH:MM`, in local time.
    #[serde(default)]
    pub repeat_daily: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub created: f64,
    #[serde(default)]
    pub by: String,
    #[serde(default)]
    pub last_fired: Option<f64>,
    #[serde(default)]
    pub fires: u64,
}

fn yes() -> bool { true }

pub struct Schedule {
    path: PathBuf,
}

impl Schedule {
    pub fn new(path: impl Into<PathBuf>) -> Schedule {
        Schedule { path: path.into() }
    }

    /// Every alarm. A file that will not parse is an error rather than an empty schedule:
    /// quietly forgetting every alarm a person set is worse than refusing to continue.
    pub fn all(&self) -> anyhow::Result<Vec<Alarm>> {
        match crate::paths::read_opt(&self.path)? {
            None => Ok(Vec::new()),
            Some(s) if s.trim().is_empty() => Ok(Vec::new()),
            Some(s) => serde_json::from_str(&s)
                .map_err(|e| anyhow::anyhow!("{} is not a readable schedule: {e}", self.path.display())),
        }
    }

    fn save(&self, v: &[Alarm]) -> anyhow::Result<()> {
        let mut s = serde_json::to_string_pretty(v)?;
        s.push('\n');
        crate::paths::atomic_write(&self.path, s.as_bytes())?;
        Ok(())
    }

    /// Set an alarm. `when` is a one-off moment, `every` a repeat; at least one must parse.
    pub fn add(&self, text: &str, when: Option<&str>, every: Option<&str>, by: &str) -> anyhow::Result<Alarm> {
        let text = text.trim();
        if text.is_empty() {
            anyhow::bail!("an alarm needs something to say");
        }
        let now = groow_proto::event::now();
        let (repeat_s, repeat_daily) = match every {
            Some(e) => parse_repeat(e),
            None => (None, None),
        };
        let at = when
            .and_then(parse_when)
            .or_else(|| repeat_daily.as_deref().and_then(|d| next_daily(d, now)))
            .or_else(|| repeat_s.map(|s| now + s));
        let when = match at {
            Some(t) => t,
            None => anyhow::bail!("when? use --in 30m, --at 18:30, or --every 2h / 'daily 06:30'"),
        };
        let a = Alarm {
            id: super::inbox::short_id_pub(),
            text: text.to_string(),
            when,
            repeat_s,
            repeat_daily,
            enabled: true,
            created: now,
            by: by.to_string(),
            last_fired: None,
            fires: 0,
        };
        let mut all = self.all()?;
        all.push(a.clone());
        self.save(&all)?;
        Ok(a)
    }

    /// Everything due, with repeats rescheduled and one-offs removed, in one atomic update.
    pub fn due(&self, now: f64) -> anyhow::Result<Vec<Alarm>> {
        let mut all = self.all()?;
        let mut fired = Vec::new();
        let mut keep = Vec::with_capacity(all.len());
        for a in all.drain(..) {
            if !a.enabled || a.when > now {
                keep.push(a);
                continue;
            }
            let mut f = a.clone();
            f.last_fired = Some(now);
            f.fires += 1;
            fired.push(f.clone());
            if let Some(s) = a.repeat_s.filter(|s| *s > 0.0) {
                let mut next = f.clone();
                // Skip missed repeats rather than firing a backlog: an alarm every ten minutes
                // that was asleep for a day should go off once, not a hundred and forty times.
                let mut t = a.when + s;
                while t <= now {
                    t += s;
                }
                next.when = t;
                keep.push(next);
            } else if let Some(d) = a.repeat_daily.as_deref() {
                if let Some(t) = next_daily(d, now) {
                    let mut next = f.clone();
                    next.when = t;
                    keep.push(next);
                }
            }
        }
        if !fired.is_empty() {
            self.save(&keep)?;
        }
        Ok(fired)
    }

    /// Cancel by id or by a unique prefix of one.
    pub fn cancel(&self, id: &str) -> anyhow::Result<usize> {
        let all = self.all()?;
        let before = all.len();
        let kept: Vec<Alarm> = all.into_iter().filter(|a| !a.id.starts_with(id)).collect();
        let removed = before - kept.len();
        if removed > 0 {
            self.save(&kept)?;
        }
        Ok(removed)
    }
}

/// `30m`, `2h`, `1.5d`, optionally prefixed with `every`. Returns seconds.
pub fn parse_delay(spec: &str) -> Option<f64> {
    let s = spec.trim().to_lowercase();
    let s = s.strip_prefix("every").map(|r| r.trim()).unwrap_or(&s);
    let (num, unit) = s.split_at(s.find(|c: char| c.is_ascii_alphabetic())?);
    let n: f64 = num.trim().parse().ok()?;
    let mult = match unit.trim() {
        "s" => 1.0,
        "m" => 60.0,
        "h" => 3600.0,
        "d" => 86400.0,
        "w" => 604800.0,
        _ => return None,
    };
    // A zero delay is a real instruction ("now"), not a parse failure.
    if n < 0.0 { return None; }
    Some(n * mult)
}

/// `daily 06:30`, `every day at 6:30`. Returns a normalised `HH:MM`.
pub fn parse_daily(spec: &str) -> Option<String> {
    let s = spec.trim().to_lowercase();
    let rest = s.strip_prefix("daily").or_else(|| s.strip_prefix("every day"))?;
    let rest = rest.trim().strip_prefix("at").unwrap_or(rest).trim();
    parse_hhmm(rest)
}

/// `18:30` or `at 18:30`.
pub fn parse_hhmm(spec: &str) -> Option<String> {
    let s = spec.trim();
    let s = s.strip_prefix("at").map(|r| r.trim()).unwrap_or(s);
    let (h, m) = s.split_once(':')?;
    let h: u8 = h.trim().parse().ok()?;
    let m: u8 = m.trim().parse().ok()?;
    if h > 23 || m > 59 {
        return None;
    }
    Some(format!("{h:02}:{m:02}"))
}

/// A repeat is either an interval or a time of day, never both.
pub fn parse_repeat(spec: &str) -> (Option<f64>, Option<String>) {
    if let Some(d) = parse_daily(spec) {
        return (None, Some(d));
    }
    (parse_delay(spec), None)
}

/// When a one-off should fire: `in 30m`, `30m`, `at 18:30`, or an absolute timestamp.
pub fn parse_when(spec: &str) -> Option<f64> {
    let now = groow_proto::event::now();
    let s = spec.trim();
    if s.is_empty() {
        return None;
    }
    let lower = s.to_lowercase();
    if let Some(rest) = lower.strip_prefix("in ") {
        return parse_delay(rest).map(|d| now + d);
    }
    if let Some(d) = parse_delay(&lower) {
        return Some(now + d);
    }
    if let Some(hhmm) = parse_hhmm(s) {
        return next_daily(&hhmm, now);
    }
    // An absolute seconds-since-epoch value, for machines rather than people.
    s.parse::<f64>().ok().filter(|t| *t > 1_000_000_000.0)
}

/// The next occurrence of a local wall-clock time strictly after `after`.
pub fn next_daily(hhmm: &str, after: f64) -> Option<f64> {
    let (h, m) = hhmm.split_once(':')?;
    let (h, m): (u8, u8) = (h.parse().ok()?, m.parse().ok()?);
    let t = Time::from_hms(h, m, 0).ok()?;
    let base = OffsetDateTime::from_unix_timestamp(after as i64).ok()?;
    let local = OffsetDateTime::now_local().map(|n| n.offset()).unwrap_or(time::UtcOffset::UTC);
    let base = base.to_offset(local);
    let mut cand = base.replace_time(t);
    if cand <= base {
        cand += Duration::days(1);
    }
    Some(cand.unix_timestamp() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sched(d: &tempfile::TempDir) -> Schedule {
        Schedule::new(d.path().join("schedule.json"))
    }

    #[test]
    fn delays_parse_in_every_unit() {
        assert_eq!(parse_delay("30s"), Some(30.0));
        assert_eq!(parse_delay("30m"), Some(1800.0));
        assert_eq!(parse_delay("2h"), Some(7200.0));
        assert_eq!(parse_delay("1d"), Some(86400.0));
        assert_eq!(parse_delay("1w"), Some(604800.0));
        assert_eq!(parse_delay("1.5h"), Some(5400.0));
        assert_eq!(parse_delay("every 2h"), Some(7200.0));
        assert_eq!(parse_delay(" 45 M "), Some(2700.0));
    }

    #[test]
    fn nonsense_delays_are_refused() {
        for bad in ["", "soon", "2x", "h", "-5m", "2 hours and a bit"] {
            assert_eq!(parse_delay(bad), None, "{bad:?} should not parse");
        }
    }

    #[test]
    fn a_zero_delay_means_now_rather_than_nothing() {
        assert_eq!(parse_delay("0s"), Some(0.0), "'now' is a real instruction");
        assert!(parse_when("in 0s").is_some());
    }

    #[test]
    fn daily_specs_normalise_to_a_padded_time() {
        assert_eq!(parse_daily("daily 06:30").as_deref(), Some("06:30"));
        assert_eq!(parse_daily("daily 6:30").as_deref(), Some("06:30"));
        assert_eq!(parse_daily("every day at 18:05").as_deref(), Some("18:05"));
        assert_eq!(parse_daily("18:05"), None, "a bare time is not a daily repeat");
        assert_eq!(parse_daily("daily 25:00"), None);
    }

    #[test]
    fn a_repeat_is_an_interval_or_a_time_but_never_both() {
        assert_eq!(parse_repeat("2h"), (Some(7200.0), None));
        assert_eq!(parse_repeat("daily 06:30"), (None, Some("06:30".into())));
        assert_eq!(parse_repeat("whenever"), (None, None));
    }

    #[test]
    fn a_daily_time_always_lands_in_the_future() {
        let now = groow_proto::event::now();
        for t in ["00:00", "06:30", "12:00", "23:59"] {
            let next = next_daily(t, now).unwrap();
            assert!(next > now, "{t} landed in the past");
            assert!(next - now <= 86400.0 + 1.0, "{t} landed more than a day out");
        }
    }

    #[test]
    fn an_alarm_fires_once_and_disappears() {
        let d = tempfile::tempdir().unwrap();
        let s = sched(&d);
        let a = s.add("look at the kettle", Some("in 0s"), None, "groow").unwrap();
        let fired = s.due(a.when + 1.0).unwrap();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].fires, 1);
        assert!(s.all().unwrap().is_empty(), "a one-off should not linger");
    }

    #[test]
    fn a_repeating_alarm_survives_and_moves_forward() {
        let d = tempfile::tempdir().unwrap();
        let s = sched(&d);
        let a = s.add("read the news", None, Some("2h"), "groow").unwrap();
        let fired = s.due(a.when + 1.0).unwrap();
        assert_eq!(fired.len(), 1);
        let left = s.all().unwrap();
        assert_eq!(left.len(), 1);
        assert!(left[0].when > a.when, "the repeat did not move forward");
    }

    #[test]
    fn a_long_sleep_does_not_produce_a_backlog_of_alarms() {
        let d = tempfile::tempdir().unwrap();
        let s = sched(&d);
        let a = s.add("every ten minutes", None, Some("10m"), "groow").unwrap();
        // A whole day passes with the core down.
        let fired = s.due(a.when + 86400.0).unwrap();
        assert_eq!(fired.len(), 1, "a missed day must fire once, not a hundred times");
        let next = s.all().unwrap()[0].when;
        assert!(next > a.when + 86400.0, "the next one should be in the future");
    }

    #[test]
    fn nothing_is_due_before_its_time() {
        let d = tempfile::tempdir().unwrap();
        let s = sched(&d);
        let a = s.add("later", Some("in 1h"), None, "groow").unwrap();
        assert!(s.due(a.when - 60.0).unwrap().is_empty());
        assert_eq!(s.all().unwrap().len(), 1);
    }

    #[test]
    fn an_alarm_needs_something_to_say_and_a_time() {
        let d = tempfile::tempdir().unwrap();
        let s = sched(&d);
        assert!(s.add("   ", Some("in 1h"), None, "groow").is_err());
        let e = s.add("something", None, None, "groow").unwrap_err().to_string();
        assert!(e.contains("when?"), "unhelpful error: {e}");
    }

    #[test]
    fn cancelling_accepts_a_prefix() {
        let d = tempfile::tempdir().unwrap();
        let s = sched(&d);
        let a = s.add("one", Some("in 1h"), None, "groow").unwrap();
        s.add("two", Some("in 2h"), None, "groow").unwrap();
        assert_eq!(s.cancel(&a.id[..3]).unwrap(), 1);
        assert_eq!(s.all().unwrap().len(), 1);
        assert_eq!(s.cancel("zzzzzz").unwrap(), 0);
    }

    #[test]
    fn a_disabled_alarm_never_fires() {
        let d = tempfile::tempdir().unwrap();
        let s = sched(&d);
        let a = s.add("off", Some("in 0s"), None, "groow").unwrap();
        let mut all = s.all().unwrap();
        all[0].enabled = false;
        s.save(&all).unwrap();
        assert!(s.due(a.when + 100.0).unwrap().is_empty());
        assert_eq!(s.all().unwrap().len(), 1, "it stays, it just does not fire");
    }

    #[test]
    fn a_corrupt_schedule_is_reported_rather_than_silently_emptied() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("schedule.json"), "[{\"id\": broken}]").unwrap();
        assert!(sched(&d).all().is_err(), "losing every alarm silently is the worst outcome");
    }
}
