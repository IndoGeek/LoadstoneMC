//! The Play state: spawns the player on a small flat platform and runs the
//! in-game loop. Every connection shares one [`World`] and one player registry,
//! so joins, movements, block edits and disconnects are broadcast to everyone.

use std::collections::HashSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use loadstone_protocol::packets::play::{
    degree_to_angle, pack_position, unpack_position, Abilities, BlockChange, BlockDig, BlockPlace,
    BundleDelimiter, ChatMessage, ChunkBatchFinished, ChunkBatchStart, ClientPosition,
    EntityHeadRotation, EntityLook, EntityMetadata, Experience, MapChunk, MovementFlags,
    PingRequest, PlayKeepAlive, PlayKeepAliveResponse, PlayLogin, PlayPong, PlayPongResponse,
    PlayerInfoEntry, PlayerInfoUpdate, PlayerLook, PlayerPosition, PlayerPositionLook,
    PlayerRemove, RemoveEntities, SetChunkCacheCenter, SpawnEntity, SpawnInfo, SpawnPosition,
    SyncEntityPosition, SystemChat, TeleportConfirm, UnloadChunk, UpdateHealth, UpdateTime,
};
use loadstone_protocol::Nbt;
use loadstone_protocol::{Packet, PacketReader, PacketWriter};
use loadstone_world::encode::{self, BLOCK_AIR, BLOCK_COBBLESTONE};
use loadstone_world::{Chunk, ChunkPos, Entity};
use tokio::sync::mpsc;
use tracing::{debug, info};
use uuid::Uuid;

use crate::connection::{Connection, ConnectionConfig};
use crate::error::NetError;

/// The chunk-view radius (view distance 1 sends a 3x3 area). Chunks are
/// streamed to each player as they cross chunk boundaries.
const VIEW_DISTANCE: i32 = 1;
const SIMULATION_DISTANCE: i32 = 1;
/// Uniform keep-alive interval while the player is in the game.
const KEEP_ALIVE_PERIOD: Duration = Duration::from_secs(5);

const HEIGHTMAP_WORLD_SURFACE: i32 = 1;
const HEIGHTMAP_MOTION_BLOCKING: i32 = 4;
const HEIGHTMAP_MOTION_BLOCKING_NO_LEAVES: i32 = 5;

/// Metadata blob that tells other clients to render every skin layer:
/// index 9 (`PLAYER_MODE_CUSTOMISATION`), type 3 (byte), value `0x7F`, end.
const PLAYER_SKIN_METADATA: [u8; 4] = [0x09, 0x03, 0x7F, 0xFF];

/// The cast-face directional offsets in the order the protocol numbers them:
/// bottom, top, north, south, west, east.
const FACE_OFFSETS: [(i32, i32, i32); 6] = [
    (0, -1, 0),
    (0, 1, 0),
    (0, 0, -1),
    (0, 0, 1),
    (-1, 0, 0),
    (1, 0, 0),
];

fn encode_body<P: Packet>(packet: &P) -> Vec<u8> {
    let mut writer = PacketWriter::with_capacity(64);
    packet.encode(&mut writer);
    writer.into_vec()
}

fn text(message: &str) -> Vec<u8> {
    Nbt::text(message).to_anonymous_bytes()
}

/// The chunk a block coordinate falls in, using floor division so negative
/// coordinates round toward negative infinity.
fn chunk_of(x: f64, z: f64) -> (i32, i32) {
    (x.div_euclid(16.0) as i32, z.div_euclid(16.0) as i32)
}

/// Every chunk coordinate within `radius` of `(center_x, center_z)`.
fn visible_chunks(center_x: i32, center_z: i32, radius: i32) -> Vec<(i32, i32)> {
    let mut chunks = Vec::with_capacity(((radius * 2 + 1) * (radius * 2 + 1)) as usize);
    for dz in -radius..=radius {
        for dx in -radius..=radius {
            chunks.push((center_x + dx, center_z + dz));
        }
    }
    chunks
}

