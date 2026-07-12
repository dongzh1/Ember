// EMBER start: packet-only NPC storage
use std::{fs, path::PathBuf};

use pumpkin_protocol::Property;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

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
    /// connected player's own profile). Only meaningful for `entity_type`
    /// `"player"`/`"mannequin"`; ignored for every other entity type.
    pub skin: Option<Property>,
    /// Console command run on click, with `%player%` replaced by the
    /// clicking player's name. `None` means the NPC is purely decorative.
    pub click_command: Option<String>,
    // EMBER start - packet-only NPCs generalized to any entity type
    /// Resource name (e.g. `"player"`, `"mannequin"`, `"zombie"`) of the
    /// entity type this NPC renders as. Defaults to `"player"` so existing
    /// `npc/npcs.json` files (written before this field existed) parse and
    /// behave identically to before.
    #[serde(default = "default_entity_type")]
    pub entity_type: String,
    /// Block resource name (e.g. `"sand"`) to render, only meaningful when
    /// `entity_type` is `"falling_block"` — that entity's appearance comes
    /// from the spawn packet's block-state data, not from metadata.
    #[serde(default)]
    pub block: Option<String>,
    /// Item resource name (e.g. `"diamond_sword"`) to render, only
    /// meaningful when `entity_type` is `"item"` — that entity is otherwise
    /// invisible (its appearance is entirely metadata-driven).
    #[serde(default)]
    pub item: Option<String>,
    /// Continuously turn to face the nearest currently-visible player (a
    /// "shopkeeper" look), re-evaluated on `LOOK_INTERVAL_TICKS`.
    #[serde(default)]
    pub look_at_nearest_player: bool,
    /// Client-side crouch pose.
    #[serde(default)]
    pub sneaking: bool,
    // EMBER end
}

// EMBER start - packet-only NPCs generalized to any entity type
fn default_entity_type() -> String {
    "player".to_string()
}
// EMBER end

/// NPC definitions, persisted to `npc/npcs.json`.
///
/// Its own folder, not the vanilla-mirroring `data/` folder
/// (whitelist/ops/bans/usercache): it's an Ember-only feature, not something
/// upstream Pumpkin also has a file for.
#[derive(Deserialize, Serialize, Default)]
#[serde(transparent)]
pub struct NpcConfig {
    pub npcs: Vec<NpcEntry>,
}

const NPC_FOLDER: &str = "npc/";
const NPC_FILE: &str = "npcs.json";

impl NpcConfig {
    #[must_use]
    pub fn find(&self, name: &str) -> Option<&NpcEntry> {
        self.npcs
            .iter()
            .find(|npc| npc.name.eq_ignore_ascii_case(name))
    }

    fn path() -> PathBuf {
        let exec_dir = std::env::current_dir().expect("Failed to get current directory");
        exec_dir.join(NPC_FOLDER).join(NPC_FILE)
    }

    #[must_use]
    pub fn load() -> Self {
        let path = Self::path();
        if let Some(folder) = path.parent()
            && !folder.exists()
        {
            debug!("creating new npc folder");
            fs::create_dir_all(folder).expect("Failed to create npc folder");
        }

        if path.exists() {
            let file_content = fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("Couldn't read npc config at {}", path.display()));
            serde_json::from_str(&file_content).unwrap_or_else(|err| {
                panic!(
                    "Couldn't parse npc config at {}. Reason: {err}. This is probably caused by a config update. Just delete the old npc config and restart.",
                    path.display(),
                )
            })
        } else {
            let content = Self::default();
            if let Err(err) = fs::write(
                &path,
                serde_json::to_string_pretty(&content).expect("Failed to serialize npc config"),
            ) {
                warn!(
                    "Couldn't write default npc config to {}: {err}",
                    path.display()
                );
            }
            content
        }
    }

    pub fn save(&self) {
        let path = Self::path();
        let content = match serde_json::to_string_pretty(self) {
            Ok(content) => content,
            Err(err) => {
                warn!("Couldn't serialize npc config to {}: {err}", path.display());
                return;
            }
        };
        if let Err(err) = fs::write(&path, content) {
            warn!("Couldn't write npc config to {}: {err}", path.display());
        }
    }
}
// EMBER end
