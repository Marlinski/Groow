//! Where the mind keeps its own things, and how they get on its path.
//!
//! This file is deliberately small. Skills are not a construct of the core: they are files in
//! the mind's home, which it owns and may write, rename or delete at will. The core does two
//! things for them and nothing else.
//!
//! It puts a copy of the shipped ones in place on a first start, the way a system gives a new
//! account a skeleton home, and never touches them again. And it puts the mind's `bin`
//! directory first on the path of every process it starts, so a command the mind wrote is
//! reachable by name and shadows anything shipped.
//!
//! What a skill looks like inside is a convention, not a rule the core enforces. The shipped
//! ones follow the Agent Skills format, a `SKILL.md` with a name and a description beside a
//! `scripts` directory, because that is a good shape and other tools understand it. Nothing
//! here parses it. The mind reads its own files with `cat`, lists them with `ls`, and if it
//! decides a plain note in a different shape serves it better, nothing stops it.

use std::fs;
use std::path::{Path, PathBuf};

/// Where the mind keeps its skills. Plainly in its home, not hidden, because they are its.
pub fn shelf(home: &Path) -> PathBuf {
    home.join("skills")
}

/// Where its commands live, first on its path.
pub fn bin_dir(home: &Path) -> PathBuf {
    home.join("bin")
}

/// Put the shipped skills in place on a first start, without ever touching one it has changed.
///
/// Returns the names installed, which will be empty on every start after the first.
pub fn seed(home: &Path, from: &Path) -> std::io::Result<Vec<String>> {
    let shelf = shelf(home);
    let bin = bin_dir(home);
    fs::create_dir_all(&shelf)?;
    fs::create_dir_all(&bin)?;

    let mut put = Vec::new();
    let rd = match fs::read_dir(from) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(put),
        Err(e) => return Err(e),
    };
    let mut dirs: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    for src in dirs {
        let Some(name) = src.file_name().and_then(|n| n.to_str()) else { continue };
        let dst = shelf.join(name);
        if dst.exists() {
            // Once it has touched a skill, that skill is its own, including whether its
            // commands are on the path.
            continue;
        }
        copy_tree(&src, &dst)?;
        put_on_path(&dst, &bin)?;
        put.push(name.to_string());
    }
    Ok(put)
}

/// Copy a skill's scripts into the mind's bin directory so they can be run by name.
///
/// Copied rather than linked: in the sandbox the shipped copy may be out of the mind's reach
/// while its bin directory is its own, and a link into a place it cannot write would be a
/// command it cannot replace.
fn put_on_path(skill_dir: &Path, bin: &Path) -> std::io::Result<()> {
    let rd = match fs::read_dir(skill_dir.join("scripts")) {
        Ok(rd) => rd,
        Err(_) => return Ok(()),
    };
    for e in rd.filter_map(|e| e.ok()) {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let Some(name) = p.file_name() else { continue };
        let target = bin.join(name);
        if target.exists() {
            continue;
        }
        fs::copy(&p, &target)?;
        make_runnable(&target)?;
    }
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for e in fs::read_dir(from)?.filter_map(|e| e.ok()) {
        let src = e.path();
        let dst = to.join(e.file_name());
        if src.is_dir() {
            copy_tree(&src, &dst)?;
        } else {
            fs::copy(&src, &dst)?;
            if src.parent().and_then(|p| p.file_name()).map(|n| n == "scripts").unwrap_or(false) {
                make_runnable(&dst)?;
            }
        }
    }
    Ok(())
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

/// Everything the mind can run by name: what came with it, and what it has written since.
/// Used only to tell a person what is there; nothing in the core depends on it.
pub fn commands(home: &Path) -> Vec<String> {
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
        fs::create_dir_all(s.join("web/scripts")).unwrap();
        fs::write(s.join("web/SKILL.md"), "---\nname: web\ndescription: Reads a page.\n---\n\nRun it.\n").unwrap();
        fs::write(s.join("web/scripts/web"), "#!/usr/bin/env python3\nprint('hi')\n").unwrap();
        fs::create_dir_all(s.join("news/scripts")).unwrap();
        fs::write(s.join("news/SKILL.md"), "---\nname: news\ndescription: Headlines.\n---\n\nRun it.\n").unwrap();
        fs::write(s.join("news/scripts/news"), "#!/bin/sh\necho hi\n").unwrap();
        s
    }

    #[test]
    fn a_first_start_puts_the_shipped_skills_in_the_home() {
        let d = tempfile::tempdir().unwrap();
        let from = shipped(d.path());
        let home = d.path().join("home");
        let mut put = seed(&home, &from).unwrap();
        put.sort();
        assert_eq!(put, vec!["news".to_string(), "web".to_string()]);
        assert!(shelf(&home).join("web/SKILL.md").exists(), "the page is in its home");
        assert!(is_runnable(&bin_dir(&home).join("web")), "and the command is on its path");
    }

    #[test]
    fn a_skill_may_be_written_in_any_language() {
        let d = tempfile::tempdir().unwrap();
        let from = shipped(d.path());
        let home = d.path().join("home");
        seed(&home, &from).unwrap();
        assert!(fs::read_to_string(bin_dir(&home).join("web")).unwrap().contains("python3"));
        assert!(fs::read_to_string(bin_dir(&home).join("news")).unwrap().contains("/bin/sh"));
    }

    #[test]
    fn anything_the_mind_has_touched_is_left_alone() {
        let d = tempfile::tempdir().unwrap();
        let from = shipped(d.path());
        let home = d.path().join("home");
        seed(&home, &from).unwrap();

        fs::write(shelf(&home).join("web/SKILL.md"), "I rewrote this in my own way.\n").unwrap();
        fs::write(bin_dir(&home).join("web"), "#!/bin/sh\necho mine\n").unwrap();

        assert!(seed(&home, &from).unwrap().is_empty(), "a later start should install nothing");
        assert_eq!(fs::read_to_string(shelf(&home).join("web/SKILL.md")).unwrap(), "I rewrote this in my own way.\n");
        assert!(fs::read_to_string(bin_dir(&home).join("web")).unwrap().contains("echo mine"));
    }

    #[test]
    fn the_core_does_not_care_what_is_inside_one() {
        // A skill is the mind's own data. If it decides a plain note serves it better, that is
        // its business and nothing here should object.
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        fs::create_dir_all(shelf(&home).join("tides")).unwrap();
        fs::write(shelf(&home).join("tides/notes.txt"), "whatever shape I like").unwrap();
        assert!(seed(&home, &d.path().join("nowhere")).unwrap().is_empty());
        assert!(shelf(&home).join("tides/notes.txt").exists());
    }

    #[test]
    fn what_it_wrote_itself_is_on_the_path_like_anything_else() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        fs::create_dir_all(bin_dir(&home)).unwrap();
        let own = bin_dir(&home).join("tides");
        fs::write(&own, "#!/bin/sh\necho high tide\n").unwrap();
        make_runnable(&own).unwrap();
        fs::write(bin_dir(&home).join("draft"), "not finished").unwrap();
        assert_eq!(commands(&home), vec!["tides".to_string()]);
    }

    #[test]
    fn nothing_breaks_when_there_is_nothing_there() {
        let d = tempfile::tempdir().unwrap();
        assert!(seed(d.path(), &d.path().join("nowhere")).unwrap().is_empty());
        assert!(commands(d.path()).is_empty());
    }
}
