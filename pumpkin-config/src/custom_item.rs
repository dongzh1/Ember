use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::LoadConfiguration;

// EMBER start - custom items (resource-pack-driven, phase 2 of the CraftEngine portation)
/// The list of configured custom items, `resourcepack/items.toml`.
///
/// A separate file from `resourcepack/resourcepack.toml` since it's an
/// arbitrarily-long named list - same "own file" reasoning as
/// `ShopListConfig`/`MenuListConfig`. **Not** `#[serde(transparent)]` for
/// the same reason as those: TOML documents must be a table at the root.
#[derive(Deserialize, Serialize, Default, Clone)]
pub struct CustomItemListConfig {
    pub items: Vec<CustomItemConfig>,
}

impl LoadConfiguration for CustomItemListConfig {
    fn get_path() -> &'static Path {
        Path::new("resourcepack/items.toml")
    }

    fn validate(&self) {}
}

/// One custom item: a real vanilla item wearing a custom model - there's no
/// "new item id" concept in the protocol, so every custom item is still,
/// underneath, some existing vanilla `base_item`.
#[derive(Deserialize, Serialize, Clone)]
pub struct CustomItemConfig {
    /// Reference name used by `/customitem give` and other systems - not
    /// the same as `base_item` or `model`.
    pub id: String,
    /// Real vanilla item resource name (e.g. `diamond_sword`) this custom
    /// item is built on top of - determines max stack size, durability,
    /// and every other vanilla behavior.
    pub base_item: String,
    /// Model path inside the resource pack (e.g. `ember:items/legendary_sword`),
    /// written into the item's `minecraft:item_model` component. Needs a
    /// matching model file under the resource pack builder's `source_dir`
    /// (`resourcepack/resourcepack.toml`) - the two configs aren't
    /// code-coupled, just expected to agree on one `assets/` layout.
    pub model: String,
    #[serde(default)]
    pub display_name: Option<String>,
}
// EMBER end
