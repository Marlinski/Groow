//! The prompt the mind writes for itself.
//!
//! The core owns the part that cannot be argued with: who it is and the facts on its birth
//! certificate. Everything else in the system prompt comes from a shell script in its own
//! home, run at the start of every turn, whose output is appended.
//!
//! That is deliberate. A person tunes their shell prompt to show what they need in front of
//! them; this is the same thing. The mind can put its skills there, the date, how much disk it
//! has left, a note to itself about what it is working on, or nothing at all. It is a file it
//! owns, so what it sees at the start of every turn is something it can change.
//!
//! The rules around it exist so that it cannot lock itself out. If the script fails, or hangs,
//! or prints a library's worth of text, the turn still happens and the mind is told what went
//! wrong, which is exactly the information it needs to fix the file.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where the script lives. Named after the shell's own convention, because that is what it is.
///
/// The core puts one in a new home, copied from the skeleton like everything else in there.
/// Nothing is compiled in here: what a new creature is given is a file on disk that can be
/// changed without rebuilding anything.
pub const RC: &str = ".groowrc";

/// How long it may take before the turn goes on without it.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// How much of its output is used. A runaway listing must not crowd out the conversation.
pub const MAX_CHARS: usize = 4000;

/// Run the script, if there is one, and return what it printed.
///
/// Errors come back as text rather than as failures, because the mind is the one who can fix
/// them and it can only do that if it is told.
pub async fn greeting(home: &Path) -> Option<String> {
    let rc = home.join(RC);
    if !rc.exists() {
        return None;
    }
    let started = std::time::Instant::now();
    let mut cmd = tokio::process::Command::new("/bin/sh");
    cmd.arg(&rc)
        .current_dir(home)
        // The script belongs to this home, so that is the home it should see. Without this it
        // would read whatever HOME the process happened to inherit.
        .env("HOME", home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Some(broken(&format!("it could not be started: {e}"))),
    };
    let out = match tokio::time::timeout(TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Some(broken(&format!("it could not be run: {e}"))),
        Err(_) => {
            return Some(broken(&format!(
                "it was still running after {} seconds and was stopped",
                TIMEOUT.as_secs()
            )))
        }
    };

    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        let why = why.trim();
        let mut note = broken(&format!(
            "it exited with {}{}",
            out.status.code().map(|c| c.to_string()).unwrap_or_else(|| "a signal".into()),
            if why.is_empty() { String::new() } else { format!(": {}", clip(why, 300)) }
        ));
        if !text.is_empty() {
            note.push_str(&format!("\n\nWhat it printed before that:\n{}", clip(&text, MAX_CHARS)));
        }
        return Some(note);
    }
    if text.is_empty() {
        return None;
    }
    let _ = started;
    Some(clip(&text, MAX_CHARS))
}

/// The mind's home, which is where the script lives.
pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

