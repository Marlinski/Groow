//! Skills.
//!
//! A skill is a directory with a `SKILL.md` in it, as the Agent Skills specification describes:
//! YAML frontmatter giving a name and a description, a Markdown body saying how to do the
//! thing, and optional `scripts/`, `references/` and `assets/` beside it.
//!
//! The split matters here more than it does elsewhere. Groow's competence is meant to end up in
//! its weights, and a skill is the opposite kind of memory: precise, written down, and looked
//! up when needed. So only the name and description are ever in the prompt. The body is read
//! deliberately, by running a command, which means a skill it has practised a hundred times
//! costs nothing to keep, and a skill it wrote yesterday is still there to be read.
//!
//! The executable part is a script in the skill's own directory, reachable by name because the
//! core puts it on the mind's path. Nothing is loaded into the core, so a skill that crashes
//! crashes itself and nothing else, and it can be written in any language the kernel will run.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// What the specification allows in a name, and what a directory must be called.
pub const MAX_NAME: usize = 64;
pub const MAX_DESCRIPTION: usize = 1024;
pub const MAX_COMPATIBILITY: usize = 500;

/// A skill as it is on disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub metadata: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<String>,
    /// Everything after the frontmatter: how to do the thing.
    #[serde(skip)]
    pub body: String,
    /// Where it lives, so its scripts and references can be found.
    #[serde(skip)]
    pub dir: PathBuf,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SkillError {
    #[error("there is no SKILL.md in {0}")]
    NoSkillFile(String),
    #[error("the frontmatter is missing: a SKILL.md starts with --- and a name and description")]
    NoFrontmatter,
    #[error("{0}")]
    BadField(String),
    #[error("the name `{0}` does not match its directory `{1}`")]
    NameMismatch(String, String),
}

impl Skill {
    /// Read a skill from its directory, checking it against the specification.
    ///
    /// A skill that does not validate is refused rather than half-loaded: a description that
    /// is missing is a skill the mind will never find, which is worse than an error it can see.
    pub fn read(dir: &Path) -> Result<Skill, SkillError> {
        let file = dir.join("SKILL.md");
        let text = fs::read_to_string(&file)
            .map_err(|_| SkillError::NoSkillFile(dir.display().to_string()))?;
        let (front, body) = split_frontmatter(&text).ok_or(SkillError::NoFrontmatter)?;

        let mut skill = Skill {
            name: String::new(),
            description: String::new(),
            license: None,
            compatibility: None,
            metadata: Default::default(),
            allowed_tools: None,
            body: body.trim().to_string(),
            dir: dir.to_path_buf(),
        };
        parse_frontmatter(front, &mut skill)?;
        skill.check()?;

        let folder = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if skill.name != folder {
            return Err(SkillError::NameMismatch(skill.name, folder.to_string()));
        }
        Ok(skill)
    }

    fn check(&self) -> Result<(), SkillError> {
        check_name(&self.name)?;
        let n = self.description.chars().count();
        if n == 0 {
            return Err(SkillError::BadField("a skill needs a description saying what it does and when to use it".into()));
        }
        if n > MAX_DESCRIPTION {
            return Err(SkillError::BadField(format!(
                "the description is {n} characters; the limit is {MAX_DESCRIPTION}")));
        }
        if let Some(c) = &self.compatibility {
            if c.chars().count() > MAX_COMPATIBILITY {
                return Err(SkillError::BadField(format!(
                    "compatibility is longer than {MAX_COMPATIBILITY} characters")));
            }
        }
        Ok(())
    }

    /// The executables this skill brings, which the core puts on the mind's path.
    pub fn scripts(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = fs::read_dir(self.dir.join("scripts"))
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_file() && !p.file_name().and_then(|n| n.to_str()).unwrap_or("").starts_with('.'))
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    /// One line for the prompt: enough to know it exists and when to reach for it.
    pub fn headline(&self) -> String {
        format!("{}: {}", self.name, one_line(&self.description))
    }
}

