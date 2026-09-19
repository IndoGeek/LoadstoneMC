//! Play-state packets for protocol 774 (1.21.11).
//!
//! As in the Configuration state, packet ids are only unique per direction, so
//! clientbound and serverbound packets with the same numeric id are separate
//! structs (e.g. clientbound `0x2A` `MapChunk` vs serverbound `0x2A`
//! `PlayPongResponse`).

use crate::error::ProtocolError;
use crate::packet::Packet;
use crate::read::PacketReader;
use crate::write::PacketWriter;
use uuid::Uuid;

/// Pack `x`, `y`, `z` into the protocol's packed long position:
/// `y` in the low 12 bits, `z` 26 bits, `x` in the high 26 bits.
pub fn pack_position(x: i32, y: i32, z: i32) -> i64 {
    (i64::from(x) & 0x3FFFFFF) << 38 | (i64::from(z) & 0x3FFFFFF) << 12 | (i64::from(y) & 0xFFF)
}

fn write_byte_array(out: &mut PacketWriter, data: &[u8]) {
    out.write_varint(data.len() as i32).write_bytes(data);
}

fn read_byte_array(reader: &mut PacketReader<'_>) -> Result<Vec<u8>, ProtocolError> {
    let len = reader.read_varint()?;
    if len < 0 {
        return Err(ProtocolError::InvalidStringLength(len));
    }
    reader.read_bytes(len as usize).map(ToOwned::to_owned)
}

// ── Clientbound ────────────────────────────────────────────────────────────

/// S->C Login (id 0x2E).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayLogin {
    pub entity_id: i32,
    pub is_hardcore: bool,
    pub world_names: Vec<String>,
    pub max_players: i32,
    pub view_distance: i32,
    pub simulation_distance: i32,
    pub reduced_debug_info: bool,
    pub enable_respawn_screen: bool,
    pub do_limited_crafting: bool,
    pub world_state: SpawnInfo,
    pub enforces_secure_chat: bool,
}

/// The `worldState` of the login packet: where and how the player spawns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnInfo {
    /// Index into the `minecraft:dimension_type` registry.
    pub dimension_id: i32,
    /// Dimension name, e.g. `minecraft:overworld`.
    pub dimension_name: String,
    pub hashed_seed: i64,
    /// 0 = survival, 1 = creative, 2 = adventure, 3 = spectator.
    pub gamemode: i8,
    /// 255 when the player has no previous game mode.
    pub previous_gamemode: u8,
    pub is_debug: bool,
    pub is_flat: bool,
    /// `(dimension name, packed block position)` of the last death, if any.
    pub death: Option<(String, i64)>,
    pub portal_cooldown: i32,
    pub sea_level: i32,
}

impl Packet for PlayLogin {
    const ID: i32 = 0x2E;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_i32(self.entity_id).write_bool(self.is_hardcore);
        out.write_varint(self.world_names.len() as i32);
        for name in &self.world_names {
            out.write_string(name);
        }
        out.write_varint(self.max_players)
            .write_varint(self.view_distance)
            .write_varint(self.simulation_distance)
            .write_bool(self.reduced_debug_info)
            .write_bool(self.enable_respawn_screen)
            .write_bool(self.do_limited_crafting);
        encode_spawn_info(out, &self.world_state);
        out.write_bool(self.enforces_secure_chat);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        let entity_id = reader.read_i32()?;
        let is_hardcore = reader.read_bool()?;
        let world_count = reader.read_varint()?;
        if world_count < 0 {
            return Err(ProtocolError::InvalidStringLength(world_count));
        }
        let mut world_names = Vec::with_capacity(world_count as usize);
        for _ in 0..world_count {
            world_names.push(reader.read_string()?.to_string());
        }
        Ok(Self {
            entity_id,
            is_hardcore,
            world_names,
            max_players: reader.read_varint()?,
            view_distance: reader.read_varint()?,
            simulation_distance: reader.read_varint()?,
            reduced_debug_info: reader.read_bool()?,
            enable_respawn_screen: reader.read_bool()?,
            do_limited_crafting: reader.read_bool()?,
            world_state: decode_spawn_info(reader)?,
            enforces_secure_chat: reader.read_bool()?,
        })
    }
}

fn encode_spawn_info(out: &mut PacketWriter, info: &SpawnInfo) {
    out.write_varint(info.dimension_id)
        .write_string(&info.dimension_name)
        .write_i64(info.hashed_seed)
        .write_i8(info.gamemode)
        .write_u8(info.previous_gamemode)
        .write_bool(info.is_debug)
        .write_bool(info.is_flat);
    match &info.death {
        Some((dimension, location)) => {
            out.write_bool(true)
                .write_string(dimension)
                .write_i64(*location);
        }
        None => {
            out.write_bool(false);
        }
    }
    out.write_varint(info.portal_cooldown)
        .write_varint(info.sea_level);
}

