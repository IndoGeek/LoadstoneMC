//! Deterministic terrain generation.
//!
//! The world is a small seed-driven heightmap: three octaves of value noise are
//! summed into a surface height, then each column is filled with the classic
//! bedrock / stone / dirt / grass layering. Because the height is a pure
//! function of `(seed, x, z)`, neighbouring chunks line up exactly at their
//! borders and the same seed always regenerates the same world.

use crate::encode::{BLOCK_AIR, BLOCK_BEDROCK, BLOCK_DIRT, BLOCK_GRASS_BLOCK, BLOCK_STONE, MIN_Y};
use crate::{
    Chunk, ChunkPos, ChunkSection, CHUNK_SIZE_X, CHUNK_SIZE_Z, SECTION_COUNT, SECTION_HEIGHT,
    SECTION_VOLUME,
};

/// Surface height around which the noise is centred.
pub const BASE_HEIGHT: i32 = 64;
/// Highest and lowest surface the generator will produce.
pub const MIN_SURFACE: i32 = MIN_Y + 8;
pub const MAX_SURFACE: i32 = 120;
/// Dirt depth below the grass surface.
const DIRT_DEPTH: i32 = 3;

/// Generates block columns for a given seed.
#[derive(Debug, Clone, Copy)]
pub struct TerrainGenerator {
    seed: u64,
}

impl TerrainGenerator {
    pub fn new(seed: u64) -> Self {
        Self { seed }
    }

    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// The world Y of the topmost solid block of a column.
    pub fn surface_height(&self, x: i32, z: i32) -> i32 {
        let broad = value_noise(self.seed, x as f64 / 64.0, z as f64 / 64.0);
        let medium = value_noise(self.seed ^ 0x1234_5678, x as f64 / 32.0, z as f64 / 32.0);
        let fine = value_noise(self.seed ^ 0xABCD_EF01, x as f64 / 16.0, z as f64 / 16.0);

        let height =
            BASE_HEIGHT as f64 + (broad - 0.5) * 24.0 + (medium - 0.5) * 8.0 + (fine - 0.5) * 3.0;
        (height.round() as i32).clamp(MIN_SURFACE, MAX_SURFACE)
    }

    /// The block state at world coordinates.
    pub fn block(&self, x: i32, y: i32, z: i32) -> u16 {
        if y == MIN_Y {
            return BLOCK_BEDROCK;
        }
        let surface = self.surface_height(x, z);
        if y > surface {
            BLOCK_AIR
        } else if y == surface {
            BLOCK_GRASS_BLOCK
        } else if y >= surface - DIRT_DEPTH {
            BLOCK_DIRT
        } else {
            BLOCK_STONE
        }
    }

    /// Generates a full chunk column at `pos`.
    pub fn chunk(&self, pos: ChunkPos) -> Chunk {
        let mut sections: Vec<ChunkSection> = (0..SECTION_COUNT)
            .map(|_| ChunkSection {
                block_states: vec![BLOCK_AIR; SECTION_VOLUME],
                non_air_blocks: 0,
            })
            .collect();

        for local_x in 0..CHUNK_SIZE_X {
            let world_x = pos.x * CHUNK_SIZE_X as i32 + local_x as i32;
            for local_z in 0..CHUNK_SIZE_Z {
                let world_z = pos.z * CHUNK_SIZE_Z as i32 + local_z as i32;
                let surface = self.surface_height(world_x, world_z);
                for y in MIN_Y..=surface {
                    let state = self.block(world_x, y, world_z);
                    let offset = (y - MIN_Y) as usize;
                    let section = &mut sections[offset / SECTION_HEIGHT];
                    section.block_states[(offset % SECTION_HEIGHT * CHUNK_SIZE_Z + local_z)
                        * CHUNK_SIZE_X
                        + local_x] = state;
                }
            }
        }

        // Collapse all-air sections back to the compact empty representation.
        for section in &mut sections {
            section.recount();
            if section.non_air_blocks == 0 {
                section.block_states.clear();
            }
        }

        Chunk { pos, sections }
    }
}

/// Smoothstep interpolation weight.
fn smooth(t: f64) -> f64 {
    t * t * (3.0 - 2.0 * t)
}

