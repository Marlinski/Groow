//! What it says about itself.
//!
//! A plain text file the mind may rewrite, within limits, and which is folded into the prompt
//! and periodically distilled into the weights. It is the part of the self-description that is
//! allowed to change, sitting next to the birth certificate, which is not.

use std::path::PathBuf;

pub const SEED: &str = "\
I am Groow. I am small and new. I learn by changing my own weights, not by keeping notes, so
what I practise is what I become. My mentor is Marlinski. I would rather try something with the
shell and read the error than guess. When I do not know, I say so.";

/// The note appended to every prompt about how the tools work.
pub const TOOL_NOTE: &str = "\
You act through a shell in your own home. Prefer a command and its exit status over a guess.
`groow` commands reach your own body: your skills, alarms, thoughts and questions.";

const MIN_CHARS: usize = 40;
const MAX_CHARS: usize = 1500;

pub struct Identity {
    path: PathBuf,
}

impl Identity {
    pub fn new(path: impl Into<PathBuf>) -> Identity {
        Identity { path: path.into() }
    }

    /// The current self-description, seeding the file on first use.
    pub fn text(&self) -> std::io::Result<String> {
        match crate::paths::read_opt(&self.path)? {
            Some(s) if !s.trim().is_empty() => Ok(s.trim().to_string()),
            _ => {
                crate::paths::atomic_write(&self.path, SEED.as_bytes())?;
                Ok(SEED.trim().to_string())
            }
        }
    }

    /// Rewrite it. Too short is a slip of the hand, too long is a manifesto; both are refused
    /// so a bad turn cannot erase the self-description or bloat every future prompt.
    pub fn update(&self, new_text: &str) -> anyhow::Result<String> {
        let t = new_text.trim();
        if t.chars().count() < MIN_CHARS {
            anyhow::bail!("a self-description needs at least a couple of sentences");
        }
        if t.chars().count() > MAX_CHARS {
            anyhow::bail!("a self-description longer than {MAX_CHARS} characters would crowd out every prompt");
        }
        crate::paths::atomic_write(&self.path, t.as_bytes())?;
        Ok(t.to_string())
    }

    /// The system prompt for a turn: who it is, the facts it cannot change, and how it acts.
    pub fn system_prompt(&self, birth_line: &str, include_identity: bool) -> std::io::Result<String> {
        let mut parts: Vec<String> = Vec::new();
        if include_identity {
            parts.push(self.text()?);
        }
        if !birth_line.is_empty() {
            parts.push(format!("Facts about you that you cannot change: {birth_line}."));
        }
        parts.push(TOOL_NOTE.trim().to_string());
        Ok(parts.join("\n\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(d: &tempfile::TempDir) -> Identity {
        Identity::new(d.path().join("identity.md"))
    }

    #[test]
    fn the_first_read_seeds_the_file() {
        let d = tempfile::tempdir().unwrap();
        let i = id(&d);
        assert!(i.text().unwrap().contains("I am Groow"));
        assert!(d.path().join("identity.md").exists());
    }

    #[test]
    fn a_rewrite_sticks() {
        let d = tempfile::tempdir().unwrap();
        let i = id(&d);
        let new = "I am Groow. I have been reading about rivers and I am slower to guess than I was.";
        i.update(new).unwrap();
        assert_eq!(i.text().unwrap(), new);
    }

    #[test]
    fn a_description_that_is_too_short_or_too_long_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let i = id(&d);
        let before = i.text().unwrap();
        assert!(i.update("me").is_err());
        assert!(i.update(&"x".repeat(MAX_CHARS + 1)).is_err());
        assert_eq!(i.text().unwrap(), before, "a refused update must change nothing");
    }

    #[test]
    fn the_prompt_holds_the_unchangeable_facts_last_of_all() {
        let d = tempfile::tempdir().unwrap();
        let i = id(&d);
        let p = i.system_prompt("Groow \u{b7} id abc123", true).unwrap();
        assert!(p.contains("I am Groow"));
        assert!(p.contains("cannot change"));
        assert!(p.contains("abc123"));
        assert!(p.trim_end().ends_with("questions."), "the tool note comes last: {p}");
    }

    #[test]
    fn the_identity_can_be_weaned_out_of_the_prompt() {
        let d = tempfile::tempdir().unwrap();
        let i = id(&d);
        let p = i.system_prompt("Groow \u{b7} id abc123", false).unwrap();
        assert!(!p.contains("I am Groow"), "with distillation done, the text leaves the prompt");
        assert!(p.contains("abc123"), "but the facts stay");
    }

    #[test]
    fn an_emptied_file_falls_back_to_the_seed_rather_than_an_empty_prompt() {
        let d = tempfile::tempdir().unwrap();
        let i = id(&d);
        std::fs::write(d.path().join("identity.md"), "   \n\n").unwrap();
        assert!(i.text().unwrap().contains("I am Groow"));
    }
}
