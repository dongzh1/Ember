use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::LoadConfiguration;

// EMBER start - custom blocks (resource-pack-driven, phase 4 of the CraftEngine portation)
/// The list of configured custom block types, `blocks/blocks.toml`.
///
/// Same "own file, arbitrarily-long named list" reasoning as
/// `ShopListConfig`/`MenuListConfig`/`FurnitureListConfig`. **Not**
/// `#[serde(transparent)]` for the same reason as those.
#[derive(Deserialize, Serialize, Default, Clone)]
pub struct CustomBlockListConfig {
    pub blocks: Vec<CustomBlockConfig>,
}

impl LoadConfiguration for CustomBlockListConfig {
    fn get_path() -> &'static Path {
        Path::new("blocks/blocks.toml")
    }

    fn validate(&self) {}
}

/// One custom block type: a real vanilla block ("carrier") wearing a
/// resource pack skin.
///
/// Placing it consumes the configured custom item
/// (`custom_item_id`, see `CustomItemConfig`) and sets `carrier_block`'s own
/// **default state** at the target position - a resource pack retextures
/// that carrier's default-state model independently of any ordinary
/// instance of that block placed elsewhere in the world, without needing to
/// touch the carrier's own real block-state properties at all. A separate,
/// server-managed position index (`blocks/instances.toml`, not the block
/// state itself and not a vanilla `BlockEntity`) is what actually
/// identifies "this position is custom block X" - see
/// `server::custom_block` for the exact interception points
/// (`normal_use`/breaking) that consult it.
///
/// Pick a carrier whose vanilla behavior you don't mind losing the
/// interactive part of (right-clicking a custom block never runs the
/// carrier's own `normal_use`) and, ideally, one without neighbor-sensitive
/// state recalculation of its own (`note_block`, the default/typical
/// choice, silently recomputes its `instrument` property based on the
/// block below/above it - this doesn't break custom-block *identification*,
/// which never looks at the carrier's own state, but could change the
/// resource pack's model mapping if that mapping is keyed to a *specific*
/// instrument/note combination rather than the block's default state as a
/// whole).
#[derive(Deserialize, Serialize, Clone)]
pub struct CustomBlockConfig {
    /// Reference name, used by admin tooling and the placed-instance store.
    pub id: String,
    /// The `CustomItemConfig.id` a player must be holding to place this,
    /// and what they get back when it's broken.
    pub custom_item_id: String,
    /// Vanilla block resource name (e.g. `note_block`) whose default state
    /// is placed as the visual/physical carrier.
    pub carrier_block: String,
}

/// Placed custom block instances, `blocks/instances.toml`.
///
/// Server-managed runtime state (placements/breaks mutate this and
/// re-save) - same "own file, separate from static settings" reasoning as
/// `FurnitureInstanceListConfig`. The carrier block itself is saved/loaded
/// through the normal world save format like any other block (it's a real
/// block); this file is only the extra "which position is secretly which
/// custom block id" index vanilla has no concept of.
#[derive(Deserialize, Serialize, Default, Clone)]
pub struct CustomBlockInstanceListConfig {
    pub instances: Vec<CustomBlockInstanceConfig>,
}

impl LoadConfiguration for CustomBlockInstanceListConfig {
    fn get_path() -> &'static Path {
        Path::new("blocks/instances.toml")
    }

    fn validate(&self) {}
}

#[derive(Deserialize, Serialize, Clone)]
pub struct CustomBlockInstanceConfig {
    pub block_id: String,
    pub world: String,
    pub x: i32,
    pub y: i32,
    pub z: i32,
}
// EMBER end
