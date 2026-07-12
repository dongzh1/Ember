use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::LoadConfiguration;

// EMBER start - per-player home worlds
/// Configuration for Ember's per-player home worlds.
///
/// Lives in its own `home/home.toml`, not `ember.toml`, following the same
/// "big feature gets its own folder" convention as `economy/economy.toml`
/// and `auth/auth.toml`.
#[derive(Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct HomeConfig {
    /// Name of the world new players' homes are cloned from on first visit
    /// (see `Server::clone_world`). An operator is expected to build and
    /// load this world ahead of time; `/home` reports a clear error if it
    /// exists neither loaded nor on disk.
    pub template_world: String,
}

impl Default for HomeConfig {
    fn default() -> Self {
        Self {
            template_world: "home_template".to_string(),
        }
    }
}

impl LoadConfiguration for HomeConfig {
    fn get_path() -> &'static Path {
        Path::new("home/home.toml")
    }

    fn validate(&self) {}
}
// EMBER end