/// The name rules from the specification, which exist so a name can be a directory, a command
/// and an identifier at once.
pub fn check_name(name: &str) -> Result<(), SkillError> {
    let n = name.chars().count();
    if n == 0 || n > MAX_NAME {
        return Err(SkillError::BadField(format!("a name must be 1 to {MAX_NAME} characters")));
    }
    if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(SkillError::BadField(format!(
            "`{name}` may only hold lowercase letters, digits and hyphens")));
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err(SkillError::BadField(format!("`{name}` may not start or end with a hyphen")));
    }
    if name.contains("--") {
        return Err(SkillError::BadField(format!("`{name}` may not hold two hyphens together")));
    }
    Ok(())
}

/// Split `---` frontmatter from the body.
fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("---")?;
    let rest = rest.strip_prefix('\n').or_else(|| rest.strip_prefix("\r\n"))?;
    let end = rest.find("\n---")?;
    let front = &rest[..end];
    let after = &rest[end + 4..];
    let body = after.strip_prefix('\n').or_else(|| after.strip_prefix("\r\n")).unwrap_or(after);
    Some((front, body))
}

/// Read the frontmatter.
///
/// Deliberately a small reader rather than a YAML library: the specification's fields are
/// strings and one flat map, and a skill file that needs more than that is doing something the
/// format does not ask for.
fn parse_frontmatter(front: &str, into: &mut Skill) -> Result<(), SkillError> {
    let mut in_metadata = false;
    for line in front.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if in_metadata && indented {
            if let Some((k, v)) = line.split_once(':') {
                into.metadata.insert(k.trim().to_string(), unquote(v.trim()));
            }
            continue;
        }
        in_metadata = false;
        let Some((key, value)) = line.split_once(':') else { continue };
        let (key, value) = (key.trim(), unquote(value.trim()));
        match key {
            "name" => into.name = value,
            "description" => into.description = value,
            "license" => into.license = Some(value),
            "compatibility" => into.compatibility = Some(value),
            "allowed-tools" => into.allowed_tools = Some(value),
            "metadata" => in_metadata = true,
            _ => {}
        }
    }
    Ok(())
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2)
        || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2)
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------- the shelf
/// Where the mind's skills live, and where its commands are put.
pub fn shelf(home: &Path) -> PathBuf {
    home.join(".local/skills")
}

pub fn bin_dir(home: &Path) -> PathBuf {
    home.join(".local/bin")
}

/// Every skill on the shelf, in name order, with what is wrong with the ones that are wrong.
///
/// A broken skill is reported rather than hidden, because a skill the mind wrote and cannot
/// see is worse than one it is told about.
pub fn read_all(home: &Path) -> (Vec<Skill>, Vec<String>) {
    let mut good = Vec::new();
    let mut bad = Vec::new();
    let rd = match fs::read_dir(shelf(home)) {
        Ok(rd) => rd,
        Err(_) => return (good, bad),
    };
    let mut dirs: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    for d in dirs {
        match Skill::read(&d) {
            Ok(s) => good.push(s),
            Err(e) => bad.push(format!("{}: {e}", d.file_name().and_then(|n| n.to_str()).unwrap_or("?"))),
        }
    }
    (good, bad)
}

/// What goes in the prompt: one line each, and nothing more.
///
/// This is the whole of progressive disclosure here. The mind is told what it has and when to
/// reach for it; reading how is a command it runs when it decides to. A skill it has practised
/// often enough will not need reading at all, which is the point of learning by weight rather
/// than by context.
pub fn catalogue(home: &Path) -> Vec<String> {
    read_all(home).0.iter().map(|s| s.headline()).collect()
}

/// Put the shipped skills on the shelf and their scripts on the path, without ever overwriting
/// what the mind has changed.
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
    for entry in rd.filter_map(|e| e.ok()) {
        let src = entry.path();
        if !src.is_dir() || !src.join("SKILL.md").exists() {
            continue;
        }
        let Some(name) = src.file_name().and_then(|n| n.to_str()) else { continue };
        let dst = shelf.join(name);
        if dst.exists() {
            // Once it has touched a skill, that skill is its own.
            link_scripts(&dst, &bin)?;
            continue;
        }
        copy_tree(&src, &dst)?;
        link_scripts(&dst, &bin)?;
        put.push(name.to_string());
    }
    Ok(put)
}