/// Builds the `map_chunk` packet for a generated world chunk.
fn build_map_chunk(chunk: &Chunk, x: i32, z: i32, biome_id: u32) -> MapChunk {
    let heights = encode::column_heights(chunk);
    let heightmap = encode::pack_heightmap(&heights, encode::WORLD_HEIGHT);
    MapChunk {
        x,
        z,
        heightmaps: vec![
            (HEIGHTMAP_WORLD_SURFACE, heightmap.clone()),
            (HEIGHTMAP_MOTION_BLOCKING, heightmap.clone()),
            (HEIGHTMAP_MOTION_BLOCKING_NO_LEAVES, heightmap),
        ],
        chunk_data: encode::encode_chunk_column(chunk, biome_id),
        block_entities: Vec::new(),
        sky_light_mask: vec![((1 << 24) - 1) as i64],
        block_light_mask: Vec::new(),
        empty_sky_light_mask: Vec::new(),
        empty_block_light_mask: Vec::new(),
        sky_light: encode::full_sky_light(24),
        block_light: Vec::new(),
    }
}

/// Encodes the `map_chunk` packets for the given chunk columns, generating and
/// caching them in the shared world first. The lock is held only for the
/// encode, never across an await.
fn encode_chunks(config: &ConnectionConfig, coords: &[(i32, i32)], biome_id: u32) -> Vec<Vec<u8>> {
    let mut world = config.world.lock().unwrap();
    coords
        .iter()
        .map(|&(cx, cz)| {
            let chunk = world.chunk(ChunkPos { x: cx, z: cz });
            encode_body(&build_map_chunk(chunk, cx, cz, biome_id))
        })
        .collect()
}

/// A snapshot of a player who was already in the world when a newcomer joined.
struct ExistingPlayer {
    uuid: Uuid,
    name: String,
    entity_id: i32,
    x: f64,
    y: f64,
    z: f64,
    yaw: f32,
    pitch: f32,
}

/// Runs the Play state for a player that just finished Configuration.
///
/// The player is registered in the shared server state before the spawn
/// sequence, so others can see them; they are removed and announced on every
/// exit path.
pub async fn play_phase(
    conn: Connection,
    uuid: Uuid,
    username: &str,
    config: &ConnectionConfig,
) -> Result<(), NetError> {
    let (out, rx) = mpsc::unbounded_channel();
    let spawn = config.world.lock().unwrap().spawn();

    let (entity_id, existing) = {
        let mut state = config.state.lock().unwrap();
        let entity_id = state.next_entity_id;
        state.next_entity_id += 1;
        state.players.insert(
            uuid,
            crate::connection::SharedPlayer {
                entity_id,
                name: username.to_string(),
                x: spawn.0,
                y: spawn.1,
                z: spawn.2,
                yaw: 0.0,
                pitch: 0.0,
                visible_entities: HashSet::new(),
                out,
            },
        );
        let existing = state
            .players
            .iter()
            .filter(|(id, _)| **id != uuid)
            .map(|(id, player)| ExistingPlayer {
                uuid: *id,
                name: player.name.clone(),
                entity_id: player.entity_id,
                x: player.x,
                y: player.y,
                z: player.z,
                yaw: player.yaw,
                pitch: player.pitch,
            })
            .collect();
        (entity_id, existing)
    };

    let result = run_play(conn, uuid, username, config, entity_id, existing, rx, spawn).await;
    leave_world(config, uuid, entity_id);
    result
}

