//! Registry data: blocks, items, biomes, dimensions, etc.
//!
//! The synchronized registries and network tags the client needs before it can
//! enter Play are captured from the vanilla 1.21.11 server and embedded here as
//! JSON (see `data/`). `minecraft:core` is negotiated during Configuration, so
//! the entries are sent without NBT and resolved from the client's own copy of
//! the data pack — only the entry *names and order* matter, which is why this
//! stays small and readable.
//!
//! `BlockRegistry` below is still a hand-written placeholder used by the world
//! code; it is unrelated to the synchronized data.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;

/// A data pack announced during known-pack negotiation.
#[derive(Debug, Clone, Deserialize)]
pub struct KnownPack {
    pub namespace: String,
    pub id: String,
    pub version: String,
}

/// One synchronized registry: its id and its entries in protocol order.
#[derive(Debug, Clone, Deserialize)]
pub struct RegistryDef {
    pub id: String,
    pub entries: Vec<String>,
}

/// The full set of synchronized registries, in the order the server sends them.
#[derive(Debug, Clone, Deserialize)]
pub struct SyncedRegistries {
    pub minecraft_version: String,
    pub protocol: i32,
    pub known_pack: KnownPack,
    pub registries: Vec<RegistryDef>,
}

/// A named set of registry entry ids.
#[derive(Debug, Clone, Deserialize)]
pub struct NetworkTag {
    pub name: String,
    pub entries: Vec<i32>,
}

/// All tags belonging to one registry.
#[derive(Debug, Clone, Deserialize)]
pub struct TagRegistry {
    pub registry: String,
    pub tags: Vec<NetworkTag>,
}

/// The complete network tag set.
#[derive(Debug, Clone, Deserialize)]
pub struct NetworkTags {
    pub minecraft_version: String,
    pub registries: Vec<TagRegistry>,
}

const SYNCED_REGISTRIES_JSON: &str = include_str!("../data/synced_registries.json");
const NETWORK_TAGS_JSON: &str = include_str!("../data/network_tags.json");

/// Synchronized registry definitions, parsed once on first use.
pub fn synced_registries() -> &'static SyncedRegistries {
    static DATA: OnceLock<SyncedRegistries> = OnceLock::new();
    DATA.get_or_init(|| {
        serde_json::from_str(SYNCED_REGISTRIES_JSON)
            .expect("embedded synced_registries.json is valid")
    })
}

/// Network tags, parsed once on first use.
pub fn network_tags() -> &'static NetworkTags {
    static DATA: OnceLock<NetworkTags> = OnceLock::new();
    DATA.get_or_init(|| {
        serde_json::from_str(NETWORK_TAGS_JSON).expect("embedded network_tags.json is valid")
    })
}

/// The runtime id of `name` within a synchronized registry, i.e. its index in
/// the protocol-ordered entry list. Used to send block/entity/registry ids in
/// Play state packets (they are meaningless without the Configuration spin-up).
pub fn runtime_id(registry_id: &str, name: &str) -> Option<i32> {
    synced_registries()
        .registries
        .iter()
        .find(|registry| registry.id == registry_id)
        .and_then(|registry| registry.entries.iter().position(|entry| entry == name))
        .map(|index| index as i32)
}

/// Minimal block registry: block state id -> properties.
#[derive(Debug, Clone, Default)]
pub struct BlockRegistry {
    pub by_id: Vec<String>,
    pub id_by_name: HashMap<String, u32>,
}

impl BlockRegistry {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn register(&mut self, name: impl Into<String>) -> u32 {
        let name = name.into();
        if let Some(&id) = self.id_by_name.get(&name) {
            return id;
        }
        let id = self.by_id.len() as u32;
        self.id_by_name.insert(name.clone(), id);
        self.by_id.push(name);
        id
    }

    pub fn get(&self, id: u32) -> Option<&str> {
        self.by_id.get(id as usize).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegistryReportEntry {
    pub protocol_id: i32,
    #[serde(rename = "min_state_id", default)]
    pub min_state_id: i32,
    #[serde(rename = "max_state_id", default)]
    pub max_state_id: i32,
    #[serde(rename = "default_state_id", default)]
    pub default_state_id: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synchronized_registries_load_and_cover_the_client_requirements() {
        let synced = synced_registries();
        assert_eq!(synced.minecraft_version, "1.21.11");
        assert_eq!(synced.protocol, 774);
        assert_eq!(synced.known_pack.id, "core");
        assert_eq!(synced.registries.len(), 23);

        let find = |id: &str| synced.registries.iter().find(|r| r.id == id).unwrap();
        // The two registries the client needs to join a world at all.
        assert_eq!(find("minecraft:dimension_type").entries.len(), 4);
        assert_eq!(find("minecraft:worldgen/biome").entries.len(), 65);
        assert!(find("minecraft:dimension_type")
            .entries
            .contains(&"minecraft:overworld".to_string()));
    }

    #[test]
    fn network_tags_load() {
        let tags = network_tags();
        assert_eq!(tags.registries.len(), 14);
        let total: usize = tags.registries.iter().map(|r| r.tags.len()).sum();
        assert_eq!(total, 602);
        assert!(tags
            .registries
            .iter()
            .any(|r| r.registry == "minecraft:block"));
    }
}
