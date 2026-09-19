//! Anvil region file I/O (`r.<x>.<z>.mca`).
//!
//! Layout (Minecraft 1.21.x):
//! - an 8 KiB header: 1024 location entries (3-byte sector offset + 1-byte
//!   sector count) followed by 1024 unused timestamps;
//! - chunk records from sector 2 onward, each a 4-byte length, a 1-byte
//!   compression id (2 = zlib) and the compressed, named-root chunk NBT.
//!
//! Only the block state, heightmap and biome data of a chunk are written;
//! entities, block entities, ticks and light are omitted (`isLightOn = 0`), so
//! the game relights and repopulates them on load. Palette names are mapped
//! through [`crate::blocks`], which covers every state the generator produces.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Error, ErrorKind, Result};
use std::path::{Path, PathBuf};

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use loadstone_protocol::{Nbt, PacketReader};

use crate::blocks;
use crate::encode::{self, BLOCK_AIR, MIN_Y, WORLD_HEIGHT};
use crate::{Chunk, ChunkPos, ChunkSection, SECTION_COUNT, SECTION_VOLUME};

/// `DataVersion` for 1.21.11 (from the vanilla `version.json`).
pub const DATA_VERSION: i32 = 4671;
/// Regions span 32x32 chunks.
pub const REGION_SIZE: i32 = 32;
/// The default biome written for every chunk.
const PLAINS: &str = "minecraft:plains";

/// The `region/` directory inside a world folder.
pub fn region_dir(world_dir: &Path) -> PathBuf {
    world_dir.join("region")
}

/// The region coordinate containing a chunk.
pub fn region_of(pos: ChunkPos) -> (i32, i32) {
    (pos.x.div_euclid(REGION_SIZE), pos.z.div_euclid(REGION_SIZE))
}

/// The path of the region file for a chunk.
pub fn region_path(world_dir: &Path, pos: ChunkPos) -> PathBuf {
    let (rx, rz) = region_of(pos);
    region_dir(world_dir).join(format!("r.{rx}.{rz}.mca"))
}

/// Reads every chunk present in one region file. A missing file is not an error.
pub fn read_region(path: &Path) -> Result<Vec<Chunk>> {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    if data.len() < 8192 {
        return Ok(Vec::new());
    }

    let mut chunks = Vec::new();
    for slot in 0..1024 {
        let entry = &data[slot * 4..slot * 4 + 4];
        let offset =
            (usize::from(entry[0]) << 16 | usize::from(entry[1]) << 8 | usize::from(entry[2]))
                * 4096;
        let sector_count = usize::from(entry[3]);
        if offset == 0 || sector_count == 0 || offset + 5 > data.len() {
            continue;
        }
        let length = u32::from_be_bytes([
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]) as usize;
        if length == 0 || offset + 4 + length > data.len() {
            continue;
        }
        let compression = data[offset + 4];
        let payload = &data[offset + 5..offset + 4 + length];
        let raw = decompress(compression, payload)?;
        let mut reader = PacketReader::new(&raw);
        let (_, nbt) = Nbt::read_root(&mut reader).map_err(invalid_nbt)?;
        chunks.push(chunk_from_nbt(&nbt)?);
    }
    Ok(chunks)
}

/// Writes every chunk into its region file, replacing the region contents.
///
/// Callers pass all known chunks for each affected region; the region is
/// rewritten from scratch, so omitting a chunk drops it from disk.
pub fn write_chunks(world_dir: &Path, chunks: &[Chunk], last_update: i64) -> Result<()> {
    let mut by_region: BTreeMap<(i32, i32), Vec<&Chunk>> = BTreeMap::new();
    for chunk in chunks {
        by_region
            .entry(region_of(chunk.pos))
            .or_default()
            .push(chunk);
    }

    let dir = region_dir(world_dir);
    fs::create_dir_all(&dir)?;
    for ((rx, rz), region_chunks) in by_region {
        let path = dir.join(format!("r.{rx}.{rz}.mca"));
        write_region(&path, &region_chunks, last_update)?;
    }
    Ok(())
}

