//! Reaching a Groow that lives in a container.
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

/// Where the state lives inside the body.
pub const STATE_INSIDE: &str = "/home/groow/state";

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

pub fn have_docker() -> bool {
    Command::new("docker").arg("--version").stdout(Stdio::null()).stderr(Stdio::null())
        .status().map(|s| s.success()).unwrap_or(false)
}

/// Run the same command inside the body, as its owner, and take on its exit code.
pub fn run_inside(args: &[String], interactive: bool) -> anyhow::Result<std::process::ExitStatus> {
    let dir = project().ok_or_else(|| anyhow::anyhow!("no docker-compose.yml above this directory"))?;
    let mut c = Command::new("docker");
    c.arg("compose").arg("exec");
    if !interactive {
        c.arg("-T");
    }
    c.args(["-u", "0", "groow", "groow", "--state", STATE_INSIDE]);
    c.args(args);
    c.current_dir(&dir);
    Ok(c.status()?)
}

/// Build the body if it is not there, and wake it.
pub fn wake(rebuild: bool) -> anyhow::Result<()> {
    let dir = project().ok_or_else(|| {
        anyhow::anyhow!("there is no docker-compose.yml here, so there is no body to wake. \
Run this from the project, or use `groow start --here` to run it on this machine.")
    })?;
    if !have_docker() {
        anyhow::bail!("docker is not installed, so there is no body to wake. \
`groow start --here` runs it on this machine instead.");
    }

    let built = Command::new("docker")
        .args(["image", "inspect", "groow:latest"])
        .stdout(Stdio::null()).stderr(Stdio::null())
        .status().map(|s| s.success()).unwrap_or(false);
    if rebuild || !built {
        eprintln!("building its body (a few minutes, once)…");
        let ok = Command::new("docker").args(["compose", "build", "groow"])
            .current_dir(&dir).status()?.success();
        if !ok {
            anyhow::bail!("the body would not build");
        }
    }

    let first = !dir.join("home/state/birth.json").exists();
    if first {
        eprintln!("no birth certificate yet, so this is a birth: it will fetch its base model, about 8 GB, once.");
    }
    let ok = Command::new("docker").args(["compose", "up", "-d", "groow"])
        .current_dir(&dir).status()?.success();
    if !ok {
        anyhow::bail!("the body would not start");
    }
    Ok(())
}

pub fn stop() -> anyhow::Result<()> {
    let dir = project().ok_or_else(|| anyhow::anyhow!("no docker-compose.yml above this directory"))?;
    Command::new("docker").args(["compose", "stop", "groow"]).current_dir(&dir).status()?;
    Ok(())
}

pub fn logs() -> anyhow::Result<()> {
    let dir = project().ok_or_else(|| anyhow::anyhow!("no docker-compose.yml above this directory"))?;
    Command::new("docker").args(["compose", "logs", "-f", "groow"]).current_dir(&dir).status()?;
    Ok(())
}

/// A shell in its home, as the mind rather than as its owner.
pub fn shell() -> anyhow::Result<()> {
    let dir = project().ok_or_else(|| anyhow::anyhow!("no docker-compose.yml above this directory"))?;
    Command::new("docker")
        .args(["compose", "exec", "-u", "groow", "groow", "bash", "-l"])
        .current_dir(&dir)
        .status()?;
    Ok(())
}

/// Whether it can actually answer yet, rather than merely being up.
///
/// The core comes up in a moment; the weights take the best part of a minute. Until the brain
/// is loaded there is nothing a turn could do, so this is what "awake" has to mean.
pub fn brain_ready() -> bool {
    let Some(dir) = project() else { return false };
    let out = Command::new("docker")
        .args(["compose", "exec", "-T", "-u", "0", "groow", "groow", "--state", STATE_INSIDE, "status"])
        .current_dir(&dir)
        .stderr(Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v.get("brain").and_then(|b| b.as_bool()))
                .unwrap_or(false)
        }
        _ => false,
    }
}

/// Whether a state directory is the one the sandbox writes through its bind mount.
///
/// Talking to that socket from the host would arrive with the mind's authority, not yours,
/// because inside the container your user id belongs to the mind.
pub fn is_the_sandboxes(state: &Path) -> bool {
    state.ends_with("home/state")
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
        assert!(is_the_sandboxes(Path::new("/srv/groow/home/state")));
        assert!(is_the_sandboxes(Path::new("home/state")));
        assert!(!is_the_sandboxes(Path::new("/srv/groow/state")));
        assert!(!is_the_sandboxes(Path::new("state")));
    }

    #[test]
    fn the_project_is_found_by_its_compose_file() {
        // Whatever the answer here, it must not panic or wander off the filesystem.
        let _ = project();
    }
}
