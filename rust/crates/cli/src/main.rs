//! The `groow` command.


mod args;
mod doctor;
mod sandbox;
mod start;
mod show;
mod talk;

use args::{Cli, Command};
use clap::Parser;


/// The exit code for a command the mind is not allowed to run.
const NOT_YOURS: i32 = 3;

#[tokio::main]
async fn main() {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => exit_on_bad_command(e),
    };

    // The refusal comes before anything is loaded, so a command the mind may not run never
    // touches the state at all.
    if args::is_the_mind() {
        if let Some(why) = args::mentor_only(cli.cmd.name()) {
            eprintln!("`groow {}` is your mentor's, not yours: {why}.", cli.cmd.name());
            std::process::exit(NOT_YOURS);
        }
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("GROOW_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let home = cli.home.clone().unwrap_or_else(groow_core::paths::default_home);
    let config =
        cli.config.clone().unwrap_or_else(|| groow_core::paths::config_in(&home, cli.skel.as_deref()));

    let code = match run(cli, home, config).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    };
    std::process::exit(code);
}

/// Print what clap would have printed, unless we can say something more useful first.
///
/// The commonest mistake anyone makes here, the mind included, is to write `groow news` for a
/// command that is simply `news`. Skills are executables on the path, not subcommands, and
/// "unrecognized subcommand" does not say that. When the word names something runnable, say
/// what to run instead; otherwise let clap speak, because its usage message is a good one.
fn exit_on_bad_command(e: clap::Error) -> ! {
    use clap::error::{ContextKind, ContextValue, ErrorKind};
    if e.kind() == ErrorKind::InvalidSubcommand {
        if let Some(ContextValue::String(word)) = e.get(ContextKind::InvalidSubcommand) {
            if let Some(found) = on_the_path(word) {
                eprintln!(
                    "`{word}` is a command, not part of `groow`: run it on its own.\n\
                     \n    {word}\n\n\
                     It is at {}. `groow --help` lists what groow itself does.",
                    found.display()
                );
                std::process::exit(2);
            }
        }
    }
    e.exit()
}

/// Where a name would be found if it were run, following the same path a turn is given.
fn on_the_path(name: &str) -> Option<std::path::PathBuf> {
    if name.contains('/') {
        return None;
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| is_runnable(p))
}

fn is_runnable(p: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

async fn run(cli: Cli, home: std::path::PathBuf, config: std::path::PathBuf) -> anyhow::Result<()> {
    let state = home.join("state");
    // Starting is always here, in this process. Whether "here" is a terminal, a container or a
    // microvm is a runtime concern and none of this command's business: something else decides
    // where the core runs, and then runs it. In this project that something is `make start`.
    match &cli.cmd {
        Command::Start { as_user } => {
            return start::start(home.clone(), config, cli.skel.clone(), as_user.clone()).await
        }
        Command::RunTurn => return start::run_turn().await,
        Command::RunThought { id } => return start::run_thought(id.clone()).await,
        Command::Doctor => return doctor::doctor(&state, &config),
        _ => {}
    }

    // Everything else goes wherever Groow actually is. Finding it is not the same as deciding
    // where it should run: this only follows the creature to where it already is, and if it is
    // awake in the container this project ships with, the same command runs inside it. You
    // should not have to know which, or learn a second command to find out.
    if !args::is_the_mind() && sandbox::running() {
        if matches!(cli.cmd, Command::Stop) {
            eprintln!("stopping the core; its body is supervised, so it will come back. \
`make stop` puts the body to sleep.");
        }
        let inside = std::env::args().skip(1).collect::<Vec<_>>();
        let status = sandbox::run_inside(&without_paths(inside), matches!(cli.cmd, Command::Ui))?;
        std::process::exit(status.code().unwrap_or(1));
    }
    match cli.cmd {
        Command::Ui => groow_ui::app::run(state.join("core.sock")).await,
        other => talk::talk(other, state, cli.json).await,
    }
}

/// Drop the path arguments, whose values mean something different inside the body.
fn without_paths(args: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
            continue;
        }
        if a == "--home" || a == "--config" || a == "--skel" {
            skip = true;
            continue;
        }
        if a.starts_with("--home=") || a.starts_with("--config=") || a.starts_with("--skel=") {
            continue;
        }
        out.push(a);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_skill_is_found_on_the_path_and_a_subcommand_is_not() {
        // The distinction the error message rests on: `news` is an executable, `status` is
        // part of groow and is not on the path at all.
        assert!(on_the_path("sh").is_some(), "a real command should be found");
        assert!(on_the_path("definitely-not-a-command-anywhere").is_none());
        assert!(on_the_path("some/path").is_none(), "a path is not a bare name");
    }

    #[test]
    fn a_directory_is_not_something_to_run() {
        assert!(!is_runnable(std::path::Path::new("/usr/bin")));
        assert!(is_runnable(std::path::Path::new("/bin/sh")));
    }
}