fn write_region(path: &Path, chunks: &[&Chunk], last_update: i64) -> Result<()> {
    let mut header = vec![0u8; 8192];
    let mut body = Vec::new();
    let mut next_sector = 2usize;

    for chunk in chunks {
        let nbt = chunk_to_nbt(chunk, last_update);
        let compressed = compress(&nbt.to_named_bytes(""))?;
        let length = compressed.len() + 1;
        let total = 4 + length;
        let sectors = total.div_ceil(4096);

        let slot = (chunk.pos.x.rem_euclid(REGION_SIZE)
            + chunk.pos.z.rem_euclid(REGION_SIZE) * REGION_SIZE) as usize;
        header[slot * 4] = (next_sector >> 16) as u8;
        header[slot * 4 + 1] = (next_sector >> 8) as u8;
        header[slot * 4 + 2] = next_sector as u8;
        header[slot * 4 + 3] = sectors as u8;

        body.extend_from_slice(&(length as u32).to_be_bytes());
        body.push(2); // zlib
        body.extend_from_slice(&compressed);
        body.resize(body.len() + (sectors * 4096 - total), 0);
        next_sector += sectors;
    }

    header.extend_from_slice(&body);
    fs::write(path, header)
}

fn compress(data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Write;
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data)?;
    encoder.finish()
}

fn decompress(compression: u8, data: &[u8]) -> Result<Vec<u8>> {
    if compression != 2 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!("unsupported region compression {compression}"),
        ));
    }
    let mut decoder = ZlibDecoder::new(data);
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut out)?;
    Ok(out)
}

fn invalid_nbt(error: impl std::fmt::Display) -> Error {
    Error::new(ErrorKind::InvalidData, error.to_string())
}

/// Serializes a chunk to the region-file chunk NBT.
pub fn chunk_to_nbt(chunk: &Chunk, last_update: i64) -> Nbt {
    let heights = encode::column_heights(chunk);
    let heightmap = encode::pack_heightmap(&heights, WORLD_HEIGHT);
    let long_array = || Nbt::LongArray(heightmap.clone());

    let sections: Vec<Nbt> = chunk
        .sections
        .iter()
        .enumerate()
        .map(|(index, section)| {
            let section_y = (MIN_Y / 16 + index as i32) as i8;
            Nbt::compound([
                ("Y", Nbt::Byte(section_y)),
                ("block_states", block_states_to_nbt(section)),
                (
                    "biomes",
                    Nbt::compound([("palette", Nbt::List(vec![Nbt::String(PLAINS.into())]))]),
                ),
            ])
        })
        .collect();

    Nbt::compound([
        ("DataVersion", Nbt::Int(DATA_VERSION)),
        ("xPos", Nbt::Int(chunk.pos.x)),
        ("yPos", Nbt::Int(MIN_Y / 16)),
        ("zPos", Nbt::Int(chunk.pos.z)),
        ("Status", Nbt::String("minecraft:full".into())),
        ("LastUpdate", Nbt::Long(last_update)),
        ("InhabitedTime", Nbt::Long(0)),
        ("isLightOn", Nbt::Byte(0)),
        (
            "Heightmaps",
            Nbt::compound([
                ("WORLD_SURFACE", long_array()),
                ("MOTION_BLOCKING", long_array()),
                ("MOTION_BLOCKING_NO_LEAVES", long_array()),
                ("OCEAN_FLOOR", long_array()),
            ]),
        ),
        ("sections", Nbt::List(sections)),
        ("block_entities", Nbt::List(Vec::new())),
        ("block_ticks", Nbt::List(Vec::new())),
        ("fluid_ticks", Nbt::List(Vec::new())),
        (
            "PostProcessing",
            Nbt::List((0..SECTION_COUNT).map(|_| Nbt::List(Vec::new())).collect()),
        ),
        (
            "structures",
            Nbt::compound([
                (
                    "References",
                    Nbt::compound(std::iter::empty::<(&str, Nbt)>()),
                ),
                ("starts", Nbt::compound(std::iter::empty::<(&str, Nbt)>())),
            ]),
        ),
    ])
}

