use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::LoadConfiguration;

// EMBER start - HUD system (boss-bar display, references BetterHud)
/// Settings for Ember's built-in HUD system: one boss bar per player,
/// refreshed periodically, its title built from `%player_health%`-style
/// placeholders (see `server::placeholder::PlaceholderManager`).
///
/// Lives in its own `hud/hud.toml`, not `ember.toml` - same "own folder for
/// a feature-sized config" reasoning as `economy/economy.toml`.
#[derive(Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct HudConfig {
    /// Whether the HUD system is active at all.
    pub enabled: bool,
    /// How often (in ticks) the boss bar content is recomputed and resent.
    /// Doesn't need to be every tick - a HUD showing coordinates/health
    /// doesn't need sub-second precision, and resending a boss bar title
    /// every single tick is needless packet traffic for content that
    /// mostly doesn't change tick-to-tick.
    #[serde(default = "default_refresh_ticks")]
    pub refresh_ticks: u32,
    /// Whether a player who has never used `/hud toggle` sees the HUD by
    /// default.
    pub enabled_by_default: bool,
    /// The boss bar title template. `%...%` tokens are expanded through
    /// `PlaceholderManager` - see its built-ins for what's available out of
    /// the box (`player_health`, `player_x`/`y`/`z`, `server_tps`, ...).
    #[serde(default = "default_title")]
    pub title: String,
    /// Custom font resource-pack namespace applied to the title text, if
    /// any (e.g. `"ember:hud"`). Left empty (the default) to use the
    /// client's normal font - no custom resource pack assets required to
    /// use the HUD at all. Only meaningful once matching font-provider
    /// assets exist under the resource pack builder's `source_dir` (see
    /// `resourcepack_builder`); this system doesn't generate them.
    pub font: String,
}

// A manual `Default` (not derived) so `refresh_ticks`/`title` carry their
// documented non-zero/non-empty defaults even when the whole `[hud]` config
// is missing from the file, matching `EconomyConfig`'s precedent for the
// same reason.
impl Default for HudConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            refresh_ticks: default_refresh_ticks(),
            enabled_by_default: true,
            title: default_title(),
            font: String::new(),
        }
    }
}

impl LoadConfiguration for HudConfig {
    fn get_path() -> &'static Path {
        Path::new("hud/hud.toml")
    }

    fn validate(&self) {}
}

const fn default_refresh_ticks() -> u32 {
    20
}

fn default_title() -> String {
    "§c%player_health%§7/§c%player_max_health%❤ §7| §f%player_world% \
     (%player_x%, %player_y%, %player_z%) §7| §e%server_online% online §7| §btps %server_tps%"
        .to_string()
}

/// Per-player HUD on/off preferences, `hud/player_state.toml`.
///
/// Server-managed runtime state (`/hud toggle` mutates and re-saves this),
/// not something an admin hand-authors like `hud.toml` - same "own file,
/// separate from static settings" reasoning as `FurnitureInstanceListConfig`.
/// A player's preference here is a server-wide setting, not a per-world one
/// (whether *this player* wants to see a HUD isn't information about which
/// world they're standing in), so unlike furniture/custom block instances
/// it stays server-level rather than moving into any one world's folder.
#[derive(Deserialize, Serialize, Default, Clone)]
pub struct HudPlayerStateListConfig {
    pub players: Vec<HudPlayerStateConfig>,
}

impl LoadConfiguration for HudPlayerStateListConfig {
    fn get_path() -> &'static Path {
        Path::new("hud/player_state.toml")
    }

    fn validate(&self) {}
}

#[derive(Deserialize, Serialize, Clone)]
pub struct HudPlayerStateConfig {
    pub uuid: uuid::Uuid,
    pub enabled: bool,
}
// EMBER end
