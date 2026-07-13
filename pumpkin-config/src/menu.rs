use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::LoadConfiguration;

// EMBER start - floating packet-only menu system
/// The list of configured floating menus, `menu/menus.toml`.
///
/// A separate file from a hypothetical `menu/menu.toml` settings file (which
/// doesn't exist - there are no global settings, just this list) since it's
/// an arbitrarily-long named list - same "own file" reasoning as
/// `ShopListConfig`/`LotteryListConfig`. **Not** `#[serde(transparent)]` for
/// the same reason as those: TOML documents must be a table at the root.
#[derive(Deserialize, Serialize, Clone)]
pub struct MenuListConfig {
    pub menus: Vec<MenuConfig>,
}

impl Default for MenuListConfig {
    fn default() -> Self {
        Self {
            menus: vec![MenuConfig::default()],
        }
    }
}

impl LoadConfiguration for MenuListConfig {
    fn get_path() -> &'static Path {
        Path::new("menu/menus.toml")
    }

    fn validate(&self) {}
}

/// One floating menu: a title plus a row of clickable buttons.
///
/// All placed relative to a fixed anchor computed once when the menu opens
/// (the player's eye position, `distance` blocks ahead along their
/// horizontal facing at that moment - not continuously re-tracked as they
/// look around).
#[derive(Deserialize, Serialize, Clone)]
pub struct MenuConfig {
    pub name: String,
    pub title: String,
    /// Blocks in front of the player's eyes (horizontal facing only - pitch
    /// is ignored so looking up/down at open time doesn't tilt the menu).
    #[serde(default = "default_distance")]
    pub distance: f64,
    /// Height of the title above the anchor, in blocks.
    #[serde(default = "default_title_height")]
    pub title_height: f64,
    pub buttons: Vec<MenuButton>,
}

impl Default for MenuConfig {
    fn default() -> Self {
        Self {
            name: "main".to_string(),
            title: "主菜单".to_string(),
            distance: default_distance(),
            title_height: default_title_height(),
            buttons: vec![
                MenuButton {
                    item: "compass".to_string(),
                    label: "回到主城".to_string(),
                    command: "spawn".to_string(),
                    offset_right: -1.2,
                    offset_up: 0.0,
                    offset_forward: 0.0,
                    scale: default_scale(),
                },
                MenuButton {
                    item: "red_bed".to_string(),
                    label: "回到我的世界".to_string(),
                    command: "home".to_string(),
                    offset_right: 0.0,
                    offset_up: 0.0,
                    offset_forward: 0.0,
                    scale: default_scale(),
                },
                MenuButton {
                    item: "emerald".to_string(),
                    label: "全球市场".to_string(),
                    command: "market list".to_string(),
                    offset_right: 1.2,
                    offset_up: 0.0,
                    offset_forward: 0.0,
                    scale: default_scale(),
                },
            ],
        }
    }
}

#[derive(Deserialize, Serialize, Clone)]
pub struct MenuButton {
    /// Vanilla item resource name shown as the button's icon.
    pub item: String,
    pub label: String,
    /// Run through the command dispatcher as the clicking player (like they
    /// typed it themselves) when this button is clicked. `%player%` is
    /// substituted with the clicking player's name first, for parity with
    /// `NpcEntry::click_command` - most buttons (`spawn`, `home`, ...) don't
    /// need it since they already act on the command's own player sender.
    pub command: String,
    /// Offset from the menu's anchor, in blocks: right (positive = the
    /// player's own right hand at open time), up, and forward (positive =
    /// further from the player).
    #[serde(default)]
    pub offset_right: f64,
    #[serde(default)]
    pub offset_up: f64,
    #[serde(default)]
    pub offset_forward: f64,
    #[serde(default = "default_scale")]
    pub scale: f64,
}

const fn default_distance() -> f64 {
    3.0
}

const fn default_title_height() -> f64 {
    0.8
}

const fn default_scale() -> f64 {
    1.5
}
// EMBER end
