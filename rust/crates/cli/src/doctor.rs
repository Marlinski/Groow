//! Checking the state on disk, without needing the core to be running.
//!
//! This is what to reach for when something is wrong and nothing is answering.

use std::path::Path;

use serde_json::{json, Value};

pub fn doctor(state: &Path, config: &Path) -> anyhow::Result<()> {
    let report = examine(state, config);
    println!("{}", serde_json::to_string_pretty(&report)?);
    let problems = report["problems"].as_array().map(|a| a.len()).unwrap_or(0);
    if problems == 0 {
        eprintln!("nothing wrong that can be seen from here");
        Ok(())
    } else {
        eprintln!("{problems} problem(s) found");
        anyhow::bail!("the state is not healthy")
    }
}

/// Look at everything and say what is wrong, without changing anything.
pub fn examine(state: &Path, config: &Path) -> Value {
    let mut problems: Vec<String> = Vec::new();
    let mut notes = json!({});

    if !state.exists() {
        problems.push(format!("there is no state directory at {}", state.display()));
        return json!({"state": state.display().to_string(), "problems": problems});
    }

    // The certificate is the one file that must be there and must be readable.
    let birth = state.join("birth.json");
    match std::fs::read_to_string(&birth) {
        Ok(s) => match serde_json::from_str::<Value>(&s) {
            Ok(v) => {
                notes["name"] = v.get("name").cloned().unwrap_or(Value::Null);
                notes["born"] = v.get("born").cloned().unwrap_or(Value::Null);
            }
            Err(e) => problems.push(format!("the birth certificate is not readable JSON: {e}")),
        },
        Err(_) => problems.push("there is no birth certificate; it has not been born yet".into()),
    }

    if config.exists() {
        if let Ok(s) = std::fs::read_to_string(config) {
            if serde_json::from_str::<Value>(&s).is_err() {
                problems.push(format!("{} is not valid JSON, so it would refuse to start", config.display()));
            }
        }
    }

    // How much conversation there is, and whether the newest file can be read at all.
    let main = state.join("main");
    let mut files: Vec<_> = std::fs::read_dir(&main)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    files.sort();
    notes["journal_files"] = json!(files.len());
    if let Some(last) = files.last() {
        match std::fs::read_to_string(last) {
            Ok(text) => {
                let lines = text.lines().filter(|l| !l.trim().is_empty()).count();
                let bad = text
                    .lines()
                    .filter(|l| !l.trim().is_empty() && serde_json::from_str::<Value>(l).is_err())
                    .count();
                notes["latest_journal_lines"] = json!(lines);
                if bad > 0 {
                    problems.push(format!("{bad} unreadable line(s) in {}", last.display()));
                }
            }
            Err(e) => problems.push(format!("cannot read {}: {e}", last.display())),
        }
    }

    // Anything stuck in the mailbox is work that never got done.
    let waiting = count_files(&state.join("mailbox/new"), "json");
    let held = count_files(&state.join("mailbox/cur"), "json");
    notes["waiting"] = json!(waiting);
    if held > 0 {
        problems.push(format!("{held} signal(s) left claimed by a process that is gone; they will be retried"));
    }

    // A socket left behind by a core that is not running.
    let sock = state.join("core.sock");
    notes["socket"] = json!(sock.exists());

    let thoughts = count_files(&state.join("thoughts"), "json");
    notes["thoughts"] = json!(thoughts);

    json!({
        "state": state.display().to_string(),
        "notes": notes,
        "problems": problems,
    })
}

fn count_files(dir: &Path, ext: &str) -> usize {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map(|x| x == ext).unwrap_or(false))
                .count()
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn born(d: &Path) {
        std::fs::create_dir_all(d.join("main")).unwrap();
        std::fs::create_dir_all(d.join("mailbox/new")).unwrap();
        std::fs::create_dir_all(d.join("mailbox/cur")).unwrap();
        std::fs::write(
            d.join("birth.json"),
            r#"{"id":"abc","name":"Groow","born":1000.0,"lineage":"m","hardware":"h","mentor":"M","version":"1"}"#,
        )
        .unwrap();
    }

    #[test]
    fn a_healthy_state_has_nothing_to_report() {
        let d = tempfile::tempdir().unwrap();
        born(d.path());
        let r = examine(d.path(), &d.path().join("groow.json"));
        assert_eq!(r["problems"].as_array().unwrap().len(), 0, "{r}");
        assert_eq!(r["notes"]["name"], "Groow");
    }

    #[test]
    fn a_missing_state_is_the_only_thing_reported() {
        let d = tempfile::tempdir().unwrap();
        let r = examine(&d.path().join("nowhere"), &d.path().join("groow.json"));
        assert_eq!(r["problems"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn an_unborn_state_says_so_plainly() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("main")).unwrap();
        let r = examine(d.path(), &d.path().join("groow.json"));
        let text = r["problems"].to_string();
        assert!(text.contains("not been born"), "{text}");
    }

    #[test]
    fn a_broken_config_is_caught_before_it_stops_a_start() {
        let d = tempfile::tempdir().unwrap();
        born(d.path());
        let cfg = d.path().join("groow.json");
        std::fs::write(&cfg, "{ not json").unwrap();
        let r = examine(d.path(), &cfg);
        assert!(r["problems"].to_string().contains("refuse to start"));
    }

    #[test]
    fn a_corrupt_journal_line_is_found_and_counted() {
        let d = tempfile::tempdir().unwrap();
        born(d.path());
        std::fs::write(
            d.path().join("main/2026-09-18_120000.jsonl"),
            "{\"role\":\"user\"}\n{ broken\n{\"role\":\"assistant\"}\n",
        )
        .unwrap();
        let r = examine(d.path(), &d.path().join("groow.json"));
        assert!(r["problems"].to_string().contains("unreadable line"));
        assert_eq!(r["notes"]["latest_journal_lines"], 3);
    }

    #[test]
    fn work_left_claimed_by_a_dead_process_is_pointed_out() {
        let d = tempfile::tempdir().unwrap();
        born(d.path());
        std::fs::write(d.path().join("mailbox/cur/0-1-1-user.json"), "{}").unwrap();
        let r = examine(d.path(), &d.path().join("groow.json"));
        assert!(r["problems"].to_string().contains("will be retried"));
    }

    #[test]
    fn waiting_work_is_counted_but_is_not_a_problem() {
        let d = tempfile::tempdir().unwrap();
        born(d.path());
        std::fs::write(d.path().join("mailbox/new/0-1-1-user.json"), "{}").unwrap();
        let r = examine(d.path(), &d.path().join("groow.json"));
        assert_eq!(r["notes"]["waiting"], 1);
        assert_eq!(r["problems"].as_array().unwrap().len(), 0, "a queue is normal");
    }
}
