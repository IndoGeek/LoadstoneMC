//! End-to-end login tests: a simulated vanilla client handshakes, logs in
//! (offline and online modes), negotiates AES/CFB8 encryption and zlib
//! compression, verifies the server's Login Success, and drives the whole
//! Configuration state to its acknowledgement.
//!
//! Online mode uses a local mock "Mojang session server" so no real accounts
//! or internet are required.

use std::io::Write as _;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use loadstone_net::auth::compute_server_id;
use loadstone_net::{run_connection, Cfb8, ConnectionConfig};
use loadstone_protocol::packets::configuration::{
    AcknowledgeFinishConfiguration, ClientInformation, ClientboundKnownPacks, CustomPayload,
    FeatureFlags, FinishConfiguration, RegistryData, ServerboundKnownPacks, UpdateTags,
};
use loadstone_protocol::packets::login::{
    EncryptionRequest, EncryptionResponse, LoginAcknowledged, LoginStart, LoginSuccess,
    SetCompression,
};
use loadstone_protocol::packets::play::{
    pack_position, Abilities, BlockChange, BlockDig, BlockPlace, BundleDelimiter,
    ChunkBatchFinished, ChunkBatchReceived, ChunkBatchStart, ClientPosition, EntityMetadata,
    Experience, MapChunk, PlayKeepAlive, PlayKeepAliveResponse, PlayLogin, PlayerInfoUpdate,
    PlayerPosition, PlayerRemove, RemoveEntities, ServerData, SetChunkCacheCenter, SpawnEntity,
    SpawnPosition, SyncEntityPosition, SystemChat, TeleportConfirm, UnloadChunk, UpdateHealth,
    UpdateTime, ENTITY_TYPE_PLAYER,
};
use loadstone_protocol::packets::Handshake;
use loadstone_protocol::write_varint;
use loadstone_protocol::{Packet, PacketReader, PROTOCOL_VERSION};
use loadstone_world::terrain::TerrainGenerator;
use rsa::pkcs8::DecodePublicKey;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use uuid::Uuid;

const MOCK_PLAYER_ID: &str = "0f9b0e00-0000-4000-8000-000000000000";

/// A vanilla-behaving test client with optional streaming encryption.
struct Client {
    stream: TcpStream,
    out: Option<Cfb8>,
    inp: Option<Cfb8>,
    compression: Option<i32>,
}

impl Client {
    async fn connect(addr: &str) -> Self {
        let stream = TcpStream::connect(addr).await.unwrap();
        Self {
            stream,
            out: None,
            inp: None,
            compression: None,
        }
    }

    fn enable_encryption(&mut self, secret: [u8; 16]) {
        self.out = Some(Cfb8::new(&secret, &secret, true));
        self.inp = Some(Cfb8::new(&secret, &secret, false));
    }

    async fn read_byte_decrypted(&mut self) -> Option<u8> {
        let raw = self.stream.read_u8().await.ok()?;
        let byte = match &mut self.inp {
            Some(cipher) => {
                let mut b = [raw];
                cipher.transform(&mut b);
                b[0]
            }
            None => raw,
        };
        Some(byte)
    }

    async fn read_varint(&mut self) -> u32 {
        let mut value = 0u32;
        let mut position = 0u32;
        loop {
            let byte = self.read_byte_decrypted().await.expect("eof in varint");
            value |= ((byte & 0x7F) as u32) << position;
            if byte & 0x80 == 0 {
                return value;
            }
            position += 7;
        }
    }

    async fn recv_packet(&mut self) -> (i32, Vec<u8>) {
        let len = self.read_varint().await as usize;
        let mut raw = vec![0u8; len];
        self.stream.read_exact(&mut raw).await.unwrap();
        if let Some(cipher) = &mut self.inp {
            cipher.transform(&mut raw);
        }

        let mut data = match self.compression {
            None => raw,
            Some(_) => {
                let (data_len, n) = read_varint(&raw);
                let rest = raw[n..].to_vec();
                if data_len == 0 {
                    rest
                } else {
                    let mut out = Vec::with_capacity(data_len as usize);
                    flate2::write::ZlibDecoder::new(&mut out)
                        .write_all(&rest)
                        .unwrap();
                    out
                }
            }
        };

        let (id, n) = read_varint(&data);
        data.drain(..n);
        (id as i32, data)
    }