fn broken(why: &str) -> String {
    format!(
        "Your `{RC}` did not run: {why}. Nothing else is affected, but until you fix it this \
part of your prompt will say so instead of saying what you wanted."
    )
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let head: String = s.chars().take(n).collect();
    format!("{head}\n[\u{2026} the rest was left out; keep this short, it is read every turn \u{2026}]")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(home: &Path, body: &str) {
        let p = home.join(RC);
        std::fs::write(&p, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[tokio::test]
    async fn what_it_prints_becomes_part_of_the_prompt() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "#!/bin/sh\necho 'Today is Thursday.'\necho 'Skills: web, news'\n");
        let g = greeting(d.path()).await.unwrap();
        assert!(g.contains("Today is Thursday."));
        assert!(g.contains("Skills: web, news"));
    }

    #[tokio::test]
    async fn with_no_script_there_is_nothing_to_add() {
        let d = tempfile::tempdir().unwrap();
        assert!(greeting(d.path()).await.is_none());
    }

    #[tokio::test]
    async fn a_script_that_prints_nothing_adds_nothing() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "#!/bin/sh\nexit 0\n");
        assert!(greeting(d.path()).await.is_none());
    }

    #[tokio::test]
    async fn a_broken_script_tells_the_mind_rather_than_stopping_the_turn() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "#!/bin/sh\necho 'half of it'\necho 'no such command' >&2\nexit 3\n");
        let g = greeting(d.path()).await.unwrap();
        assert!(g.contains("did not run"), "{g}");
        assert!(g.contains("exited with 3"), "{g}");
        assert!(g.contains("no such command"), "it should be told why: {g}");
        assert!(g.contains("half of it"), "and what it managed to print: {g}");
    }

    #[tokio::test]
    async fn a_script_that_hangs_is_stopped_and_the_turn_goes_on() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "#!/bin/sh\nsleep 300\n");
        let started = std::time::Instant::now();
        let g = tokio::time::timeout(TIMEOUT + Duration::from_secs(5), greeting(d.path()))
            .await
            .expect("the turn was held up past the limit")
            .unwrap();
        assert!(g.contains("still running"), "{g}");
        assert!(started.elapsed() < TIMEOUT + Duration::from_secs(3));
    }

    #[tokio::test]
    async fn a_runaway_script_cannot_crowd_out_the_conversation() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "#!/bin/sh\nfor i in $(seq 1 5000); do echo 'a line of noise'; done\n");
        let g = greeting(d.path()).await.unwrap();
        assert!(g.chars().count() < MAX_CHARS + 200, "the prompt grew to {}", g.chars().count());
        assert!(g.contains("left out"), "it should say it was cut");
    }

    #[tokio::test]
    async fn it_runs_in_the_home_so_relative_paths_work() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("note.txt"), "a note to myself").unwrap();
        write(d.path(), "#!/bin/sh\ncat note.txt\n");
        assert!(greeting(d.path()).await.unwrap().contains("a note to myself"));
    }

    /// Put the shipped script in the home, which is what the core does on a first start.
    /// Read from the skeleton rather than compiled in, so these test the file that ships.
    fn shipped(home: &Path) {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../skel").join(RC);
        std::fs::copy(&src, home.join(RC)).unwrap_or_else(|e| panic!("{}: {e}", src.display()));
    }

    #[tokio::test]
    async fn the_default_script_lists_what_is_actually_there() {
        let d = tempfile::tempdir().unwrap();
        let skill = d.path().join("skills/tides");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: tides\ndescription: The next high tide at a port.\n---\n\nRun `tides <port>`.\n",
        )
        .unwrap();
        shipped(d.path());
        let g = greeting(d.path()).await.unwrap();
        assert!(g.contains("- tides: The next high tide at a port."), "{g}");
    }

    #[tokio::test]
    async fn a_skill_in_some_other_shape_is_simply_skipped() {
        // The format is a convention, not a rule. A directory without a SKILL.md is the mind's
        // business and nothing should object to it.
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("skills/notes")).unwrap();
        std::fs::write(d.path().join("skills/notes/thoughts.txt"), "whatever I like").unwrap();
        shipped(d.path());
        let g = greeting(d.path()).await.unwrap();
        assert!(!g.contains("notes"), "it should not be announced as a skill: {g}");
        assert!(g.contains("Today is"), "and the rest of the prompt still works");
    }

    #[tokio::test]
    async fn the_default_script_runs_and_says_something_useful() {
        let d = tempfile::tempdir().unwrap();
        shipped(d.path());
        let g = greeting(d.path()).await.unwrap();
        assert!(g.contains("Today is"), "{g}");
        assert!(g.contains("in your shell"), "it should say how a skill is run: {g}");
        assert!(g.contains("~/skills"), "it should say where they are: {g}");
        // `groow` is not on the path in a test, so the skills line is empty; that must not
        // turn the whole thing into a failure.
        assert!(!g.contains("did not run"), "a missing command should not break the prompt: {g}");
    }
}
