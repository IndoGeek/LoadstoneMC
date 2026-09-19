//! World model, chunk storage and (eventually) Anvil region I/O.

pub mod encode;
pub mod world;

pub use world::World;

pub const CHUNK_SIZE_X: usize = 16;
pub const CHUNK_SIZE_Z: usize = 16;
pub const SECTION_HEIGHT: usize = 16;
/// Number of 16-block sections in an overworld column (height 384, min Y -64).
pub const SECTION_COUNT: usize = 24;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChunkPos {
    pub x: i32,
    pub z: i32,
}

#[derive(Debug, Default)]
pub struct Chunk {
    pub pos: ChunkPos,
    pub sections: Vec<ChunkSection>,
}

#[derive(Debug, Default, Clone)]
pub struct ChunkSection {
    /// Paletted block state ids, indexed as (y * 16 + z) * 16 + x.
    pub block_states: Vec<u16>,
    pub non_air_blocks: i16,
}