    async fn send_packet(&mut self, id: i32, body: &[u8]) {
        let mut data = Vec::with_capacity(body.len() + 5);
        write_varint(&mut data, id);
        data.extend_from_slice(body);

        let mut frame = Vec::with_capacity(data.len() + 8);
        match self.compression {
            None => {
                write_varint(&mut frame, data.len() as i32);
                frame.extend_from_slice(&data);
            }
            Some(threshold) => {
                let mut inner = Vec::with_capacity(data.len() + 8);
                if data.len() >= threshold as usize {
                    let mut compressed = Vec::new();
                    flate2::write::ZlibEncoder::new(
                        &mut compressed,
                        flate2::Compression::default(),
                    )
                    .write_all(&data)
                    .unwrap();
                    write_varint(&mut inner, data.len() as i32);
                    inner.extend_from_slice(&compressed);
                } else {
                    write_varint(&mut inner, 0);
                    inner.extend_from_slice(&data);
                }
                write_varint(&mut frame, inner.len() as i32);
                frame.extend_from_slice(&inner);
            }
        }

        if let Some(cipher) = &mut self.out {
            cipher.transform(&mut frame);
        }
        self.stream.write_all(&frame).await.unwrap();
        self.stream.flush().await.unwrap();
    }

    async fn handshake(&mut self, next_state: i32) {
        let handshake = Handshake {
            protocol_version: PROTOCOL_VERSION,
            server_address: "localhost".to_string(),
            server_port: 25565,
            next_state,
        };
        let mut body = loadstone_protocol::PacketWriter::new();
        handshake.encode(&mut body);
        self.send_packet(Handshake::ID, body.as_slice()).await;
    }

    async fn login_start(&mut self, name: &str) -> Uuid {
        let uuid = Uuid::new_v4();
        let start = LoginStart {
            name: name.to_string(),
            uuid,
        };
        let mut body = loadstone_protocol::PacketWriter::new();
        start.encode(&mut body);
        self.send_packet(LoginStart::ID, body.as_slice()).await;
        uuid
    }

    async fn acknowledged(&mut self) {
        self.send_packet(LoginAcknowledged::ID, &[]).await;
    }

    async fn recv_timed(&mut self) -> (i32, Vec<u8>) {
        tokio::time::timeout(Duration::from_secs(5), self.recv_packet())
            .await
            .expect("timeout waiting for a packet")
    }

    /// Reads the next meaningful Play packet, answering keep-alives and
    /// skipping the metadata/delimiter noise that surrounds entity spawns.
    async fn next_event(&mut self) -> (i32, Vec<u8>) {
        loop {
            let (id, body) = self.recv_timed().await;
            if id == PlayKeepAlive::ID {
                let alive = PlayKeepAlive::decode(&mut PacketReader::new(&body)).unwrap();
                let mut writer = loadstone_protocol::PacketWriter::new();
                PlayKeepAliveResponse {
                    keep_alive_id: alive.keep_alive_id,
                }
                .encode(&mut writer);
                self.send_packet(PlayKeepAliveResponse::ID, writer.as_slice())
                    .await;
                continue;
            }
            if id == EntityMetadata::ID || id == BundleDelimiter::ID {
                continue;
            }
            return (id, body);
        }
    }

    /// Reads until the given player's spawn appears (through the tab-list
    /// bundle) and returns their entity id.
    async fn await_player_spawn(&mut self, expected_uuid: Uuid) -> i32 {
        loop {
            let (id, body) = self.next_event().await;
            if id == PlayerInfoUpdate::ID {
                let info = PlayerInfoUpdate::decode(&mut PacketReader::new(&body)).unwrap();
                if info.entries.iter().any(|entry| entry.uuid == expected_uuid) {
                    continue;
                }
            } else if id == SpawnEntity::ID {
                let spawn = SpawnEntity::decode(&mut PacketReader::new(&body)).unwrap();
                if spawn.uuid == expected_uuid {
                    assert_eq!(spawn.entity_type, ENTITY_TYPE_PLAYER);
                    return spawn.entity_id;
                }
            } else {
                panic!("unexpected play packet {id:#x} while awaiting a spawn");
            }
        }
    }

