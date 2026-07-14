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
/// face with it in hand. Renders as a packet-only display entity at the
/// clicked position - not a real block, no collision - either an
/// `item_display` showing the held custom item's own model (`render_mode =
/// "item"`, the default), or a `block_display` showing a chosen vanilla
/// blockstate (`render_mode = "block"` + `block`) for pieces meant to read
/// as block-shaped rather than a floating icon.
#[derive(Deserialize, Serialize, Clone)]
pub struct FurnitureConfig {
    /// Reference name, used by admin tooling and the placed-instance store.
    pub id: String,
    /// The `CustomItemConfig.id` a player must be holding to place this.
    pub custom_item_id: String,
    #[serde(default)]
    pub render_mode: RenderMode,
    /// Only consulted when `render_mode = "block"`: the vanilla block
    /// resource name (e.g. `note_block`) shown via `block_display` - a
    /// resource pack can retexture its default state independently of any
    /// real block of that type placed elsewhere, the same "rare/unused
    /// state as a visual carrier" idea phase four's real custom blocks use.
    #[serde(default)]
    pub block: String,
    #[serde(default = "default_hitbox_size")]
    pub hitbox_width: f64,
    #[serde(default = "default_hitbox_size")]
    pub hitbox_height: f64,
    #[serde(default = "default_scale")]
    pub scale: f64,
}

#[derive(Deserialize, Serialize, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RenderMode {
    #[default]
    Item,
    Block,
}

const fn default_hitbox_size() -> f64 {
    1.0
}

const fn default_scale() -> f64 {
    1.0
}
// EMBER end
