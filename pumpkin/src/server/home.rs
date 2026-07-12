// EMBER start - per-player home worlds
//! Per-player home worlds: `home_<uuid>`, cloned from a template world.
//!
//! Cloned the first time a player visits, loaded directly from disk every
//! time after that. See `pumpkin_config::HomeConfig`'s doc comment for why
//! the template name lives in its own `home/home.toml` rather than
//! `ember.toml`.

use pumpkin_config::{HomeConfig, LoadConfiguration};
use uuid::Uuid;

pub struct HomeManager {
    template_world: String,
}

impl HomeManager {
    /// Loads `home/home.toml` itself (own folder, own file), the same
    /// self-contained pattern as `EconomyManager::new`/`LoginManager::new`.
    #[must_use]
    pub fn new() -> Self {
        let exec_dir = std::env::current_dir().expect("Failed to get current directory");
        Self::from_config(&HomeConfig::load(&exec_dir))
    }

    fn from_config(config: &HomeConfig) -> Self {
        Self {
            template_world: config.template_world.clone(),
        }
    }

    #[must_use]
    pub fn template_world(&self) -> &str {
        &self.template_world
    }

    /// The world name a given player's home lives in. Centralized here so
    /// every caller (the `/home` command today, anything else later) agrees
    /// on the same naming convention.
    #[must_use]
    pub fn world_name_for(player_uuid: Uuid) -> String {
        format!("home_{player_uuid}")
    }
}

impl Default for HomeManager {
    fn default() -> Self {
        Self::new()
    }
}
// EMBER end
