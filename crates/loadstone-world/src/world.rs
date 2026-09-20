//! The shared world: deterministic terrain plus the chunks loaded from disk.
//!
//! Terrain is generated on demand from a seed and cached. Player edits mutate
//! the cached chunk and mark it dirty; saving writes every cached chunk to its
//! Anvil region file, so a reload restores edits on top of the generated
//! terrain. Because generation is a pure function of the seed, ungenerated
//! chunks always come back identical.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Result;
use std::path::{Path, PathBuf};

use crate::anvil;
use crate::encode::{self, BLOCK_AIR};
use crate::terrain::TerrainGenerator;
use crate::{Chunk, ChunkPos};

/// How far player edits may reach from the spawn chunk. This is deliberately
/// wider than the server's chunk view distance so walking around the streaming
/// view never leaves the editable area.
pub const WORLD_BORDER_RADIUS: i32 = 8;
/// The top of the world, exclusive (build height).
pub const WORLD_TOP_Y: i32 = encode::MIN_Y + encode::WORLD_HEIGHT;
/// The world X/Z the player spawns at, centred in the spawn chunk.
pub const SPAWN_X: f64 = 8.5;
pub const SPAWN_Z: f64 = 8.5;

/// Whether the player may interact with a coordinate: inside the demo world
/// border, above the unbreakable bedrock floor and below the build height.
pub fn allow_editing(x: i32, y: i32, z: i32) -> bool {
    let within_x = (-WORLD_BORDER_RADIUS..=WORLD_BORDER_RADIUS).contains(&x.div_euclid(16));
    let within_z = (-WORLD_BORDER_RADIUS..=WORLD_BORDER_RADIUS).contains(&z.div_euclid(16));
    within_x && within_z && y > encode::MIN_Y && y < WORLD_TOP_Y
}

/// A block world: generated terrain plus edits loaded from or saved to disk.
#[derive(Debug)]
pub struct World {
    generator: TerrainGenerator,
    chunks: HashMap<ChunkPos, Chunk>,
    dirty: HashSet<ChunkPos>,
    spawn: (f64, f64, f64),
}

impl Default for World {
    fn default() -> Self {
        Self::with_seed(0)
    }
}

impl World {
    pub fn new() -> Self {
        Self::default()
    }

    /// A world whose ungenerated chunks come from `seed`.
    pub fn with_seed(seed: u64) -> Self {
        let generator = TerrainGenerator::new(seed);
        let surface = generator.surface_height(SPAWN_X as i32, SPAWN_Z as i32);
        Self {
            generator,
            chunks: HashMap::new(),
            dirty: HashSet::new(),
            spawn: (SPAWN_X, f64::from(surface + 1), SPAWN_Z),
        }
    }

    pub fn seed(&self) -> u64 {
        self.generator.seed()
    }

    /// The world spawn point, standing on the generated surface.
    pub fn spawn(&self) -> (f64, f64, f64) {
        self.spawn
    }

    /// The world Y of the topmost generated block of a column, ignoring edits.
    pub fn surface_height(&self, x: i32, z: i32) -> i32 {
        self.generator.surface_height(x, z)
    }

    /// The cached chunk at `pos`, generating it if necessary.
    pub fn chunk(&mut self, pos: ChunkPos) -> &Chunk {
        self.chunks
            .entry(pos)
            .or_insert_with(|| self.generator.chunk(pos))
    }

    /// The cached chunk at `pos`, generating it if necessary.
    pub fn chunk_mut(&mut self, pos: ChunkPos) -> &mut Chunk {
        self.dirty.insert(pos);
        self.chunks
            .entry(pos)
            .or_insert_with(|| self.generator.chunk(pos))
    }

    /// The current block state at a coordinate.
    pub fn get_block(&mut self, x: i32, y: i32, z: i32) -> u16 {
        let pos = ChunkPos {
            x: x.div_euclid(16),
            z: z.div_euclid(16),
        };
        self.chunk(pos).get_block(x, y, z)
    }

    /// Applies a block edit.
    pub fn set_block(&mut self, x: i32, y: i32, z: i32, state: u16) {
        let pos = ChunkPos {
            x: x.div_euclid(16),
            z: z.div_euclid(16),
        };
        self.chunk_mut(pos).set_block(x, y, z, state);
    }

    /// A block can be dug up when it is interactive and not already air.
    pub fn is_breakable(&mut self, x: i32, y: i32, z: i32) -> bool {
        allow_editing(x, y, z) && self.get_block(x, y, z) != BLOCK_AIR
    }

