//! Skills.
//!
//! A skill is an executable file in the mind's own `bin` directory. That is the whole
//! specification. It can be a Python script, a shell script, a compiled binary or anything
//! else the kernel will run, because it is reached through the shell like any other command:
//! it gets arguments, it prints, and it exits with a status. The mind learns from the status
//! and the error message, which is exactly what it would get from any other command.
//!
//! Nothing here loads code into the core, so a broken skill can crash its own process and
//! nothing else. The core only puts the shipped ones in place on a first start, and never
//! overwrites one the mind has changed.

use std::fs;
use std::path::{Path, PathBuf};

/// Where the mind's own commands live. On its `PATH`, and writable by it.
pub fn bin_dir(home: &Path) -> PathBuf {
    home.join(".local/bin")
}

/// Put the shipped skills in place, without ever overwriting the mind's own work.
///
/// Returns the names installed. A skill that already exists is left alone, whatever its
/// contents: once the mind has touched a command, that command is its own.
pub fn seed(home: &Path, from: &Path) -> std::io::Result<Vec<String>> {
    let bin = bin_dir(home);
    fs::create_dir_all(&bin)?;
    let mut put = Vec::new();
    let rd = match fs::read_dir(from) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(put),
        Err(e) => return Err(e),
    };
    for entry in rd.filter_map(|e| e.ok()) {
        let src = entry.path();
        if !src.is_file() {
            continue;
        }
        let Some(name) = src.file_name().and_then(|n| n.to_str()) else { continue };
        // A name with a dot is a note or a module, not a command.
        if name.starts_with('.') || name.contains('.') {
            continue;
        }
        let dst = bin.join(name);
        if dst.exists() {
            continue;
        }
        fs::copy(&src, &dst)?;
        make_runnable(&dst)?;
        put.push(name.to_string());
    }
    Ok(put)
}

/// Everything the mind can run that it did not get from us.
pub fn installed(home: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(bin_dir(home))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| is_runnable(&e.path()))
                .filter_map(|e| e.file_name().to_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn make_runnable(p: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(p, fs::Permissions::from_mode(0o755))?;
    }
    let _ = p;
    Ok(())
}

fn is_runnable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return p.is_file()
            && fs::metadata(p).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false);
    }
    #[cfg(not(unix))]
    p.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shipped(d: &Path) -> PathBuf {
        let s = d.join("shipped");
        fs::create_dir_all(&s).unwrap();
        fs::write(s.join("web"), "#!/usr/bin/env python3\nprint('hi')\n").unwrap();
        fs::write(s.join("news"), "#!/bin/sh\necho hi\n").unwrap();
        fs::write(s.join("README.md"), "not a command").unwrap();
        s
    }

    #[test]
    fn shipped_skills_land_on_the_path_and_can_be_run() {
        let d = tempfile::tempdir().unwrap();
        let from = shipped(d.path());
        let put = seed(d.path(), &from).unwrap();
        let mut put = put;
        put.sort();
        assert_eq!(put, vec!["news".to_string(), "web".to_string()]);
        assert!(is_runnable(&bin_dir(d.path()).join("web")), "a skill must be executable");
    }

    #[test]
    fn a_skill_may_be_written_in_any_language() {
        let d = tempfile::tempdir().unwrap();
        let from = shipped(d.path());
        seed(d.path(), &from).unwrap();
        let py = fs::read_to_string(bin_dir(d.path()).join("web")).unwrap();
        let sh = fs::read_to_string(bin_dir(d.path()).join("news")).unwrap();
        assert!(py.contains("python3"));
        assert!(sh.contains("/bin/sh"), "nothing here cares what language it is");
    }

    #[test]
    fn notes_and_modules_are_not_treated_as_commands() {
        let d = tempfile::tempdir().unwrap();
        let from = shipped(d.path());
        seed(d.path(), &from).unwrap();
        assert!(!bin_dir(d.path()).join("README.md").exists());
    }

    #[test]
    fn a_skill_the_mind_has_changed_is_never_overwritten() {
        let d = tempfile::tempdir().unwrap();
        let from = shipped(d.path());
        seed(d.path(), &from).unwrap();
        let mine = bin_dir(d.path()).join("web");
        fs::write(&mine, "#!/bin/sh\necho I rewrote this myself\n").unwrap();

        let again = seed(d.path(), &from).unwrap();
        assert!(again.is_empty(), "a second start should install nothing");
        assert!(fs::read_to_string(&mine).unwrap().contains("rewrote this myself"));
    }

    #[test]
    fn seeding_is_safe_when_there_is_nothing_to_seed() {
        let d = tempfile::tempdir().unwrap();
        assert!(seed(d.path(), &d.path().join("nowhere")).unwrap().is_empty());
    }

    #[test]
    fn what_the_mind_has_written_itself_is_listed_too() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(bin_dir(d.path())).unwrap();
        let own = bin_dir(d.path()).join("tides");
        fs::write(&own, "#!/bin/sh\necho high tide\n").unwrap();
        make_runnable(&own).unwrap();
        // A file it wrote but never made executable is not a command yet.
        fs::write(bin_dir(d.path()).join("draft"), "not finished").unwrap();
        assert_eq!(installed(d.path()), vec!["tides".to_string()]);
    }
}
