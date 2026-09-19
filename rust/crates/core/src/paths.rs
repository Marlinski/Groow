//! Where everything lives, and how it is written.
//!
//! One rule governs every write in the core: a reader must never see a half-written file. So
//! nothing is written in place. A temporary file next to the target is filled, flushed,
//! fsynced, and renamed over the target, which is atomic on any POSIX filesystem. The Python
//! version rewrote several of these files in place and could truncate them on a crash; that
//! class of corruption is designed out here rather than guarded against.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The state directory and everything under it.
///
/// Only what the core maintains. The mind's own things, its skills, its commands, its manual
/// and somewhere to work, are in its home and not here; see `crate::home`.
#[derive(Debug, Clone)]
pub struct Paths {
    pub state: PathBuf,
}

impl Paths {
    /// Everything is resolved to an absolute path here, once.
    ///
    /// A process the core spawns runs in the mind's home, not in the directory the core was
    /// started from, so a relative path handed to it points somewhere else entirely. Doing
    /// this at the edge means nothing downstream has to remember it.
    pub fn new(state: impl Into<PathBuf>) -> Self {
        let state = state.into();
        let state = if state.is_absolute() {
            state
        } else {
            std::env::current_dir().map(|d| d.join(&state)).unwrap_or(state)
        };
        Paths { state }
    }

    pub fn journal(&self) -> PathBuf { self.state.join("main") }
    pub fn mailbox(&self) -> PathBuf { self.state.join("mailbox") }
    pub fn mailbox_new(&self) -> PathBuf { self.mailbox().join("new") }
    pub fn mailbox_cur(&self) -> PathBuf { self.mailbox().join("cur") }
    pub fn thoughts(&self) -> PathBuf { self.state.join("thoughts") }
    pub fn training(&self) -> PathBuf { self.state.join("training") }
    pub fn limbic(&self) -> PathBuf { self.state.join("limbic") }
    pub fn logs(&self) -> PathBuf { self.state.join("log") }

    pub fn birth(&self) -> PathBuf { self.state.join("birth.json") }
    pub fn identity(&self) -> PathBuf { self.state.join("identity.md") }
    pub fn inbox(&self) -> PathBuf { self.state.join("mentor_inbox.jsonl") }
    pub fn schedule(&self) -> PathBuf { self.state.join("schedule.json") }
    pub fn incidents(&self) -> PathBuf { self.state.join("incidents.jsonl") }
    pub fn activity(&self) -> PathBuf { self.logs().join("activity.jsonl") }
    pub fn url_file(&self) -> PathBuf { self.state.join("groow.url") }
    pub fn socket(&self) -> PathBuf { self.state.join("core.sock") }

    /// The statistics database. Lives beside the state but is readable only by the core,
    /// so the mind can never see or edit how it is being scored.
    pub fn db(&self) -> PathBuf { self.state.join("groow.db") }

    /// Create every directory the core writes into. Idempotent.
    pub fn ensure(&self) -> std::io::Result<()> {
        for d in [
            self.state.clone(),
            self.journal(),
            self.mailbox_new(),
            self.mailbox_cur(),
            self.thoughts(),
            self.training(),
            self.limbic(),
            self.logs(),
        ] {
            fs::create_dir_all(&d)?;
        }
        Ok(())
    }
}

/// The project this is being run from, if it is being run from one.
///
/// A checkout is recognised by its compose file, because that is the thing that decides where
/// the creature lives: the same `home` directory is the mind's home whether the core is
/// running in the container or on this machine.
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

/// What a new home is given: the shipped skills, manual, prompt script and settings.
///
/// Named after `/etc/skel`, which is the same idea: a directory whose contents are copied into
/// a home the first time there is one, and never touched again. Keeping it in one place is
/// what keeps the source of the creature separate from the creature. Everything under `skel`
/// is in the repository and never written to; everything under `home` belongs to the mind and
/// is never in the repository.
pub fn skel() -> PathBuf {
    if let Some(p) = std::env::var_os("GROOW_SKEL") {
        return PathBuf::from(p);
    }
    if let Some(p) = project().map(|p| p.join("skel")) {
        if p.is_dir() {
            return p;
        }
    }
    PathBuf::from("/usr/share/groow/skel")
}

/// The mind's home.
///
/// In a checkout this is the project's `home` directory, which is exactly what the container
/// mounts at `/home/groow`. Running the core on this machine therefore wakes the same creature
/// the sandbox does, rather than a second one beside it, and its skills and workspace land in
/// its own home rather than in yours.
///
/// Outside a checkout it is the home of whoever is running it.
pub fn default_home() -> PathBuf {
    if let Some(p) = std::env::var_os("GROOW_HOME") {
        return PathBuf::from(p);
    }
    if let Some(project) = project() {
        return project.join("home");
    }
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// Where the state is, unless something says otherwise. Beside the mind, in its home, exactly
/// as it is inside the body.
pub fn default_state() -> PathBuf {
    if let Some(p) = std::env::var_os("GROOW_STATE") {
        return PathBuf::from(p);
    }
    default_home().join("state")
}

/// The settings, which live beside the creature in its home.
///
/// The one in the project is the shipped default, used only until the home has its own. Reading
/// the project's copy while the sandbox reads the home's would mean the same creature running
/// under two different settings depending on how it was started.
pub fn default_config() -> PathBuf {
    config_in(&default_home())
}

/// The settings for a particular home.
pub fn config_in(home: &Path) -> PathBuf {
    if let Some(p) = std::env::var_os("GROOW_CONFIG") {
        return PathBuf::from(p);
    }
    let mine = home.join("groow.json");
    if mine.is_file() {
        return mine;
    }
    let shipped = skel().join("groow.json");
    if shipped.is_file() {
        return shipped;
    }
    mine
}

/// Write a file so that a concurrent reader sees either the old content or the new, never a
/// mixture and never a truncation.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    // A unique name so two writers to the same target cannot clobber each other's temporary.
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("f"),
        std::process::id()
    ));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.flush()?;
        f.sync_all()?;
    }
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Append one line, durably. Used for the journal and the training sets, where losing the
/// last record after a crash would lose a turn or a lesson.
pub fn append_line(path: &Path, line: &str) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        fs::create_dir_all(d)?;
    }
    let mut f = fs::OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(line.as_bytes())?;
    if !line.ends_with('\n') {
        f.write_all(b"\n")?;
    }
    f.flush()?;
    f.sync_data()?;
    Ok(())
}

