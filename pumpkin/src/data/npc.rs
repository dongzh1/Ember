// EMBER start: packet-only NPC storage
use std::path::Path;

use pumpkin_protocol::Property;
use serde::{Deserialize, Serialize};

use super::{LoadJSONConfiguration, SaveJSONConfiguration};

/// A single packet-only NPC definition.
///
/// Unlike `minecraft:mannequin`, this is never a real world entity — it has
/// no `Entity`/NBT/save footprint. `crate::server::npc::NpcManager` spawns it
/// purely via per-viewer packets (`CPlayerInfoUpdate`/`CSpawnEntity`) to
/// whichever players are currently in view-distance range.
#[derive(Serialize, Deserialize, Clone)]
pub struct NpcEntry {
    pub name: String,
    pub world: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
    pub pitch: f32,
    /// The `textures` tab-list property to fake. `None` renders the client's
    /// default skin for the NPC's fake UUID (no player is ever resolved
    /// against Mojang for this — the value is always copied from a currently
    /// connected player's own profile).
    pub skin: Option<Property>,
    /// Console command run on click, with `%player%` replaced by the
    /// clicking player's name. `None` means the NPC is purely decorative.
    pub click_command: Option<String>,
}

#[derive(Deserialize, Serialize, Default)]
#[serde(transparent)]
pub struct NpcConfig {
    pub npcs: Vec<NpcEntry>,
}

impl NpcConfig {
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&NpcEntry> {
        self.npcs
            .iter()
            .find(|npc| npc.name.eq_ignore_ascii_case(name))
    }
}

impl LoadJSONConfiguration for NpcConfig {
    fn get_path() -> &'static Path {
        Path::new("npcs.json")
    }
    fn validate(&self) {}
}

impl SaveJSONConfiguration for NpcConfig {}
// EMBER end
