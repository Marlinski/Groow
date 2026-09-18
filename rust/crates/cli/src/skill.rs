//! `groow skill`: what you have, and how to use one.
//!
//! This is the second step of progressive disclosure. The prompt already says which skills
//! exist and when each is worth reaching for; this is how the mind pulls the rest of one into
//! its context, deliberately, when it has decided it needs it.
//!
//! It reads files and nothing else, so it works whether or not the core is running.

use std::path::PathBuf;

use groow_core::skills;

pub fn skill(action: &str, name: Option<&str>) -> anyhow::Result<()> {
    let home = home();
    match action {
        "list" | "" => list(&home),
        "read" | "show" | "cat" => read(&home, name),
        "check" => check(&home),
        other => anyhow::bail!("`groow skill {other}` is not something I know; try list, read or check"),
    }
}

fn list(home: &PathBuf) -> anyhow::Result<()> {
    let (good, bad) = skills::read_all(home);
    if good.is_empty() && bad.is_empty() {
        eprintln!("no skills yet. A skill is a directory in {} with a SKILL.md in it.",
                  skills::shelf(home).display());
        return Ok(());
    }
    for s in &good {
        println!("{}", s.headline());
    }
    if !bad.is_empty() {
        eprintln!("\nskills that could not be read:");
        for b in &bad {
            eprintln!("  {b}");
        }
    }
    eprintln!("\n`groow skill read <name>` for how and when to use one.");
    Ok(())
}

fn read(home: &PathBuf, name: Option<&str>) -> anyhow::Result<()> {
    let Some(name) = name else {
        anyhow::bail!("which skill? `groow skill list` shows them");
    };
    let dir = skills::shelf(home).join(name);
    let s = skills::Skill::read(&dir)
        .map_err(|e| anyhow::anyhow!("{name}: {e}"))?;

    println!("# {}\n\n{}\n", s.name, s.description);
    if let Some(c) = &s.compatibility {
        println!("Needs: {c}\n");
    }
    println!("{}", s.body);

    let scripts = s.scripts();
    if !scripts.is_empty() {
        let names: Vec<String> = scripts
            .iter()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(|s| s.to_string()))
            .collect();
        eprintln!("\ncommands it brings: {}", names.join(", "));
    }
    Ok(())
}

/// Check every skill on the shelf, including ones the mind wrote itself.
fn check(home: &PathBuf) -> anyhow::Result<()> {
    let (good, bad) = skills::read_all(home);
    for s in &good {
        println!("ok    {}", s.name);
    }
    for b in &bad {
        println!("bad   {b}");
    }
    if bad.is_empty() {
        eprintln!("{} skill(s), all readable", good.len());
        Ok(())
    } else {
        anyhow::bail!("{} skill(s) could not be read", bad.len())
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}