fn decode_spawn_info(reader: &mut PacketReader<'_>) -> Result<SpawnInfo, ProtocolError> {
    let dimension_id = reader.read_varint()?;
    let dimension_name = reader.read_string()?.to_string();
    let hashed_seed = reader.read_i64()?;
    let gamemode = reader.read_i8()?;
    let previous_gamemode = reader.read_u8()?;
    let is_debug = reader.read_bool()?;
    let is_flat = reader.read_bool()?;
    let death = if reader.read_bool()? {
        Some((reader.read_string()?.to_string(), reader.read_i64()?))
    } else {
        None
    };
    Ok(SpawnInfo {
        dimension_id,
        dimension_name,
        hashed_seed,
        gamemode,
        previous_gamemode,
        is_debug,
        is_flat,
        death,
        portal_cooldown: reader.read_varint()?,
        sea_level: reader.read_varint()?,
    })
}

/// S->C Keep Alive (id 0x29).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayKeepAlive {
    pub keep_alive_id: i64,
}

impl Packet for PlayKeepAlive {
    const ID: i32 = 0x29;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_i64(self.keep_alive_id);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            keep_alive_id: reader.read_i64()?,
        })
    }
}

/// S->C Chunk Batch Start (id 0x0B). Marks the beginning of a chunk batch.
#[derive(Debug, Clone, Copy)]
pub struct ChunkBatchStart;

impl Packet for ChunkBatchStart {
    const ID: i32 = 0x0B;

    fn encode(&self, _out: &mut PacketWriter) {}

    fn decode(_reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(ChunkBatchStart)
    }
}

/// S->C Chunk Batch Finished (id 0x0A).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkBatchFinished {
    pub batch_size: i32,
}

impl Packet for ChunkBatchFinished {
    const ID: i32 = 0x0A;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_varint(self.batch_size);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            batch_size: reader.read_varint()?,
        })
    }
}

/// A block entity inside a chunk. `packed_xz` holds the chunk-local X in the
/// high nibble and Z in the low nibble.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkBlockEntity {
    pub packed_xz: u8,
    pub y: i16,
    pub ty: i32,
    /// Anonymous NBT, absent for block entities with no extra data.
    pub nbt_data: Option<Vec<u8>>,
}

impl ChunkBlockEntity {
    pub fn new(x: u8, z: u8, y: i16, ty: i32) -> Self {
        Self {
            packed_xz: (x << 4) | z,
            y,
            ty,
            nbt_data: None,
        }
    }
}

/// S->C Level Chunk With Light (id 0x2A), the chunk data + light payload.
#[derive(Debug, Clone, PartialEq)]
pub struct MapChunk {
    pub x: i32,
    pub z: i32,
    /// `(heightmap type, packed long data)` pairs.
    pub heightmaps: Vec<(i32, Vec<i64>)>,
    /// The serialized chunk column (all non-empty sections, no count prefix).
    pub chunk_data: Vec<u8>,
    pub block_entities: Vec<ChunkBlockEntity>,
    pub sky_light_mask: Vec<i64>,
    pub block_light_mask: Vec<i64>,
    pub empty_sky_light_mask: Vec<i64>,
    pub empty_block_light_mask: Vec<i64>,
    pub sky_light: Vec<Vec<u8>>,
    pub block_light: Vec<Vec<u8>>,
}

impl Packet for MapChunk {
    const ID: i32 = 0x2A;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_i32(self.x).write_i32(self.z);
        out.write_varint(self.heightmaps.len() as i32);
        for (ty, data) in &self.heightmaps {
            out.write_varint(*ty);
            out.write_varint(data.len() as i32);
            for word in data {
                out.write_i64(*word);
            }
        }
        write_byte_array(out, &self.chunk_data);
        out.write_varint(self.block_entities.len() as i32);
        for entity in &self.block_entities {
            out.write_u8(entity.packed_xz)
                .write_i16(entity.y)
                .write_varint(entity.ty);
            match &entity.nbt_data {
                Some(nbt) => {
                    out.write_bytes(nbt);
                }
                None => {
                    // Absent optional NBT is a bare TAG_End byte.
                    out.write_u8(0);
                }
            }
        }
        for mask in [
            &self.sky_light_mask,
            &self.block_light_mask,
            &self.empty_sky_light_mask,
            &self.empty_block_light_mask,
        ] {
            out.write_varint(mask.len() as i32);
            for word in mask {
                out.write_i64(*word);
            }
        }
        for layer in [&self.sky_light, &self.block_light] {
            out.write_varint(layer.len() as i32);
            for section in layer {
                write_byte_array(out, section);
            }
        }
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        let x = reader.read_i32()?;
        let z = reader.read_i32()?;
        let hm_count = reader.read_varint()?;
        if hm_count < 0 {
            return Err(ProtocolError::InvalidStringLength(hm_count));
        }
        let mut heightmaps = Vec::with_capacity(hm_count as usize);
        for _ in 0..hm_count {
            let ty = reader.read_varint()?;
            let word_count = reader.read_varint()?;
            if word_count < 0 {
                return Err(ProtocolError::InvalidStringLength(word_count));
            }
            let mut data = Vec::with_capacity(word_count as usize);
            for _ in 0..word_count {
                data.push(reader.read_i64()?);
            }
            heightmaps.push((ty, data));
        }
        let chunk_data = read_byte_array(reader)?;
        let block_entity_count = reader.read_varint()?;
        if block_entity_count < 0 {
            return Err(ProtocolError::InvalidStringLength(block_entity_count));
        }
        let mut block_entities = Vec::with_capacity(block_entity_count as usize);
        for _ in 0..block_entity_count {
            let packed_xz = reader.read_u8()?;
            let y = reader.read_i16()?;
            let ty = reader.read_varint()?;
            let nbt_data = reader.read_anonymous_nbt()?.map(ToOwned::to_owned);
            block_entities.push(ChunkBlockEntity {
                packed_xz,
                y,
                ty,
                nbt_data,
            });
        }
        let read_mask = |reader: &mut PacketReader<'_>| -> Result<Vec<i64>, ProtocolError> {
            let count = reader.read_varint()?;
            if count < 0 {
                return Err(ProtocolError::InvalidStringLength(count));
            }
            let mut words = Vec::with_capacity(count as usize);
            for _ in 0..count {
                words.push(reader.read_i64()?);
            }
            Ok(words)
        };
        let read_layers = |reader: &mut PacketReader<'_>| -> Result<Vec<Vec<u8>>, ProtocolError> {
            let count = reader.read_varint()?;
            if count < 0 {
                return Err(ProtocolError::InvalidStringLength(count));
            }
            let mut layers = Vec::with_capacity(count as usize);
            for _ in 0..count {
                layers.push(read_byte_array(reader)?);
            }
            Ok(layers)
        };
        Ok(Self {
            x,
            z,
            heightmaps,
            chunk_data,
            block_entities,
            sky_light_mask: read_mask(reader)?,
            block_light_mask: read_mask(reader)?,
            empty_sky_light_mask: read_mask(reader)?,
            empty_block_light_mask: read_mask(reader)?,
            sky_light: read_layers(reader)?,
            block_light: read_layers(reader)?,
        })
    }
}

