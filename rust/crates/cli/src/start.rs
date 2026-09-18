//! Waking up, and the two processes the core starts for itself.

use std::path::PathBuf;
use std::sync::Arc;

use groow_core::brain::Brain;
use groow_core::config::Config;
use groow_core::db::Db;
use groow_core::hub::Hub;
use groow_core::paths::Paths;
use groow_core::peer;
use groow_core::server::Core;
use groow_core::spawn::Spawner;
use groow_core::store::birth::Birth;

/// Bring the core up and stay until it is asked to stop.
pub async fn start(state: PathBuf, config: PathBuf, as_user: Option<String>) -> anyhow::Result<()> {
    let mut cfg = Config::load(&config)?;
    cfg.state_dir = state.to_string_lossy().to_string();
    if let Some(u) = &as_user {
        cfg.agent_user = u.clone();
    }
    let paths = Paths::new(&state);
    paths.ensure()?;

    let agent_uid = peer::uid_of(&cfg.agent_user).filter(|u| *u != peer::current_uid());
    if !cfg.agent_user.is_empty() && agent_uid.is_none() && peer::is_root() {
        tracing::warn!("no user called `{}`; the mind will run as root", cfg.agent_user);
    }

    let birth = Birth::load_or_create(
        &paths.birth(),
        &cfg.model_id,
        &hardware(),
        "Marlinski",
        env!("CARGO_PKG_VERSION"),
    )?;

    let db = Db::open(&paths.db())?;
    let hub = Hub::new(cfg.clone(), paths.clone(), birth.clone(), db)?;
    let (handle, _hub_task) = hub.spawn();

    let spawner = Spawner {
        exe: std::env::current_exe()?,
        uid: agent_uid,
        gid: agent_uid,
        home: home_for(&cfg),
        socket: paths.socket(),
        state: state.clone(),
    };
    // Put the shipped commands in its bin directory, without touching any it has changed.
    let home = home_for(&cfg);
    match groow_harness::greeting::seed(&home) {
        Ok(true) => eprintln!("  prompt   wrote {}, which is yours to change", groow_harness::greeting::RC),
        Ok(false) => {}
        Err(e) => tracing::warn!("could not write the prompt script: {e}"),
    }
    match groow_core::skills::seed(&home, &shipped_skills()) {
        Ok(put) if !put.is_empty() => eprintln!("  skills   installed {}", put.join(", ")),
        Ok(_) => {}
        Err(e) => tracing::warn!("could not install the shipped skills: {e}"),
    }

    match groow_core::spawn::lock_state(&state, agent_uid) {
        Ok(true) => tracing::info!("the state is readable and not writable by the mind"),
        Ok(false) => {}
        Err(e) => tracing::warn!("could not set the permissions on the state: {e}"),
    }

    let core = Arc::new(Core {
        hub: handle.clone(),
        cfg: Arc::new(cfg.clone()),
        brain: Brain::new(cfg.brain_url()),
        spawner: spawner.clone(),
        socket: paths.socket(),
        agent_uid,
        python: python_for(),
        config: config.clone(),
    });

    let listening = core.clone().listen().await?;
    let scheduler = tokio::spawn(core.clone().run_scheduler());

    eprintln!("{} is awake.", birth.name);
    eprintln!("  socket   {}", paths.socket().display());
    eprintln!("  state    {}", state.display());
    eprintln!("  brain    {}", cfg.brain_url());
    eprintln!("  {}", spawner.describe());
    if !core.brain.healthy().await {
        eprintln!("  note: the brain is not answering yet, so turns will fail until it is up.");
    }
    eprintln!("Open the window with `groow ui`. Stop it with `groow stop`.");

    // Either a signal or a `stop` brings it down; both land here.
    tokio::select! {
        _ = tokio::signal::ctrl_c() => eprintln!("\nstopping"),
        _ = wait_for_stop(handle.clone()) => {}
    }
    scheduler.abort();
    listening.abort();
    let _ = std::fs::remove_file(paths.socket());
    eprintln!("asleep.");
    Ok(())
}

/// Watch for the hub shutting down, which is what `groow stop` causes.
async fn wait_for_stop(handle: groow_core::hub::Handle) {
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        if handle.status().await.is_err() {
            return;
        }
    }
}

/// One conscious turn, in this process.
pub async fn run_turn() -> anyhow::Result<()> {
    let socket = groow_harness::client::Client::socket_path();
    let mut client = groow_harness::client::Client::connect(&socket).await?;
    let settings = groow_harness::run::Settings::default();
    match groow_harness::run::run_turn(&mut client, &settings).await {
        Ok(out) => {
            tracing::info!(turn = %out.turn, seconds = out.seconds, "turn finished");
            Ok(())
        }
        Err(e) => {
            // The core learns from the connection closing; this is only for the log.
            tracing::error!("the turn ended early: {e}");
            Err(anyhow::anyhow!("{e}"))
        }
    }
}

/// One inner thought, in this process.
pub async fn run_thought(id: String) -> anyhow::Result<()> {
    std::env::set_var("GROOW_THOUGHT", &id);
    let socket = groow_harness::client::Client::socket_path();
    let mut client = groow_harness::client::Client::connect(&socket).await?;
    let settings = groow_harness::run::Settings {
        surface: groow_harness::tools::Surface::Thought,
        ..Default::default()
    };
    groow_harness::run::run_turn(&mut client, &settings)
        .await
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// The interpreter that runs the learning passes: the one in the body, or whatever is on the
/// path when this is a development checkout.
fn python_for() -> String {
    if let Ok(p) = std::env::var("GROOW_PYTHON") {
        return p;
    }
    for c in ["/opt/venv/bin/python", ".venv/bin/python"] {
        if std::path::Path::new(c).exists() {
            return c.to_string();
        }
    }
    "python3".to_string()
}

/// Where the skills that ship with the project live, next to the binary or in the checkout.
fn shipped_skills() -> PathBuf {
    if let Ok(p) = std::env::var("GROOW_SKILLS") {
        return PathBuf::from(p);
    }
    for c in ["skills", "../skills", "../../skills"] {
        let p = PathBuf::from(c);
        if p.is_dir() {
            return p;
        }
    }
    PathBuf::from("/usr/share/groow/skills")
}

fn home_for(cfg: &Config) -> PathBuf {
    if !cfg.home_dir.is_empty() {
        return PathBuf::from(&cfg.home_dir);
    }
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// What it is running on, recorded once on the birth certificate.
fn hardware() -> String {
    // Asking the driver directly avoids depending on the Python side being up.
    if let Ok(out) = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=name,memory.total", "--format=csv,noheader"])
        .output()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        if let Some(line) = text.lines().next() {
            if !line.trim().is_empty() {
                return line.trim().replace(", ", " ");
            }
        }
    }
    std::env::consts::ARCH.to_string()
}