fn block_states_to_nbt(section: &ChunkSection) -> Nbt {
    let mut palette: Vec<u16> = Vec::new();
    for &state in &section.block_states {
        if !palette.contains(&state) {
            palette.push(state);
        }
    }
    if palette.is_empty() {
        palette.push(BLOCK_AIR);
    }

    let palette_nbt = Nbt::List(
        palette
            .iter()
            .map(|&state| {
                let def = blocks::block_def(state).unwrap_or_else(|| &blocks::SUPPORTED_BLOCKS[0]);
                let mut entries = vec![("Name".to_string(), Nbt::String(def.name.to_string()))];
                if let Some(snowy) = def.snowy {
                    entries.push((
                        "Properties".to_string(),
                        Nbt::compound([(
                            "snowy",
                            Nbt::String(if snowy { "true" } else { "false" }.to_string()),
                        )]),
                    ));
                }
                Nbt::Compound(entries)
            })
            .collect(),
    );

    if palette.len() == 1 {
        Nbt::compound([("palette", palette_nbt)])
    } else {
        Nbt::compound([
            ("palette", palette_nbt),
            (
                "data",
                Nbt::LongArray(pack_palette(&section.block_states, &palette)),
            ),
        ])
    }
}

/// Deserializes a chunk from region-file chunk NBT.
pub fn chunk_from_nbt(nbt: &Nbt) -> Result<Chunk> {
    let x = int_field(nbt, "xPos")?;
    let z = int_field(nbt, "zPos")?;
    let mut chunk = Chunk::empty(ChunkPos { x, z });

    let sections = list_field(nbt, "sections")?;
    for section_nbt in sections {
        let section_y = byte_field(section_nbt, "Y")? as i32;
        let index = section_y - MIN_Y / 16;
        if index < 0 || index as usize >= SECTION_COUNT {
            continue;
        }
        let block_states = compound_field(section_nbt, "block_states")?;
        let palette = list_field(block_states, "palette")?;
        let states: Vec<u16> = palette
            .iter()
            .map(|entry| {
                let name = string_field(entry, "Name")?;
                Ok(blocks::state_for_name(name).unwrap_or(BLOCK_AIR))
            })
            .collect::<Result<_>>()?;

        let target = &mut chunk.sections[index as usize];
        if states.len() <= 1 {
            let state = states.first().copied().unwrap_or(BLOCK_AIR);
            if state != BLOCK_AIR {
                target.block_states = vec![state; SECTION_VOLUME];
                target.recount();
            }
        } else if let Some(Nbt::LongArray(data)) = compound_get(block_states, "data") {
            let values = unpack_palette(data, states.len())?;
            target.block_states = values
                .into_iter()
                .map(|value| states.get(usize::from(value)).copied().unwrap_or(BLOCK_AIR))
                .collect();
            target.recount();
            if target.non_air_blocks == 0 {
                target.block_states.clear();
            }
        }
    }

    Ok(chunk)
}

/// Minimum bits per palette entry for block states.
fn bits_for(palette_len: usize) -> usize {
    let bits = usize::BITS as usize - (palette_len - 1).leading_zeros() as usize;
    bits.max(4)
}

fn pack_palette(states: &[u16], palette: &[u16]) -> Vec<i64> {
    let bits = bits_for(palette.len());
    let per_long = 64 / bits;
    let mask = (1u64 << bits) - 1;
    let mut words = vec![0u64; states.len().div_ceil(per_long)];
    for (i, &state) in states.iter().enumerate() {
        let index = palette.iter().position(|&value| value == state).unwrap() as u64;
        words[i / per_long] |= (index & mask) << ((i % per_long) * bits);
    }
    words.into_iter().map(|word| word as i64).collect()
}

fn unpack_palette(data: &[i64], palette_len: usize) -> Result<Vec<u16>> {
    let bits = bits_for(palette_len);
    let per_long = 64 / bits;
    let mask = (1u64 << bits) - 1;
    let mut values = Vec::with_capacity(SECTION_VOLUME);
    for &word in data {
        let word = word as u64;
        for slot in 0..per_long {
            if values.len() == SECTION_VOLUME {
                break;
            }
            values.push(((word >> (slot * bits)) & mask) as u16);
        }
    }
    values.resize(SECTION_VOLUME, 0);
    Ok(values)
}

