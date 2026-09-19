use anyhow::Context;
use clap::Parser;
use loadstone_net::{run_connection, ConnectionConfig};
use loadstone_protocol::{MINECRAFT_VERSION, PROTOCOL_VERSION};
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
    let net_config = ConnectionConfig {
        motd: args.motd,
        max_players: args.max_players,
        online_players: 0,
        online_mode: args.online_mode,
        sessionserver_url: args.sessionserver_url,
        ..Default::default()
    };

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

    loop {
        let (stream, peer) = match listener.accept().await {
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