    /// Drive the vanilla Configuration handshake and return the registries the
    /// server sent, as `(registry id, entry count)` in packet order.
    async fn complete_configuration(&mut self) -> Vec<(String, usize)> {
        // The server opens with its brand and feature flags, then offers known packs.
        let packs = loop {
            let (id, body) = self.recv_timed().await;
            if id == CustomPayload::ID || id == FeatureFlags::ID {
                continue;
            }
            assert_eq!(id, ClientboundKnownPacks::ID, "unexpected config packet");
            break ClientboundKnownPacks::decode(&mut PacketReader::new(&body))
                .unwrap()
                .packs;
        };
        assert_eq!(packs.len(), 1);
        assert_eq!(packs[0].id, "core");
        assert_eq!(packs[0].version, "1.21.11");

        // Echo the offered packs so the server can omit registry NBT.
        let mut body = loadstone_protocol::PacketWriter::new();
        ServerboundKnownPacks { packs }.encode(&mut body);
        self.send_packet(ServerboundKnownPacks::ID, body.as_slice())
            .await;

        let info = ClientInformation {
            locale: "en_us".to_string(),
            view_distance: 12,
            chat_flags: 0,
            chat_colors: true,
            skin_parts: 0x7f,
            main_hand: 1,
            text_filtering: false,
            server_listing: true,
            particle_status: 0,
        };
        let mut body = loadstone_protocol::PacketWriter::new();
        info.encode(&mut body);
        self.send_packet(ClientInformation::ID, body.as_slice())
            .await;

        let mut registries = Vec::new();
        loop {
            let (id, body) = self.recv_timed().await;
            if id == RegistryData::ID {
                let packet = RegistryData::decode(&mut PacketReader::new(&body)).unwrap();
                assert!(
                    packet.entries.iter().all(|entry| entry.data.is_none()),
                    "registry NBT must be omitted when core is known"
                );
                registries.push((packet.registry_id, packet.entries.len()));
            } else if id == UpdateTags::ID {
                let tags = UpdateTags::decode(&mut PacketReader::new(&body)).unwrap();
                assert!(!tags.registries.is_empty());
            } else if id == FinishConfiguration::ID {
                self.send_packet(AcknowledgeFinishConfiguration::ID, &[])
                    .await;
                break;
            } else {
                panic!("unexpected configuration packet {id:#x}");
            }
        }
        registries
    }

    /// Drive the Play state to completion: consume the login and spawn sequence,
    /// acknowledge the chunk batch, and echo the post-join packets back to the
    /// caller in a snapshot for assertions.
    async fn enter_play(&mut self, expected_uuid: Uuid) -> PlaySnapshot {
        let mut login: Option<PlayLogin> = None;
        let mut spawn = None;
        let mut chunks = Vec::new();
        let mut batch = None;
        let mut position = None;
        let mut player_info = None;
        let mut health = None;
        let mut welcome = None;
        let mut spawns = Vec::new();
        let mut ack_sent = false;

        loop {
            if ack_sent
                && login.is_some()
                && spawn.is_some()
                && chunks.len() >= 9
                && batch.is_some()
                && position.is_some()
                && player_info.is_some()
                && health.is_some()
                && welcome.is_some()
            {
                break;
            }

            let (id, body) = self.recv_timed().await;
            match id {
                id if id == PlayLogin::ID => {
                    login = Some(PlayLogin::decode(&mut PacketReader::new(&body)).unwrap());
                }
                id if id == SpawnPosition::ID => {
                    spawn = Some(SpawnPosition::decode(&mut PacketReader::new(&body)).unwrap());
                }
                id if id == ChunkBatchStart::ID => {}
                id if id == SetChunkCacheCenter::ID => {}
                id if id == MapChunk::ID => {
                    chunks.push(MapChunk::decode(&mut PacketReader::new(&body)).unwrap());
                }
                id if id == ChunkBatchFinished::ID => {
                    batch =
                        Some(ChunkBatchFinished::decode(&mut PacketReader::new(&body)).unwrap());
                }
                id if id == ClientPosition::ID => {
                    position = Some(ClientPosition::decode(&mut PacketReader::new(&body)).unwrap());
                    self.send_packet(TeleportConfirm::ID, &[0x00]).await;
                }
                id if id == PlayerInfoUpdate::ID => {
                    let info = PlayerInfoUpdate::decode(&mut PacketReader::new(&body)).unwrap();
                    // The first tab entry is always the joining player; later
                    // entries describe players who were already here.
                    if player_info.is_none() {
                        assert_eq!(info.entries.len(), 1);
                        assert_eq!(info.entries[0].uuid, expected_uuid);
                        player_info = Some(info);
                    }
                }
                id if id == SpawnEntity::ID => {
                    spawns.push(SpawnEntity::decode(&mut PacketReader::new(&body)).unwrap());
                }
                id if id == EntityMetadata::ID || id == BundleDelimiter::ID => {}
                id if id == Abilities::ID => {}
                id if id == UpdateHealth::ID => {
                    health = Some(UpdateHealth::decode(&mut PacketReader::new(&body)).unwrap());
                }
                id if id == Experience::ID => {}
                id if id == UpdateTime::ID => {}
                id if id == ServerData::ID => {}
                id if id == SystemChat::ID => {
                    welcome = Some(SystemChat::decode(&mut PacketReader::new(&body)).unwrap());
                }
                id if id == PlayKeepAlive::ID => {
                    let alive = PlayKeepAlive::decode(&mut PacketReader::new(&body)).unwrap();
                    let mut writer = loadstone_protocol::PacketWriter::new();
                    PlayKeepAliveResponse {
                        keep_alive_id: alive.keep_alive_id,
                    }
                    .encode(&mut writer);
                    self.send_packet(PlayKeepAliveResponse::ID, writer.as_slice())
                        .await;
                }
                other => panic!("unexpected play packet {other:#x}"),
            }

            if !ack_sent && chunks.len() >= 9 {
                let mut writer = loadstone_protocol::PacketWriter::new();
                ChunkBatchReceived {
                    chunks_per_tick: 10.0,
                }
                .encode(&mut writer);
                self.send_packet(ChunkBatchReceived::ID, writer.as_slice())
                    .await;
                ack_sent = true;
            }
        }

        PlaySnapshot {
            login: login.unwrap(),
            spawn: spawn.unwrap(),
            chunks,
            batch: batch.unwrap(),
            position: position.unwrap(),
            health: health.unwrap(),
            welcome: welcome.unwrap(),
            spawns,
        }
    }
    /// Breaks and places single blocks around the spawn, asserting the server
    /// replies with the matching S->C BlockChange packets. Bedrock edits must
    /// receive no answer at all.
    async fn mutate_world(&mut self, surface_y: i32) {
        // Break the grass block just below the spawn point.
        let mut writer = loadstone_protocol::PacketWriter::new();
        BlockDig {
            status: 2,
            location: pack_position(8, surface_y, 8),
            face: 1,
            sequence: 1,
        }
        .encode(&mut writer);
        self.send_packet(BlockDig::ID, writer.as_slice()).await;

        let (id, body) = self.recv_timed().await;
        assert_eq!(id, BlockChange::ID);
        let change = BlockChange::decode(&mut PacketReader::new(&body)).unwrap();
        assert_eq!(change.location, pack_position(8, surface_y, 8));
        assert_eq!(change.block_state, 0, "dig should clear the block to air");

        // Place a block on the top face of the (now air) spot.
        let mut writer = loadstone_protocol::PacketWriter::new();
        BlockPlace {
            hand: 0,
            location: pack_position(8, surface_y, 8),
            direction: 1,
            cursor_x: 0.5,
            cursor_y: 1.0,
            cursor_z: 0.5,
            inside_block: false,
            world_border_hit: false,
            sequence: 2,
        }
        .encode(&mut writer);
        self.send_packet(BlockPlace::ID, writer.as_slice()).await;

        let (id, body) = self.recv_timed().await;
        assert_eq!(id, BlockChange::ID);
        let change = BlockChange::decode(&mut PacketReader::new(&body)).unwrap();
        assert_eq!(change.location, pack_position(8, surface_y + 1, 8));
        assert_eq!(change.block_state, 14, "place should set cobblestone");

        // Bedrock is unbreakable: expect radio silence from the server.
        let mut writer = loadstone_protocol::PacketWriter::new();
        BlockDig {
            status: 2,
            location: pack_position(0, -64, 0),
            face: 0,
            sequence: 3,
        }
        .encode(&mut writer);
        self.send_packet(BlockDig::ID, writer.as_slice()).await;

        let quiet = tokio::time::timeout(Duration::from_millis(700), self.recv_packet()).await;
        match quiet {
            Err(_) => {}
            Ok((id, _)) => panic!("expected silence after digging bedrock, got packet {id:#x}"),
        }
    }