#[allow(clippy::too_many_arguments)]
async fn run_play(
    mut conn: Connection,
    uuid: Uuid,
    username: &str,
    config: &ConnectionConfig,
    entity_id: i32,
    existing: Vec<ExistingPlayer>,
    rx: mpsc::UnboundedReceiver<(i32, Vec<u8>)>,
    spawn: (f64, f64, f64),
) -> Result<(), NetError> {
    let dimension_id =
        loadstone_registry::runtime_id("minecraft:dimension_type", "minecraft:overworld")
            .ok_or_else(|| {
                NetError::Internal("minecraft:overworld missing from synced registries".into())
            })?;
    let biome_id = loadstone_registry::runtime_id("minecraft:worldgen/biome", "minecraft:plains")
        .ok_or_else(|| {
        NetError::Internal("minecraft:plains missing from synced registries".into())
    })?;
    let biome_id = biome_id as u32;

    // 1. Login: dimension, spawn info and world rendering settings.
    let login = PlayLogin {
        entity_id,
        is_hardcore: config.hardcore,
        world_names: vec!["minecraft:overworld".to_string()],
        max_players: config.max_players,
        view_distance: VIEW_DISTANCE,
        simulation_distance: SIMULATION_DISTANCE,
        reduced_debug_info: false,
        enable_respawn_screen: true,
        do_limited_crafting: false,
        world_state: SpawnInfo {
            dimension_id,
            dimension_name: "minecraft:overworld".to_string(),
            hashed_seed: 0,
            gamemode: config.gamemode,
            previous_gamemode: 255,
            is_debug: false,
            is_flat: false,
            death: None,
            portal_cooldown: 0,
            // The vanilla capture showed a surprising -63 in this field during
            // testing; 63 is the conventional overworld sea level.
            sea_level: 63,
        },
        enforces_secure_chat: false,
    };
    let body = encode_body(&login);
    conn.write_packet(PlayLogin::ID, &body).await?;

    // 2. World spawn, then the chunk batch: the client loads the 3x3 platform
    // before leaving the "downloading terrain" screen.
    let spawn_pos = SpawnPosition {
        dimension_name: "minecraft:overworld".to_string(),
        position: pack_position(0, spawn.1.floor() as i32, 0),
        yaw: 0.0,
        pitch: 0.0,
    };
    let body = encode_body(&spawn_pos);
    conn.write_packet(SpawnPosition::ID, &body).await?;

    let spawn_center = chunk_of(spawn.0, spawn.2);
    let initial = visible_chunks(spawn_center.0, spawn_center.1, VIEW_DISTANCE);
    let initial_bodies = encode_chunks(config, &initial, biome_id);

    let body = encode_body(&SetChunkCacheCenter {
        chunk_x: spawn_center.0,
        chunk_z: spawn_center.1,
    });
    conn.write_packet(SetChunkCacheCenter::ID, &body).await?;

    let body = encode_body(&ChunkBatchStart);
    conn.write_packet(ChunkBatchStart::ID, &body).await?;
    for body in &initial_bodies {
        conn.write_packet(MapChunk::ID, body).await?;
    }
    // The batch is closed immediately: a client sends `chunk_batch_received`
    // *in response to* `chunk_batch_finished`, so waiting for it here would
    // deadlock against a real client (and leave it on "Loading terrain").
    let body = encode_body(&ChunkBatchFinished {
        batch_size: initial.len() as i32,
    });
    conn.write_packet(ChunkBatchFinished::ID, &body).await?;
    let loaded_chunks: HashSet<(i32, i32)> = initial.into_iter().collect();

    // 3. Place the player at the spawn point and introduce them in the tab list.
    let teleport = ClientPosition::absolute(0, spawn.0, spawn.1, spawn.2, 0.0, 0.0);
    let body = encode_body(&teleport);
    conn.write_packet(ClientPosition::ID, &body).await?;

    let body = encode_body(&player_info_add(uuid, username));
    conn.write_packet(PlayerInfoUpdate::ID, &body).await?;

    // 4. Introduce the players who were already here, in one atomic bundle.
    send_existing_players(&mut conn, &existing).await?;
    // And tell everyone else that this player joined.
    broadcast_join(config, uuid, entity_id, username, spawn);

    // 5. Core gameplay state the client expects right after joining.
    let abilities = Abilities {
        flags: 0x05, // invulnerable | allow flying
        flying_speed: 0.05,
        walking_speed: 0.05,
    };
    let body = encode_body(&abilities);
    conn.write_packet(Abilities::ID, &body).await?;

    let health = UpdateHealth {
        health: 20.0,
        food: 20,
        food_saturation: 5.0,
    };
    let body = encode_body(&health);
    conn.write_packet(UpdateHealth::ID, &body).await?;

    let experience = Experience {
        experience_bar: 0.0,
        level: 0,
        total_experience: 0,
    };
    let body = encode_body(&experience);
    conn.write_packet(Experience::ID, &body).await?;

    let now_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let time = UpdateTime {
        age: now_millis,
        time: 6000, // noon: a bright, welcoming first spawn
        tick_day_time: false,
    };
    let body = encode_body(&time);
    conn.write_packet(UpdateTime::ID, &body).await?;

    let server_data = loadstone_protocol::packets::play::ServerData {
        motd: text("A LoadstoneMC Server"),
        icon: None,
    };
    let body = encode_body(&server_data);
    conn.write_packet(loadstone_protocol::packets::play::ServerData::ID, &body)
        .await?;

    let welcome = SystemChat {
        content: text(&format!("Welcome, {}!", username)),
        is_action_bar: false,
    };
    let body = encode_body(&welcome);
    conn.write_packet(SystemChat::ID, &body).await?;

    info!(name = %username, entity_id, "player entered the world");
    // Vanilla's line, and the one a player's "did I get in?" is answered by.
    info!("{username} joined the game");
    play_loop(
        &mut conn,
        uuid,
        username,
        config,
        biome_id,
        spawn,
        loaded_chunks,
        rx,
    )
    .await
}

