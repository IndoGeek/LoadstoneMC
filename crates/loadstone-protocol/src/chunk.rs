//! Decoding for the chunk column, so the encoder can be checked against bytes a
//! real server sent.
//!
//! `crates/loadstone-world/src/encode.rs` writes this format; nothing here writes
//! it. The reader lives in this crate because the two things that go wrong with a
//! chunk — a field width and a bit-packing scheme — are both invisible to a round
//! trip through one implementation: the same misreading of the protocol agrees
//! with itself perfectly. `tests/vanilla_fixtures.rs` therefore runs this decoder
//! over a chunk packet a real 1.21.11 server sent, and asserts the geometry that
//! comes out of it.
//!
//! Two different packing schemes meet in one packet, and neither is written down
//! in the protocol dump:
//!
//! - **Block and biome containers use the spanning scheme.** Values are packed
//!   contiguously into the bit stream, least significant bit first, and a value
//!   may straddle a `long` boundary. The array holds exactly
//!   `ceil(entries * bits / 64)` longs.
//! - **Heightmaps use the per-long scheme.** Each `long` holds `64 / bits` whole
//!   values and a value never straddles a boundary, so the array is
//!   `ceil(entries / (64 / bits))` longs. For 256 nine-bit entries that is 37,
//!   not the 36 the spanning scheme would give.
//!
//! Both were settled by measuring a real chunk rather than by reading a
//! specification: the captured heightmap is 37 longs and decodes to a constant 5,
//! which is exactly the capture world's grass surface minus its minimum Y. A
//! container of the right size with the wrong packing still yields plausible
//! numbers, which is why the tests assert geometry and not shape.
//!
//! Note the block container's scheme is only *distinguishable* when `bits` does
//! not divide 64; at the 4 bits a small palette uses, both schemes agree byte for
//! byte. The captured block container is 4 bits, so it cannot settle that
//! question — the heightmap is what does.

use crate::error::ProtocolError;
use crate::read::PacketReader;

/// Blocks in one chunk section: 16 × 16 × 16.
pub const SECTION_BLOCK_COUNT: usize = 4096;
/// Biomes in one section: 4 × 4 × 4.
pub const SECTION_BIOME_COUNT: usize = 64;
/// Columns in a chunk, which is how many entries a heightmap holds.
pub const CHUNK_COLUMN_COUNT: usize = 256;
/// Sections in a full-height chunk (384 blocks / 16).
pub const OVERWORLD_SECTION_COUNT: usize = 24;

/// Heightmap kinds, from the protocol dump.
pub const HEIGHTMAP_WORLD_SURFACE: i32 = 1;
pub const HEIGHTMAP_MOTION_BLOCKING: i32 = 4;

/// Index of a block inside a section: vanilla orders the axes `(y, z, x)`.
pub fn section_index(x: usize, y: usize, z: usize) -> usize {
    (y << 8) | (z << 4) | x
}

/// Pack values contiguously, least significant bit first, across long
/// boundaries.
///
/// Only the tests need this half. An encoder that is never used in anger is an
/// encoder nobody checks, so this crate keeps the reader and leaves writing to
/// `loadstone-world`.
#[cfg(test)]
fn pack_spanning(values: &[u16], bits: u32) -> Vec<i64> {
    let total_bits = values.len() * bits as usize;
    let mut longs = vec![0u64; total_bits.div_ceil(64)];
    for (index, &value) in values.iter().enumerate() {
        let start = index * bits as usize;
        let long_index = start / 64;
        let offset = start % 64;
        let value = (value as u64) & ((1u64 << bits) - 1);
        // A value can run past the end of its first long; the high part goes into
        // the next one. This is the case a "one value per long" implementation
        // gets wrong only for some bit widths, which is the worst kind of wrong.
        longs[long_index] |= value << offset;
        let end = offset + bits as usize;
        if end > 64 {
            let spilled = end - 64;
            longs[long_index + 1] |= value >> (bits as usize - spilled);
        }
    }
    longs.into_iter().map(|word| word as i64).collect()
}