    /// Sends a movement update with the on-ground flag set.
    async fn send_position(&mut self, x: f64, y: f64, z: f64) {
        let mut writer = loadstone_protocol::PacketWriter::new();
        PlayerPosition { x, y, z, flags: 1 }.encode(&mut writer);
        self.send_packet(PlayerPosition::ID, writer.as_slice())
            .await;
    }

    /// Sends a finished block dig at the given coordinate.
    async fn dig_block(&mut self, x: i32, y: i32, z: i32, sequence: i32) {
        let mut writer = loadstone_protocol::PacketWriter::new();
        BlockDig {
            status: 2,
            location: pack_position(x, y, z),
            face: 1,
            sequence,
        }
        .encode(&mut writer);
        self.send_packet(BlockDig::ID, writer.as_slice()).await;
    }
}

/// Everything the server sent while the client entered the world.
struct PlaySnapshot {
    login: PlayLogin,
    spawn: SpawnPosition,
    chunks: Vec<MapChunk>,
    batch: ChunkBatchFinished,
    position: ClientPosition,
    health: UpdateHealth,
    welcome: SystemChat,
    spawns: Vec<SpawnEntity>,
}

fn read_varint(bytes: &[u8]) -> (u32, usize) {
    let mut value = 0u32;
    let mut position = 0u32;
    for (i, byte) in bytes.iter().enumerate() {
        value |= ((byte & 0x7F) as u32) << position;
        if byte & 0x80 == 0 {
            return (value, i + 1);
        }
        position += 7;
    }
    (value, bytes.len())
}

