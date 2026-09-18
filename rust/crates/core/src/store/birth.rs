//! The birth certificate.
//!
//! Written once, made read-only, and never touched again. There is no operation anywhere in
//! the core that rewrites it, and the mind has no tool that could. It is the one thing about
//! itself it cannot talk itself out of.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Birth {
    pub id: String,
    pub name: String,
    pub born: f64,
    pub lineage: String,
    pub hardware: String,
    pub mentor: String,
    pub version: String,
}

impl Birth {
    /// Read the certificate, or write one if this is the first breath.
    pub fn load_or_create(
        path: &Path,
        lineage: &str,
        hardware: &str,
        mentor: &str,
        version: &str,
    ) -> anyhow::Result<Birth> {
        if let Some(s) = crate::paths::read_opt(path)? {
            return serde_json::from_str(&s)
                .map_err(|e| anyhow::anyhow!("the birth certificate is unreadable: {e}"));
        }
        let b = Birth {
            id: long_id(),
            name: "Groow".into(),
            born: groow_proto::event::now(),
            lineage: lineage.to_string(),
            hardware: hardware.to_string(),
            mentor: mentor.to_string(),
            version: version.to_string(),
        };
        let mut s = serde_json::to_string_pretty(&b)?;
        s.push('\n');
        crate::paths::atomic_write(path, s.as_bytes())?;
        // Best effort: on a filesystem that will not hold permissions this is not fatal,
        // because the real protection is that no code path here ever writes it again.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444));
        }
        Ok(b)
    }

    pub fn age(&self, now: f64) -> f64 {
        (now - self.born).max(0.0)
    }

    /// Coarse age, for a status line.
    pub fn age_text(&self, now: f64) -> String {
        let s = self.age(now) as u64;
        let (d, h, m) = (s / 86400, (s % 86400) / 3600, (s % 3600) / 60);
        if d > 0 { format!("{d}d {h}h") } else if h > 0 { format!("{h}h {m}m") } else { format!("{m}m") }
    }

    /// Precise age, for the card that ticks every second.
    pub fn age_clock(&self, now: f64) -> String {
        let s = self.age(now) as u64;
        format!("{}d {:02}:{:02}:{:02}", s / 86400, (s % 86400) / 3600, (s % 3600) / 60, s % 60)
    }

    pub fn born_text(&self) -> String {
        match time::OffsetDateTime::from_unix_timestamp(self.born as i64) {
            Ok(t) => format!(
                "{:04}-{:02}-{:02} {:02}:{:02} UTC",
                t.year(), t.month() as u8, t.day(), t.hour(), t.minute()
            ),
            Err(_) => "unknown".into(),
        }
    }

    /// The line that goes into the prompt as the facts it cannot change.
    pub fn line(&self, now: f64) -> String {
        format!(
            "{} \u{b7} id {} \u{b7} born {} \u{b7} age {} \u{b7} lineage {}",
            self.name, self.id, self.born_text(), self.age_text(now), self.lineage
        )
    }
}

fn long_id() -> String {
    use rand::Rng;
    let mut r = rand::rng();
    (0..8).map(|_| std::char::from_digit(r.random_range(0..16u32), 16).unwrap_or('0')).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(d: &tempfile::TempDir) -> (Birth, std::path::PathBuf) {
        let p = d.path().join("birth.json");
        let b = Birth::load_or_create(&p, "Qwen/Qwen3-4B", "V100 32 GB", "Marlinski", "0.3.0").unwrap();
        (b, p)
    }

    #[test]
    fn it_is_written_once_and_never_again() {
        let d = tempfile::tempdir().unwrap();
        let (first, p) = make(&d);
        let second = Birth::load_or_create(&p, "something/else", "other hardware", "someone", "9.9").unwrap();
        assert_eq!(first, second, "a second birth must not overwrite the first");
        assert_eq!(second.lineage, "Qwen/Qwen3-4B");
    }

    #[test]
    fn the_file_is_read_only_on_disk() {
        let d = tempfile::tempdir().unwrap();
        let (_, p) = make(&d);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o222;
            assert_eq!(mode, 0, "nobody should be able to write the certificate");
        }
    }

    #[test]
    fn ages_read_the_way_a_person_would_say_them() {
        let d = tempfile::tempdir().unwrap();
        let (b, _) = make(&d);
        let t = b.born;
        assert_eq!(b.age_text(t + 90.0), "1m");
        assert_eq!(b.age_text(t + 3700.0), "1h 1m");
        assert_eq!(b.age_text(t + 90000.0), "1d 1h");
        assert_eq!(b.age_clock(t + 90000.0), "1d 01:00:00");
        assert_eq!(b.age_clock(t), "0d 00:00:00");
    }

    #[test]
    fn a_clock_that_went_backwards_does_not_give_a_negative_age() {
        let d = tempfile::tempdir().unwrap();
        let (b, _) = make(&d);
        assert_eq!(b.age(b.born - 5000.0), 0.0);
        assert_eq!(b.age_clock(b.born - 5000.0), "0d 00:00:00");
    }

    #[test]
    fn the_prompt_line_carries_the_facts_it_cannot_change() {
        let d = tempfile::tempdir().unwrap();
        let (b, _) = make(&d);
        let line = b.line(b.born);
        for part in ["Groow", &b.id, "Qwen/Qwen3-4B"] {
            assert!(line.contains(part), "{part} missing from {line}");
        }
    }

    #[test]
    fn a_corrupt_certificate_is_reported_rather_than_replaced() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("birth.json");
        std::fs::write(&p, "{ not json").unwrap();
        assert!(Birth::load_or_create(&p, "m", "h", "me", "1").is_err(), "a new identity must never be minted over a damaged one");
    }
}