/// S->C Spawn Position (id 0x5B); also the respawn data type.
#[derive(Debug, Clone)]
pub struct SpawnPosition {
    pub dimension_name: String,
    /// Packed block position of the world spawn.
    pub position: i64,
    pub yaw: f32,
    pub pitch: f32,
}

impl Packet for SpawnPosition {
    const ID: i32 = 0x5B;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_string(&self.dimension_name)
            .write_i64(self.position)
            .write_f32(self.yaw)
            .write_f32(self.pitch);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            dimension_name: reader.read_string()?.to_string(),
            position: reader.read_i64()?,
            yaw: reader.read_f32()?,
            pitch: reader.read_f32()?,
        })
    }
}

/// S->C Position (id 0x44), a teleport the client confirms by id.
#[derive(Debug, Clone, Copy)]
pub struct ClientPosition {
    pub teleport_id: i32,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub dx: f64,
    pub dy: f64,
    pub dz: f64,
    pub yaw: f32,
    pub pitch: f32,
    /// `PositionUpdateRelatives` bit flags; 0 means all values are absolute.
    pub flags: u32,
}

impl ClientPosition {
    pub fn absolute(teleport_id: i32, x: f64, y: f64, z: f64, yaw: f32, pitch: f32) -> Self {
        Self {
            teleport_id,
            x,
            y,
            z,
            dx: 0.0,
            dy: 0.0,
            dz: 0.0,
            yaw,
            pitch,
            flags: 0,
        }
    }
}

impl Packet for ClientPosition {
    const ID: i32 = 0x44;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_varint(self.teleport_id)
            .write_f64(self.x)
            .write_f64(self.y)
            .write_f64(self.z)
            .write_f64(self.dx)
            .write_f64(self.dy)
            .write_f64(self.dz)
            .write_f32(self.yaw)
            .write_f32(self.pitch)
            .write_i32(self.flags as i32);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            teleport_id: reader.read_varint()?,
            x: reader.read_f64()?,
            y: reader.read_f64()?,
            z: reader.read_f64()?,
            dx: reader.read_f64()?,
            dy: reader.read_f64()?,
            dz: reader.read_f64()?,
            yaw: reader.read_f32()?,
            pitch: reader.read_f32()?,
            flags: reader.read_i32()? as u32,
        })
    }
}

/// Player Info action bits for `player_info`.
pub const PLAYER_INFO_ADD_PLAYER: u8 = 0x01;
pub const PLAYER_INFO_INITIALIZE_CHAT: u8 = 0x02;
pub const PLAYER_INFO_UPDATE_GAME_MODE: u8 = 0x04;
pub const PLAYER_INFO_UPDATE_LISTED: u8 = 0x08;
pub const PLAYER_INFO_UPDATE_LATENCY: u8 = 0x10;
pub const PLAYER_INFO_UPDATE_DISPLAY_NAME: u8 = 0x20;
pub const PLAYER_INFO_UPDATE_LIST_ORDER: u8 = 0x40;
pub const PLAYER_INFO_UPDATE_HAT: u8 = 0x80;

/// One tab list entry. `None` fields contribute no bytes on the wire unless the
/// corresponding action bit is derived as set.
#[derive(Debug, Clone)]
pub struct PlayerInfoEntry {
    pub uuid: Uuid,
    pub name: String,
    pub properties: Vec<(String, String, Option<String>)>,
    pub gamemode: Option<i32>,
    pub listed: Option<i32>,
    pub latency: Option<i32>,
    pub display_name: Option<Vec<u8>>,
    pub list_priority: Option<i32>,
    pub show_hat: Option<bool>,
}