fn unpack_spanning(longs: &[i64], bits: u32, count: usize) -> Vec<u16> {
    let mask = (1u64 << bits) - 1;
    let mut values = Vec::with_capacity(count);
    for index in 0..count {
        let start = index * bits as usize;
        let long_index = start / 64;
        let offset = start % 64;
        let mut value = (longs[long_index] as u64) >> offset;
        let end = offset + bits as usize;
        if end > 64 && long_index + 1 < longs.len() {
            let spilled = end - 64;
            value |= (longs[long_index + 1] as u64) << (bits as usize - spilled);
        }
        values.push((value & mask) as u16);
    }
    values
}

/// Pack values so that each long holds whole values and none straddles.
#[cfg(test)]
fn pack_per_long(values: &[u16], bits: u32) -> Vec<i64> {
    let per_long = 64 / bits as usize;
    let mut longs = vec![0u64; values.len().div_ceil(per_long)];
    for (index, &value) in values.iter().enumerate() {
        let long_index = index / per_long;
        let offset = (index % per_long) * bits as usize;
        longs[long_index] |= ((value as u64) & ((1u64 << bits) - 1)) << offset;
    }
    longs.into_iter().map(|word| word as i64).collect()
}

fn unpack_per_long(longs: &[i64], bits: u32, count: usize) -> Vec<u16> {
    let per_long = 64 / bits as usize;
    let mask = (1u64 << bits) - 1;
    (0..count)
        .map(|index| {
            let long_index = index / per_long;
            let offset = (index % per_long) * bits as usize;
            if long_index >= longs.len() {
                return 0;
            }
            (((longs[long_index] as u64) >> offset) & mask) as u16
        })
        .collect()
}

/// A palette-encoded array of ids, as used for blocks and biomes.
///
/// Values come out as *global* ids whether the wire used a palette or the direct
/// encoding, so a caller never has to know which one was used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PalettedContainer {
    pub values: Vec<u16>,
}

impl PalettedContainer {
    fn uniform(value: u16, count: usize) -> Self {
        Self {
            values: vec![value; count],
        }
    }

    pub fn decode(reader: &mut PacketReader<'_>, count: usize) -> Result<Self, ProtocolError> {
        let bits = reader.read_u8()? as u32;
        if bits == 0 {
            // A single value for the whole container, and no array at all.
            return Ok(Self::uniform(reader.read_varint()? as u16, count));
        }
        if bits > 8 {
            // Direct encoding: a fixed-width stream of global ids, no palette.
            let long_count = (count * bits as usize).div_ceil(64);
            let mut longs = Vec::with_capacity(long_count);
            for _ in 0..long_count {
                longs.push(reader.read_i64()?);
            }
            return Ok(Self {
                values: unpack_spanning(&longs, bits, count),
            });
        }

        let palette_len = reader.read_varint()?;
        if palette_len <= 0 {
            return Err(ProtocolError::InvalidStringLength(palette_len));
        }
        let mut palette = Vec::with_capacity(palette_len as usize);
        for _ in 0..palette_len {
            palette.push(reader.read_varint()? as u16);
        }
        let long_count = (count * bits as usize).div_ceil(64);
        let mut longs = Vec::with_capacity(long_count);
        for _ in 0..long_count {
            longs.push(reader.read_i64()?);
        }
        // Indices, resolved here. Returning them unresolved would look fine —
        // they are small numbers, and small numbers are valid block ids.
        Ok(Self {
            values: unpack_spanning(&longs, bits, count)
                .into_iter()
                .map(|index| palette.get(index as usize).copied().unwrap_or(0))
                .collect(),
        })
    }
}

/// One 16×16×16 slice of a chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkSection {
    /// How many blocks in the section are not air. The client uses this to skip
    /// empty sections, so a wrong value makes it render nothing.
    pub non_air: i16,
    pub blocks: PalettedContainer,
    pub biomes: PalettedContainer,
}

impl ChunkSection {
    /// Read one section. Sections carry no length prefix of their own, so this is
    /// read in a loop until the chunk buffer runs out — which is also the check
    /// that the buffer is the length the packet said it was.
    pub fn decode(reader: &mut PacketReader<'_>) -> Result<Self, ProtocolError> {
        Ok(Self {
            non_air: reader.read_i16()?,
            blocks: PalettedContainer::decode(reader, SECTION_BLOCK_COUNT)?,
            biomes: PalettedContainer::decode(reader, SECTION_BIOME_COUNT)?,
        })
    }
}

