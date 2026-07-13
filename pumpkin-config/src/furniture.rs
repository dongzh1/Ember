use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::LoadConfiguration;

// EMBER start - custom furniture (resource-pack-driven, phase 3 of the CraftEngine portation)
/// The list of configured furniture types, `furniture/furniture.toml`.
///
/// A separate file for the same "own file, arbitrarily-long named list"
/// reasoning as `ShopListConfig`/`MenuListConfig`. **Not**
/// `#[serde(transparent)]` for the same reason as those.
#[derive(Deserialize, Serialize, Default, Clone)]
pub struct FurnitureListConfig {
    pub furniture: Vec<FurnitureConfig>,
}

impl LoadConfiguration for FurnitureListConfig {
    fn get_path() -> &'static Path {
        Path::new("furniture/furniture.toml")
    }

    fn validate(&self) {}
}

/// One furniture type: placing it is picking up a configured custom item.
///
/// (`custom_item_id`, see `CustomItemConfig`) and right-clicking a block
/// face with it in hand. The placed furniture renders with that same
/// custom item's model, floating via a packet-only `item_display` at the
/// clicked position - not a real block, no collision.
#[derive(Deserialize, Serialize, Clone)]
pub struct FurnitureConfig {
    /// Reference name, used by admin tooling and the placed-instance store.
    pub id: String,
    /// The `CustomItemConfig.id` a player must be holding to place this.
    pub custom_item_id: String,
    #[serde(default = "default_hitbox_size")]
    pub hitbox_width: f64,
    #[serde(default = "default_hitbox_size")]
    pub hitbox_height: f64,
    #[serde(default = "default_scale")]
    pub scale: f64,
}

const fn default_hitbox_size() -> f64 {
    1.0
}

const fn default_scale() -> f64 {
    1.0
}

/// Placed furniture instances, `furniture/instances.toml`.
///
/// Server-managed runtime state (placements/breaks mutate this and
/// re-save), not something an admin hand-authors like `furniture.toml`.
/// Kept in its own file for the same reason `npc/npcs.json` is separate
/// from any settings file: a frequently-mutated list shouldn't share a
/// file with static settings.
#[derive(Deserialize, Serialize, Default, Clone)]
pub struct FurnitureInstanceListConfig {
    pub instances: Vec<FurnitureInstanceConfig>,
}

impl LoadConfiguration for FurnitureInstanceListConfig {
    fn get_path() -> &'static Path {
        Path::new("furniture/instances.toml")
    }

    fn validate(&self) {}
}

#[derive(Deserialize, Serialize, Clone)]
pub struct FurnitureInstanceConfig {
    /// Stable identity, independent of this list's order - the runtime
    /// state keeps the same id so a break can remove the right entry
    /// without matching on (possibly-colliding) position values.
    pub instance_id: uuid::Uuid,
    pub furniture_id: String,
    pub world: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub yaw: f32,
}
// EMBER end
