use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use loadstone_world::{EntityStore, World};
use tokio::sync::mpsc;
use uuid::Uuid;

use loadstone_protocol::packets::configuration::{
    AcknowledgeFinishConfiguration, ClientInformation, ClientboundKnownPacks,
    ConfigurationKeepAlive, ConfigurationKeepAliveResponse, ConfigurationPong, CustomPayload,
    FeatureFlags, FinishConfiguration, KnownPack, NetworkTag, RegistryData, RegistryEntry,
    ServerboundKnownPacks, TagRegistry, UpdateTags,
};
use loadstone_protocol::packets::handshake::Handshake;
use loadstone_protocol::packets::login::{
    EncryptionRequest, EncryptionResponse, LoginAcknowledged, LoginDisconnect, LoginStart,
    LoginSuccess, SetCompression,
};
use loadstone_protocol::packets::status::{
    PingRequest, ServerListStatus, StatusDescription, StatusPlayers, StatusRequest, StatusResponse,
    StatusVersion,
};
use loadstone_protocol::{Packet, PacketReader, PacketWriter, PROTOCOL_VERSION};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tracing::{debug, error, info, warn};

use crate::auth;
use crate::codec::{read_frame, write_frame, FrameError, InboundFrame};
use crate::crypto::EncryptedStream;
use crate::error::NetError;

pub const COMPRESSION_THRESHOLD: i32 = 256;

/// A player currently in the world, as seen by every other connection: their
/// identity, last known position and the channel that pushes packets to their
/// own connection.
#[derive(Debug)]
pub struct SharedPlayer {
    pub entity_id: i32,
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
    /// Mobs this player currently knows about, so the entity ticker can send
    /// `add_entity` exactly once and `remove_entities` when they leave view.
    pub visible_entities: HashSet<i32>,
    pub out: mpsc::UnboundedSender<(i32, Vec<u8>)>,
}

/// The state every connection shares: the editable block world and the set of
/// players currently in it.
#[derive(Debug, Default)]
pub struct ServerState {
    pub next_entity_id: i32,
    pub players: HashMap<Uuid, SharedPlayer>,
}

#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    pub motd: String,
    pub max_players: i32,
    pub online_players: i32,
    /// When `false`, login is offline-mode: no encryption, no session check,
    /// any provided profile is accepted.
    pub online_mode: bool,
    /// Whether server-list pings are answered at all (`enable-status`).
    pub enable_status: bool,
    /// The zlib threshold advertised in Set Compression. `-1` skips both the
    /// packet and the codec switch, so `network-compression-threshold` is the
    /// only place the decision lives.
    pub compression_threshold: i32,
    /// The game mode the login packet reports (`gamemode`); 0 is survival.
    pub gamemode: i8,
    /// Whether the client is told the world is hardcore (`hardcore`).
    pub hardcore: bool,
    /// The keypair every online-mode login uses. A server generates one at
    /// startup, the way vanilla does; `None` makes each connection generate its
    /// own, which is what tests and embedders want.
    pub server_keys: Option<Arc<auth::ServerKeys>>,
    /// Base URL for Mojang session verification (used in online mode).
    pub sessionserver_url: String,
    /// The shared block world, edited live by every player.
    pub world: Arc<Mutex<World>>,
    /// The shared mob population, simulated by the entity ticker.
    pub entities: Arc<Mutex<EntityStore>>,
    /// The shared player registry (entity ids, positions, outbound channels).
    pub state: Arc<Mutex<ServerState>>,
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        // Populate the default world so tests and an unconfigured server have
        // mobs to stream.
        let mut world = World::new();
        let mut entities = EntityStore::new();
        entities.populate(&mut world);
        Self {
            motd: "A LoadstoneMC Server".to_string(),
            max_players: 20,
            online_players: 0,
            online_mode: false,
            enable_status: true,
            compression_threshold: COMPRESSION_THRESHOLD,
            gamemode: 0,
            hardcore: false,
            server_keys: None,
            sessionserver_url: "https://sessionserver.mojang.com/session/minecraft/hasJoined"
                .to_string(),
            world: Arc::new(Mutex::new(world)),
            entities: Arc::new(Mutex::new(entities)),
            state: Arc::new(Mutex::new(ServerState::default())),
        }
    }
}