async fn spawn_server(config: ConnectionConfig) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let handle = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let _result = run_connection(stream, config).await;
    });
    (addr, handle)
}

/// Accepts any number of connections, each sharing the same server state. The
/// returned handle runs until aborted.
async fn spawn_server_multi(config: ConnectionConfig) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let handle = tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(x) => x,
                Err(_) => break,
            };
            let cfg = config.clone();
            tokio::spawn(async move {
                let _result = run_connection(stream, cfg).await;
            });
        }
    });
    (addr, handle)
}

/// A minimal HTTP server that answers `GET /hasJoined?username=X&serverId=Y`,
/// recording requests so the test can compare `serverId` values.
async fn spawn_mock_sessionserver(
    recorded: Arc<Mutex<Vec<String>>>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let handle = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buf = [0u8; 512];
        while !request.windows(4).any(|w| w == b"\r\n\r\n") && request.len() < 8192 {
            let n = stream.read(&mut buf).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            request.extend_from_slice(&buf[..n]);
        }
        let head = String::from_utf8_lossy(&request);
        let path = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("");
        recorded.lock().unwrap().push(path.to_string());

        // The query is `?username=X&serverId=Y`, so the value ends at `&`.
        let query = path.split('?').nth(1).unwrap_or("");
        let mut username = None;
        for pair in query.split('&') {
            if let Some(value) = pair.strip_prefix("username=") {
                username = Some(value.to_string());
            }
        }
        let body = match username.as_deref() {
            Some("MockPlayer") => {
                format!(r#"{{"id":"{MOCK_PLAYER_ID}","name":"MockPlayer","properties":[]}}"#)
            }
            _ => String::new(),
        };
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
    });
    (addr, handle)
}

fn base_config(online_mode: bool) -> ConnectionConfig {
    ConnectionConfig {
        online_mode,
        ..Default::default()
    }
}

/// The generated surface block Y at the default spawn (seed 0, block 8,8).
fn spawn_surface_y() -> i32 {
    TerrainGenerator::new(0).surface_height(8, 8)
}

/// Drives one offline client through login, configuration and into Play.
async fn offline_join(addr: &str, name: &str) -> (Client, Uuid, PlaySnapshot) {
    let mut client = Client::connect(addr).await;
    client.handshake(2).await;
    let uuid = client.login_start(name).await;

    let (id, body) = tokio::time::timeout(Duration::from_secs(5), client.recv_packet())
        .await
        .expect("timeout waiting for set compression");
    assert_eq!(id, SetCompression::ID);
    let threshold = SetCompression::decode(&mut PacketReader::new(&body)).unwrap();
    client.compression = Some(threshold.threshold);

    let (id, body) = tokio::time::timeout(Duration::from_secs(5), client.recv_packet())
        .await
        .expect("timeout waiting for login success");
    assert_eq!(id, LoginSuccess::ID);
    let success = LoginSuccess::decode(&mut PacketReader::new(&body)).unwrap();
    assert_eq!(success.uuid, uuid);

    client.acknowledged().await;
    client.complete_configuration().await;
    let snapshot = client.enter_play(uuid).await;
    (client, uuid, snapshot)
}

