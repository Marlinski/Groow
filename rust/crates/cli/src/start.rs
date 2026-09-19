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
pub async fn start(
    home: PathBuf,
    config: PathBuf,
    skel: Option<PathBuf>,
    as_user: Option<String>,
) -> anyhow::Result<()> {
    let state = home.join("state");
    // Before anything else, because without it there is nothing to give a home that does not
    // exist yet, and a creature born without its skills or its manual is worse than none.
    let skel = groow_core::paths::skel(skel.as_deref())?;
    let mut cfg = Config::load(&config)?;
    cfg.state_dir = state.to_string_lossy().to_string();
    cfg.home_dir = home.to_string_lossy().to_string();
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

    can_we_write(&state)?;
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
    // Make the mind's home and copy the skeleton into it, without touching anything it has
    // changed. Everything in there is its own; the state directory below is the core's.
    let home = home_for(&cfg);
    match groow_core::home::prepare(&home, &skel) {
        Ok(made) => {
            if !made.skills.is_empty() {
                eprintln!("  skills   installed {}", made.skills.join(", "));
            }
            if !made.recipes.is_empty() {
                eprintln!("  manual   installed {} pages", made.recipes.len());
            }
            if !made.files.is_empty() {
                eprintln!("  given    its own {}, from {}", made.files.join(" and "), skel.display());
            }
        }
        Err(e) => tracing::warn!("could not prepare the home: {e}"),
    }

    // Everything in the home was just made by root, so it is given to the mind before the
    // state is taken away from it. Those two together are the whole boundary.
    match groow_core::spawn::hand_home_over(&home, agent_uid) {
        Ok(true) => tracing::info!("the home belongs to the mind"),
        Ok(false) => {}
        Err(e) => tracing::warn!("could not hand the home over: {e}"),
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

/// Refuse to start on a state we cannot write, and say why.
///
/// This happens for one reason in practice: the sandbox has had this state, and the core in
/// there runs as root and takes ownership of it so that the mind cannot write its own history.
/// Running here as an ordinary user then collides with that, and the raw failure is a database
/// error several layers down that says nothing about the cause.
fn can_we_write(state: &std::path::Path) -> anyhow::Result<()> {
    if !state.exists() {
        return Ok(());
    }
    let probe = state.join(".writable");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(_) => {
            let owner = owner_of(state);
            anyhow::bail!(
                "{} belongs to {owner} and this user cannot write it.\n\
                 It has been run in the sandbox, where the core is root and takes the state so \
                 the mind cannot rewrite its own history.\n\
                 Wake it there with `make start`, or run it here as root with `sudo groow start`.",
                state.display()
            )
        }
    }
}

fn owner_of(p: &std::path::Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(m) = std::fs::metadata(p) {
            return if m.uid() == 0 { "root".to_string() } else { format!("uid {}", m.uid()) };
        }
    }
    "another user".to_string()
}

/// The mind's home. In a checkout this is the project's `home`, the same directory the
/// container mounts, so running here wakes the creature that lives there rather than a new one.
fn home_for(cfg: &Config) -> PathBuf {
    if !cfg.home_dir.is_empty() {
        return PathBuf::from(&cfg.home_dir);
    }
    groow_core::paths::default_home()
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