/// A tab-list "add player" entry with the fields we always populate.
fn player_info_add(uuid: Uuid, name: &str) -> PlayerInfoUpdate {
    PlayerInfoUpdate {
        entries: vec![PlayerInfoEntry {
            uuid,
            name: name.to_string(),
            properties: Vec::new(),
            // Vanilla sets the initialize_chat action to "no session" for
            // every tab-list entry; the client does not require it.
            initialize_chat: None,
            gamemode: Some(0),
            listed: Some(1),
            latency: Some(0),
            display_name: None,
            list_priority: None,
            show_hat: None,
        }],
    }
}

/// The packets that introduce a player to a client: tab entry, spawn and the
/// skin-layer metadata. Sent inside a bundle delimiter pair.
async fn send_existing_players(
    conn: &mut Connection,
    existing: &[ExistingPlayer],
) -> Result<(), NetError> {
    if existing.is_empty() {
        return Ok(());
    }
    let delimiter = encode_body(&BundleDelimiter);
    conn.write_packet(BundleDelimiter::ID, &delimiter).await?;
    for player in existing {
        let body = encode_body(&player_info_add(player.uuid, &player.name));
        conn.write_packet(PlayerInfoUpdate::ID, &body).await?;
        let spawn = SpawnEntity::player(
            player.entity_id,
            player.uuid,
            player.x,
            player.y,
            player.z,
            player.yaw,
            player.pitch,
        );
        let body = encode_body(&spawn);
        conn.write_packet(SpawnEntity::ID, &body).await?;
        conn.write_packet(EntityMetadata::ID, &skin_metadata(player.entity_id))
            .await?;
    }
    conn.write_packet(BundleDelimiter::ID, &delimiter).await?;
    Ok(())
}

fn skin_metadata(entity_id: i32) -> Vec<u8> {
    encode_body(&EntityMetadata {
        entity_id,
        blob: PLAYER_SKIN_METADATA.to_vec(),
    })
}