fn compound_get<'a>(nbt: &'a Nbt, key: &str) -> Option<&'a Nbt> {
    match nbt {
        Nbt::Compound(entries) => entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value),
        _ => None,
    }
}

fn int_field(nbt: &Nbt, key: &str) -> Result<i32> {
    match compound_get(nbt, key) {
        Some(Nbt::Int(value)) => Ok(*value),
        _ => Err(Error::new(
            ErrorKind::InvalidData,
            format!("missing int field {key}"),
        )),
    }
}

fn byte_field(nbt: &Nbt, key: &str) -> Result<i8> {
    match compound_get(nbt, key) {
        Some(Nbt::Byte(value)) => Ok(*value),
        _ => Err(Error::new(
            ErrorKind::InvalidData,
            format!("missing byte field {key}"),
        )),
    }
}

fn string_field<'a>(nbt: &'a Nbt, key: &str) -> Result<&'a str> {
    match compound_get(nbt, key) {
        Some(Nbt::String(value)) => Ok(value),
        _ => Err(Error::new(
            ErrorKind::InvalidData,
            format!("missing string field {key}"),
        )),
    }
}

fn compound_field<'a>(nbt: &'a Nbt, key: &str) -> Result<&'a Nbt> {
    match compound_get(nbt, key) {
        Some(value @ Nbt::Compound(_)) => Ok(value),
        _ => Err(Error::new(
            ErrorKind::InvalidData,
            format!("missing compound field {key}"),
        )),
    }
}

fn list_field<'a>(nbt: &'a Nbt, key: &str) -> Result<&'a [Nbt]> {
    match compound_get(nbt, key) {
        Some(Nbt::List(items)) => Ok(items),
        _ => Err(Error::new(
            ErrorKind::InvalidData,
            format!("missing list field {key}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terrain::TerrainGenerator;

    #[test]
    fn chunk_survives_an_nbt_roundtrip() {
        let mut chunk = TerrainGenerator::new(5).chunk(ChunkPos { x: -2, z: 3 });
        chunk.set_block(-2 * 16 + 1, 90, 3 * 16 + 4, encode::BLOCK_COBBLESTONE);
        chunk.set_block(-2 * 16 + 2, 90, 3 * 16 + 4, encode::BLOCK_COBBLESTONE);

        let nbt = chunk_to_nbt(&chunk, 1234);
        let decoded = chunk_from_nbt(&nbt).unwrap();

        assert_eq!(decoded.pos, chunk.pos);
        for x in -32..0 {
            for z in 48..64 {
                for y in [MIN_Y, MIN_Y + 1, 0, 63, 64, 65, 90, 120] {
                    assert_eq!(
                        decoded.get_block(x, y, z),
                        chunk.get_block(x, y, z),
                        "block mismatch at {x},{y},{z}"
                    );
                }
            }
        }
    }

    #[test]
    fn region_file_roundtrips_through_disk() {
        let dir = std::env::temp_dir().join(format!("loadstone-region-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);

        let chunks = vec![
            TerrainGenerator::new(11).chunk(ChunkPos { x: 0, z: 0 }),
            TerrainGenerator::new(11).chunk(ChunkPos { x: 1, z: 0 }),
        ];
        write_chunks(&dir, &chunks, 0).unwrap();

        let path = region_path(&dir, ChunkPos { x: 0, z: 0 });
        let loaded = read_region(&path).unwrap();
        assert_eq!(loaded.len(), 2);

        for original in &chunks {
            let found = loaded
                .iter()
                .find(|chunk| chunk.pos == original.pos)
                .expect("chunk present after reload");
            let surface =
                TerrainGenerator::new(11).surface_height(original.pos.x * 16, original.pos.z * 16);
            assert_eq!(
                found.get_block(original.pos.x * 16, surface, original.pos.z * 16),
                encode::BLOCK_GRASS_BLOCK
            );
        }

        let _ = fs::remove_dir_all(&dir);
    }
}