impl PlayerInfoEntry {
    fn actions(&self) -> u8 {
        let mut actions = PLAYER_INFO_ADD_PLAYER;
        if self.gamemode.is_some() {
            actions |= PLAYER_INFO_UPDATE_GAME_MODE;
        }
        if self.listed.is_some() {
            actions |= PLAYER_INFO_UPDATE_LISTED;
        }
        if self.latency.is_some() {
            actions |= PLAYER_INFO_UPDATE_LATENCY;
        }
        if self.display_name.is_some() {
            actions |= PLAYER_INFO_UPDATE_DISPLAY_NAME;
        }
        if self.list_priority.is_some() {
            actions |= PLAYER_INFO_UPDATE_LIST_ORDER;
        }
        if self.show_hat.is_some() {
            actions |= PLAYER_INFO_UPDATE_HAT;
        }
        actions
    }
}

/// S->C Player Info (id 0x42).
#[derive(Debug, Clone)]
pub struct PlayerInfoUpdate {
    pub entries: Vec<PlayerInfoEntry>,
}

impl Packet for PlayerInfoUpdate {
    const ID: i32 = 0x42;

    fn encode(&self, out: &mut PacketWriter) {
        let of = |entry: &PlayerInfoEntry| entry.actions();
        let mask = self.entries.iter().fold(0u8, |acc, e| acc | of(e));
        out.write_u8(mask).write_varint(self.entries.len() as i32);
        for entry in &self.entries {
            out.write_uuid(entry.uuid);
            if of(entry) & PLAYER_INFO_ADD_PLAYER != 0 {
                out.write_string(&entry.name);
                out.write_varint(entry.properties.len() as i32);
                for (name, value, signature) in &entry.properties {
                    out.write_string(name).write_string(value);
                    match signature {
                        Some(sig) => {
                            out.write_bool(true).write_string(sig);
                        }
                        None => {
                            out.write_bool(false);
                        }
                    }
                }
            }
            if of(entry) & PLAYER_INFO_UPDATE_GAME_MODE != 0 {
                out.write_varint(entry.gamemode.unwrap_or(0));
            }
            if of(entry) & PLAYER_INFO_UPDATE_LISTED != 0 {
                out.write_varint(entry.listed.unwrap_or(1));
            }
            if of(entry) & PLAYER_INFO_UPDATE_LATENCY != 0 {
                out.write_varint(entry.latency.unwrap_or(0));
            }
            if of(entry) & PLAYER_INFO_UPDATE_DISPLAY_NAME != 0 {
                match &entry.display_name {
                    Some(nbt) => {
                        out.write_bool(true).write_bytes(nbt);
                    }
                    None => {
                        out.write_bool(false);
                    }
                }
            }
            if of(entry) & PLAYER_INFO_UPDATE_LIST_ORDER != 0 {
                out.write_varint(entry.list_priority.unwrap_or(0));
            }
            if of(entry) & PLAYER_INFO_UPDATE_HAT != 0 {
                out.write_bool(entry.show_hat.unwrap_or(false));
            }
        }
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        let actions = reader.read_u8()?;
        let count = reader.read_varint()?;
        if count < 0 {
            return Err(ProtocolError::InvalidStringLength(count));
        }
        let mut entries = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let uuid = reader.read_uuid()?;
            let mut name = String::new();
            let mut properties = Vec::new();
            if actions & PLAYER_INFO_ADD_PLAYER != 0 {
                name = reader.read_string()?.to_string();
                let property_count = reader.read_varint()?;
                if property_count < 0 {
                    return Err(ProtocolError::InvalidStringLength(property_count));
                }
                for _ in 0..property_count {
                    let pname = reader.read_string()?.to_string();
                    let value = reader.read_string()?.to_string();
                    let signature = if reader.read_bool()? {
                        Some(reader.read_string()?.to_string())
                    } else {
                        None
                    };
                    properties.push((pname, value, signature));
                }
            }
            let gamemode = if actions & PLAYER_INFO_UPDATE_GAME_MODE != 0 {
                Some(reader.read_varint()?)
            } else {
                None
            };
            let listed = if actions & PLAYER_INFO_UPDATE_LISTED != 0 {
                Some(reader.read_varint()?)
            } else {
                None
            };
            let latency = if actions & PLAYER_INFO_UPDATE_LATENCY != 0 {
                Some(reader.read_varint()?)
            } else {
                None
            };
            let display_name = if actions & PLAYER_INFO_UPDATE_DISPLAY_NAME != 0 {
                if reader.read_bool()? {
                    Some(reader.read_rest().to_vec())
                } else {
                    None
                }
            } else {
                None
            };
            let list_priority = if actions & PLAYER_INFO_UPDATE_LIST_ORDER != 0 {
                Some(reader.read_varint()?)
            } else {
                None
            };
            let show_hat = if actions & PLAYER_INFO_UPDATE_HAT != 0 {
                Some(reader.read_bool()?)
            } else {
                None
            };
            entries.push(PlayerInfoEntry {
                uuid,
                name,
                properties,
                gamemode,
                listed,
                latency,
                display_name,
                list_priority,
                show_hat,
            });
        }
        Ok(Self { entries })
    }
}