/// Pushes this player's tab entry, spawn and metadata to every other player.
fn broadcast_join(
    config: &ConnectionConfig,
    uuid: Uuid,
    entity_id: i32,
    username: &str,
    spawn: (f64, f64, f64),
) {
    let entity = SpawnEntity::player(entity_id, uuid, spawn.0, spawn.1, spawn.2, 0.0, 0.0);
    let frames = [
        (BundleDelimiter::ID, encode_body(&BundleDelimiter)),
        (
            PlayerInfoUpdate::ID,
            encode_body(&player_info_add(uuid, username)),
        ),
        (SpawnEntity::ID, encode_body(&entity)),
        (EntityMetadata::ID, skin_metadata(entity_id)),
        (BundleDelimiter::ID, encode_body(&BundleDelimiter)),
    ];

    let state = config.state.lock().unwrap();
    for (id, player) in &state.players {
        if *id == uuid {
            continue;
        }
        for (packet_id, body) in &frames {
            let _ = player.out.send((*packet_id, body.clone()));
        }
    }
}

/// Removes a player from the registry and tells everyone else they left.
fn leave_world(config: &ConnectionConfig, uuid: Uuid, entity_id: i32) {
    let mut state = config.state.lock().unwrap();
    // Taken before the removal, since the name goes with it.
    let name = state.players.remove(&uuid).map(|player| player.name);

    let remove_tab = encode_body(&PlayerRemove {
        players: vec![uuid],
    });
    let remove_entity = encode_body(&RemoveEntities {
        entity_ids: vec![entity_id],
    });
    for player in state.players.values() {
        let _ = player.out.send((PlayerRemove::ID, remove_tab.clone()));
        let _ = player.out.send((RemoveEntities::ID, remove_entity.clone()));
    }
    drop(state);

    if let Some(name) = name {
        info!("{name} left the game");
    }
}

/// Sends one already-encoded packet to every player except `source`.
fn broadcast_except(config: &ConnectionConfig, source: Uuid, packet_id: i32, body: Vec<u8>) {
    let state = config.state.lock().unwrap();
    for (uuid, player) in &state.players {
        if *uuid == source {
            continue;
        }
        let _ = player.out.send((packet_id, body.clone()));
    }
}

/// Advances the shared mob simulation by one tick and streams the results to
/// every player. It is a no-op while nobody is online, and each player only
/// learns about mobs whose chunk is inside their loaded view, so `add_entity`
/// always precedes any movement for that mob.
pub fn tick_entities(config: &ConnectionConfig) {
    let targets: Vec<(f64, f64, f64)> = {
        let state = config.state.lock().unwrap();
        if state.players.is_empty() {
            return;
        }
        state
            .players
            .values()
            .map(|player| (player.x, player.y, player.z))
            .collect()
    };

    let mobs: Vec<Entity> = {
        let mut world = config.world.lock().unwrap();
        let mut entities = config.entities.lock().unwrap();
        entities.tick(&mut world, &targets);
        entities.mobs().cloned().collect()
    };
    if mobs.is_empty() {
        return;
    }

    let mut state = config.state.lock().unwrap();
    for player in state.players.values_mut() {
        let (center_x, center_z) = chunk_of(player.x, player.z);
        let visible: HashSet<i32> = mobs
            .iter()
            .filter(|mob| {
                let (cx, cz) = chunk_of(mob.x, mob.z);
                (cx - center_x).abs() <= VIEW_DISTANCE && (cz - center_z).abs() <= VIEW_DISTANCE
            })
            .map(|mob| mob.id)
            .collect();

        let removed: Vec<i32> = player
            .visible_entities
            .difference(&visible)
            .copied()
            .collect();
        if !removed.is_empty() {
            let body = encode_body(&RemoveEntities {
                entity_ids: removed,
            });
            let _ = player.out.send((RemoveEntities::ID, body));
        }

        for mob in mobs.iter().filter(|mob| visible.contains(&mob.id)) {
            if !player.visible_entities.contains(&mob.id) {
                let spawn = SpawnEntity::mob(
                    mob.id,
                    Uuid::from_bytes(mob.uuid),
                    mob.kind.entity_type_id(),
                    mob.x,
                    mob.y,
                    mob.z,
                    mob.yaw,
                    mob.pitch,
                );
                let _ = player.out.send((SpawnEntity::ID, encode_body(&spawn)));
            }
            if mob.moved {
                let sync = SyncEntityPosition {
                    entity_id: mob.id,
                    x: mob.x,
                    y: mob.y,
                    z: mob.z,
                    vx: mob.vx,
                    vy: mob.vy,
                    vz: mob.vz,
                    yaw: mob.yaw,
                    pitch: mob.pitch,
                    on_ground: mob.on_ground,
                };
                let _ = player
                    .out
                    .send((SyncEntityPosition::ID, encode_body(&sync)));
            }
        }

        player.visible_entities = visible;
    }
}

