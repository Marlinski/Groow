//! The `groow` command.

mod args;
mod doctor;
mod skill;
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

    let state = resolve_state(&cli);
    let config = cli.config.clone().unwrap_or_else(|| {
        let here = std::path::PathBuf::from("groow.json");
        if here.exists() { here } else { state.join("..").join("groow.json") }
    });

    let code = match run(cli, state, config).await {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    };
    std::process::exit(code);
}

async fn run(cli: Cli, state: std::path::PathBuf, config: std::path::PathBuf) -> anyhow::Result<()> {
    match cli.cmd {
        Command::Start { as_user } => start::start(state, config, as_user).await,
        Command::RunTurn => start::run_turn().await,
        Command::RunThought { id } => start::run_thought(id).await,
        Command::Ui => groow_ui::app::run(state.join("core.sock")).await,
        Command::Skill { action, name } => skill::skill(&action, name.as_deref()),
        Command::Doctor => doctor::doctor(&state, &config),
        other => talk::talk(other, state).await,
    }
}

/// Where the state is, in order of what was asked for and what exists.
fn resolve_state(cli: &Cli) -> std::path::PathBuf {
    if let Some(p) = &cli.state {
        return p.clone();
    }
    if let Some(p) = std::env::var_os("GROOW_STATE") {
        return std::path::PathBuf::from(p);
    }
    groow_ui::link::default_state()
}
