//! Wire encoding for chunks: paletted-block sections, heightmaps and light.
//!
//! Format notes (1.21.x):
//! - A chunk column is 24 sections (min Y -64, height 384), sent bottom-up
//!   with no count prefix. Each section is `u16 non-empty-count`, `u16
//!   fluid-count`, a block-states paletted container and a biomes paletted
//!   container.
//! - Paletted containers with one value use "bits per entry 0" plus a single
//!   global id. Indirect palettes pack 4-8 bit indices LSB-first into raw
//!   i64 words with no VarInt length prefix.

use loadstone_protocol::PacketWriter;

use crate::{Chunk, CHUNK_SIZE_X, CHUNK_SIZE_Z, SECTION_COUNT, SECTION_HEIGHT};

/// Lowest block Y of the overworld.
pub const MIN_Y: i32 = -64;
/// Total block height of the overworld.
pub const WORLD_HEIGHT: i32 = 384;
/// Vanilla 1.21.11 global block state ids used by the flat world.
pub const BLOCK_AIR: u16 = 0;
pub const BLOCK_GRASS_BLOCK: u16 = 9;
pub const BLOCK_DIRT: u16 = 10;
pub const BLOCK_COBBLESTONE: u16 = 14;
pub const BLOCK_BEDROCK: u16 = 85;

/// The flat spawn platform: bedrock at the bottom, dirt filling the middle,
/// a grass top at world Y 64 (player spawns at Y 65).
pub const SPAWN_X: f64 = 8.5;
pub const SPAWN_Z: f64 = 8.5;
pub const SPAWN_Y: f64 = 65.0;
pub const GRASS_TOP_Y: i32 = 64;

/// Builds the flat world chunk template centered on `(cx, cz)`. Every column
/// is identical, so heightmap values are constant (grass top at `GRASS_TOP_Y`).
pub fn flat_chunk(cx: i32, cz: i32) -> Chunk {
    let mut chunk = Chunk {
        pos: crate::ChunkPos { x: cx, z: cz },
        sections: Vec::with_capacity(SECTION_COUNT),
    };

    let filled = |state: u16| crate::ChunkSection {
        block_states: vec![state; CHUNK_SIZE_X * CHUNK_SIZE_Z * SECTION_HEIGHT],
        non_air_blocks: 4096,
    };

    for _ in 0..SECTION_COUNT {
        chunk.sections.push(crate::ChunkSection::default());
    }
    chunk.sections[0] = filled(BLOCK_BEDROCK);
    for section in &mut chunk.sections[1..8] {
        *section = filled(BLOCK_DIRT);
    }

    // Top section: a grass cap on layer 0, air above within the section.
    let mut top = chunk.sections[8].clone();
    top.block_states = vec![BLOCK_AIR; CHUNK_SIZE_X * CHUNK_SIZE_Z * SECTION_HEIGHT];
    for z in 0..CHUNK_SIZE_Z {
        for x in 0..CHUNK_SIZE_X {
            top.block_states[z * CHUNK_SIZE_X + x] = BLOCK_GRASS_BLOCK;
        }
    }
    top.non_air_blocks = 256;
    chunk.sections[8] = top;

    chunk
}

fn index(y: usize, z: usize, x: usize) -> usize {
    (y * CHUNK_SIZE_Z + z) * CHUNK_SIZE_X + x
}

fn write_paletted_container(out: &mut PacketWriter, states: &[u16], biome_global_id: u32) {
    let mut palette: Vec<u16> = Vec::new();
    for &state in states {
        if !palette.contains(&state) {
            palette.push(state);
        }
    }

    if palette.len() <= 1 {
        let value = palette.first().copied().unwrap_or(BLOCK_AIR);
        out.write_u8(0).write_varint(i32::from(value));
    } else {
        let bits = match palette.len() {
            2..=16 => 4,
            17..=32 => 5,
            33..=64 => 6,
            65..=128 => 7,
            129..=256 => 8,
            _ => 15,
        };
        out.write_u8(bits as u8);
        if bits < 15 {
            out.write_varint(palette.len() as i32);
            for state in &palette {
                out.write_varint(i32::from(*state));
            }
        }
        pack_indices(out, states, &palette, bits);
    }

    // Biome container: always the single-value form for now.
    out.write_u8(0).write_varint(biome_global_id as i32);
}