#[derive(Debug)]
pub struct RawPacket {
    pub id: i32,
    pub body: Vec<u8>,
}

/// A registered connection. The transport switches from plain to encrypted
/// mid-login, after which all frames go through AES/CFB8.
/// How many of the most recent small packets are kept per connection, and the
/// size above which a packet is too big to be the one a client complains about.
pub const RECENT_PACKETS: usize = 4;
pub const RECENT_PACKET_BYTES: usize = 128;

pub struct Connection {
    transport: Transport,
    compression_threshold: Option<i32>,
    /// The last few small clientbound packets, oldest first.
    ///
    /// A client that dies mid-session says nothing: it just closes the socket,
    /// so the reason goes with it. What it rejected is the last thing it was
    /// sent, and a client's `N bytes extra whilst reading packet X` complaint is
    /// always about a small packet whose layout is wrong, so the large ones
    /// (chunks, registries) are left out of this window.
    recent: VecDeque<(i32, Vec<u8>)>,
}

/// A connection owns exactly one of these for its whole lifetime, so the size
/// gap between the variants is never paid per element.
#[allow(clippy::large_enum_variant)]
enum Transport {
    Plain(TcpStream),
    Encrypted(EncryptedStream),
}

impl Transport {
    fn try_encrypt(self, secret: &[u8; 16]) -> Result<Self, NetError> {
        match self {
            Transport::Plain(stream) => {
                Ok(Transport::Encrypted(EncryptedStream::new(stream, secret)))
            }
            Transport::Encrypted(_) => Err(NetError::AlreadyEncrypted),
        }
    }
}

