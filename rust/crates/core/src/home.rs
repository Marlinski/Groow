//! The mind's home.
//!
//! Everything in here belongs to the mind: its skills, its commands, its manual, and somewhere
//! to work. It may write, rename or delete any of it. That is the whole distinction from the
//! state directory, which the core maintains and the mind can only read.
//!
//! The core does two things here and nothing else. On a first start it puts the shipped skills
//! and the shipped manual in place, the way a system gives a new account a skeleton home, and
//! never touches them again. And it puts `bin` first on the path of every process it starts,
//! so a command the mind wrote is reachable by name and shadows anything shipped.
//!
//! What any of these files look like inside is a convention, not a rule the core enforces. The
//! shipped skills follow the Agent Skills format, a `SKILL.md` beside a `scripts` directory,
//! because that is a good shape and other tools understand it. Nothing here parses one. The
//! mind reads its own files with `cat` and lists them with `ls`, and if a plain note in some
//! other shape serves it better, nothing stops it.

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

/// Somewhere to work. Nothing in the core reads it.
pub fn workspace(home: &Path) -> PathBuf {
    home.join("workspace")
}

/// Its manual, which it may rewrite.
pub fn recipes(home: &Path) -> PathBuf {
    home.join("recipes")
}

/// Make the home, and copy the skeleton into it on a first start.
///
/// `skel` is one directory shaped like a home: `skills`, `recipes`, `groow.json`. It is the
/// shipped source and is never written to. Everything here is copied once and then left alone.
/// What the mind does with it afterwards is its business.
pub fn prepare(home: &Path, skel: &Path) -> std::io::Result<Prepared> {
    for d in [shelf(home), bin_dir(home), workspace(home), recipes(home)] {
        fs::create_dir_all(d)?;
    }
    Ok(Prepared {
        skills: seed(home, &skel.join("skills"))?,
        recipes: seed_recipes(home, &skel.join("recipes"))?,
        settings: seed_file(home, skel, "groow.json")?,
    })
}

/// Give the home its own copy of one file from the skeleton, once.
///
/// Without this the shipped copy is read forever when running here, while the sandbox reads
/// the one in the home: the same creature under two sets of settings depending on how it was
/// started. Afterwards the file is the home's, like everything else in there.
pub fn seed_file(home: &Path, skel: &Path, name: &str) -> std::io::Result<bool> {
    let mine = home.join(name);
    if mine.exists() {
        return Ok(false);
    }
    let shipped = skel.join(name);
    if !shipped.is_file() {
        return Ok(false);
    }
    fs::create_dir_all(home)?;
    fs::copy(&shipped, &mine)?;
    Ok(true)
}

#[derive(Debug, Default)]
pub struct Prepared {
    pub skills: Vec<String>,
    pub recipes: Vec<String>,
    pub settings: bool,
}

