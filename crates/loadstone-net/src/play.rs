//! The Play state: spawns the player on a small flat platform and runs the
//! in-game loop (keep-alive, ping/pong, chat echo) until disconnect.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use loadstone_protocol::packets::play::{
    pack_position, Abilities, ChatMessage, ChunkBatchFinished, ChunkBatchReceived, ChunkBatchStart,
    ChunkBlockEntity, ClientPosition, Experience, MapChunk, PingRequest, PlayKeepAlive,
    PlayKeepAliveResponse, PlayLogin, PlayPong, PlayPongResponse, PlayerInfoEntry,
    PlayerInfoUpdate, SpawnInfo, SpawnPosition, SystemChat, TeleportConfirm, UpdateHealth,
    UpdateTime, PLAYER_INFO_ADD_PLAYER, PLAYER_INFO_UPDATE_GAME_MODE, PLAYER_INFO_UPDATE_LATENCY,
    PLAYER_INFO_UPDATE_LISTED,
};
use loadstone_protocol::Nbt;
use loadstone_protocol::{Packet, PacketReader, PacketWriter};
use loadstone_world::encode as world;
use tracing::{debug, info};
use uuid::Uuid;

use crate::connection::Connection;
use crate::error::NetError;

/// The initial chunk-view radius (view distance 1 sends a 3x3 area).
const VIEW_DISTANCE: i32 = 1;
const SIMULATION_DISTANCE: i32 = 1;
const ENTITY_ID: i32 = 0;
/// Uniform keep-alive interval while the player is in the game.
const KEEP_ALIVE_PERIOD: Duration = Duration::from_secs(5);

const HEIGHTMAP_WORLD_SURFACE: i32 = 1;
const HEIGHTMAP_MOTION_BLOCKING: i32 = 4;
const HEIGHTMAP_MOTION_BLOCKING_NO_LEAVES: i32 = 5;

fn encode_body<P: Packet>(packet: &P) -> Vec<u8> {
    let mut writer = PacketWriter::with_capacity(64);
    packet.encode(&mut writer);
    writer.into_vec()
}

fn text(message: &str) -> Vec<u8> {
    Nbt::text(message).to_anonymous_bytes()
}

/// Runs the Play state for a player that just finished Configuration.
pub async fn play_phase(mut conn: Connection, uuid: Uuid, username: &str) -> Result<(), NetError> {
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

    let world_origin = world::flat_chunk(0, 0);
    let heights = world::column_heights(&world_origin);
    let heightmap_data = world::pack_heightmap(&heights, world::WORLD_HEIGHT);
    let chunk_data = world::encode_chunk_column(&world_origin, biome_id);
    let sky_light = world::full_sky_light(24);
    let sky_light_mask = vec![((1 << 24) - 1) as i64];

    // 1. Login: dimension, spawn info and world rendering settings.
    let login = PlayLogin {
        entity_id: ENTITY_ID,
        is_hardcore: false,
        world_names: vec!["minecraft:overworld".to_string()],
        max_players: 20,
        view_distance: VIEW_DISTANCE,
        simulation_distance: SIMULATION_DISTANCE,
        reduced_debug_info: false,
        enable_respawn_screen: true,
        do_limited_crafting: false,
        world_state: SpawnInfo {
            dimension_id,
            dimension_name: "minecraft:overworld".to_string(),
            hashed_seed: 0,
            gamemode: 0, // survival
            previous_gamemode: 255,
            is_debug: false,
            is_flat: true,
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
        position: pack_position(0, world::GRASS_TOP_Y + 1, 0),
        yaw: 0.0,
        pitch: 0.0,
    };
    let body = encode_body(&spawn_pos);
    conn.write_packet(SpawnPosition::ID, &body).await?;

    let body = encode_body(&ChunkBatchStart);
    conn.write_packet(ChunkBatchStart::ID, &body).await?;
    for cx in -1..=1 {
        for cz in -1..=1 {
            let mp = MapChunk {
                x: cx,
                z: cz,
                heightmaps: vec![
                    (HEIGHTMAP_WORLD_SURFACE, heightmap_data.clone()),
                    (HEIGHTMAP_MOTION_BLOCKING, heightmap_data.clone()),
                    (HEIGHTMAP_MOTION_BLOCKING_NO_LEAVES, heightmap_data.clone()),
                ],
                chunk_data: chunk_data.clone(),
                block_entities: vec![] as Vec<ChunkBlockEntity>,
                sky_light_mask: sky_light_mask.clone(),
                block_light_mask: Vec::new(),
                empty_sky_light_mask: Vec::new(),
                empty_block_light_mask: Vec::new(),
                sky_light: sky_light.clone(),
                block_light: Vec::new(),
            };
            let body = encode_body(&mp);
            conn.write_packet(MapChunk::ID, &body).await?;
        }
    }
    // The client acknowledges the batch; the finished packet lifts the tunnel.
    loop {
        let frame = conn.read_packet().await?;
        if frame.id == ChunkBatchReceived::ID {
            let _ = ChunkBatchReceived::decode(&mut PacketReader::new(&frame.body))?;
            break;
        }
    }
    let body = encode_body(&ChunkBatchFinished { batch_size: 9 });
    conn.write_packet(ChunkBatchFinished::ID, &body).await?;

    // 3. Place the player at the spawn point and introduce them in the tab list.
    let teleport =
        ClientPosition::absolute(0, world::SPAWN_X, world::SPAWN_Y, world::SPAWN_Z, 0.0, 0.0);
    let body = encode_body(&teleport);
    conn.write_packet(ClientPosition::ID, &body).await?;

    let player_info = PlayerInfoUpdate {
        entries: vec![PlayerInfoEntry {
            uuid,
            name: username.to_string(),
            properties: Vec::new(),
            gamemode: Some(0),
            listed: Some(1),
            latency: Some(0),
            display_name: None,
            list_priority: None,
            show_hat: None,
        }],
    };
    let action_mask: u8 = PLAYER_INFO_ADD_PLAYER
        | PLAYER_INFO_UPDATE_GAME_MODE
        | PLAYER_INFO_UPDATE_LISTED
        | PLAYER_INFO_UPDATE_LATENCY;
    debug!(actions = action_mask, "sending player info update");
    let body = encode_body(&player_info);
    conn.write_packet(PlayerInfoUpdate::ID, &body).await?;

    // 4. Core gameplay state the client expects right after joining.
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

    info!(name = %username, "player entered the world");
    play_loop(&mut conn, username).await
}

/// The in-game packet loop. The client pushes keep-alives, pings, chat and
/// movement; the server replies and echoes chat back.
async fn play_loop(conn: &mut Connection, username: &str) -> Result<(), NetError> {
    let mut keep_alive_id: i64 = 1;

    loop {
        let frame = match tokio::time::timeout(KEEP_ALIVE_PERIOD, conn.read_packet()).await {
            Ok(result) => result?,
            Err(_) => {
                // Timeout: push a keep-alive probe and continue waiting.
                let body = encode_body(&PlayKeepAlive { keep_alive_id });
                conn.write_packet(PlayKeepAlive::ID, &body).await?;
                keep_alive_id = keep_alive_id.wrapping_add(1);
                continue;
            }
        };

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
            }
            _ => {
                // Movement, abilities, chunk acks and the like: nothing to do on
                // a flat, unchanging world.
                debug!(name = %username, id = frame.id, "ignoring play packet");
            }
        }
    }
}
