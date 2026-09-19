//! The small set of vanilla block states LoadstoneMC generates and persists.
//!
//! Terrain generation only ever produces these states, and player edits only
//! ever place [`BLOCK_COBBLESTONE`], so this table is enough to translate
//! between the numeric global state ids used on the wire and the `Name` /
//! `Properties` form Anvil region files store. Unknown palette entries read
//! from disk fall back to [`None`] and are treated as air.

use crate::encode::{
    BLOCK_AIR, BLOCK_BEDROCK, BLOCK_COBBLESTONE, BLOCK_DIRT, BLOCK_GLASS, BLOCK_GRASS_BLOCK,
    BLOCK_SAND, BLOCK_STONE,
};

/// A block state that round-trips through Anvil NBT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockDef {
    /// Vanilla 1.21.11 global block state id.
    pub state: u16,
    pub name: &'static str,
    /// `snowy` property value for blocks that have one (grass blocks).
    pub snowy: Option<bool>,
}

/// Every block state the world can generate or persist.
pub const SUPPORTED_BLOCKS: &[BlockDef] = &[
    BlockDef {
        state: BLOCK_AIR,
        name: "minecraft:air",
        snowy: None,
    },
    BlockDef {
        state: BLOCK_STONE,
        name: "minecraft:stone",
        snowy: None,
    },
    BlockDef {
        state: BLOCK_GRASS_BLOCK,
        name: "minecraft:grass_block",
        snowy: Some(false),
    },
    BlockDef {
        state: BLOCK_DIRT,
        name: "minecraft:dirt",
        snowy: None,
    },
    BlockDef {
        state: BLOCK_COBBLESTONE,
        name: "minecraft:cobblestone",
        snowy: None,
    },
    BlockDef {
        state: BLOCK_SAND,
        name: "minecraft:sand",
        snowy: None,
    },
    BlockDef {
        state: BLOCK_BEDROCK,
        name: "minecraft:bedrock",
        snowy: None,
    },
    BlockDef {
        state: BLOCK_GLASS,
        name: "minecraft:glass",
        snowy: None,
    },
];

/// The definition for a global block state id, if it is a supported block.
pub fn block_def(state: u16) -> Option<&'static BlockDef> {
    SUPPORTED_BLOCKS.iter().find(|block| block.state == state)
}

/// The global state id for a block name, ignoring properties.
pub fn state_for_name(name: &str) -> Option<u16> {
    SUPPORTED_BLOCKS
        .iter()
        .find(|block| block.name == name)
        .map(|block| block.state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_blocks_are_unique_and_resolvable() {
        for block in SUPPORTED_BLOCKS {
            assert_eq!(block_def(block.state), Some(block));
            assert_eq!(state_for_name(block.name), Some(block.state));
        }
        assert_eq!(block_def(4321), None);
        assert_eq!(state_for_name("minecraft:not_a_block"), None);
    }
}