/// A column of surface heights, packed per long.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heightmap {
    pub kind: i32,
    pub values: Vec<u16>,
}

impl Heightmap {
    pub const BITS: u32 = 9;

    /// Decode one heightmap from the packed words a packet carries.
    pub fn from_packed(kind: i32, packed: &[i64]) -> Self {
        Self {
            kind,
            values: unpack_per_long(packed, Self::BITS, CHUNK_COLUMN_COUNT),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spanning_packing_survives_values_that_straddle_a_long() {
        // 5 bits gives 12 values per long and 4 unused bits, so every eighth
        // value starts near the end of one long and finishes in the next. This is
        // the only kind of width where spanning and per-long packing disagree,
        // which makes it the case worth pinning.
        let values: Vec<u16> = (0..64).map(|index| (index * 7) % 32).collect();
        let packed = pack_spanning(&values, 5);
        assert_eq!(packed.len(), (64 * 5usize).div_ceil(64));
        assert_eq!(unpack_spanning(&packed, 5, values.len()), values);

        // The same values under per-long packing must *not* come back, otherwise
        // this test would pass whichever scheme the code used.
        assert_ne!(unpack_per_long(&packed, 5, values.len()), values);
    }

    #[test]
    fn heightmap_packing_uses_whole_values_per_long() {
        // 9 bits into 64 gives 7 whole values per long: 256 columns is 37 longs.
        // Spanning packing would need 36, and the real packet has 37.
        let values: Vec<u16> = (0..CHUNK_COLUMN_COUNT)
            .map(|index| (index as u16) % 320)
            .collect();
        let packed = pack_per_long(&values, Heightmap::BITS);
        assert_eq!(packed.len(), 37);
        assert_eq!(
            unpack_per_long(&packed, Heightmap::BITS, values.len()),
            values
        );

        // The same bytes read as a spanning stream must *not* give the heights
        // back, or this test would pass under either scheme.
        let spanning = unpack_spanning(&packed, Heightmap::BITS, CHUNK_COLUMN_COUNT);
        assert_ne!(spanning, values);
    }

    #[test]
    fn section_index_orders_axes_y_then_z_then_x() {
        assert_eq!(section_index(0, 0, 0), 0);
        assert_eq!(section_index(1, 0, 0), 1);
        assert_eq!(section_index(0, 0, 1), 16);
        assert_eq!(section_index(0, 1, 0), 256);
        assert_eq!(section_index(15, 15, 15), 4095);
    }

    #[test]
    fn a_single_value_container_needs_no_array() {
        // Bits = 0, then the one value: no palette and no packed longs follow.
        let bytes = [0u8, 42];
        let mut reader = PacketReader::new(&bytes);
        let container = PalettedContainer::decode(&mut reader, 4).unwrap();
        assert_eq!(container.values, vec![42, 42, 42, 42]);
        assert!(reader.is_empty());
    }

    #[test]
    fn palette_indices_are_resolved_into_ids() {
        // 4 bits, a palette of two ids, then a full section of alternating
        // indices.
        let indices: Vec<u16> = (0..SECTION_BLOCK_COUNT)
            .map(|index| (index % 2) as u16)
            .collect();
        let mut bytes = vec![4u8, 2, 85, 1];
        for long in pack_spanning(&indices, 4) {
            bytes.extend_from_slice(&(long as u64).to_be_bytes());
        }

        let mut reader = PacketReader::new(&bytes);
        let container = PalettedContainer::decode(&mut reader, SECTION_BLOCK_COUNT).unwrap();
        assert!(reader.is_empty());
        // The ids, not the indices: 0 and 1 are believable block ids, so a decoder
        // that returned indices would look right and be wrong.
        assert_eq!(container.values[0], 85);
        assert_eq!(container.values[1], 1);
        assert!(container
            .values
            .iter()
            .all(|&value| value == 85 || value == 1));
    }
}
