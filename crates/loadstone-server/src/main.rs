use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use loadstone_net::{run_connection, tick_entities, ConnectionConfig};
use loadstone_protocol::{MINECRAFT_VERSION, PROTOCOL_VERSION};
use loadstone_world::{world, EntityStore, World};
use tokio::net::TcpListener;
use tracing::{error, info, warn};

mod properties;

use properties::ServerProperties;

#[derive(Debug, Parser)]
#[command(name = "loadstone", version, about)]
struct Args {
    /// Directory holding `server.properties`, `eula.txt` and the world.
    #[arg(long, default_value = ".")]
    dir: PathBuf,

    /// Accept the Minecraft EULA, writing `eula.txt`. A server that has not
    /// accepted it refuses to start, the way vanilla does.
    #[arg(long)]
    accept_eula: bool,

    /// Address to bind (host:port). Defaults to `server-ip`:`server-port` from
    /// `server.properties`.
    #[arg(long)]
    bind: Option<String>,

    /// Server list MOTD. Defaults to the `MOTD` environment variable (the
    /// convention Pterodactyl uses), then `motd` in `server.properties`.
    #[arg(long)]
    motd: Option<String>,

    /// Maximum number of players. Defaults to `max-players`.
    #[arg(long)]
    max_players: Option<i32>,

    /// Require encryption and verify accounts against Mojang's session server.
    /// Defaults to `online-mode`. Accepts a bare flag or an explicit
    /// `--online-mode=true|false` so a process manager can pass the value from a
    /// variable.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    online_mode: Option<bool>,

    /// Base URL used for Mojang-style session verification (`hasJoined`).
    #[arg(
        long,
        default_value = "https://sessionserver.mojang.com/session/minecraft/hasJoined"
    )]
    sessionserver_url: String,

    /// World directory holding `region/` and the seed sidecar. Defaults to
    /// `level-name` under [`Args::dir`].
    #[arg(long)]
    world: Option<PathBuf>,

    /// Terrain seed. Defaults to the saved seed, or 0 for a new world.
    #[arg(long)]
    seed: Option<u64>,

    /// Seconds between automatic saves of dirty chunks (0 disables).
    #[arg(long, default_value_t = 30)]
    save_interval: u64,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("loadstone=debug,info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    let args = Args::parse();
    let dir = args.dir.clone();
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;

    // `server.properties` is the source of truth and the flags only override it
    // when they are given, which is how vanilla's own flags behave. The file is
    // written back on every start so keys a future version adds appear without
    // the operator having to know they exist.
    let props_path = dir.join("server.properties");
    let props = match ServerProperties::load(&props_path)? {
        Some(props) => props,
        None => {
            info!(path = %props_path.display(), "no server.properties yet; writing a vanilla one");
            ServerProperties::new()
        }
    };
    props
        .save(&props_path)
        .with_context(|| format!("failed to write {}", props_path.display()))?;

    let eula_path = dir.join("eula.txt");
    if args.accept_eula {
        write_eula(&eula_path)
            .with_context(|| format!("failed to write {}", eula_path.display()))?;
        info!(path = %eula_path.display(), "accepted the Minecraft EULA");
    } else if !eula_accepted(&eula_path) {
        anyhow::bail!(
            "the Minecraft EULA (https://aka.ms/MinecraftEULA) has not been accepted; \
             start once with --accept-eula, or set eula=true in {}",
            eula_path.display()
        );
    }

    let ignored = props.unhonoured_keys();
    info!(
        path = %props_path.display(),
        honoured = properties::HONOURED.len(),
        not_acted_on_yet = ignored.len(),
        "server.properties loaded"
    );

    let bind = args.bind.clone().unwrap_or_else(|| {
        let ip = props
            .optional_string("server-ip")
            .unwrap_or_else(|| "0.0.0.0".to_string());
        format!("{ip}:{}", props.integer("server-port"))
    });
    let motd = args
        .motd
        .clone()
        .or_else(|| std::env::var("MOTD").ok())
        .unwrap_or_else(|| props.string("motd"));
    let world_dir = args
        .world
        .clone()
        .unwrap_or_else(|| dir.join(props.string("level-name")));
    let max_players = args
        .max_players
        .unwrap_or_else(|| props.integer("max-players"));
    let online_mode = args
        .online_mode
        .unwrap_or_else(|| props.boolean("online-mode"));
    let gamemode = match properties::gamemode_id(&props.string("gamemode")) {
        Some(id) => id,
        None => {
            warn!(
                value = %props.string("gamemode"),
                "unknown gamemode in server.properties; using survival"
            );
            0
        }
    };
    let hardcore = props.boolean("hardcore");

    let seed = match args.seed {
        Some(seed) => seed,
        None => world::read_seed(&world_dir).unwrap_or(None).unwrap_or(0),
    };
    let mut world = World::with_seed(seed);
    if let Err(error) = world.load_from(&world_dir) {
        warn!(dir = %world_dir.display(), "failed to load world: {error}");
    }
    info!(
        dir = %world_dir.display(),
        seed,
        loaded_chunks = world.chunk_count(),
        "world ready"
    );

    let mut entities = EntityStore::new();
    entities.populate(&mut world);
    info!(mobs = entities.len(), "spawned mobs");

    let net_config = ConnectionConfig {
        motd,
        max_players,
        online_players: 0,
        online_mode,
        enable_status: props.boolean("enable-status"),
        compression_threshold: props.integer("network-compression-threshold"),
        gamemode,
        hardcore,
        sessionserver_url: args.sessionserver_url,
        world: std::sync::Arc::new(std::sync::Mutex::new(world)),
        entities: std::sync::Arc::new(std::sync::Mutex::new(entities)),
        state: Default::default(),
    };
    let world_handle = net_config.world.clone();

    let listener = TcpListener::bind(&bind)
        .await
        .with_context(|| format!("failed to bind {bind}"))?;
    info!(
        version = MINECRAFT_VERSION,
        protocol = PROTOCOL_VERSION,
        online_mode,
        motd = %net_config.motd,
        "loadstone server listening on {bind}"
    );

    if args.save_interval > 0 {
        spawn_autosave(world_handle.clone(), world_dir.clone(), args.save_interval);
    }
    spawn_entity_ticker(net_config.clone());

    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                info!("shutdown requested, saving world");
                break;
            }
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(x) => x,
                    Err(e) => {
                        warn!("accept failed: {e}");
                        continue;
                    }
                };
                let cfg = net_config.clone();
                tokio::spawn(async move {
                    if let Err(e) = run_connection(stream, cfg).await {
                        error!(%peer, "connection error: {e}");
                    }
                });
            }
        }
    }

    save_world(&world_handle, &world_dir, true);
    Ok(())
}

