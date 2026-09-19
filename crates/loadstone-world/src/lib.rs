//! World model, chunk storage, terrain generation and Anvil region I/O.

pub mod anvil;
pub mod blocks;
pub mod encode;
pub mod terrain;
pub mod world;

pub use world::World;

pub const CHUNK_SIZE_X: usize = 16;
pub const CHUNK_SIZE_Z: usize = 16;
pub const SECTION_HEIGHT: usize = 16;
/// Number of 16-block sections in an overworld column (height 384, min Y -64).
pub const SECTION_COUNT: usize = 24;
/// Blocks in one section (16x16x16).
pub const SECTION_VOLUME: usize = CHUNK_SIZE_X * CHUNK_SIZE_Z * SECTION_HEIGHT;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ChunkPos {
    pub x: i32,
    pub z: i32,
}

#[derive(Debug, Default, Clone)]
pub struct Chunk {
    pub pos: ChunkPos,
    pub sections: Vec<ChunkSection>,
}

impl Chunk {
    /// An all-air chunk column with the full section count.
    pub fn empty(pos: ChunkPos) -> Self {
        Self {
            pos,
            sections: vec![ChunkSection::default(); SECTION_COUNT],
        }
    }

    /// The block state at world coordinates, treating out-of-range Y as air.
    pub fn get_block(&self, x: i32, y: i32, z: i32) -> u16 {
        let Some(section) = self.section_at(y) else {
            return encode::BLOCK_AIR;
        };
        let local_y = (y - encode::MIN_Y).rem_euclid(SECTION_HEIGHT as i32) as usize;
        let local_x = x.rem_euclid(CHUNK_SIZE_X as i32) as usize;
        let local_z = z.rem_euclid(CHUNK_SIZE_Z as i32) as usize;
        section.state(local_y, local_z, local_x)
    }

    /// Sets the block state at world coordinates. Out-of-range Y is ignored.
    pub fn set_block(&mut self, x: i32, y: i32, z: i32, state: u16) {
        let Some(section) = self.section_at_mut(y) else {
            return;
        };
        let local_y = (y - encode::MIN_Y).rem_euclid(SECTION_HEIGHT as i32) as usize;
        let local_x = x.rem_euclid(CHUNK_SIZE_X as i32) as usize;
        let local_z = z.rem_euclid(CHUNK_SIZE_Z as i32) as usize;
        section.set(local_y, local_z, local_x, state);
    }

    fn section_at(&self, y: i32) -> Option<&ChunkSection> {
        let index = (y - encode::MIN_Y).div_euclid(SECTION_HEIGHT as i32);
        if index < 0 || index as usize >= self.sections.len() {
            return None;
        }
        self.sections.get(index as usize)
    }

    fn section_at_mut(&mut self, y: i32) -> Option<&mut ChunkSection> {
        let index = (y - encode::MIN_Y).div_euclid(SECTION_HEIGHT as i32);
        if index < 0 || index as usize >= self.sections.len() {
            return None;
        }
        self.sections.get_mut(index as usize)
    }

    /// Whether every section is air.
    pub fn is_empty(&self) -> bool {
        self.sections
            .iter()
            .all(|section| section.non_air_blocks == 0)
    }
}

#[derive(Debug, Default, Clone)]
pub struct ChunkSection {
    /// Paletted block state ids, indexed as (y * 16 + z) * 16 + x. An empty
    /// vector means the whole section is air.
    pub block_states: Vec<u16>,
    pub non_air_blocks: i16,
}

impl ChunkSection {
    pub fn state(&self, local_y: usize, local_z: usize, local_x: usize) -> u16 {
        if self.block_states.is_empty() {
            return encode::BLOCK_AIR;
        }
        self.block_states[Self::index(local_y, local_z, local_x)]
    }

    pub fn set(&mut self, local_y: usize, local_z: usize, local_x: usize, state: u16) {
        if self.block_states.is_empty() {
            if state == encode::BLOCK_AIR {
                return;
            }
            self.block_states = vec![encode::BLOCK_AIR; SECTION_VOLUME];
        }
        self.block_states[Self::index(local_y, local_z, local_x)] = state;
        self.recount();
    }

    /// Recomputes [`Self::non_air_blocks`] from the current contents.
    pub fn recount(&mut self) {
        self.non_air_blocks = self
            .block_states
            .iter()
            .filter(|&&state| state != encode::BLOCK_AIR)
            .count() as i16;
    }

    fn index(local_y: usize, local_z: usize, local_x: usize) -> usize {
        (local_y * CHUNK_SIZE_Z + local_z) * CHUNK_SIZE_X + local_x
    }
}
