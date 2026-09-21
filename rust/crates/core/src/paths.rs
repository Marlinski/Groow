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
    pub fn inbox(&self) -> PathBuf { self.state.join("inbox") }
    pub fn inbox_new(&self) -> PathBuf { self.inbox().join("new") }
    pub fn inbox_cur(&self) -> PathBuf { self.inbox().join("cur") }
    pub fn thoughts(&self) -> PathBuf { self.state.join("thoughts") }
    pub fn training(&self) -> PathBuf { self.state.join("training") }
    pub fn limbic(&self) -> PathBuf { self.state.join("limbic") }
    pub fn logs(&self) -> PathBuf { self.state.join("log") }

    pub fn birth(&self) -> PathBuf { self.state.join("birth.json") }
    pub fn identity(&self) -> PathBuf { self.state.join("identity.md") }
    pub fn schedule(&self) -> PathBuf { self.state.join("schedule.json") }
    pub fn incidents(&self) -> PathBuf { self.state.join("incidents.jsonl") }
    pub fn url_file(&self) -> PathBuf { self.state.join("groow.url") }
    pub fn socket(&self) -> PathBuf { self.state.join("core.sock") }

    /// The statistics database. Lives beside the state but is readable only by the core,
    /// so the mind can never see or edit how it is being scored.
    pub fn db(&self) -> PathBuf { self.state.join("groow.db") }

    /// Create every directory the core writes into. Idempotent.
    /// The inbox was called the mailbox when the mind could also put questions to its mentor,
    /// and there was a second thing called an inbox to keep them apart from. A creature born
    /// before that went away still has the old directory, and what is waiting in it is a
    /// person's message, so it is moved rather than left behind.
    fn take_the_old_name(&self) -> std::io::Result<()> {
        let was = self.state.join("mailbox");
        if was.is_dir() && !self.inbox().exists() {
            fs::rename(&was, self.inbox())?;
        }
        Ok(())
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        self.take_the_old_name()?;
        for d in [
            self.state.clone(),
            self.journal(),
            self.inbox_new(),
            self.inbox_cur(),
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

/// Where the skeleton is when Groow has been installed rather than checked out.
///
/// Not `/etc/skel`: that is the system's own, copied into the home of every account anyone
/// makes on this machine, and Groow's settings have no business turning up there.
pub const SKEL_INSTALLED: &str = "/usr/share/groow/skel";

/// What a new home is given: the shipped skills, manual, prompt script and settings.
///
/// Named after `/etc/skel`, which is the same idea: a directory whose contents are copied into
/// a home the first time there is one, and never touched again. Keeping it in one place is
/// what keeps the source of the creature separate from the creature. Everything under the
/// skeleton is read and never written; everything under `home` belongs to the mind and is
/// never in the repository.
///
/// It is found on disk and never built into the binary. A creature is born from files you can
/// read and change without rebuilding anything, and if they are not there it says so rather
/// than quietly birthing something out of its own compiled-in idea of a home.
pub fn skel(told: Option<&Path>) -> Result<PathBuf, NoSkel> {
    let mut tried = Vec::new();
    // Said explicitly, on the command line or in the environment. An explicit answer that is
    // wrong is an error, not a reason to go looking somewhere else.
    let said = told
        .map(|p| (p.to_path_buf(), "--skel"))
        .or_else(|| std::env::var_os("GROOW_SKEL").map(|p| (PathBuf::from(p), "GROOW_SKEL")));
    if let Some((p, how)) = said {
        return if p.is_dir() { Ok(p) } else { Err(NoSkel { tried: vec![p], said: Some(how) }) };
    }
    for p in [project().map(|p| p.join("skel")), Some(PathBuf::from(SKEL_INSTALLED))]
        .into_iter()
        .flatten()
    {
        if p.is_dir() {
            return Ok(p);
        }
        tried.push(p);
    }
    Err(NoSkel { tried, said: None })
}

/// There is nothing to birth a creature from.
#[derive(Debug)]
pub struct NoSkel {
    /// Where it looked, in order.
    pub tried: Vec<PathBuf>,
    /// Which explicit answer sent it there, if one did.
    pub said: Option<&'static str>,
}

impl std::fmt::Display for NoSkel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let where_ = self.tried.iter().map(|p| p.display().to_string()).collect::<Vec<_>>();
        match self.said {
            Some(how) => write!(
                f,
                "{how} says the skeleton is at {}, and there is no directory there.",
                where_.join("")
            ),
            None => write!(
                f,
                "there is no skeleton to give a new home, so there is nothing to start from.\n\
                 Looked in: {}.\n\
                 Point at one with `--skel <dir>` or GROOW_SKEL; in a checkout it is `./skel`.",
                where_.join(", ")
            ),
        }
    }
}

impl std::error::Error for NoSkel {}

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
    config_in(&default_home(), None)
}