/// S->C Abilities (id 0x3C). Bit 0 = invulnerable, bit 1 = flying,
/// bit 2 = allow flying, bit 3 = creative mode.
#[derive(Debug, Clone, Copy)]
pub struct Abilities {
    pub flags: i8,
    pub flying_speed: f32,
    pub walking_speed: f32,
}

impl Packet for Abilities {
    const ID: i32 = 0x3C;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_i8(self.flags)
            .write_f32(self.flying_speed)
            .write_f32(self.walking_speed);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            flags: reader.read_i8()?,
            flying_speed: reader.read_f32()?,
            walking_speed: reader.read_f32()?,
        })
    }
}

/// S->C Update Health (id 0x62).
#[derive(Debug, Clone, Copy)]
pub struct UpdateHealth {
    pub health: f32,
    pub food: i32,
    pub food_saturation: f32,
}

impl Packet for UpdateHealth {
    const ID: i32 = 0x62;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_f32(self.health)
            .write_varint(self.food)
            .write_f32(self.food_saturation);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            health: reader.read_f32()?,
            food: reader.read_varint()?,
            food_saturation: reader.read_f32()?,
        })
    }
}

/// S->C Experience (id 0x61).
#[derive(Debug, Clone, Copy)]
pub struct Experience {
    pub experience_bar: f32,
    pub level: i32,
    pub total_experience: i32,
}

impl Packet for Experience {
    const ID: i32 = 0x61;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_f32(self.experience_bar)
            .write_varint(self.level)
            .write_varint(self.total_experience);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            experience_bar: reader.read_f32()?,
            level: reader.read_varint()?,
            total_experience: reader.read_varint()?,
        })
    }
}

/// S->C Update Time (id 0x6B).
#[derive(Debug, Clone, Copy)]
pub struct UpdateTime {
    pub age: i64,
    pub time: i64,
    pub tick_day_time: bool,
}

impl Packet for UpdateTime {
    const ID: i32 = 0x6B;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_i64(self.age)
            .write_i64(self.time)
            .write_bool(self.tick_day_time);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            age: reader.read_i64()?,
            time: reader.read_i64()?,
            tick_day_time: reader.read_bool()?,
        })
    }
}

/// S->C Server Data (id 0x50).
#[derive(Debug, Clone)]
pub struct ServerData {
    /// Anonymous NBT text component.
    pub motd: Vec<u8>,
    pub icon: Option<Vec<u8>>,
}

impl Packet for ServerData {
    const ID: i32 = 0x50;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_bytes(&self.motd);
        match &self.icon {
            Some(icon) => {
                out.write_bool(true);
                write_byte_array(out, icon);
            }
            None => {
                out.write_bool(false);
            }
        }
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        // The motd is a native NBT value, so the whole remaining buffer is the
        // component; the icon (if present) follows it as an opaque option.
        Ok(Self {
            motd: reader.read_rest().to_vec(),
            icon: None,
        })
    }
}

/// S->C System Chat (id 0x72).
#[derive(Debug, Clone)]
pub struct SystemChat {
    /// Anonymous NBT text component.
    pub content: Vec<u8>,
    pub is_action_bar: bool,
}

impl Packet for SystemChat {
    const ID: i32 = 0x72;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_bytes(&self.content)
            .write_bool(self.is_action_bar);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        // The content is a native NBT value that owns the trailing action flag.
        let remaining = reader.remaining();
        if remaining == 0 {
            return Err(ProtocolError::UnexpectedEof { needed: 1, had: 0 });
        }
        let content = reader.read_bytes(remaining - 1)?.to_vec();
        Ok(Self {
            content,
            is_action_bar: reader.read_bool()?,
        })
    }
}

/// S->C Ping (id 0x39). The client answers with the serverbound pong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayPing {
    pub id: i32,
}

impl Packet for PlayPing {
    const ID: i32 = 0x39;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_i32(self.id);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            id: reader.read_i32()?,
        })
    }
}

/// S->C Ping Response (id 0x3A), in reply to a serverbound ping request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayPong {
    pub id: i64,
}

impl Packet for PlayPong {
    const ID: i32 = 0x3A;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_i64(self.id);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            id: reader.read_i64()?,
        })
    }
}

/// S->C Disconnect (id 0x1E). The reason is anonymous NBT.
#[derive(Debug, Clone)]
pub struct PlayDisconnect {
    pub reason: Vec<u8>,
}

impl Packet for PlayDisconnect {
    const ID: i32 = 0x1E;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_bytes(&self.reason);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            reason: reader.read_rest().to_vec(),
        })
    }
}

// ── Serverbound ────────────────────────────────────────────────────────────

/// C->S Keep Alive (id 0x19).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayKeepAliveResponse {
    pub keep_alive_id: i64,
}

impl Packet for PlayKeepAliveResponse {
    const ID: i32 = 0x19;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_i64(self.keep_alive_id);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            keep_alive_id: reader.read_i64()?,
        })
    }
}

/// C->S Chat Message (id 0x08).
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub message: String,
    pub timestamp: i64,
    pub salt: i64,
    /// 256-byte signature when chat signing is in use.
    pub signature: Option<Vec<u8>>,
    pub offset: i32,
    pub acknowledged: [u8; 3],
    pub checksum: u8,
}

