use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::LoadConfiguration;

// EMBER start - offline-mode login verification
/// Configuration for Ember's built-in offline-mode login wall.
///
/// Only meaningful when `online_mode = false`: without Mojang authentication
/// anyone can join under any username, so this makes players register/log
/// in with a password instead. Off by default even then - an admin who just
/// wants offline mode for a private/LAN server shouldn't be surprised by a
/// password wall appearing.
///
/// Lives in its own `auth/auth.toml`, not `ember.toml`: same reasoning as
/// `EconomyConfig` - a feature this size gets its own folder.
///
/// Named `LoginConfig`, not `AuthConfig`: `AuthenticationConfig`
/// (`networking::auth`) already exists for the unrelated Mojang
/// session-server settings - distinct names avoid confusing the two.
#[derive(Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct LoginConfig {
    pub enabled: bool,
    /// `MySQL` connection URL, e.g. `mysql://user:pass@localhost:3306/ember`.
    pub url: String,
    /// How long a successful login is remembered for (same account, same
    /// IP): rejoining within this window skips the password prompt entirely.
    #[serde(default = "default_session_hours")]
    pub session_hours: u64,
    #[serde(default = "default_min_password_length")]
    pub min_password_length: u32,
    /// Wrong login attempts allowed before the player is kicked (register
    /// has no such limit - there's no password to guess yet).
    #[serde(default = "default_max_login_attempts")]
    pub max_login_attempts: u32,
}

impl Default for LoginConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            session_hours: default_session_hours(),
            min_password_length: default_min_password_length(),
            max_login_attempts: default_max_login_attempts(),
        }
    }
}

const fn default_session_hours() -> u64 {
    24
}

const fn default_min_password_length() -> u32 {
    4
}

const fn default_max_login_attempts() -> u32 {
    5
}

impl LoadConfiguration for LoginConfig {
    fn get_path() -> &'static Path {
        Path::new("auth/auth.toml")
    }

    fn validate(&self) {}
}
// EMBER end
