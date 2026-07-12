use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::LoadConfiguration;

// EMBER start - built-in economy system
/// Configuration for Ember's built-in economy system.
///
/// Deliberately server-builtin rather than a plugin (see `EMBER.md`'s
/// "服务端 vs 插件边界" section for why this is an intentional exception).
/// Balances are stored in `MySQL` only — there is no file-backed mode.
///
/// Lives in its own `economy/economy.toml`, not `ember.toml`: a feature this
/// size (currencies, `MySQL` url, starting balance) gets its own folder
/// rather than being one more section in Ember's general settings file.
#[derive(Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct EconomyConfig {
    /// Whether the economy system is active. Off by default: this feature
    /// requires a `MySQL` database, and most Ember servers shouldn't be
    /// forced to stand one up just to boot.
    pub enabled: bool,
    /// `MySQL` connection URL, e.g. `mysql://user:pass@localhost:3306/ember`.
    pub url: String,
    /// Currency used when a command/API call doesn't specify one.
    #[serde(default = "default_currency_id")]
    pub default_currency: String,
    /// All currencies players can hold a balance in. Amounts are always
    /// whole numbers (no fractional currency).
    #[serde(default = "default_currencies")]
    pub currencies: Vec<CurrencyConfig>,
    /// Balance a player has in a currency before they've ever received or
    /// spent any of it (accounts are created lazily on first write, not on
    /// join).
    pub starting_balance: i64,
}

// A manual `Default` (not derived) so `default_currency`/`currencies` carry
// their documented non-empty defaults even when the whole `[economy]` table
// is missing from the config file, matching `EasyConfig`'s precedent
// (`chunk.rs`) for the same reason: a derived `Default` would silently give
// an empty currency list instead of the one usable out of the box.
impl Default for EconomyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            url: String::new(),
            default_currency: default_currency_id(),
            currencies: default_currencies(),
            starting_balance: 0,
        }
    }
}

fn default_currency_id() -> String {
    "coins".to_string()
}

fn default_currencies() -> Vec<CurrencyConfig> {
    vec![CurrencyConfig {
        id: default_currency_id(),
        display_name: "Coins".to_string(),
    }]
}

/// A single currency players can hold a balance in.
#[derive(Deserialize, Serialize, Clone)]
pub struct CurrencyConfig {
    /// Stable identifier used in commands/API calls and as the storage key
    /// (e.g. `coins`, `gems`). Not user-facing.
    pub id: String,
    /// Name shown to players in command feedback (e.g. `Coins`).
    pub display_name: String,
}

impl LoadConfiguration for EconomyConfig {
    fn get_path() -> &'static Path {
        Path::new("economy/economy.toml")
    }

    fn validate(&self) {}
}
// EMBER end
