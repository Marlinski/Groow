//! Starting the processes that do the thinking.
//!
//! The core runs as root and owns the state. The mind runs as a lesser user and owns nothing:
//! it can read the state directory but not write to it, and it reaches the core only through
//! the socket. Everything it is allowed to change, it changes by asking.
//!
//! When the core is not root, which is the ordinary development case, processes run as
//! whoever started it. The separation is then a convention rather than a guarantee, and the
//! core says so at startup rather than pretending otherwise.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::{Child, Command};

/// How the mind's processes are launched.
#[derive(Debug, Clone)]
pub struct Spawner {
    /// The `groow` binary, as the core itself was launched.
    pub exe: PathBuf,
    /// The user the mind runs as, when the core has the authority to drop to it.
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    /// The mind's home, which is its working directory.
    pub home: PathBuf,
    /// The socket it talks back on.
    pub socket: PathBuf,
    /// The state directory, readable to it.
    pub state: PathBuf,
}

impl Spawner {
    /// Whether the mind will really run as a different user, or only nominally.
    pub fn is_isolated(&self) -> bool {
        crate::peer::is_root() && self.uid.map(|u| u != 0).unwrap_or(false)
    }

    /// A description for the startup line, so the operator knows which of the two it got.
    pub fn describe(&self) -> String {
        match (crate::peer::is_root(), self.uid) {
            (true, Some(u)) if u != 0 => format!("the mind runs as uid {u} and cannot write the state"),
            (true, _) => "the core is root but no agent user is set, so the mind runs as root too".into(),
            (false, _) => "the core is not root, so the mind runs as the same user and the state is writable to it".into(),
        }
    }

    fn base(&self, args: &[&str]) -> Command {
        let mut c = Command::new(&self.exe);
        c.args(args)
            .current_dir(&self.home)
            .env_clear()
            .env("HOME", &self.home)
            // Its own commands come first: a skill it writes shadows anything shipped, and
            // it can reach its skills by name from any directory.
            .env(
                "PATH",
                format!(
                    "{}:/usr/local/bin:/usr/bin:/bin",
                    crate::skills::bin_dir(&self.home).display()
                ),
            )
            .env("GROOW_SOCKET", &self.socket)
            .env("GROOW_STATE", &self.state)
            // The mark that tells the command line it is being run by the mind itself, so that
            // the mentor's commands refuse rather than letting it operate on its own body.
            .env("GROOW_SELF", "1")
            .env("LANG", "C.UTF-8")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if self.is_isolated() {
            if let Some(u) = self.uid {
                c.uid(u);
            }
            if let Some(g) = self.gid {
                c.gid(g);
            }
        }
        c
    }

    /// One conscious turn.
    pub fn turn(&self) -> std::io::Result<Child> {
        self.base(&["run-turn"]).spawn()
    }

    /// One inner thought.
    pub fn thought(&self, id: &str) -> std::io::Result<Child> {
        self.base(&["run-thought", id]).spawn()
    }
}



/// Make the state directory readable but not writable by the mind, when we have the authority.
///
/// This is the whole point of the split: the conversation, the statistics and the birth
/// certificate are visible to the mind and beyond its reach. Its own working areas, the
/// skills it writes and the workspace it uses, stay writable.
pub fn lock_state(state: &Path, agent_uid: Option<u32>) -> std::io::Result<bool> {
    if !crate::peer::is_root() {
        return Ok(false);
    }
    let Some(uid) = agent_uid.filter(|u| *u != 0) else { return Ok(false) };
    use std::os::unix::fs::PermissionsExt;

    // Root owns the state; everyone else may look.
    set(state, 0o755)?;
    for name in ["main", "mailbox", "limbic", "log", "thoughts", "training"] {
        let p = state.join(name);
        if p.exists() {
            set(&p, 0o755)?;
        }
    }
    // These are the mind's own, so it must be able to write them.
    for name in ["skills", "recipes", "workspace"] {
        let p = state.join(name);
        if p.exists() {
            std::os::unix::fs::chown(&p, Some(uid), None)?;
            set(&p, 0o755)?;
        }
    }
    // How it is being scored is not its business.
    let db = state.join("groow.db");
    if db.exists() {
        set(&db, 0o600)?;
    }
    return Ok(true);

    fn set(p: &Path, mode: u32) -> std::io::Result<()> {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawner(d: &tempfile::TempDir, uid: Option<u32>) -> Spawner {
        Spawner {
            exe: PathBuf::from("/bin/echo"),
            uid,
            gid: None,
            home: d.path().to_path_buf(),
            socket: d.path().join("core.sock"),
            state: d.path().join("state"),
        }
    }

    #[test]
    fn isolation_is_only_claimed_when_it_is_real() {
        let d = tempfile::tempdir().unwrap();
        let s = spawner(&d, Some(1000));
        assert_eq!(s.is_isolated(), crate::peer::is_root(), "isolation needs the authority to drop privileges");
        let same_user = spawner(&d, Some(0));
        assert!(!same_user.is_isolated());
    }

    #[test]
    fn the_description_is_honest_about_which_case_it_is() {
        let d = tempfile::tempdir().unwrap();
        let s = spawner(&d, Some(1000));
        let text = s.describe();
        if crate::peer::is_root() {
            assert!(text.contains("cannot write"));
        } else {
            assert!(text.contains("not root"), "it must admit the weaker guarantee: {text}");
        }
    }

    #[tokio::test]
    async fn its_own_commands_come_first_on_the_path() {
        let d = tempfile::tempdir().unwrap();
        let mut s = spawner(&d, None);
        s.exe = PathBuf::from("/usr/bin/env");
        let out = s.base(&[]).output().await.unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let path = text.lines().find(|l| l.starts_with("PATH=")).unwrap_or("");
        assert!(path.contains(".local/bin"), "its skills are not reachable: {path}");
        assert!(
            path.find(".local/bin") < path.find("/usr/bin"),
            "a skill it wrote should shadow anything shipped: {path}"
        );
    }

    #[tokio::test]
    async fn a_spawned_process_gets_a_clean_environment_pointing_at_the_core() {
        let d = tempfile::tempdir().unwrap();
        let mut s = spawner(&d, None);
        s.exe = PathBuf::from("/usr/bin/env");
        let out = s.base(&[]).output().await.unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("GROOW_SELF=1"), "the mind must be marked as itself: {text}");
        assert!(text.contains("GROOW_SOCKET="));
        assert!(text.contains("GROOW_STATE="));
        assert!(!text.contains("SSH_"), "the host's environment must not leak in");
    }

    #[test]
    fn locking_the_state_is_a_no_op_without_the_authority_to_do_it() {
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join("state");
        std::fs::create_dir_all(state.join("skills")).unwrap();
        let locked = lock_state(&state, Some(1000)).unwrap();
        assert_eq!(locked, crate::peer::is_root());
    }

    #[test]
    fn locking_without_an_agent_user_changes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        assert!(!lock_state(&state, None).unwrap());
        assert!(!lock_state(&state, Some(0)).unwrap(), "root is not a lesser user");
    }
}