/// Answers the server's encryption request the way a vanilla client does: wrap a
/// fresh session key with its RSA public key, echo the verify token, then switch
/// the whole connection to AES/CFB8. Returns the `serverId` hash the client would
/// claim, so the test can compare it with what the server asked Mojang about.
///
/// `token_override` sends something other than the echoed token, which protocol 774
/// allows on the wire and the server must refuse.
async fn answer_encryption_request(client: &mut Client, token_override: Option<Vec<u8>>) -> String {
    let (id, body) = tokio::time::timeout(Duration::from_secs(5), client.recv_packet())
        .await
        .expect("timeout waiting for encryption request");
    assert_eq!(id, EncryptionRequest::ID);
    let request = EncryptionRequest::decode(&mut PacketReader::new(&body)).unwrap();
    assert!(request.server_id.is_empty());
    assert!(request.should_authenticate);
    assert!(!request.public_key.is_empty());
    assert!(!request.verify_token.is_empty());

    let shared_secret: [u8; 16] = rand::random();
    let public_key = RsaPublicKey::from_public_key_der(&request.public_key).unwrap();
    let encrypted_secret = public_key
        .encrypt(&mut rand::thread_rng(), Pkcs1v15Encrypt, &shared_secret)
        .unwrap();

    let response = EncryptionResponse {
        shared_secret: encrypted_secret,
        verify_token: token_override.unwrap_or_else(|| request.verify_token.clone()),
    };
    let mut body = loadstone_protocol::PacketWriter::new();
    response.encode(&mut body);
    client
        .send_packet(EncryptionResponse::ID, body.as_slice())
        .await;

    // From here on the wire is encrypted.
    client.enable_encryption(shared_secret);

    compute_server_id(&shared_secret, &request.public_key)
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_login_reaches_play_state() {
    let (addr, server) = spawn_server(base_config(false)).await;

    let mut client = Client::connect(&addr).await;
    client.handshake(2).await;
    let sent_uuid = client.login_start("Alice").await;

    let (id, body) = tokio::time::timeout(Duration::from_secs(5), client.recv_packet())
        .await
        .expect("timeout waiting for set compression");
    assert_eq!(id, SetCompression::ID);
    let mut reader = PacketReader::new(&body);
    let threshold = SetCompression::decode(&mut reader).unwrap();
    assert_eq!(threshold.threshold, loadstone_net::COMPRESSION_THRESHOLD);
    client.compression = Some(threshold.threshold);

    let (id, body) = tokio::time::timeout(Duration::from_secs(5), client.recv_packet())
        .await
        .expect("timeout waiting for login success");
    assert_eq!(id, LoginSuccess::ID);
    let success = LoginSuccess::decode(&mut PacketReader::new(&body)).unwrap();
    assert_eq!(success.uuid, sent_uuid);
    assert_eq!(success.username, "Alice");
    assert!(success.properties.is_empty());

    client.acknowledged().await;
    let registries = client.complete_configuration().await;
    assert_eq!(registries.len(), 23);
    assert!(registries.contains(&("minecraft:dimension_type".to_string(), 4)));
    assert!(registries.contains(&("minecraft:worldgen/biome".to_string(), 65)));

    let snapshot = client.enter_play(sent_uuid).await;

    assert_eq!(snapshot.login.entity_id, 0);
    assert_eq!(snapshot.login.view_distance, 1);
    assert_eq!(snapshot.login.simulation_distance, 1);
    assert_eq!(snapshot.login.world_names, vec!["minecraft:overworld"]);
    assert!(!snapshot.login.is_hardcore);
    assert_eq!(
        snapshot.login.world_state.dimension_name,
        "minecraft:overworld"
    );
    assert_eq!(snapshot.login.world_state.gamemode, 0);
    assert_eq!(snapshot.login.world_state.previous_gamemode, 255);
    assert!(!snapshot.login.world_state.is_flat);
    assert!(!snapshot.login.world_state.is_debug);
    assert_eq!(snapshot.login.world_state.hashed_seed, 0);
    assert_eq!(snapshot.login.world_state.sea_level, 63);
    assert_eq!(snapshot.login.world_state.portal_cooldown, 0);

    // 3x3 generated world around (0,0).
    assert!(snapshot.chunks.len() == 9);
    let (x_min, x_max) = snapshot
        .chunks
        .iter()
        .map(|c| c.x)
        .fold((i32::MAX, i32::MIN), |(lo, hi), x| (lo.min(x), hi.max(x)));
    let (z_min, z_max) = snapshot
        .chunks
        .iter()
        .map(|c| c.z)
        .fold((i32::MAX, i32::MIN), |(lo, hi), z| (lo.min(z), hi.max(z)));
    assert_eq!((x_min, x_max), (-1, 1));
    assert_eq!((z_min, z_max), (-1, 1));
    assert!(snapshot.chunks.iter().all(|c| !c.chunk_data.is_empty()));
    let world_surface = snapshot.chunks[0].heightmaps.first().unwrap();
    assert!(world_surface.1.len() == 37);
    assert!(snapshot.batch.batch_size == 9);

    // Real terrain, not a flat template: the heightmaps of the 3x3 view must
    // not all be identical.
    assert!(
        snapshot
            .chunks
            .iter()
            .map(|c| c.heightmaps[0].1.clone())
            .collect::<std::collections::HashSet<_>>()
            .len()
            > 1,
        "generated chunks should differ"
    );

    // The player spawns standing on the generated surface at (8.5, surface+1, 8.5).
    assert_eq!(snapshot.spawn.dimension_name, "minecraft:overworld");
    assert_eq!(snapshot.position.teleport_id, 0);
    assert_eq!(snapshot.position.x, 8.5);
    assert_eq!(snapshot.position.y, f64::from(spawn_surface_y() + 1));
    assert_eq!(snapshot.position.z, 8.5);
    assert_eq!(snapshot.position.flags, 0);

    // Full health, full hunger, and a chat welcome carrying the player name.
    assert_eq!(snapshot.health.health, 20.0);
    assert_eq!(snapshot.health.food, 20);
    assert_eq!(snapshot.health.food_saturation, 5.0);
    assert!(!snapshot.welcome.is_action_bar);
    let wire = String::from_utf8_lossy(&snapshot.welcome.content);
    assert!(wire.contains("Welcome"), "got: {wire}");
    assert!(wire.contains("Alice"), "got: {wire}");

    // The world is mutable: dig a block, place one back, and confirm edits echo.
    client.mutate_world(spawn_surface_y()).await;

    // The client is dropped at the end of this function, so the server can exit.
    drop(client);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("server did not finish play")
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn two_players_share_the_world() {
    let (addr, server) = spawn_server_multi(base_config(false)).await;

    // Alice is first: entity 0, no other players to spawn.
    let (mut alice, alice_uuid, alice_snapshot) = offline_join(&addr, "Alice").await;
    assert_eq!(alice_snapshot.login.entity_id, 0);
    assert!(alice_snapshot.spawns.is_empty());

    // Bob joins second and is shown Alice.
    let (mut bob, bob_uuid, bob_snapshot) = offline_join(&addr, "Bob").await;
    assert_eq!(bob_snapshot.login.entity_id, 1);
    assert_eq!(bob_snapshot.spawns.len(), 1);
    assert_eq!(bob_snapshot.spawns[0].uuid, alice_uuid);
    assert_eq!(bob_snapshot.spawns[0].entity_id, 0);

    // Alice is told Bob joined and learns his entity id.
    let bob_entity = alice.await_player_spawn(bob_uuid).await;
    assert_eq!(bob_entity, 1);

    // Bob's movement reaches Alice as a Sync Entity Position.
    bob.send_position(9.0, 65.0, 8.5).await;
    let (id, body) = alice.next_event().await;
    assert_eq!(id, SyncEntityPosition::ID, "expected a movement sync");
    let sync = SyncEntityPosition::decode(&mut PacketReader::new(&body)).unwrap();
    assert_eq!(sync.entity_id, bob_entity);
    assert_eq!(sync.x, 9.0);
    assert_eq!(sync.y, 65.0);
    assert_eq!(sync.z, 8.5);
    assert!(sync.on_ground);

    // Bob breaks a block: he gets the authoritative echo and Alice sees it too.
    let surface = spawn_surface_y();
    bob.dig_block(8, surface, 8, 1).await;
    let (id, body) = bob.next_event().await;
    assert_eq!(id, BlockChange::ID);
    let echo = BlockChange::decode(&mut PacketReader::new(&body)).unwrap();
    assert_eq!(echo.location, pack_position(8, surface, 8));
    assert_eq!(echo.block_state, 0);

    let (id, body) = alice.next_event().await;
    assert_eq!(id, BlockChange::ID, "expected the shared block change");
    let shared = BlockChange::decode(&mut PacketReader::new(&body)).unwrap();
    assert_eq!(shared.location, pack_position(8, surface, 8));
    assert_eq!(shared.block_state, 0);

    // Bob disconnects: Alice is told to remove the tab entry and the entity.
    drop(bob);
    let (id, body) = alice.next_event().await;
    assert_eq!(id, PlayerRemove::ID);
    let removed = PlayerRemove::decode(&mut PacketReader::new(&body)).unwrap();
    assert!(removed.players.contains(&bob_uuid));

    let (id, body) = alice.next_event().await;
    assert_eq!(id, RemoveEntities::ID);
    let gone = RemoveEntities::decode(&mut PacketReader::new(&body)).unwrap();
    assert!(gone.entity_ids.contains(&bob_entity));

    drop(alice);
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn chunks_stream_across_chunk_borders() {
    let (addr, server) = spawn_server(base_config(false)).await;
    let (mut client, _uuid, snapshot) = offline_join(&addr, "Alice").await;
    assert_eq!(snapshot.chunks.len(), 9);

    // Step one chunk east: chunk (0, 0) -> chunk (1, 0).
    client.send_position(24.5, 65.0, 8.5).await;

    // The server moves the client's view centre first.
    let (id, body) = client.next_event().await;
    assert_eq!(id, SetChunkCacheCenter::ID);
    let center = SetChunkCacheCenter::decode(&mut PacketReader::new(&body)).unwrap();
    assert_eq!((center.chunk_x, center.chunk_z), (1, 0));

    // Then the three columns (x = 2) that just entered view are batched.
    let (id, _) = client.next_event().await;
    assert_eq!(id, ChunkBatchStart::ID);
    let mut loaded = Vec::new();
    let batch_size = loop {
        let (id, body) = client.next_event().await;
        if id == MapChunk::ID {
            loaded.push(MapChunk::decode(&mut PacketReader::new(&body)).unwrap());
        } else {
            assert_eq!(id, ChunkBatchFinished::ID);
            break ChunkBatchFinished::decode(&mut PacketReader::new(&body))
                .unwrap()
                .batch_size;
        }
    };
    assert_eq!(batch_size, 3);
    assert_eq!(loaded.len(), 3);
    assert!(loaded.iter().all(|chunk| chunk.x == 2));

    // And the three columns (x = -1) that fell out of view are unloaded.
    for _ in 0..3 {
        let (id, body) = client.next_event().await;
        assert_eq!(id, UnloadChunk::ID);
        let unloaded = UnloadChunk::decode(&mut PacketReader::new(&body)).unwrap();
        assert_eq!(unloaded.chunk_x, -1);
    }

    drop(client);
    server.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn online_login_negotiates_encryption_and_session() {
    let recorded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let (mock_url, mock_server) = spawn_mock_sessionserver(recorded.clone()).await;

    let mut config = base_config(true);
    config.sessionserver_url = format!("http://{mock_url}");
    let (addr, server) = spawn_server(config).await;

    let mut client = Client::connect(&addr).await;
    client.handshake(2).await;
    client.login_start("MockPlayer").await;

    let client_server_id = answer_encryption_request(&mut client, None).await;

    let (id, body) = tokio::time::timeout(Duration::from_secs(5), client.recv_packet())
        .await
        .expect("timeout waiting for set compression");
    assert_eq!(id, SetCompression::ID);
    let mut reader = PacketReader::new(&body);
    let threshold = SetCompression::decode(&mut reader).unwrap();
    client.compression = Some(threshold.threshold);

    let (id, body) = tokio::time::timeout(Duration::from_secs(5), client.recv_packet())
        .await
        .expect("timeout waiting for login success");
    assert_eq!(id, LoginSuccess::ID);
    let success = LoginSuccess::decode(&mut PacketReader::new(&body)).unwrap();
    assert_eq!(success.uuid.to_string(), MOCK_PLAYER_ID);
    assert_eq!(success.username, "MockPlayer");

    client.acknowledged().await;
    let registries = client.complete_configuration().await;
    assert_eq!(registries.len(), 23);

    let play_uuid = Uuid::parse_str(MOCK_PLAYER_ID).unwrap();
    let snapshot = client.enter_play(play_uuid).await;
    assert_eq!(snapshot.position.x, 8.5);
    assert_eq!(snapshot.health.health, 20.0);
    client.mutate_world(spawn_surface_y()).await;

    drop(client);
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("server did not finish play")
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), mock_server)
        .await
        .unwrap()
        .unwrap();

    // The serverId the server sent to the session server must equal the one
    // the client computed locally: proves both sides agree on the hash.
    let requests = recorded.lock().unwrap();
    let url = requests.first().expect("sessionserver was not contacted");
    let query_server_id = url
        .split("serverId=")
        .nth(1)
        .expect("missing serverId in query");
    assert_eq!(query_server_id, client_server_id);
}

#[tokio::test(flavor = "multi_thread")]
async fn online_login_rejects_unverified_username() {
    let recorded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let (mock_url, mock_server) = spawn_mock_sessionserver(recorded.clone()).await;

    let mut config = base_config(true);
    config.sessionserver_url = format!("http://{mock_url}");
    let (addr, server) = spawn_server(config).await;
    let mut client = Client::connect(&addr).await;
    client.handshake(2).await;
    client.login_start("Intruder").await;

    answer_encryption_request(&mut client, None).await;

    let (id, body) = tokio::time::timeout(Duration::from_secs(5), client.recv_packet())
        .await
        .expect("timeout waiting for login disconnect");
    assert_eq!(id, loadstone_protocol::packets::login::LoginDisconnect::ID);
    let mut reader = PacketReader::new(&body);
    let reason = reader.read_string().unwrap().to_string();
    assert!(
        reason.contains("Failed to verify username"),
        "got: {reason}"
    );

    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), mock_server)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn online_login_rejects_an_unpaired_verify_token() {
    let recorded: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let (mock_url, mock_server) = spawn_mock_sessionserver(recorded.clone()).await;

    let mut config = base_config(true);
    config.sessionserver_url = format!("http://{mock_url}");
    let (addr, server) = spawn_server(config).await;

    let mut client = Client::connect(&addr).await;
    client.handshake(2).await;
    client.login_start("MockPlayer").await;

    // A well-formed key exchange, but the token from the request is not echoed back.
    answer_encryption_request(&mut client, Some(vec![0u8; 16])).await;

    let (id, body) = tokio::time::timeout(Duration::from_secs(5), client.recv_packet())
        .await
        .expect("timeout waiting for login disconnect");
    assert_eq!(id, loadstone_protocol::packets::login::LoginDisconnect::ID);
    let mut reader = PacketReader::new(&body);
    let reason = reader.read_string().unwrap().to_string();
    assert!(reason.contains("Invalid verify token"), "got: {reason}");

    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();

    // The token was refused before any session lookup, so Mojang was never asked and
    // the mock is still waiting for a request that must never arrive.
    assert!(recorded.lock().unwrap().is_empty());
    mock_server.abort();
}