/// Applies a movement to the registry and returns the resulting
/// `(entity_id, x, y, z, yaw, pitch)` for the broadcast.
fn record_movement(
    config: &ConnectionConfig,
    uuid: Uuid,
    pos: Option<(f64, f64, f64)>,
    look: Option<(f32, f32)>,
) -> Option<(i32, f64, f64, f64, f32, f32)> {
    let mut state = config.state.lock().unwrap();
    let player = state.players.get_mut(&uuid)?;
    if let Some((x, y, z)) = pos {
        player.x = x;
        player.y = y;
        player.z = z;
    }
    if let Some((yaw, pitch)) = look {
        player.yaw = yaw;
        player.pitch = pitch;
    }
    Some((
        player.entity_id,
        player.x,
        player.y,
        player.z,
        player.yaw,
        player.pitch,
    ))
}

/// The in-game packet loop. Reads from the client, replies to keep-alives and
/// pings, applies movement/block edits to the shared world, streams chunks when
/// the player crosses a chunk border, and flushes packets pushed by others.
#[allow(clippy::too_many_arguments)]
async fn play_loop(
    conn: &mut Connection,
    uuid: Uuid,
    username: &str,
    config: &ConnectionConfig,
    biome_id: u32,
    spawn: (f64, f64, f64),
    mut loaded_chunks: HashSet<(i32, i32)>,
    mut rx: mpsc::UnboundedReceiver<(i32, Vec<u8>)>,
) -> Result<(), NetError> {
    let mut keep_alive_id: i64 = 1;
    let mut deadline = tokio::time::Instant::now() + KEEP_ALIVE_PERIOD;
    let mut center = chunk_of(spawn.0, spawn.2);

    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                deadline = tokio::time::Instant::now() + KEEP_ALIVE_PERIOD;
                let body = encode_body(&PlayKeepAlive { keep_alive_id });
                conn.write_packet(PlayKeepAlive::ID, &body).await?;
                keep_alive_id = keep_alive_id.wrapping_add(1);
            }
            outgoing = rx.recv() => {
                match outgoing {
                    Some((id, body)) => conn.write_packet(id, &body).await?,
                    None => break,
                }
            }
            frame = conn.read_packet() => {
                let frame = frame?;
                if let Some((x, z)) = handle_frame(conn, uuid, username, config, frame).await? {
                    let new_center = chunk_of(x, z);
                    if new_center != center {
                        stream_chunks(
                            conn,
                            config,
                            biome_id,
                            &mut loaded_chunks,
                            new_center,
                        )
                        .await?;
                        center = new_center;
                    }
                }
            }
        }
    }

    Ok(())
}