    /// A block can be placed into when the spot is interactive and empty.
    pub fn is_empty(&mut self, x: i32, y: i32, z: i32) -> bool {
        allow_editing(x, y, z) && self.get_block(x, y, z) == BLOCK_AIR
    }

    /// Whether any edits have been made since the last save.
    pub fn is_dirty(&self) -> bool {
        !self.dirty.is_empty()
    }

    /// How many chunks are currently cached in memory.
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// Loads every chunk from the world's region files. A missing directory is
    /// not an error; the world simply starts generated-only.
    pub fn load_from(&mut self, world_dir: &Path) -> Result<()> {
        let dir = anvil::region_dir(world_dir);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("mca") {
                continue;
            }
            for chunk in anvil::read_region(&path)? {
                self.chunks.insert(chunk.pos, chunk);
            }
        }
        Ok(())
    }

    /// Writes every cached chunk to its region file, clearing the dirty set.
    pub fn save(&mut self, world_dir: &Path) -> Result<()> {
        let last_update = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or(0);
        let chunks: Vec<Chunk> = self.chunks.values().cloned().collect();
        anvil::write_chunks(world_dir, &chunks, last_update)?;
        self.write_seed(world_dir)?;
        self.dirty.clear();
        Ok(())
    }

    fn write_seed(&self, world_dir: &Path) -> Result<()> {
        fs::create_dir_all(world_dir)?;
        fs::write(seed_path(world_dir), self.seed().to_string())
    }
}

/// The sidecar file holding the world seed (generated chunks with no edits are
/// not written to region files, so the seed is needed to regenerate them).
pub fn seed_path(world_dir: &Path) -> PathBuf {
    world_dir.join("loadstone.seed")
}

/// Reads a world seed previously written by [`World::save`].
pub fn read_seed(world_dir: &Path) -> Result<Option<u64>> {
    match fs::read_to_string(seed_path(world_dir)) {
        Ok(text) => Ok(text.trim().parse().ok()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::BLOCK_COBBLESTONE;
    use crate::terrain::TerrainGenerator;

    #[test]
    fn spawn_sits_on_the_generated_surface() {
        let world = World::with_seed(3);
        let (x, y, z) = world.spawn();
        assert_eq!((x, z), (SPAWN_X, SPAWN_Z));
        assert_eq!(
            y,
            f64::from(TerrainGenerator::new(3).surface_height(8, 8) + 1)
        );
    }

    #[test]
    fn edits_override_terrain_and_are_tracked() {
        let mut world = World::with_seed(0);
        let y = TerrainGenerator::new(0).surface_height(8, 8);
        assert_eq!(world.get_block(8, y, 8), encode::BLOCK_GRASS_BLOCK);
        world.set_block(8, y, 8, BLOCK_COBBLESTONE);
        assert_eq!(world.get_block(8, y, 8), BLOCK_COBBLESTONE);
        assert!(world.is_dirty());
    }

    #[test]
    fn breakability_and_emptiness_respect_the_rules() {
        let mut world = World::with_seed(0);
        let y = TerrainGenerator::new(0).surface_height(8, 8);
        assert!(world.is_breakable(8, y, 8)); // grass
        assert!(world.is_breakable(8, y - 2, 8)); // dirt
        assert!(!world.is_breakable(8, -64, 8)); // bedrock
        assert!(!world.is_breakable(8, y + 1, 8)); // air
        assert!(!world.is_breakable(1000, y, 1000)); // outside the world border
        assert!(!world.is_breakable(8, 319, 8)); // above the build height

        assert!(world.is_empty(8, y + 1, 8));
        assert!(!world.is_empty(8, y, 8));
    }

    #[test]
    fn save_and_load_roundtrips_chunks_and_edits() {
        let dir = std::env::temp_dir().join(format!("loadstone-world-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);

        let mut world = World::with_seed(21);
        let y = TerrainGenerator::new(21).surface_height(8, 8);
        world.set_block(8, y + 1, 8, BLOCK_COBBLESTONE);
        world.save(&dir).unwrap();
        assert!(!world.is_dirty());

        let mut loaded = World::with_seed(read_seed(&dir).unwrap().unwrap());
        loaded.load_from(&dir).unwrap();
        assert_eq!(loaded.get_block(8, y + 1, 8), BLOCK_COBBLESTONE);
        assert_eq!(loaded.get_block(8, y, 8), encode::BLOCK_GRASS_BLOCK);

        let _ = fs::remove_dir_all(&dir);
    }
}