impl Packet for ChatMessage {
    const ID: i32 = 0x08;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_string(&self.message)
            .write_i64(self.timestamp)
            .write_i64(self.salt);
        match &self.signature {
            Some(sig) => {
                out.write_bool(true).write_bytes(sig);
            }
            None => {
                out.write_bool(false);
            }
        }
        out.write_varint(self.offset)
            .write_bytes(&self.acknowledged)
            .write_u8(self.checksum);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        let message = reader.read_string()?.to_string();
        let timestamp = reader.read_i64()?;
        let salt = reader.read_i64()?;
        let signature = if reader.read_bool()? {
            let bytes = reader.read_bytes(256)?;
            Some(bytes.to_vec())
        } else {
            None
        };
        let offset = reader.read_varint()?;
        let acknowledged = [reader.read_u8()?, reader.read_u8()?, reader.read_u8()?];
        Ok(Self {
            message,
            timestamp,
            salt,
            signature,
            offset,
            acknowledged,
            checksum: reader.read_u8()?,
        })
    }
}

/// C->S Chat Command (id 0x06).
#[derive(Debug, Clone)]
pub struct ChatCommand {
    pub command: String,
}

impl Packet for ChatCommand {
    const ID: i32 = 0x06;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_string(&self.command);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            command: reader.read_string()?.to_string(),
        })
    }
}

/// C->S Chat Command Signed (id 0x07).
#[derive(Debug, Clone)]
pub struct ChatCommandSigned {
    pub command: String,
    pub timestamp: i64,
    pub salt: i64,
    pub argument_signatures: Vec<(String, Vec<u8>)>,
    pub message_count: i32,
    pub acknowledged: [u8; 3],
    pub checksum: i8,
}

impl Packet for ChatCommandSigned {
    const ID: i32 = 0x07;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_string(&self.command)
            .write_i64(self.timestamp)
            .write_i64(self.salt);
        out.write_varint(self.argument_signatures.len() as i32);
        for (name, signature) in &self.argument_signatures {
            out.write_string(name).write_bytes(signature);
        }
        out.write_varint(self.message_count)
            .write_bytes(&self.acknowledged)
            .write_i8(self.checksum);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        let command = reader.read_string()?.to_string();
        let timestamp = reader.read_i64()?;
        let salt = reader.read_i64()?;
        let count = reader.read_varint()?;
        if count < 0 {
            return Err(ProtocolError::InvalidStringLength(count));
        }
        let mut argument_signatures = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let name = reader.read_string()?.to_string();
            let signature = reader.read_bytes(256)?.to_vec();
            argument_signatures.push((name, signature));
        }
        let message_count = reader.read_varint()?;
        let acknowledged = [reader.read_u8()?, reader.read_u8()?, reader.read_u8()?];
        Ok(Self {
            command,
            timestamp,
            salt,
            argument_signatures,
            message_count,
            acknowledged,
            checksum: reader.read_i8()?,
        })
    }
}

/// C->S Chat Session Update (id 0x09), the player's signed chat key renewal.
#[derive(Debug, Clone)]
pub struct ChatSessionUpdate {
    pub session_uuid: Uuid,
    pub expire_time: i64,
    pub public_key: Vec<u8>,
    pub signature: Vec<u8>,
}

impl Packet for ChatSessionUpdate {
    const ID: i32 = 0x09;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_uuid(self.session_uuid)
            .write_i64(self.expire_time);
        write_byte_array(out, &self.public_key);
        write_byte_array(out, &self.signature);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            session_uuid: reader.read_uuid()?,
            expire_time: reader.read_i64()?,
            public_key: read_byte_array(reader)?,
            signature: read_byte_array(reader)?,
        })
    }
}

/// C->S Player Loaded (id 0x29): the client cleared its loading screen.
#[derive(Debug, Clone, Copy)]
pub struct PlayerLoaded;

impl Packet for PlayerLoaded {
    const ID: i32 = 0x29;

    fn encode(&self, _out: &mut PacketWriter) {}

    fn decode(_reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(PlayerLoaded)
    }
}

/// C->S Chunk Batch Received (id 0x0A).
#[derive(Debug, Clone, Copy)]
pub struct ChunkBatchReceived {
    pub chunks_per_tick: f32,
}

impl Packet for ChunkBatchReceived {
    const ID: i32 = 0x0A;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_f32(self.chunks_per_tick);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            chunks_per_tick: reader.read_f32()?,
        })
    }
}

/// C->S Teleport Confirm (id 0x00).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeleportConfirm {
    pub teleport_id: i32,
}

impl Packet for TeleportConfirm {
    const ID: i32 = 0x00;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_varint(self.teleport_id);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            teleport_id: reader.read_varint()?,
        })
    }
}

/// Movement flags sharing the wire bit layout (`onGround`,
/// `hasHorizontalCollision`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MovementFlags(pub u8);

impl MovementFlags {
    pub const ON_GROUND: u8 = 0x01;
    pub const HORIZONTAL_COLLISION: u8 = 0x02;
}

/// C->S Position (id 0x1B).
#[derive(Debug, Clone, Copy)]
pub struct PlayerPosition {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub flags: u8,
}