/// Copy the shipped manual into the home, never over a page the mind has rewritten.
pub fn seed_recipes(home: &Path, from: &Path) -> std::io::Result<Vec<String>> {
    let to = recipes(home);
    fs::create_dir_all(&to)?;
    let mut put = Vec::new();
    let rd = match fs::read_dir(from) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(put),
        Err(e) => return Err(e),
    };
    for e in rd.filter_map(|e| e.ok()) {
        let src = e.path();
        if !src.is_file() {
            continue;
        }
        let Some(name) = src.file_name().and_then(|n| n.to_str()) else { continue };
        let dst = to.join(name);
        if dst.exists() {
            continue;
        }
        fs::copy(&src, &dst)?;
        put.push(name.to_string());
    }
    Ok(put)
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
        p.is_file() && fs::metadata(p).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
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
    fn a_first_start_makes_the_whole_home() {
        let d = tempfile::tempdir().unwrap();
        let from = shipped(d.path());
        let manual = d.path().join("manual");
        fs::create_dir_all(&manual).unwrap();
        fs::write(manual.join("shell.md"), "how the shell works").unwrap();

        let home = d.path().join("home");
        let skel = d.path().join("skel");
        fs::create_dir_all(&skel).unwrap();
        fs::rename(&from, skel.join("skills")).unwrap();
        fs::rename(&manual, skel.join("recipes")).unwrap();
        fs::write(skel.join("groow.json"), "{}").unwrap();

        let made = prepare(&home, &skel).unwrap();
        assert!(made.settings, "the home is given its own settings");
        assert_eq!(made.recipes, vec!["shell.md".to_string()]);
        assert_eq!(made.skills.len(), 2);
        for d in [shelf(&home), bin_dir(&home), workspace(&home), recipes(&home)] {
            assert!(d.is_dir(), "{} was not made", d.display());
        }
    }

    #[test]
    fn the_home_gets_its_own_settings_and_then_keeps_them() {
        let d = tempfile::tempdir().unwrap();
        let skel = d.path().join("skel");
        fs::create_dir_all(&skel).unwrap();
        fs::write(skel.join("groow.json"), "{\"api_port\": 7373}").unwrap();
        let home = d.path().join("home");

        assert!(seed_file(&home, &skel, "groow.json").unwrap(), "the home should be given a copy");
        fs::write(home.join("groow.json"), "{\"api_port\": 9999}").unwrap();
        assert!(!seed_file(&home, &skel, "groow.json").unwrap(), "and then left to keep it");

        assert_eq!(fs::read_to_string(skel.join("groow.json")).unwrap(), "{\"api_port\": 7373}", "the skeleton is never written to");
        assert!(fs::read_to_string(home.join("groow.json")).unwrap().contains("9999"));
    }

    #[test]
    fn a_page_of_the_manual_it_has_rewritten_is_left_alone() {
        let d = tempfile::tempdir().unwrap();
        let manual = d.path().join("manual");
        fs::create_dir_all(&manual).unwrap();
        fs::write(manual.join("shell.md"), "the shipped page").unwrap();
        let home = d.path().join("home");
        seed_recipes(&home, &manual).unwrap();
        fs::write(recipes(&home).join("shell.md"), "what I have learned since").unwrap();

        assert!(seed_recipes(&home, &manual).unwrap().is_empty());
        assert_eq!(fs::read_to_string(recipes(&home).join("shell.md")).unwrap(), "what I have learned since");
    }

    #[test]
    fn the_skeleton_is_only_ever_read() {
        // The whole point of keeping it separate: the repository holds what a creature is
        // given, the home holds what it has become, and starting one never writes to the other.
        let d = tempfile::tempdir().unwrap();
        let skel = d.path().join("skel");
        fs::create_dir_all(&skel).unwrap();
        fs::rename(shipped(d.path()), skel.join("skills")).unwrap();
        fs::create_dir_all(skel.join("recipes")).unwrap();
        fs::write(skel.join("recipes/home.md"), "the shipped page").unwrap();
        fs::write(skel.join("groow.json"), "{}").unwrap();
        let before = fingerprint(&skel);

        let home = d.path().join("home");
        prepare(&home, &skel).unwrap();
        fs::write(shelf(&home).join("web/SKILL.md"), "mine now").unwrap();
        fs::write(recipes(&home).join("home.md"), "mine now").unwrap();
        fs::write(home.join("groow.json"), "{\"api_port\": 9999}").unwrap();
        prepare(&home, &skel).unwrap();

        assert_eq!(before, fingerprint(&skel), "the skeleton changed");
    }

    /// Every file under a directory and what is in it.
    fn fingerprint(root: &Path) -> Vec<(PathBuf, String)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(d) = stack.pop() {
            for e in fs::read_dir(&d).unwrap().filter_map(|e| e.ok()) {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    let rel = p.strip_prefix(root).unwrap().to_path_buf();
                    out.push((rel, fs::read_to_string(&p).unwrap_or_default()));
                }
            }
        }
        out.sort();
        out
    }

    #[test]
    fn nothing_breaks_when_there_is_nothing_there() {
        let d = tempfile::tempdir().unwrap();
        assert!(seed(d.path(), &d.path().join("nowhere")).unwrap().is_empty());
        assert!(seed_recipes(d.path(), &d.path().join("nowhere")).unwrap().is_empty());
        assert!(commands(d.path()).is_empty());
    }
}