fn pack_indices(out: &mut PacketWriter, states: &[u16], palette: &[u16], bits: usize) {
    let values_per_long = 64 / bits;
    let num_longs = states.len().div_ceil(values_per_long);
    let mask = (1u64 << bits) - 1;
    let mut words = vec![0u64; num_longs];
    for (i, &state) in states.iter().enumerate() {
        let index = if bits == 15 {
            u64::from(state)
        } else {
            palette.iter().position(|&value| value == state).unwrap() as u64
        };
        words[i / values_per_long] |= (index & mask) << ((i % values_per_long) * bits);
    }
    for word in words {
        out.write_i64(word as i64);
    }
}

/// Serializes a chunk column as the `chunkData` field of the map chunk packet.
pub fn encode_chunk_column(chunk: &Chunk, biome_global_id: u32) -> Vec<u8> {
    let mut out = PacketWriter::with_capacity(2048);
    for section in &chunk.sections {
        out.write_i16(section.non_air_blocks).write_i16(0);
        write_paletted_container(&mut out, &section.block_states, biome_global_id);
    }
    out.into_vec()
}

/// Height per column (world Y of the topmost non-air block + 1 - min Y),
/// stored in the protocol's heightmap layout. Index order is z * 16 + x.
pub fn column_heights(chunk: &Chunk) -> Vec<u16> {
    let mut heights = vec![0u16; CHUNK_SIZE_X * CHUNK_SIZE_Z];
    for z in 0..CHUNK_SIZE_Z {
        for x in 0..CHUNK_SIZE_X {
            'section: for (section_index, section) in chunk.sections.iter().enumerate().rev() {
                if section.non_air_blocks == 0 {
                    continue;
                }
                for ly in (0..SECTION_HEIGHT).rev() {
                    if section.block_states[index(ly, z, x)] != BLOCK_AIR {
                        let world_y = MIN_Y + (section_index * SECTION_HEIGHT + ly) as i32;
                        heights[z * CHUNK_SIZE_X + x] = (world_y - MIN_Y + 1) as u16;
                        break 'section;
                    }
                }
            }
        }
    }
    heights
}

/// Packs per-column heights into the 37-long wire array (bits 9, LSB-first).
pub fn pack_heightmap(heights: &[u16], world_height: i32) -> Vec<i64> {
    let bits = (world_height as u32 + 1).next_power_of_two().ilog2() as usize;
    let values_per_long = 64 / bits;
    let num_longs = 256usize.div_ceil(values_per_long);
    let mask = (1u64 << bits) - 1;
    let mut words = vec![0u64; num_longs];
    for (i, &height) in heights.iter().enumerate() {
        words[i / values_per_long] |= (u64::from(height) & mask) << ((i % values_per_long) * bits);
    }
    words.into_iter().map(|word| word as i64).collect()
}

/// Returns full-brightness sky light for every section: 2048 bytes each.
pub fn full_sky_light(section_count: usize) -> Vec<Vec<u8>> {
    vec![vec![0xFFu8; 16 * 16 * 16 / 2]; section_count]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_chunk_has_expected_layout() {
        let chunk = flat_chunk(0, 0);
        assert_eq!(chunk.sections.len(), SECTION_COUNT);
        assert_eq!(chunk.sections[0].block_states[0], BLOCK_BEDROCK);
        assert_eq!(chunk.sections[7].block_states[0], BLOCK_DIRT);
        assert_eq!(
            chunk.sections[8].block_states[index(0, 0, 0)],
            BLOCK_GRASS_BLOCK
        );
        assert_eq!(chunk.sections[8].block_states[index(15, 15, 15)], BLOCK_AIR);
        assert_eq!(chunk.sections[8].non_air_blocks, 256);
        assert_eq!(chunk.sections[9].non_air_blocks, 0);
    }

    #[test]
    fn heightmap_uses_nine_bits_and_thirty_seven_longs() {
        let chunk = flat_chunk(0, 0);
        let heights = column_heights(&chunk);
        assert!(heights.iter().all(|&h| h == 129));
        let packed = pack_heightmap(&heights, WORLD_HEIGHT);
        assert_eq!(packed.len(), 37);
        // 129 fits in 9 bits; every word must stay a multiple of 129 << k bits.
    }

    #[test]
    fn section_count_matches_world() {
        assert_eq!(
            SECTION_COUNT,
            (WORLD_HEIGHT / SECTION_HEIGHT as i32) as usize
        );
    }
}