/// Resolves when the server should shut down: on Ctrl-C, on `SIGTERM`, or when
/// `stop`/`exit`/`quit` is typed on stdin (which is how Pterodactyl asks a
/// server to stop). Reaching end-of-file on stdin is not a shutdown signal, so
/// the server keeps running when it has no controlling terminal.
async fn shutdown_signal() {
    use tokio::io::AsyncBufReadExt as _;

    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    let stdin_stop = async {
        let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    let command = line.trim().to_ascii_lowercase();
                    if matches!(command.as_str(), "stop" | "exit" | "quit" | "shutdown") {
                        break;
                    }
                }
                // EOF or a read error means no console is attached: wait forever
                // rather than interpreting it as a stop request.
                Ok(None) | Err(_) => std::future::pending::<()>().await,
            }
        }
    };

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
        _ = stdin_stop => {}
    }
}

/// Steps the shared mob simulation every server tick (50 ms) and streams the
/// results to players. It idles while nobody is online.
fn spawn_entity_ticker(config: ConnectionConfig) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(50));
        loop {
            ticker.tick().await;
            tick_entities(&config);
        }
    });
}

/// Periodically writes dirty chunks while the server runs.
fn spawn_autosave(
    world: std::sync::Arc<std::sync::Mutex<World>>,
    dir: PathBuf,
    interval_secs: u64,
) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
        ticker.tick().await;
        loop {
            ticker.tick().await;
            save_world(&world, &dir, false);
        }
    });
}

/// Whether `eula.txt` already says `eula=true`, the test vanilla makes before it
/// will start at all.
fn eula_accepted(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .any(|line| line.trim().eq_ignore_ascii_case("eula=true"))
        })
        .unwrap_or(false)
}

/// Records the operator's acceptance on disk, so later starts need no flag.
fn write_eula(path: &Path) -> std::io::Result<()> {
    std::fs::write(
        path,
        "#By changing the setting below to TRUE you are indicating your agreement to our EULA \
         (https://aka.ms/MinecraftEULA).\neula=true\n",
    )
}

fn save_world(world: &std::sync::Arc<std::sync::Mutex<World>>, dir: &Path, force: bool) {
    let mut world = world.lock().unwrap();
    if !force && !world.is_dirty() {
        return;
    }
    match world.save(dir) {
        Ok(()) => info!(dir = %dir.display(), "saved world"),
        Err(error) => error!(dir = %dir.display(), "failed to save world: {error}"),
    }
}
