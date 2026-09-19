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
    // These are about the body itself, or run inside it, so they never travel.
    match &cli.cmd {
        Command::Start { here, rebuild, as_user } => {
            let skel = cli.skel.clone();
            // Inside the body there is no sandbox to reach for: this is it. Without this the
            // body starts, looks for a sandbox, finds none and exits, over and over.
            return if *here || sandbox::inside_the_body() {
                start::start(home.clone(), config, skel, as_user.clone()).await
            } else {
                wake_the_sandbox(*rebuild).await
            }
        }
        Command::RunTurn => return start::run_turn().await,
        Command::RunThought { id } => return start::run_thought(id.clone()).await,
        Command::Logs => return sandbox::logs(),
        // Telling the core inside to quit would leave the body running and restart it.
        Command::Stop if sandbox::running() => {
            sandbox::stop()?;
            eprintln!("asleep. Its home is kept in ./home.");
            return Ok(());
        }
        Command::Shell => return sandbox::shell(),
        Command::Doctor => return doctor::doctor(&state, &config),
        _ => {}
    }

    // Everything else goes wherever Groow actually is. If it is awake in its sandbox, the same
    // command runs inside it; you should not have to know which, or learn a second command to
    // find out.
    if !args::is_the_mind() && sandbox::running() {
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

/// Build the body if there is not one, wake it, and wait until it answers.
async fn wake_the_sandbox(rebuild: bool) -> anyhow::Result<()> {
    let was_running = sandbox::running();
    // Always let compose decide. It leaves a container alone when nothing has changed and
    // replaces it when the image has, which is what should happen after a rebuild; skipping
    // this on the grounds that something is already running leaves the old body in place and
    // the new one on the shelf.
    sandbox::wake(rebuild)?;
    if was_running && !rebuild {
        eprintln!("already awake.");
    }
    // Waiting for the core is not enough: loading the weights takes the best part of a minute,
    // and until they are loaded it cannot answer anything. Saying it is awake before then
    // invites the first message to fail.
    eprintln!("waiting for it to wake (it has to load its weights first)…");
    for _ in 0..900 {
        if sandbox::running() && sandbox::brain_ready() {
            let _ = sandbox::run_inside(&["status".to_string()], false);
            eprintln!("\n`groow ui` opens the window. `groow stop` puts it back to sleep.");
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    anyhow::bail!("it did not wake within thirty minutes; `groow logs` shows what its body is doing")
}