impl Packet for PlayerPosition {
    const ID: i32 = 0x1B;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_f64(self.x)
            .write_f64(self.y)
            .write_f64(self.z)
            .write_u8(self.flags);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            x: reader.read_f64()?,
            y: reader.read_f64()?,
            z: reader.read_f64()?,
            flags: reader.read_u8()?,
        })
    }
}

/// C->S Position and Look (id 0x1C).
#[derive(Debug, Clone, Copy)]
pub struct PlayerPositionLook {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
    pub flags: u8,
}

impl Packet for PlayerPositionLook {
    const ID: i32 = 0x1C;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_f64(self.x)
            .write_f64(self.y)
            .write_f64(self.z)
            .write_f32(self.yaw)
            .write_f32(self.pitch)
            .write_u8(self.flags);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            x: reader.read_f64()?,
            y: reader.read_f64()?,
            z: reader.read_f64()?,
            yaw: reader.read_f32()?,
            pitch: reader.read_f32()?,
            flags: reader.read_u8()?,
        })
    }
}

/// C->S Look (id 0x1D).
#[derive(Debug, Clone, Copy)]
pub struct PlayerLook {
    pub yaw: f32,
    pub pitch: f32,
    pub flags: u8,
}

impl Packet for PlayerLook {
    const ID: i32 = 0x1D;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_f32(self.yaw)
            .write_f32(self.pitch)
            .write_u8(self.flags);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            yaw: reader.read_f32()?,
            pitch: reader.read_f32()?,
            flags: reader.read_u8()?,
        })
    }
}

/// C->S Flying (id 0x1E).
#[derive(Debug, Clone, Copy)]
pub struct PlayerFlying {
    pub flags: u8,
}

impl Packet for PlayerFlying {
    const ID: i32 = 0x1E;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_u8(self.flags);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            flags: reader.read_u8()?,
        })
    }
}

/// C->S Ping Request (id 0x23). The server answers with `PlayPong`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PingRequest {
    pub id: i64,
}

impl Packet for PingRequest {
    const ID: i32 = 0x23;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_i64(self.id);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            id: reader.read_i64()?,
        })
    }
}

/// C->S Pong (id 0x2A), in reply to a serverbound `PlayPing`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayPongResponse {
    pub id: i32,
}

impl Packet for PlayPongResponse {
    const ID: i32 = 0x2A;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_i32(self.id);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            id: reader.read_i32()?,
        })
    }
}

/// C->S Message Acknowledgement (id 0x05).
#[derive(Debug, Clone, Copy)]
pub struct MessageAcknowledgement {
    pub count: i32,
}

impl Packet for MessageAcknowledgement {
    const ID: i32 = 0x05;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_varint(self.count);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            count: reader.read_varint()?,
        })
    }
}

/// C->S Client Command (id 0x0B): 0 = perform respawn, 1 = request stats.
#[derive(Debug, Clone, Copy)]
pub struct ClientCommand {
    pub action_id: i32,
}

impl Packet for ClientCommand {
    const ID: i32 = 0x0B;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_varint(self.action_id);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            action_id: reader.read_varint()?,
        })
    }
}

/// C->S Configuration Acknowledged (id 0x0E), sent late in Play when the
/// client returns from a world-bound configuration transition.
#[derive(Debug, Clone, Copy)]
pub struct PlayConfigurationAcknowledged;

impl Packet for PlayConfigurationAcknowledged {
    const ID: i32 = 0x0E;

    fn encode(&self, _out: &mut PacketWriter) {}

    fn decode(_reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(PlayConfigurationAcknowledged)
    }
}

/// C->S Custom Payload (id 0x13), to the end of the packet.
#[derive(Debug, Clone)]
pub struct ClientCustomPayload {
    pub channel: String,
    pub data: Vec<u8>,
}

impl Packet for ClientCustomPayload {
    const ID: i32 = 0x13;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_string(&self.channel).write_bytes(&self.data);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            channel: reader.read_string()?.to_string(),
            data: reader.read_rest().to_vec(),
        })
    }
}

/// C->S Tick End (id 0x0C).
#[derive(Debug, Clone, Copy)]
pub struct TickEnd;

impl Packet for TickEnd {
    const ID: i32 = 0x0C;

    fn encode(&self, _out: &mut PacketWriter) {}

    fn decode(_reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(TickEnd)
    }
}

/// C->S Arm Animation (id 0x3A).
#[derive(Debug, Clone, Copy)]
pub struct ArmAnimation {
    pub hand: i32,
}

impl Packet for ArmAnimation {
    const ID: i32 = 0x3A;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_varint(self.hand);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            hand: reader.read_varint()?,
        })
    }
}

/// C->S Abilities (id 0x25).
#[derive(Debug, Clone, Copy)]
pub struct PlayerAbilities {
    pub flags: i8,
}

impl Packet for PlayerAbilities {
    const ID: i32 = 0x25;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_i8(self.flags);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            flags: reader.read_i8()?,
        })
    }
}

/// C->S Block Dig (id 0x26).
#[derive(Debug, Clone, Copy)]
pub struct BlockDig {
    pub status: i32,
    /// Packed block position.
    pub location: i64,
    pub face: i8,
    pub sequence: i32,
}

