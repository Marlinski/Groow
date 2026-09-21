//! Reaching a Groow that lives in a container.
//!
//! Finding the creature, never deciding where it runs. Nothing here starts, builds or stops a
//! body: where the core runs is a runtime concern, and in this project the Makefile is what
//! decides it. This is only how a command follows the creature to wherever it already is.
//!
//! There is one command. If Groow is running in its sandbox, `groow` finds it there and runs
//! the same thing inside; if it is running on this machine, `groow` talks to it directly. You
//! should not have to know which, and you should not have to learn a second command to find
//! out.
//!
//! It has to go through the container rather than the socket on the bind mount, because the
//! core decides who you are from the user id on the connection. Inside the container the
//! account with your host user id is the mind, not you, so a request from the host would
//! arrive with the mind's authority. Running the command inside as root is what makes you the
//! owner rather than the inhabitant.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Where it lives inside the body.
pub const HOME_INSIDE: &str = "/home/groow";

/// Whether this process is already running inside the body.
///
/// The image says so. A command in there must never try to start or reach a sandbox, because
/// it is in one.
pub fn inside_the_body() -> bool {
    std::env::var("GROOW_BODY").map(|v| v == "sandbox").unwrap_or(false)
}

/// The project directory, which is where the compose file is.
pub fn project() -> Option<PathBuf> {
    let mut d = std::env::current_dir().ok()?;
    loop {
        if d.join("docker-compose.yml").is_file() {
            return Some(d);
        }
        if !d.pop() {
            return None;
        }
    }
}

/// Whether Groow is awake in its sandbox right now.
pub fn running() -> bool {
    if inside_the_body() {
        return false;
    }
    let Some(dir) = project() else { return false };
    Command::new("docker")
        .args(["compose", "ps", "-q", "--status", "running", "groow"])
        .current_dir(&dir)
        .stderr(Stdio::null())
        .output()
        .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
        .unwrap_or(false)
}

/// Everything the window turns on in a terminal, turned off again.
///
/// The window runs inside the body, so the escape codes that put this terminal into its
/// alternate screen with the mouse captured came from a process on the other side of a docker
/// exec. When that process is killed rather than closed — the body rebuilt under it, the
/// container recreated — it never gets to undo them, and what is left is a terminal you cannot
/// type into. This side can always undo them, because it is still here.
const PUT_IT_BACK: &str = "\x1b[?1049l\x1b[?1006l\x1b[?1015l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?25h";

/// Run the same command inside the body, as its owner, and take on its exit code.
pub fn run_inside(args: &[String], interactive: bool) -> anyhow::Result<std::process::ExitStatus> {
    let dir = project().ok_or_else(|| anyhow::anyhow!("no docker-compose.yml above this directory"))?;
    let mut c = Command::new("docker");
    c.arg("compose").arg("exec");
    if !interactive {
        c.arg("-T");
    }
    c.args(["-u", "0", "groow", "groow", "--home", HOME_INSIDE]);
    c.args(args);
    c.current_dir(&dir);
    let status = c.status()?;
    if interactive {
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = out.write_all(PUT_IT_BACK.as_bytes());
        let _ = out.flush();
    }
    Ok(status)
}

/// Whether a home is the one the sandbox writes through its bind mount.
///
/// Talking to that socket from the host would arrive with the mind's authority, not yours,
/// because inside the container your user id belongs to the mind.
pub fn is_the_sandboxes(home: &Path) -> bool {
    home.ends_with("home")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inside_the_body_there_is_no_sandbox_to_reach_for() {
        // Without this the body starts, looks for a sandbox, finds none, and exits in a loop.
        std::env::set_var("GROOW_BODY", "sandbox");
        assert!(inside_the_body());
        assert!(!running(), "a command in the body must not think there is a sandbox around it");
        std::env::remove_var("GROOW_BODY");
        assert!(!inside_the_body());
    }

    #[test]
    fn the_sandboxes_state_is_recognised() {
        assert!(is_the_sandboxes(Path::new("/srv/groow/home")));
        assert!(is_the_sandboxes(Path::new("home")));
        assert!(!is_the_sandboxes(Path::new("/srv/groow/elsewhere")));
    }

    #[test]
    fn the_project_is_found_by_its_compose_file() {
        // Whatever the answer here, it must not panic or wander off the filesystem.
        let _ = project();
    }
}
