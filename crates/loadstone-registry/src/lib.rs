//! Registry data: blocks, items, biomes, dimensions, etc.
//!
//! Right now this is a stub. The intended source of this data is the
//! generated reports from the vanilla server jar (`--reports`) or the
//! `misode/mcmeta` dataset, converted by a build-time generator in
//! `tools/` into compact binary tables the game loop can query fast
//! without parsing JSON at startup.

use std::collections::HashMap;

use serde::Deserialize;

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
