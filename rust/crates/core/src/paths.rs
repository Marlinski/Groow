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
#[derive(Debug, Clone)]
pub struct Paths {
    pub state: PathBuf,
}

impl Paths {
    pub fn new(state: impl Into<PathBuf>) -> Self {
        Paths { state: state.into() }
    }

    pub fn journal(&self) -> PathBuf { self.state.join("main") }
    pub fn mailbox(&self) -> PathBuf { self.state.join("mailbox") }
    pub fn mailbox_new(&self) -> PathBuf { self.mailbox().join("new") }
    pub fn mailbox_cur(&self) -> PathBuf { self.mailbox().join("cur") }
    pub fn thoughts(&self) -> PathBuf { self.state.join("thoughts") }
    pub fn training(&self) -> PathBuf { self.state.join("training") }
    pub fn limbic(&self) -> PathBuf { self.state.join("limbic") }
    pub fn skills(&self) -> PathBuf { self.state.join("skills") }
    pub fn recipes(&self) -> PathBuf { self.state.join("recipes") }
    pub fn workspace(&self) -> PathBuf { self.state.join("workspace") }
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
            self.skills(),
            self.recipes(),
            self.workspace(),
            self.logs(),
        ] {
            fs::create_dir_all(&d)?;
        }
        Ok(())
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