/// Sends a chunk batch for the columns that entered view and unload packets for
/// the columns that left it, then records the new view. The view position is
/// announced first so the client knows the new centre.
async fn stream_chunks(
    conn: &mut Connection,
    config: &ConnectionConfig,
    biome_id: u32,
    loaded: &mut HashSet<(i32, i32)>,
    center: (i32, i32),
) -> Result<(), NetError> {
    let body = encode_body(&SetChunkCacheCenter {
        chunk_x: center.0,
        chunk_z: center.1,
    });
    conn.write_packet(SetChunkCacheCenter::ID, &body).await?;

    let view = visible_chunks(center.0, center.1, VIEW_DISTANCE);
    let to_load: Vec<(i32, i32)> = view
        .iter()
        .copied()
        .filter(|chunk| !loaded.contains(chunk))
        .collect();

    if !to_load.is_empty() {
        let bodies = encode_chunks(config, &to_load, biome_id);
        let body = encode_body(&ChunkBatchStart);
        conn.write_packet(ChunkBatchStart::ID, &body).await?;
        for body in &bodies {
            conn.write_packet(MapChunk::ID, body).await?;
        }
        let body = encode_body(&ChunkBatchFinished {
            batch_size: to_load.len() as i32,
        });
        conn.write_packet(ChunkBatchFinished::ID, &body).await?;
    }

    let view_set: HashSet<(i32, i32)> = view.into_iter().collect();
    for (cx, cz) in loaded.iter() {
        if !view_set.contains(&(*cx, *cz)) {
            let body = encode_body(&UnloadChunk {
                chunk_x: *cx,
                chunk_z: *cz,
            });
            conn.write_packet(UnloadChunk::ID, &body).await?;
        }
    }

    *loaded = view_set;
    Ok(())
}