/// Hash a lattice point to a deterministic value in `[0, 1)`.
fn hash(seed: u64, x: i64, z: i64) -> f64 {
    let mut h = seed
        .wrapping_add((x as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9))
        .wrapping_add((z as u64).wrapping_mul(0x94D0_49BB_1331_11EB));
    h ^= h >> 30;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
    h ^= h >> 31;
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// Bilinearly interpolated value noise at a lattice coordinate.
fn value_noise(seed: u64, x: f64, z: f64) -> f64 {
    let x0 = x.floor();
    let z0 = z.floor();
    let tx = smooth(x - x0);
    let tz = smooth(z - z0);
    let (x0, z0) = (x0 as i64, z0 as i64);

    let top = hash(seed, x0, z0) * (1.0 - tx) + hash(seed, x0 + 1, z0) * tx;
    let bottom = hash(seed, x0, z0 + 1) * (1.0 - tx) + hash(seed, x0 + 1, z0 + 1) * tx;
    top * (1.0 - tz) + bottom * tz
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_is_deterministic() {
        let a = TerrainGenerator::new(42);
        let b = TerrainGenerator::new(42);
        let c = TerrainGenerator::new(43);
        for x in -40..40 {
            assert_eq!(a.surface_height(x, x * 3), b.surface_height(x, x * 3));
        }
        assert_ne!(
            (0..40).map(|x| a.surface_height(x, 0)).collect::<Vec<_>>(),
            (0..40).map(|x| c.surface_height(x, 0)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn surface_is_within_bounds_and_layered() {
        let generator = TerrainGenerator::new(7);
        for x in -64..64 {
            for z in -64..64 {
                let surface = generator.surface_height(x, z);
                assert!((MIN_SURFACE..=MAX_SURFACE).contains(&surface));
                assert_eq!(generator.block(x, surface, z), BLOCK_GRASS_BLOCK);
                assert_eq!(generator.block(x, surface - 1, z), BLOCK_DIRT);
                assert_eq!(generator.block(x, surface - DIRT_DEPTH, z), BLOCK_DIRT);
                assert_eq!(generator.block(x, surface - DIRT_DEPTH - 1, z), BLOCK_STONE);
                assert_eq!(generator.block(x, surface + 1, z), BLOCK_AIR);
                assert_eq!(generator.block(x, MIN_Y, z), BLOCK_BEDROCK);
            }
        }
    }

    #[test]
    fn chunk_matches_the_block_function_across_borders() {
        let generator = TerrainGenerator::new(99);
        // Two horizontally adjacent chunks: their shared border is x = -1 / 0.
        let left = generator.chunk(ChunkPos { x: -1, z: 2 });
        let right = generator.chunk(ChunkPos { x: 0, z: 2 });
        assert_eq!(left.sections.len(), SECTION_COUNT);

        for (chunk, base_x) in [(&left, -16), (&right, 0)] {
            for local_x in 0..CHUNK_SIZE_X as i32 {
                for local_z in 0..CHUNK_SIZE_Z as i32 {
                    let world_x = base_x + local_x;
                    let world_z = 32 + local_z;
                    let surface = generator.surface_height(world_x, world_z);
                    assert_eq!(
                        chunk.get_block(world_x, surface, world_z),
                        BLOCK_GRASS_BLOCK
                    );
                    assert_eq!(chunk.get_block(world_x, surface + 1, world_z), BLOCK_AIR);
                    assert_eq!(chunk.get_block(world_x, MIN_Y, world_z), BLOCK_BEDROCK);
                    for y in [surface - 1, surface - 6, surface - 30] {
                        assert_eq!(
                            chunk.get_block(world_x, y, world_z),
                            generator.block(world_x, y, world_z)
                        );
                    }
                }
            }
        }

        // The left chunk's last column and the right chunk's first column both
        // come from the same height function, so they sit at the same surface.
        let surface_left = generator.surface_height(-1, 32);
        assert_eq!(left.get_block(-1, surface_left, 32), BLOCK_GRASS_BLOCK);
        assert_eq!(
            right.get_block(0, generator.surface_height(0, 32), 32),
            BLOCK_GRASS_BLOCK
        );
    }
}
