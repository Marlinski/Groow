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
            // Its own commands come first: a command it writes shadows anything shipped, and
            // it can reach them by name from any directory.
            .env(
                "PATH",
                format!(
                    "{}:/usr/local/bin:/usr/bin:/bin",
                    crate::home::bin_dir(&self.home).display()
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
        let mut c = self.base(&["run-thought", id]);
        c.env("GROOW_THOUGHT", id);
        c.spawn()
    }

    /// A learning pass, which the mind neither asks for nor can refuse.
    ///
    /// It runs as the core does, not as the mind, because it reads the statistics and writes
    /// the weights, and neither of those is the mind's to touch.
    pub fn learn(&self, what: &str, python: &str, config: &Path, state: &Path) -> std::io::Result<Child> {
        Command::new(python)
            .args(["-m", "groow.learn", what])
            .arg("--config").arg(config)
            .arg("--state").arg(state)
            .current_dir(&self.home)
            .env("HOME", &self.home)
            .env("PATH", "/opt/venv/bin:/usr/local/bin:/usr/bin:/bin")
            .env("GROOW_STATE", state)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
    }
}



/// Put the state out of the mind's reach, when we have the authority to do it.
///
/// Permissions alone are not enough. A home that came from a bind mount is owned by the
/// account that created it, and if that happens to be the account the mind runs as, then mode
/// 755 still lets it write everything: the owner bit is the one that applies. So ownership is
/// moved to root first, and only then do the modes mean what they say.
///
/// There are no exceptions carved out of it. Everything the mind owns lives in its home, so
/// the whole of the state can simply become root's; an exception list here would be a sign the
/// boundary was drawn in the wrong place.
///
/// Returns whether the state is really protected. When it is not, the caller says so plainly
/// rather than implying a guarantee that is not there.
pub fn lock_state(state: &Path, agent_uid: Option<u32>) -> std::io::Result<bool> {
    if !crate::peer::is_root() {
        return Ok(false);
    }
    let Some(uid) = agent_uid.filter(|u| *u != 0) else { return Ok(false) };

    let _ = uid;
    own_tree(state, 0)?;
    // How it is being scored is not its business, and that includes the write-ahead files,
    // which hold everything recent.
    crate::db::Db::keep_private(&state.join("groow.db"));
    Ok(true)
}

/// Give one path to `uid`, with a mode that lets everyone read and only the owner write.
fn own(p: &Path, uid: u32, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::os::unix::fs::chown(p, Some(uid), None)?;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))
}

/// The same, for a whole tree. Directories need the execute bit to be enterable.
fn own_tree(root: &Path, uid: u32) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(root)?;
    if meta.file_type().is_symlink() {
        // Following a link out of the state would change permissions somewhere else entirely.
        return Ok(());
    }
    if meta.is_dir() {
        own(root, uid, 0o755)?;
        for e in std::fs::read_dir(root)?.filter_map(|e| e.ok()) {
            own_tree(&e.path(), uid)?;
        }
    } else {
        // The birth certificate is read-only to everyone, including root, and stays that way.
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        own(root, uid, if mode == 0o444 { 0o444 } else { 0o644 })?;
    }
    Ok(())
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
        assert!(path.contains("/bin:"), "its own commands are not reachable: {path}");
        assert!(
            path.find("/bin:") < path.find("/usr/bin"),
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
        assert_eq!(locked, crate::peer::is_root(), "it must not claim a guarantee it cannot give");
    }

    #[test]
    fn locking_moves_ownership_and_not_only_the_mode_bits() {
        // Without the change of owner, a home that came from a bind mount is still the mind's
        // to write, whatever the mode says.
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join("state");
        std::fs::create_dir_all(state.join("main")).unwrap();
        std::fs::write(state.join("main/a.jsonl"), "{}").unwrap();
        if !crate::peer::is_root() {
            assert!(!lock_state(&state, Some(1000)).unwrap());
            return;
        }
        assert!(lock_state(&state, Some(1000)).unwrap());
        use std::os::unix::fs::MetadataExt;
        assert_eq!(std::fs::metadata(state.join("main")).unwrap().uid(), 0, "the conversation is root's");
        assert_eq!(std::fs::metadata(state.join("main/a.jsonl")).unwrap().uid(), 0, "and so is every file in it");
    }

    #[test]
    fn the_birth_certificate_keeps_its_read_only_mode() {
        let d = tempfile::tempdir().unwrap();
        let state = d.path().join("state");
        std::fs::create_dir_all(&state).unwrap();
        let cert = state.join("birth.json");
        std::fs::write(&cert, "{}").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cert, std::fs::Permissions::from_mode(0o444)).unwrap();
        if !crate::peer::is_root() {
            return;
        }
        lock_state(&state, Some(1000)).unwrap();
        let mode = std::fs::metadata(&cert).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o444, "nobody should be able to rewrite it, root included");
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