/// Handles one clientbound frame. Returns the player's `(x, z)` when the frame
/// carried a position update, so the caller can re-stream chunks if needed.
async fn handle_frame(
    conn: &mut Connection,
    uuid: Uuid,
    username: &str,
    config: &ConnectionConfig,
    frame: crate::connection::RawPacket,
) -> Result<Option<(f64, f64)>, NetError> {
    let mut moved = None;
    match frame.id {
        id if id == PlayKeepAliveResponse::ID => {
            let _ = PlayKeepAliveResponse::decode(&mut PacketReader::new(&frame.body))?;
            debug!(name = %username, "kept alive");
        }
        id if id == PingRequest::ID => {
            let ping = PingRequest::decode(&mut PacketReader::new(&frame.body))?;
            let body = encode_body(&PlayPong { id: ping.id });
            conn.write_packet(PlayPong::ID, &body).await?;
        }
        id if id == PlayPongResponse::ID => {
            let pong = PlayPongResponse::decode(&mut PacketReader::new(&frame.body))?;
            debug!(name = %username, id = pong.id, "pong received");
        }
        id if id == TeleportConfirm::ID => {
            let _ = TeleportConfirm::decode(&mut PacketReader::new(&frame.body))?;
        }
        id if id == ChatMessage::ID => {
            let chat = ChatMessage::decode(&mut PacketReader::new(&frame.body))?;
            info!(name = %username, message = %chat.message, "chat");
            let echo = SystemChat {
                content: text(&format!("<{}> {}", username, chat.message)),
                is_action_bar: false,
            };
            let body = encode_body(&echo);
            conn.write_packet(SystemChat::ID, &body).await?;
            broadcast_except(config, uuid, SystemChat::ID, body);
        }
        id if id == PlayerPosition::ID => {
            let movement = PlayerPosition::decode(&mut PacketReader::new(&frame.body))?;
            let on_ground = movement.flags & MovementFlags::ON_GROUND != 0;
            moved = Some((movement.x, movement.z));
            sync_entity_position(
                config,
                uuid,
                Some((movement.x, movement.y, movement.z)),
                None,
                on_ground,
            );
        }
        id if id == PlayerPositionLook::ID => {
            let movement = PlayerPositionLook::decode(&mut PacketReader::new(&frame.body))?;
            let on_ground = movement.flags & MovementFlags::ON_GROUND != 0;
            moved = Some((movement.x, movement.z));
            sync_entity_position(
                config,
                uuid,
                Some((movement.x, movement.y, movement.z)),
                Some((movement.yaw, movement.pitch)),
                on_ground,
            );
        }
        id if id == PlayerLook::ID => {
            let movement = PlayerLook::decode(&mut PacketReader::new(&frame.body))?;
            let on_ground = movement.flags & MovementFlags::ON_GROUND != 0;
            if let Some((entity_id, _, _, _, yaw, pitch)) =
                record_movement(config, uuid, None, Some((movement.yaw, movement.pitch)))
            {
                let look = EntityLook::from_degrees(entity_id, yaw, pitch, on_ground);
                broadcast_except(config, uuid, EntityLook::ID, encode_body(&look));

                let head = EntityHeadRotation {
                    entity_id,
                    head_yaw: degree_to_angle(yaw),
                };
                broadcast_except(config, uuid, EntityHeadRotation::ID, encode_body(&head));
            }
        }
        id if id == BlockDig::ID => {
            let dig = BlockDig::decode(&mut PacketReader::new(&frame.body))?;
            let (x, y, z) = unpack_position(dig.location);
            debug!(name = %username, status = dig.status, x, y, z, "block dig");
            // Status 2 marks a finished dig; the block is removed.
            if dig.status == 2 {
                let broken = {
                    let mut world = config.world.lock().unwrap();
                    if world.is_breakable(x, y, z) {
                        world.set_block(x, y, z, BLOCK_AIR);
                        true
                    } else {
                        false
                    }
                };
                if broken {
                    let body = encode_body(&BlockChange {
                        location: dig.location,
                        block_state: i32::from(BLOCK_AIR),
                    });
                    conn.write_packet(BlockChange::ID, &body).await?;
                    broadcast_except(config, uuid, BlockChange::ID, body);
                }
            }
        }
        id if id == BlockPlace::ID => {
            let place = BlockPlace::decode(&mut PacketReader::new(&frame.body))?;
            let (x, y, z) = unpack_position(place.location);
            let (dx, dy, dz) = FACE_OFFSETS[place.direction as usize % FACE_OFFSETS.len()];
            let (tx, ty, tz) = (x + dx, y + dy, z + dz);
            debug!(name = %username, x = tx, y = ty, z = tz, "block place");
            let placed = {
                let mut world = config.world.lock().unwrap();
                if world.is_empty(tx, ty, tz) {
                    world.set_block(tx, ty, tz, BLOCK_COBBLESTONE);
                    true
                } else {
                    false
                }
            };
            if placed {
                let location = pack_position(tx, ty, tz);
                let body = encode_body(&BlockChange {
                    location,
                    block_state: i32::from(BLOCK_COBBLESTONE),
                });
                conn.write_packet(BlockChange::ID, &body).await?;
                broadcast_except(config, uuid, BlockChange::ID, body);
            }
        }
        _ => {
            // Abilities, chunk acks, player-loaded and the like: nothing to do.
            debug!(name = %username, id = frame.id, "ignoring play packet");
        }
    }

    Ok(moved)
}

/// Updates the registry with a movement and broadcasts the resulting absolute
/// position/velocity to every other player. The mover keeps predicting its own
/// position, so it receives nothing back.
fn sync_entity_position(
    config: &ConnectionConfig,
    uuid: Uuid,
    pos: Option<(f64, f64, f64)>,
    look: Option<(f32, f32)>,
    on_ground: bool,
) {
    let Some((entity_id, x, y, z, yaw, pitch)) = record_movement(config, uuid, pos, look) else {
        return;
    };
    let sync = SyncEntityPosition {
        entity_id,
        x,
        y,
        z,
        vx: 0.0,
        vy: 0.0,
        vz: 0.0,
        yaw,
        pitch,
        on_ground,
    };
    broadcast_except(config, uuid, SyncEntityPosition::ID, encode_body(&sync));
}