/// The settings for a particular home.
pub fn config_in(home: &Path, skel_dir: Option<&Path>) -> PathBuf {
    if let Some(p) = std::env::var_os("GROOW_CONFIG") {
        return PathBuf::from(p);
    }
    let mine = home.join("groow.json");
    if mine.is_file() {
        return mine;
    }
    // Before the home has its own, the skeleton's is the default. Not having a skeleton is not
    // this function's problem to report: whoever is starting says that far better.
    match skel(skel_dir) {
        Ok(s) if s.join("groow.json").is_file() => s.join("groow.json"),
        _ => mine,
    }
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

    /// The working directory belongs to the process, not to a test, so the ones that move it
    /// take turns. Without this they pass alone and fail together, which is worse than either.
    static CWD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Move into `dir` for as long as the guard lives, and put the process back afterwards.
    fn inside(dir: &Path) -> impl Drop {
        // The guard is never read; holding it is the whole point.
        struct Back(std::path::PathBuf, #[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
        impl Drop for Back {
            fn drop(&mut self) {
                let _ = std::env::set_current_dir(&self.0);
            }
        }
        let held = CWD.lock().unwrap_or_else(|e| e.into_inner());
        let was = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir).unwrap();
        Back(was, held)
    }

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

        let _cwd = inside(&sub);
        std::env::remove_var("GROOW_HOME");
        std::env::remove_var("GROOW_STATE");
        let home = default_home();
        let state = default_state();

        assert!(home.ends_with("home"), "the mind's home should be the project's: {}", home.display());
        assert!(state.ends_with("home/state"), "got {}", state.display());
    }

    #[test]
    fn a_creature_born_before_the_rename_keeps_what_was_waiting() {
        let d = tempfile::tempdir().unwrap();
        let p = Paths::new(d.path().join("state"));
        std::fs::create_dir_all(d.path().join("state/mailbox/new")).unwrap();
        std::fs::write(d.path().join("state/mailbox/new/0-1-1-user.json"), "{}").unwrap();

        p.ensure().unwrap();

        assert!(p.inbox_new().join("0-1-1-user.json").is_file(), "the message was left behind");
        assert!(!d.path().join("state/mailbox").exists(), "and the old name is gone");
    }

    #[test]
    fn the_settings_come_from_the_home_once_it_has_its_own() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("docker-compose.yml"), "services: {}\n").unwrap();
        std::fs::create_dir_all(d.path().join("skel")).unwrap();
        std::fs::write(d.path().join("skel/groow.json"), "{}").unwrap();
        std::fs::create_dir_all(d.path().join("home")).unwrap();

        let _cwd = inside(d.path());
        std::env::remove_var("GROOW_HOME");
        std::env::remove_var("GROOW_CONFIG");
        std::env::remove_var("GROOW_SKEL");

        // Before the home has one, the one in the skeleton is used.
        let shipped = default_config();
        std::fs::write(d.path().join("home/groow.json"), "{}").unwrap();
        let mine = default_config();

        assert!(shipped.ends_with("skel/groow.json"), "got {}", shipped.display());
        assert!(mine.ends_with("home/groow.json"), "got {}", mine.display());
    }

    #[test]
    fn a_skeleton_that_was_named_and_is_not_there_is_an_error() {
        // Saying where it is and being wrong is a mistake to report, not a reason to go and
        // birth the creature out of somebody else's skeleton.
        let d = tempfile::tempdir().unwrap();
        let said = d.path().join("nowhere");
        let e = skel(Some(&said)).unwrap_err();
        assert_eq!(e.said, Some("--skel"));
        assert!(e.to_string().contains("nowhere"), "{e}");
    }

    #[test]
    fn a_skeleton_that_is_there_is_used_as_given() {
        let d = tempfile::tempdir().unwrap();
        let s = d.path().join("skel");
        std::fs::create_dir_all(&s).unwrap();
        assert_eq!(skel(Some(&s)).unwrap(), s);
    }

    #[test]
    fn the_checkouts_skeleton_is_found_without_being_told() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("docker-compose.yml"), "services: {}\n").unwrap();
        std::fs::create_dir_all(d.path().join("skel")).unwrap();
        let _cwd = inside(d.path());
        std::env::remove_var("GROOW_SKEL");
        let found = skel(None);
        assert!(found.unwrap().ends_with("skel"));
    }

    #[test]
    fn with_no_skeleton_anywhere_it_says_where_it_looked() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("docker-compose.yml"), "services: {}\n").unwrap();
        let _cwd = inside(d.path());
        std::env::remove_var("GROOW_SKEL");
        let e = skel(None).map(|p| p.display().to_string());

        // /usr/share/groow/skel exists on an installed machine, and then there is no error to
        // check; this is about the message when there is one.
        if let Err(e) = e {
            assert!(e.to_string().contains(SKEL_INSTALLED), "{e}");
            assert!(e.to_string().contains("--skel"), "{e}");
        }
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
        assert!(p.inbox_new().is_dir());
        assert!(p.journal().is_dir());
        assert!(p.training().is_dir());
    }
}