/// Read a whole file, returning `None` when it does not exist. Any other error is real and
/// is propagated, because silently treating a permission error as an empty file is how state
/// gets quietly destroyed.
pub fn read_opt(path: &Path) -> std::io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_replaces_the_whole_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.json");
        atomic_write(&p, b"first").unwrap();
        atomic_write(&p, b"second, and longer").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "second, and longer");
        atomic_write(&p, b"short").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "short", "the old tail must not survive");
    }

    #[test]
    fn writes_leave_no_litter_behind() {
        let d = tempfile::tempdir().unwrap();
        atomic_write(&d.path().join("a.json"), b"{}").unwrap();
        let strays: Vec<_> = fs::read_dir(d.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(strays.is_empty(), "left temporary files: {strays:?}");
    }

    #[test]
    fn it_creates_missing_parents() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("deep/deeper/x.json");
        atomic_write(&p, b"{}").unwrap();
        assert!(p.exists());
        append_line(&d.path().join("other/y.jsonl"), "{}").unwrap();
        assert!(d.path().join("other/y.jsonl").exists());
    }

    #[test]
    fn append_adds_exactly_one_newline() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("l.jsonl");
        append_line(&p, "one").unwrap();
        append_line(&p, "two\n").unwrap();
        append_line(&p, "three").unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "one\ntwo\nthree\n");
    }

    #[test]
    fn a_missing_file_is_none_not_an_error() {
        let d = tempfile::tempdir().unwrap();
        assert!(read_opt(&d.path().join("nope")).unwrap().is_none());
    }

    #[test]
    fn in_a_checkout_the_state_is_the_one_the_sandbox_uses() {
        // Otherwise running it here makes a second creature beside the one in the container,
        // with its own weights and its own conversation, and neither knows about the other.
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("docker-compose.yml"), "services: {}\n").unwrap();
        let sub = d.path().join("rust/crates");
        std::fs::create_dir_all(&sub).unwrap();

        let was = std::env::current_dir().unwrap();
        std::env::set_current_dir(&sub).unwrap();
        std::env::remove_var("GROOW_HOME");
        std::env::remove_var("GROOW_STATE");
        let home = default_home();
        let state = default_state();
        std::env::set_current_dir(was).unwrap();

        assert!(home.ends_with("home"), "the mind's home should be the project's: {}", home.display());
        assert!(state.ends_with("home/state"), "got {}", state.display());
    }

    #[test]
    fn the_settings_come_from_the_home_once_it_has_its_own() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("docker-compose.yml"), "services: {}\n").unwrap();
        std::fs::create_dir_all(d.path().join("skel")).unwrap();
        std::fs::write(d.path().join("skel/groow.json"), "{}").unwrap();
        std::fs::create_dir_all(d.path().join("home")).unwrap();

        let was = std::env::current_dir().unwrap();
        std::env::set_current_dir(d.path()).unwrap();
        std::env::remove_var("GROOW_HOME");
        std::env::remove_var("GROOW_CONFIG");
        std::env::remove_var("GROOW_SKEL");

        // Before the home has one, the one in the skeleton is used.
        let shipped = default_config();
        std::fs::write(d.path().join("home/groow.json"), "{}").unwrap();
        let mine = default_config();
        std::env::set_current_dir(was).unwrap();

        assert!(shipped.ends_with("skel/groow.json"), "got {}", shipped.display());
        assert!(mine.ends_with("home/groow.json"), "got {}", mine.display());
    }

    #[test]
    fn being_told_where_it_is_wins() {
        std::env::set_var("GROOW_STATE", "/srv/elsewhere/state");
        assert_eq!(default_state(), PathBuf::from("/srv/elsewhere/state"));
        std::env::remove_var("GROOW_STATE");
    }

    #[test]
    fn a_relative_state_directory_is_made_absolute() {
        // A process started by the core runs somewhere else, so a relative path would send it
        // looking in the wrong place. This is the bug that stopped the first real turn.
        let p = Paths::new("state");
        assert!(p.state.is_absolute(), "got {}", p.state.display());
        assert!(p.socket().is_absolute());
        assert!(p.journal().is_absolute());
    }

    #[test]
    fn an_absolute_state_directory_is_left_alone() {
        let p = Paths::new("/srv/groow/state");
        assert_eq!(p.state, std::path::PathBuf::from("/srv/groow/state"));
    }

    #[test]
    fn ensure_is_idempotent() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::new(d.path().join("state"));
        p.ensure().unwrap();
        p.ensure().unwrap();
        assert!(p.mailbox_new().is_dir());
        assert!(p.journal().is_dir());
        assert!(p.training().is_dir());
    }
}