impl AsyncRead for Transport {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match &mut *self {
            Transport::Plain(stream) => Pin::new(stream).poll_read(cx, buf),
            Transport::Encrypted(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Transport {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut *self {
            Transport::Plain(stream) => Pin::new(stream).poll_write(cx, buf),
            Transport::Encrypted(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Transport::Plain(stream) => Pin::new(stream).poll_flush(cx),
            Transport::Encrypted(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Transport::Plain(stream) => Pin::new(stream).poll_shutdown(cx),
            Transport::Encrypted(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

impl Connection {
    pub fn new(stream: TcpStream) -> Self {
        Self {
            transport: Transport::Plain(stream),
            compression_threshold: None,
            recent: VecDeque::new(),
        }
    }

    /// The small packets written most recently, oldest first.
    pub fn recent_packets(&self) -> impl Iterator<Item = (i32, &[u8])> {
        self.recent.iter().map(|(id, body)| (*id, body.as_slice()))
    }

    fn set_compression(&mut self, threshold: i32) {
        self.compression_threshold = Some(threshold);
    }

    pub(crate) async fn read_packet(&mut self) -> Result<RawPacket, NetError> {
        let InboundFrame { packet_id, payload } =
            match read_frame(&mut self.transport, self.compression_threshold).await {
                Ok(frame) => frame,
                // A client that goes away closes the socket, which shows up as
                // EOF (or a reset) part-way through a read. That is an ordinary
                // disconnect, not a fault on our side, so it is reported as such
                // wherever it happens rather than as an IO error.
                Err(FrameError::Io(e)) if crate::error::is_disconnect_kind(&e) => {
                    return Err(NetError::Closed);
                }
                Err(e) => return Err(e.into()),
            };
        Ok(RawPacket {
            id: packet_id,
            body: payload,
        })
    }

    pub(crate) async fn write_packet(&mut self, id: i32, body: &[u8]) -> Result<(), NetError> {
        // Every clientbound packet is logged with the exact length written into
        // its frame, and small ones with their bytes. A client that rejects a
        // packet says how many bytes it found extra, and this is the line that
        // shows whether the frame really carried them, so a wire bug can be told
        // apart from a layout bug from the log alone.
        if body.len() <= 64 {
            debug!(id, len = body.len(), hex = %hex(body), "clientbound packet");
        } else {
            debug!(id, len = body.len(), "clientbound packet");
        }
        if body.len() <= RECENT_PACKET_BYTES {
            if self.recent.len() == RECENT_PACKETS {
                self.recent.pop_front();
            }
            self.recent.push_back((id, body.to_vec()));
        }
        write_frame(&mut self.transport, id, body, self.compression_threshold).await?;
        Ok(())
    }

    fn enable_encryption(self, secret: [u8; 16]) -> Result<Self, NetError> {
        let Connection {
            transport,
            compression_threshold,
            recent,
        } = self;
        let transport = transport.try_encrypt(&secret)?;
        Ok(Connection {
            transport,
            compression_threshold,
            recent,
        })
    }
}

fn encode_body<P: Packet>(packet: &P) -> Vec<u8> {
    let mut writer = PacketWriter::with_capacity(64);
    packet.encode(&mut writer);
    writer.into_vec()
}

pub async fn run_connection(stream: TcpStream, config: ConnectionConfig) -> Result<(), NetError> {
    let peer = stream.peer_addr().ok();
    let mut conn = Connection::new(stream);

    // `read_packet` already maps a departing client to `Closed`.
    let frame = conn.read_packet().await?;

    if frame.id != Handshake::ID {
        warn!(?peer, id = frame.id, "expected handshake packet");
        return Err(NetError::Closed);
    }
    let handshake = Handshake::decode(&mut PacketReader::new(&frame.body))?;
    debug!(
        ?peer,
        protocol = handshake.protocol_version,
        next_state = handshake.next_state,
        "handshake"
    );

    match handshake.next_state {
        1 => status_phase(&mut conn, &config).await?,
        2 => {
            login_phase(conn, &handshake, &config).await?;
            info!(?peer, "connection finished: play state ended");
        }
        other => {
            warn!(?peer, state = other, "unknown next state");
            return Err(NetError::Closed);
        }
    }

    Ok(())
}

async fn status_phase(conn: &mut Connection, config: &ConnectionConfig) -> Result<(), NetError> {
    // `enable-status=false` means the ping is not answered at all: vanilla drops
    // the connection without reading the request, so there is nothing to reply
    // to and no status to build.
    if !config.enable_status {
        debug!("status disabled by enable-status; closing the connection");
        return Ok(());
    }

    let frame = conn.read_packet().await?;
    if frame.id != StatusRequest::ID {
        return Err(NetError::Protocol(
            loadstone_protocol::ProtocolError::UnknownPacketId {
                state: "status",
                id: frame.id,
            },
        ));
    }
    let _ = StatusRequest::decode(&mut PacketReader::new(&frame.body))?;

    let status = ServerListStatus {
        version: StatusVersion {
            name: loadstone_protocol::MINECRAFT_VERSION.to_string(),
            protocol: PROTOCOL_VERSION,
        },
        players: StatusPlayers {
            max: config.max_players,
            online: config.state.lock().unwrap().players.len() as i32,
            sample: Vec::new(),
        },
        description: StatusDescription {
            text: config.motd.clone(),
        },
        favicon: None,
        enforces_secure_chat: false,
    };

    let body = encode_body(&StatusResponse {
        json: serde_json::to_string(&status)?,
    });
    conn.write_packet(StatusResponse::ID, &body).await?;

    let frame = conn.read_packet().await?;
    if frame.id != PingRequest::ID {
        return Err(NetError::Protocol(
            loadstone_protocol::ProtocolError::UnknownPacketId {
                state: "status",
                id: frame.id,
            },
        ));
    }
    let ping = PingRequest::decode(&mut PacketReader::new(&frame.body))?;
    let body = encode_body(&ping);
    conn.write_packet(PingRequest::ID, &body).await?;
    Ok(())
}

async fn login_phase(
    conn: Connection,
    handshake: &Handshake,
    config: &ConnectionConfig,
) -> Result<(), NetError> {
    let mut conn = conn;

    if handshake.protocol_version != PROTOCOL_VERSION {
        warn!(
            protocol = handshake.protocol_version,
            "incompatible client protocol version"
        );
        let reason = format!(
            "Incompatible client: server speaks {} (protocol {}), you sent {}",
            loadstone_protocol::MINECRAFT_VERSION,
            PROTOCOL_VERSION,
            handshake.protocol_version
        );
        send_disconnect(&mut conn, reason).await?;
        return Ok(());
    }

    let frame = conn.read_packet().await?;
    if frame.id != LoginStart::ID {
        return Err(NetError::Protocol(
            loadstone_protocol::ProtocolError::UnknownPacketId {
                state: "login",
                id: frame.id,
            },
        ));
    }
    let start = LoginStart::decode(&mut PacketReader::new(&frame.body))?;
    info!(name = %start.name, uuid = %start.uuid, "login attempt");

    if !config.online_mode {
        info!(name = %start.name, mode = "offline", "login complete");
        let username = start.name.clone();
        let conn = finish_login(conn, start.uuid, start.name, config).await?;
        return configuration_phase(conn, start.uuid, &username, config).await;
    }

    // Online mode: encryption + Mojang session verification. The keypair comes
    // from the server when it has one, so a login does not pay for a 1024-bit key
    // generation, and only falls back to making one here without it.
    let keys = match &config.server_keys {
        Some(keys) => keys.clone(),
        None => Arc::new(auth::generate_keys().map_err(NetError::Auth)?),
    };
    // Vanilla uses a four-byte challenge (`Ints.toByteArray(random.nextInt())`).
    let verify_token = auth::random_bytes::<4>();

    let body = encode_body(&EncryptionRequest {
        server_id: String::new(),
        public_key: keys.public_der.clone(),
        verify_token: verify_token.to_vec(),
        should_authenticate: true,
    });
    conn.write_packet(EncryptionRequest::ID, &body).await?;

    let frame = conn.read_packet().await?;
    if frame.id != EncryptionResponse::ID {
        return Err(NetError::Protocol(
            loadstone_protocol::ProtocolError::UnknownPacketId {
                state: "login:encryption",
                id: frame.id,
            },
        ));
    }
    let response = EncryptionResponse::decode(&mut PacketReader::new(&frame.body))?;
    debug!("received encryption response, decrypting shared secret");
    let secret = auth::decrypt_with_private_key(&keys.private, &response.shared_secret)
        .map_err(NetError::Auth)?;
    if secret.len() != 16 {
        return Err(NetError::InvalidSharedSecret(secret.len()));
    }
    let mut secret_bytes = [0u8; 16];
    secret_bytes.copy_from_slice(&secret);

    // Protocol 774's Encryption Response carries only the shared secret and the token
    // echo, so the echo is the whole check: profile-key signatures were dropped from
    // login in 1.19.3. A Mojang-signed key chain now arrives in the Play state as a
    // serverbound `chat_session_update`, which is where that verification belongs.
    //
    // The echo arrives RSA-encrypted with the key from the request, exactly like the
    // shared secret, so it has to be decrypted before it can be compared. Comparing
    // the ciphertext with the plaintext token rejects every real client, because no
    // client ever sends the token back in the clear; a padding failure counts as a
    // mismatch rather than an error, so a garbled echo gets the same refusal.
    let token_echoed = match auth::decrypt_with_private_key(&keys.private, &response.verify_token) {
        Ok(echoed) => echoed == verify_token,
        Err(error) => {
            debug!(%error, "the verify token echo did not decrypt");
            false
        }
    };

    conn = conn.enable_encryption(secret_bytes)?;
    debug!("AES/CFB8 encryption enabled on the connection");

    // Encryption is on before replying, because the client switched to ciphertext the
    // moment it sent the response and could not read a plaintext packet.
    if !token_echoed {
        warn!(
            name = %start.name,
            sent = verify_token.len(),
            echoed = response.verify_token.len(),
            "encryption response did not echo the verify token"
        );
        send_disconnect(&mut conn, "Invalid verify token!".to_string()).await?;
        return Err(NetError::InvalidVerifyToken);
    }

    // Prove to Mojang that the client holds its session key.
    let server_id = auth::compute_server_id(&secret_bytes, &keys.public_der);
    let profile = match auth::check_session(&config.sessionserver_url, &start.name, &server_id)
        .await
    {
        auth::SessionOutcome::Verified(profile) => profile,
        // Mojang was asked and said no. This is the exact component vanilla sends,
        // captured from a real 1.21.11 server, so a client shows its own language's
        // wording instead of a hard-coded English sentence.
        auth::SessionOutcome::Rejected => {
            warn!(name = %start.name, "failed to verify username: no session joined with this key exchange");
            send_disconnect_component(
                &mut conn,
                r#"{"translate":"multiplayer.disconnect.unverified_username"}"#.to_string(),
            )
            .await?;
            return Ok(());
        }
        // The question never reached Mojang, so nothing was proven. Saying
        // "failed to verify username" here would blame the player for a network
        // problem, so the console gets the reason and the client is told to try
        // again rather than told it is not who it says it is.
        auth::SessionOutcome::Unavailable(reason) => {
            error!(name = %start.name, %reason, "could not check the session server");
            send_disconnect(
                &mut conn,
                "Failed to verify username! The session server could not be reached.".to_string(),
            )
            .await?;
            return Ok(());
        }
    };
    info!(name = %profile.name, uuid = %profile.uuid, mode = "online", "login complete");

    let username = profile.name.clone();
    let conn = finish_login(conn, profile.uuid, profile.name, config).await?;
    configuration_phase(conn, profile.uuid, &username, config).await
}

async fn finish_login(
    mut conn: Connection,
    uuid: uuid::Uuid,
    username: String,
    config: &ConnectionConfig,
) -> Result<Connection, NetError> {
    // Enable zlib compression before Login Success. A negative threshold is
    // vanilla's "off": the packet is not sent and frames stay uncompressed.
    if config.compression_threshold >= 0 {
        let body = encode_body(&SetCompression {
            threshold: config.compression_threshold,
        });
        conn.write_packet(SetCompression::ID, &body).await?;
        conn.set_compression(config.compression_threshold);
    } else {
        debug!("network-compression-threshold is negative; not compressing");
    }

    let body = encode_body(&LoginSuccess {
        uuid,
        username: username.clone(),
        properties: Vec::new(),
    });
    conn.write_packet(LoginSuccess::ID, &body).await?;

    // The client acknowledges and moves to the Configuration state.
    let frame = conn.read_packet().await?;
    if frame.id != LoginAcknowledged::ID {
        return Err(NetError::Protocol(
            loadstone_protocol::ProtocolError::UnknownPacketId {
                state: "login:acknowledged",
                id: frame.id,
            },
        ));
    }
    info!(name = %username, "client acknowledged login; entering configuration");
    Ok(conn)
}

/// Run the Configuration state through to the Play boundary.
///
/// The server negotiates `minecraft:core` with the client and then sends every
/// synchronized registry with its NBT omitted, so the client resolves entry
/// data from its own data pack. Network tags are always sent in full.
async fn configuration_phase(
    mut conn: Connection,
    uuid: uuid::Uuid,
    username: &str,
    config: &ConnectionConfig,
) -> Result<(), NetError> {
    let synced = loadstone_registry::synced_registries();

    // Vanilla opens the state with its brand and feature flags.
    let body = encode_body(&CustomPayload {
        channel: "minecraft:brand".to_string(),
        data: encode_brand("LoadstoneMC"),
    });
    conn.write_packet(CustomPayload::ID, &body).await?;

    let body = encode_body(&FeatureFlags {
        features: vec!["minecraft:vanilla".to_string()],
    });
    conn.write_packet(FeatureFlags::ID, &body).await?;

    // Offer the data packs we can source entry data from.
    let body = encode_body(&ClientboundKnownPacks {
        packs: vec![KnownPack {
            namespace: synced.known_pack.namespace.clone(),
            id: synced.known_pack.id.clone(),
            version: synced.known_pack.version.clone(),
        }],
    });
    conn.write_packet(ClientboundKnownPacks::ID, &body).await?;

    // The client replies with its client information and the packs it knows.
    let mut have_info = false;
    let mut client_packs = Vec::new();
    while !have_info || client_packs.is_empty() {
        let frame = conn.read_packet().await?;
        match frame.id {
            ClientInformation::ID => {
                let info = ClientInformation::decode(&mut PacketReader::new(&frame.body))?;
                debug!(
                    locale = %info.locale,
                    view_distance = info.view_distance,
                    "client information"
                );
                have_info = true;
            }
            ServerboundKnownPacks::ID => {
                let packs = ServerboundKnownPacks::decode(&mut PacketReader::new(&frame.body))?;
                client_packs = packs.packs;
            }
            ConfigurationKeepAlive::ID => {
                let alive =
                    ConfigurationKeepAliveResponse::decode(&mut PacketReader::new(&frame.body))?;
                let body = encode_body(&ConfigurationKeepAlive { id: alive.id });
                conn.write_packet(ConfigurationKeepAlive::ID, &body).await?;
            }
            ConfigurationPong::ID => {}
            other => warn!(id = other, "ignoring unexpected configuration packet"),
        }
    }

    let knows_core = client_packs.iter().any(|pack| {
        pack.namespace == synced.known_pack.namespace
            && pack.id == synced.known_pack.id
            && pack.version == synced.known_pack.version
    });
    if !knows_core {
        // Without mutual `minecraft:core` the client would have to receive full
        // registry NBT, which we do not ship. Vanilla clients always know it.
        warn!(
            name = %username,
            "client does not know minecraft:core; registry entry data cannot be resolved"
        );
    }

    for registry in &synced.registries {
        let packet = RegistryData {
            registry_id: registry.id.clone(),
            entries: registry
                .entries
                .iter()
                .map(|name| RegistryEntry {
                    name: name.clone(),
                    data: None,
                })
                .collect(),
        };
        let body = encode_body(&packet);
        conn.write_packet(RegistryData::ID, &body).await?;
    }
    debug!(
        count = synced.registries.len(),
        "sent synchronized registries"
    );

    let tags = loadstone_registry::network_tags();
    let packet = UpdateTags {
        registries: tags
            .registries
            .iter()
            .map(|registry| TagRegistry {
                registry: registry.registry.clone(),
                tags: registry
                    .tags
                    .iter()
                    .map(|tag| NetworkTag {
                        name: tag.name.clone(),
                        entries: tag.entries.clone(),
                    })
                    .collect(),
            })
            .collect(),
    };
    let body = encode_body(&packet);
    conn.write_packet(UpdateTags::ID, &body).await?;

    let body = encode_body(&FinishConfiguration);
    conn.write_packet(FinishConfiguration::ID, &body).await?;

    loop {
        let frame = conn.read_packet().await?;
        match frame.id {
            AcknowledgeFinishConfiguration::ID => break,
            ConfigurationKeepAlive::ID => {
                let alive =
                    ConfigurationKeepAliveResponse::decode(&mut PacketReader::new(&frame.body))?;
                let body = encode_body(&ConfigurationKeepAlive { id: alive.id });
                conn.write_packet(ConfigurationKeepAlive::ID, &body).await?;
            }
            ConfigurationPong::ID => {}
            other => warn!(id = other, "ignoring unexpected configuration packet"),
        }
    }

    debug!(name = %username, "configuration complete");

    debug!(name = %username, "entering play state");
    crate::play::play_phase(conn, uuid, username, config).await
}

/// The `minecraft:brand` payload is a single length-prefixed string.
/// Lower-case hex, for packet traces.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 3);
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn encode_brand(brand: &str) -> Vec<u8> {
    let mut writer = PacketWriter::new();
    writer.write_string(brand);
    writer.into_vec()
}

async fn send_disconnect(conn: &mut Connection, message: String) -> Result<(), NetError> {
    let reason_json = serde_json::to_string(&MessageText { text: message })?;
    send_disconnect_component(conn, reason_json).await
}

/// Disconnect with a reason that is already a JSON text component.
///
/// The login reason field is a component, so a translatable one has to go out
/// untouched: wrapping vanilla's `{"translate":...}` in `{"text":...}` would hand
/// the client the JSON as the sentence to display.
async fn send_disconnect_component(
    conn: &mut Connection,
    reason_json: String,
) -> Result<(), NetError> {
    let body = encode_body(&LoginDisconnect {
        reason: reason_json,
    });
    conn.write_packet(LoginDisconnect::ID, &body).await?;
    Ok(())
}

#[derive(serde::Serialize)]
struct MessageText {
    text: String,
}