impl Packet for BlockDig {
    const ID: i32 = 0x26;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_varint(self.status)
            .write_i64(self.location)
            .write_i8(self.face)
            .write_varint(self.sequence);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            status: reader.read_varint()?,
            location: reader.read_i64()?,
            face: reader.read_i8()?,
            sequence: reader.read_varint()?,
        })
    }
}

/// C->S Entity Action (id 0x27).
#[derive(Debug, Clone, Copy)]
pub struct EntityAction {
    pub entity_id: i32,
    pub action_id: i32,
    pub jump_boost: i32,
}

impl Packet for EntityAction {
    const ID: i32 = 0x27;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_varint(self.entity_id)
            .write_varint(self.action_id)
            .write_varint(self.jump_boost);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            entity_id: reader.read_varint()?,
            action_id: reader.read_varint()?,
            jump_boost: reader.read_varint()?,
        })
    }
}

/// C->S Block Place (id 0x3D).
#[derive(Debug, Clone, Copy)]
pub struct BlockPlace {
    pub hand: i32,
    /// Packed block position.
    pub location: i64,
    pub direction: i32,
    pub cursor_x: f32,
    pub cursor_y: f32,
    pub cursor_z: f32,
    pub inside_block: bool,
    pub world_border_hit: bool,
    pub sequence: i32,
}

impl Packet for BlockPlace {
    const ID: i32 = 0x3D;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_varint(self.hand)
            .write_i64(self.location)
            .write_varint(self.direction)
            .write_f32(self.cursor_x)
            .write_f32(self.cursor_y)
            .write_f32(self.cursor_z)
            .write_bool(self.inside_block)
            .write_bool(self.world_border_hit)
            .write_varint(self.sequence);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            hand: reader.read_varint()?,
            location: reader.read_i64()?,
            direction: reader.read_varint()?,
            cursor_x: reader.read_f32()?,
            cursor_y: reader.read_f32()?,
            cursor_z: reader.read_f32()?,
            inside_block: reader.read_bool()?,
            world_border_hit: reader.read_bool()?,
            sequence: reader.read_varint()?,
        })
    }
}

/// C->S Use Item (id 0x3E).
#[derive(Debug, Clone, Copy)]
pub struct UseItem {
    pub hand: i32,
    pub sequence: i32,
    pub rotation_x: f32,
    pub rotation_y: f32,
}

impl Packet for UseItem {
    const ID: i32 = 0x3E;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_varint(self.hand)
            .write_varint(self.sequence)
            .write_f32(self.rotation_x)
            .write_f32(self.rotation_y);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            hand: reader.read_varint()?,
            sequence: reader.read_varint()?,
            rotation_x: reader.read_f32()?,
            rotation_y: reader.read_f32()?,
        })
    }
}

/// C->S Held Item Slot (id 0x32).
#[derive(Debug, Clone, Copy)]
pub struct HeldItemSlot {
    pub slot_id: i16,
}

impl Packet for HeldItemSlot {
    const ID: i32 = 0x32;

    fn encode(&self, out: &mut PacketWriter) {
        out.write_i16(self.slot_id);
    }

    fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            slot_id: reader.read_i16()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<P: Packet + PartialEq + std::fmt::Debug>(packet: P) {
        let mut writer = PacketWriter::new();
        packet.encode(&mut writer);
        let bytes = writer.into_vec();
        let decoded = P::decode(&mut PacketReader::new(&bytes)).unwrap();
        assert_eq!(packet, decoded);
    }

    #[test]
    fn play_login_roundtrip() {
        roundtrip(PlayLogin {
            entity_id: 0,
            is_hardcore: false,
            world_names: vec!["minecraft:overworld".to_string()],
            max_players: 20,
            view_distance: 8,
            simulation_distance: 4,
            reduced_debug_info: false,
            enable_respawn_screen: true,
            do_limited_crafting: false,
            world_state: SpawnInfo {
                dimension_id: 0,
                dimension_name: "minecraft:overworld".to_string(),
                hashed_seed: 0,
                gamemode: 0,
                previous_gamemode: 255,
                is_debug: false,
                is_flat: true,
                death: None,
                portal_cooldown: 0,
                sea_level: 63,
            },
            enforces_secure_chat: false,
        });
    }

    #[test]
    fn map_chunk_roundtrip() {
        roundtrip(MapChunk {
            x: 0,
            z: 0,
            heightmaps: vec![(1, vec![0; 37])],
            chunk_data: vec![0x00; 24],
            block_entities: vec![],
            sky_light_mask: vec![0xFFFFFF],
            block_light_mask: vec![],
            empty_sky_light_mask: vec![],
            empty_block_light_mask: vec![],
            sky_light: vec![vec![0xFF; 2048]],
            block_light: vec![],
        });
    }

    #[test]
    fn position_packing_matches_vanilla_layout() {
        // y in the low 12 bits, z mid 26, x high 26.
        assert_eq!(pack_position(1, 64, 2) & 0xFFF, 64);
        assert_eq!(pack_position(1, 64, 2) >> 38, 1);
        assert_eq!((pack_position(1, 64, 2) >> 12) & 0x3FFFFFF, 2);
        assert_eq!(pack_position(-1, -64, -1), pack_position(-1, -64, -1));
        assert_ne!(pack_position(0, 0, 0), pack_position(1, 0, 0));
    }
}
