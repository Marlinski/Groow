//! The `groow` command.


mod args;
mod doctor;
mod sandbox;
mod start;
mod talk;

use args::{Cli, Command};
use clap::Parser;

/// The exit code for a command the mind is not allowed to run.
const NOT_YOURS: i32 = 3;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

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
        other => talk::talk(other, state).await,
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

