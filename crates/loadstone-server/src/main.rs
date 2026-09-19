use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use loadstone_net::{run_connection, ConnectionConfig};
use loadstone_protocol::{MINECRAFT_VERSION, PROTOCOL_VERSION};
use loadstone_world::{world, World};
use tokio::net::TcpListener;
use tracing::{error, info, warn};

#[derive(Debug, Parser)]
#[command(name = "loadstone", version, about)]
struct Args {
    /// Address to bind (host:port).
    #[arg(long, default_value = "0.0.0.0:25565")]
    bind: String,

    /// Server list MOTD.
    #[arg(long, default_value = "A LoadstoneMC server")]
    motd: String,

    /// Maximum number of players shown in the server list.
    #[arg(long, default_value_t = 20)]
    max_players: i32,

    /// Require encryption and verify accounts against Mojang's session server.
    #[arg(long, default_value_t = false)]
    online_mode: bool,

    /// Base URL used for Mojang-style session verification (`hasJoined`).
    #[arg(
        long,
        default_value = "https://sessionserver.mojang.com/session/minecraft/hasJoined"
    )]
    sessionserver_url: String,

    /// World directory holding `region/` and the seed sidecar.
    #[arg(long, default_value = "world")]
    world: PathBuf,

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
    let world_dir = args.world.clone();

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

    let net_config = ConnectionConfig {
        motd: args.motd,
        max_players: args.max_players,
        online_players: 0,
        online_mode: args.online_mode,
        sessionserver_url: args.sessionserver_url,
        world: std::sync::Arc::new(std::sync::Mutex::new(world)),
        ..Default::default()
    };
    let world_handle = net_config.world.clone();

    let listener = TcpListener::bind(&args.bind)
        .await
        .with_context(|| format!("failed to bind {}", args.bind))?;
    info!(
        version = MINECRAFT_VERSION,
        protocol = PROTOCOL_VERSION,
        online_mode = args.online_mode,
        "loadstone server listening on {}",
        args.bind
    );

    if args.save_interval > 0 {
        spawn_autosave(world_handle.clone(), world_dir.clone(), args.save_interval);
    }

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("interrupt received, saving world");
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