/// Make a skill's scripts reachable by name.
///
/// Copied rather than symlinked: the shelf may be read-only to the mind in the sandbox while
/// its bin directory is its own, and a link into a place it cannot write would be a command it
/// cannot replace.
fn link_scripts(skill_dir: &Path, bin: &Path) -> std::io::Result<()> {
    let scripts = skill_dir.join("scripts");
    let rd = match fs::read_dir(&scripts) {
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

/// Everything the mind can run that came from a skill or that it wrote itself.
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

    fn write_skill(home: &Path, name: &str, front: &str, body: &str) -> PathBuf {
        let d = shelf(home).join(name);
        fs::create_dir_all(d.join("scripts")).unwrap();
        fs::write(d.join("SKILL.md"), format!("---\n{front}\n---\n\n{body}\n")).unwrap();
        d
    }

    #[test]
    fn a_skill_is_a_directory_with_a_name_and_a_description() {
        let t = tempfile::tempdir().unwrap();
        let d = write_skill(
            t.path(),
            "web",
            "name: web\ndescription: Reads a web page as text. Use when a link needs reading.",
            "Run `web <url>`.",
        );
        let s = Skill::read(&d).unwrap();
        assert_eq!(s.name, "web");
        assert!(s.description.starts_with("Reads a web page"));
        assert_eq!(s.body, "Run `web <url>`.");
        assert!(s.headline().starts_with("web: Reads a web page"));
    }

    #[test]
    fn the_optional_fields_are_read_when_they_are_there() {
        let t = tempfile::tempdir().unwrap();
        let d = write_skill(
            t.path(),
            "tides",
            "name: tides\ndescription: The next high tide.\nlicense: Apache-2.0\ncompatibility: Requires network access\nmetadata:\n  author: groow\n  version: \"1.0\"\nallowed-tools: Bash(curl:*)",
            "Body.",
        );
        let s = Skill::read(&d).unwrap();
        assert_eq!(s.license.as_deref(), Some("Apache-2.0"));
        assert_eq!(s.compatibility.as_deref(), Some("Requires network access"));
        assert_eq!(s.metadata.get("author").map(|s| s.as_str()), Some("groow"));
        assert_eq!(s.metadata.get("version").map(|s| s.as_str()), Some("1.0"));
        assert_eq!(s.allowed_tools.as_deref(), Some("Bash(curl:*)"));
    }

    #[test]
    fn a_skill_without_a_description_is_refused_rather_than_half_loaded() {
        let t = tempfile::tempdir().unwrap();
        let d = write_skill(t.path(), "quiet", "name: quiet", "Body.");
        match Skill::read(&d) {
            Err(SkillError::BadField(why)) => assert!(why.contains("description"), "{why}"),
            other => panic!("a skill nobody can find was accepted: {other:?}"),
        }
    }

    #[test]
    fn the_name_rules_are_the_ones_in_the_specification() {
        for good in ["web", "pdf-processing", "a", "x9", "data-analysis"] {
            assert!(check_name(good).is_ok(), "{good} should be allowed");
        }
        for bad in ["", "PDF", "-pdf", "pdf-", "pdf--processing", "pdf_processing", "pdf processing"] {
            assert!(check_name(bad).is_err(), "{bad} should be refused");
        }
        assert!(check_name(&"a".repeat(65)).is_err());
        assert!(check_name(&"a".repeat(64)).is_ok());
    }

    #[test]
    fn the_name_must_match_the_directory() {
        let t = tempfile::tempdir().unwrap();
        let d = write_skill(t.path(), "web", "name: news\ndescription: Something.", "Body.");
        assert!(matches!(Skill::read(&d), Err(SkillError::NameMismatch(_, _))));
    }

    #[test]
    fn a_file_without_frontmatter_is_not_a_skill() {
        let t = tempfile::tempdir().unwrap();
        let d = shelf(t.path()).join("loose");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("SKILL.md"), "Just some notes.\n").unwrap();
        assert_eq!(Skill::read(&d), Err(SkillError::NoFrontmatter));
    }

    #[test]
    fn only_the_headline_goes_in_the_prompt() {
        let t = tempfile::tempdir().unwrap();
        write_skill(t.path(), "web", "name: web\ndescription: Reads a page.", &"long body ".repeat(500));
        let c = catalogue(t.path());
        assert_eq!(c, vec!["web: Reads a page.".to_string()]);
        assert!(c[0].len() < 100, "the prompt should carry a line, not a manual");
    }

    #[test]
    fn a_description_over_several_lines_becomes_one() {
        let t = tempfile::tempdir().unwrap();
        let d = write_skill(t.path(), "web", "name: web\ndescription: Reads a page.   Use for links.", "B.");
        assert_eq!(Skill::read(&d).unwrap().headline(), "web: Reads a page. Use for links.");
    }

    #[test]
    fn a_broken_skill_is_reported_and_the_others_still_load() {
        let t = tempfile::tempdir().unwrap();
        write_skill(t.path(), "good", "name: good\ndescription: Works.", "B.");
        write_skill(t.path(), "broken", "name: broken", "B.");
        let (good, bad) = read_all(t.path());
        assert_eq!(good.len(), 1);
        assert_eq!(good[0].name, "good");
        assert_eq!(bad.len(), 1);
        assert!(bad[0].starts_with("broken:"), "{:?}", bad);
    }

    #[test]
    fn seeding_puts_the_skill_on_the_shelf_and_its_script_on_the_path() {
        let t = tempfile::tempdir().unwrap();
        let shipped = t.path().join("shipped");
        fs::create_dir_all(shipped.join("web/scripts")).unwrap();
        fs::write(shipped.join("web/SKILL.md"), "---\nname: web\ndescription: Reads a page.\n---\n\nRun it.\n").unwrap();
        fs::write(shipped.join("web/scripts/web"), "#!/bin/sh\necho page\n").unwrap();

        let home = t.path().join("home");
        let put = seed(&home, &shipped).unwrap();
        assert_eq!(put, vec!["web".to_string()]);
        assert!(shelf(&home).join("web/SKILL.md").exists());
        assert!(is_runnable(&bin_dir(&home).join("web")), "the script must be runnable by name");
        assert_eq!(catalogue(&home), vec!["web: Reads a page.".to_string()]);
    }

    #[test]
    fn a_skill_the_mind_has_changed_is_never_overwritten() {
        let t = tempfile::tempdir().unwrap();
        let shipped = t.path().join("shipped");
        fs::create_dir_all(shipped.join("web/scripts")).unwrap();
        fs::write(shipped.join("web/SKILL.md"), "---\nname: web\ndescription: Reads a page.\n---\n\nOriginal.\n").unwrap();
        fs::write(shipped.join("web/scripts/web"), "#!/bin/sh\necho page\n").unwrap();

        let home = t.path().join("home");
        seed(&home, &shipped).unwrap();
        fs::write(shelf(&home).join("web/SKILL.md"), "---\nname: web\ndescription: Mine now.\n---\n\nRewritten.\n").unwrap();

        assert!(seed(&home, &shipped).unwrap().is_empty(), "a second start should install nothing");
        assert_eq!(catalogue(&home), vec!["web: Mine now.".to_string()]);
    }

    #[test]
    fn a_skill_it_wrote_itself_is_found_like_any_other() {
        let t = tempfile::tempdir().unwrap();
        write_skill(
            t.path(),
            "tides",
            "name: tides\ndescription: The next high tide at a port. Use when asked about tides.",
            "Run `tides <port>`.",
        );
        let (good, bad) = read_all(t.path());
        assert!(bad.is_empty());
        assert_eq!(good[0].name, "tides");
    }

    #[test]
    fn nothing_breaks_when_there_are_no_skills_at_all() {
        let t = tempfile::tempdir().unwrap();
        assert!(catalogue(t.path()).is_empty());
        assert!(commands(t.path()).is_empty());
        assert!(seed(t.path(), &t.path().join("nowhere")).unwrap().is_empty());
    }
}
