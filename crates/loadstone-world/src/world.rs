//! In-memory world state for the demo flat world.
//!
//! The world is the flat spawn template from [`encode::flat_chunk`] plus an
//! overlay of player edits. Block queries fall back to the template, so a
//! single plain `HashMap` of differences is enough to represent any state.

use std::collections::HashMap;

use crate::encode::{
    BLOCK_AIR, BLOCK_BEDROCK, BLOCK_DIRT, BLOCK_GRASS_BLOCK, GRASS_TOP_Y, MIN_Y, WORLD_HEIGHT,
};

/// How far player edits may reach from the spawn chunk. This is deliberately
/// wider than the server's chunk view distance so walking around the streaming
/// view never leaves the editable area.
pub const WORLD_BORDER_RADIUS: i32 = 8;
/// The top of the world, exclusive (build height).
pub const WORLD_TOP_Y: i32 = MIN_Y + WORLD_HEIGHT;

/// The block state making up the flat template at any column offset.
pub fn flat_block(x: i32, y: i32, z: i32) -> u16 {
    match y {
        y if y == MIN_Y => BLOCK_BEDROCK,
        y if y > MIN_Y && y < GRASS_TOP_Y => BLOCK_DIRT,
        GRASS_TOP_Y => BLOCK_GRASS_BLOCK,
        _ => {
            let _ = (x, z);
            BLOCK_AIR
        }
    }
}

/// Whether the player may interact with a coordinate: inside the demo world
/// border, above the unbreakable bedrock floor and below the build height.
pub fn allow_editing(x: i32, y: i32, z: i32) -> bool {
    let within_x = (-WORLD_BORDER_RADIUS..=WORLD_BORDER_RADIUS).contains(&x.div_euclid(16));
    let within_z = (-WORLD_BORDER_RADIUS..=WORLD_BORDER_RADIUS).contains(&z.div_euclid(16));
    within_x && within_z && y > MIN_Y && y < WORLD_TOP_Y
}

/// A block world: the flat template plus the set of player-made edits.
#[derive(Debug, Default)]
pub struct World {
    edits: HashMap<(i32, i32, i32), u16>,
}

impl World {
    pub fn new() -> Self {
        Self::default()
    }

    /// The current block state at a coordinate, after applying edits.
    pub fn get_block(&self, x: i32, y: i32, z: i32) -> u16 {
        self.edits
            .get(&(x, y, z))
            .copied()
            .unwrap_or_else(|| flat_block(x, y, z))
    }

    /// Applies a block edit; reverting to the template drops the edit entry.
    pub fn set_block(&mut self, x: i32, y: i32, z: i32, state: u16) {
        if state == flat_block(x, y, z) {
            self.edits.remove(&(x, y, z));
        } else {
            self.edits.insert((x, y, z), state);
        }
    }

    /// A block can be dug up when it is interactive and not already air.
    pub fn is_breakable(&self, x: i32, y: i32, z: i32) -> bool {
        allow_editing(x, y, z) && self.get_block(x, y, z) != BLOCK_AIR
    }

    /// A block can be placed into when the spot is interactive and empty.
    pub fn is_empty(&self, x: i32, y: i32, z: i32) -> bool {
        allow_editing(x, y, z) && self.get_block(x, y, z) == BLOCK_AIR
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::BLOCK_COBBLESTONE;

    #[test]
    fn template_layout_matches_the_wire_chunks() {
        assert_eq!(flat_block(4, -64, 4), BLOCK_BEDROCK);
        assert_eq!(flat_block(4, -63, 4), BLOCK_DIRT);
        assert_eq!(flat_block(4, 0, 4), BLOCK_DIRT);
        assert_eq!(flat_block(4, 63, 4), BLOCK_DIRT);
        assert_eq!(flat_block(4, 64, 4), BLOCK_GRASS_BLOCK);
        assert_eq!(flat_block(4, 65, 4), BLOCK_AIR);
    }

    #[test]
    fn edits_override_the_template_and_revert() {
        let mut world = World::new();
        assert_eq!(world.get_block(8, 64, 8), BLOCK_GRASS_BLOCK);

        world.set_block(8, 64, 8, BLOCK_COBBLESTONE);
        assert_eq!(world.get_block(8, 64, 8), BLOCK_COBBLESTONE);

        // Removing an edit returns the world to the template state.
        world.set_block(8, 64, 8, BLOCK_GRASS_BLOCK);
        assert_eq!(world.get_block(8, 64, 8), BLOCK_GRASS_BLOCK);
        assert!(world.edits.is_empty());
    }

    #[test]
    fn breakability_and_emptiness_respect_the_rules() {
        let mut world = World::new();
        assert!(world.is_breakable(8, 64, 8)); // grass
        assert!(world.is_breakable(8, 0, 8)); // dirt
        assert!(!world.is_breakable(8, -64, 8)); // bedrock
        assert!(!world.is_breakable(8, 65, 8)); // air
        assert!(!world.is_breakable(1000, 64, 1000)); // outside the world border
        assert!(!world.is_breakable(8, 319, 8)); // above the build height

        assert!(world.is_empty(8, 65, 8));
        assert!(!world.is_empty(8, 64, 8));
        world.set_block(8, 65, 8, BLOCK_COBBLESTONE);
        assert!(!world.is_empty(8, 65, 8));
    }
}
