use fun::FunConfig;
use logging::LoggingConfig;
use pumpkin_util::world_seed::Seed;
use pumpkin_util::{Difficulty, GameMode, PermissionLvl, random};
use recipe::RecipeConfig;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use std::path::PathBuf;
use std::{fs, num::NonZeroU8, path::Path};
use tracing::{debug, warn};
pub mod fun;
pub mod logging;
pub mod networking;
pub mod plugins;
pub mod recipe;

pub mod resource_pack;

pub use chat::ChatConfig;
pub use commands::CommandsConfig;
pub use networking::auth::AuthenticationConfig;
pub use networking::bedrock::BedrockConfig;
pub use networking::compression::CompressionConfig;
pub use networking::java::JavaConfig;
pub use networking::lan_broadcast::LANBroadcastConfig;
pub use networking::rcon::RCONConfig;
pub use plugins::PluginsConfig;
pub use pvp::PVPConfig;
pub use server_links::ServerLinksConfig;
// EMBER start - server-wide concurrency tuning
pub use performance::PerformanceConfig;
// EMBER end
// EMBER start - built-in economy system
pub use economy::EconomyConfig;
// EMBER end
// EMBER start - offline-mode login verification
pub use auth::LoginConfig;
// EMBER end
// EMBER start - per-player home worlds
pub use home::HomeConfig;
// EMBER end
// EMBER start - built-in shop/bank/market/lottery system
pub use shop::{
    BankSettings, BankTier, LotteryListConfig, LotteryPity, LotteryPoolConfig, LotteryPrize,
    MarketSettings, MarketSlotTier, ShopConfig, ShopItem, ShopListConfig, ShopSettings,
    ShopSystemConfig,
};
// EMBER end
// EMBER start - floating packet-only menu system
pub use menu::{MenuButton, MenuConfig, MenuListConfig};
// EMBER end
// EMBER start - HUD system (boss-bar display, references BetterHud)
pub use hud::{HudConfig, HudPlayerStateConfig, HudPlayerStateListConfig};
// EMBER end
// EMBER start - resource pack builder (self-generate + self-host/S3)
pub use resourcepack_builder::{
    HostingMode, ResourcePackBuilderConfig, S3Config, SelfHostedConfig,
};
// EMBER end
// EMBER start - custom items (resource-pack-driven, phase 2 of the CraftEngine portation)
pub use custom_item::{CustomItemConfig, CustomItemListConfig};
// EMBER end
// EMBER start - custom furniture (resource-pack-driven, phase 3 of the CraftEngine portation)
pub use furniture::{FurnitureConfig, FurnitureListConfig, RenderMode};
// EMBER end
// EMBER start - custom blocks (resource-pack-driven, phase 4 of the CraftEngine portation)
pub use custom_block::{CustomBlockConfig, CustomBlockListConfig};
// EMBER end

mod commands;

mod chat;
pub mod chunk;
// EMBER start - per-world config sidecar
pub mod ember_world;
// EMBER end
pub mod lighting;
pub mod op;

mod advancement;
// EMBER start - server-wide concurrency tuning
mod performance;
// EMBER end
// EMBER start - built-in economy system
mod economy;
// EMBER end
// EMBER start - offline-mode login verification
mod auth;
// EMBER end
// EMBER start - per-player home worlds
mod home;
// EMBER end
// EMBER start - built-in shop/bank/market/lottery system
mod shop;
// EMBER end
// EMBER start - floating packet-only menu system
mod menu;
// EMBER end
// EMBER start - HUD system (boss-bar display, references BetterHud)
mod hud;
// EMBER end
// EMBER start - resource pack builder (self-generate + self-host/S3)
mod resourcepack_builder;
// EMBER end
// EMBER start - custom items (resource-pack-driven, phase 2 of the CraftEngine portation)
mod custom_item;
// EMBER end
// EMBER start - custom furniture (resource-pack-driven, phase 3 of the CraftEngine portation)
mod furniture;
// EMBER end
// EMBER start - custom blocks (resource-pack-driven, phase 4 of the CraftEngine portation)
mod custom_block;
// EMBER end
mod player_data;
mod pvp;
mod server_links;
pub mod whitelist;
pub mod world;

use advancement::AdvancementConfig;
use networking::NetworkingConfig;
use player_data::PlayerDataConfig;
use resource_pack::ResourcePackConfig;
use world::LevelConfig;

#[derive(Deserialize, Serialize, Default)]
#[serde(default)]
pub struct PumpkinConfig {
    #[serde(flatten)]
    pub basic: BasicConfiguration,
    #[serde(flatten)]
    pub advanced: AdvancedConfiguration,
}

impl LoadConfiguration for PumpkinConfig {
    fn get_path() -> &'static Path {
        Path::new("pumpkin.toml")
    }

    fn validate(&self) {
        self.basic.validate();
        self.advanced.validate();

        let min_vd = NonZeroU8::new(2).unwrap();
        let max_vd = NonZeroU8::new(64).unwrap();

        // Validate Java
        assert!(
            self.advanced.networking.java.view_distance >= min_vd,
            "Java View distance must be at least 2"
        );
        assert!(
            self.advanced.networking.java.view_distance <= max_vd,
            "Java View distance must be less than 64"
        );
        if self.advanced.networking.java.online_mode {
            assert!(
                self.advanced.networking.java.encryption,
                "When online mode is enabled, encryption must be enabled"
            );
        }

        // Validate Bedrock
        assert!(
            self.advanced.networking.bedrock.view_distance >= min_vd,
            "Bedrock View distance must be at least 2"
        );
        assert!(
            self.advanced.networking.bedrock.view_distance <= max_vd,
            "Bedrock View distance must be less than 64"
        );
        if self.advanced.networking.bedrock.online_mode {
            assert!(
                self.advanced.networking.bedrock.encryption,
                "When online mode is enabled, bedrock_encryption must be enabled"
            );
        }

        if self.basic.allow_chat_reports {
            assert!(
                self.advanced.networking.java.online_mode,
                "When allow_chat_reports is enabled, java.online_mode must be enabled"
            );
        }
    }
}

/// Advanced configuration for optional and feature-specific server settings.
///
/// Allows enabling/disabling features, customizing behaviour, and
/// tweaking performance or experimental options.
///
/// `Important`: The configuration should match vanilla by default.
#[derive(Deserialize, Serialize, Default)]
#[serde(default)]
pub struct AdvancedConfiguration {
    /// Logging-related configuration such as log levels and output behaviour.
    pub logging: LoggingConfig,
    /// Resource pack configuration, including enforcement and pack metadata.
    pub resource_pack: ResourcePackConfig,
    /// World and level-related settings beyond basic configuration.
    pub world: LevelConfig,
    /// Networking-related features such as compression, authentication, and LAN broadcast.
    pub networking: NetworkingConfig,
    /// Command system configuration, including availability and permissions.
    pub commands: CommandsConfig,
    /// Chat-related features such as formatting, filtering, and message behaviour.
    pub chat: ChatConfig,
    /// Player-vs-player rules and mechanics.
    pub pvp: PVPConfig,
    /// Server links configuration exposed to clients.
    pub server_links: ServerLinksConfig,
    /// Persistent player data handling and storage behaviour.
    pub player_data: PlayerDataConfig,
    /// Optional fun and experimental features.
    pub fun: FunConfig,
    /// Recipe-related configuration.
    pub recipe: RecipeConfig,
    /// Plugin-related configuration.
    pub plugins: PluginsConfig,
    /// Advancement configuration
    pub advancement: AdvancementConfig,
}

// EMBER start - ember.toml: a separate file for anything Ember adds, so
// pumpkin.toml stays a recognizable, vanilla-Pumpkin-shaped file. Loaded the
// same way as `PumpkinConfig` (see `main.rs`), just from its own path.
//
// Only small, general settings live here as sections. A feature big enough
// to need its own currencies/URLs/subcommands (economy, and future ones like
// NPC settings) gets its own `LoadConfiguration` impl and folder instead
// (e.g. `economy/economy.toml`) - see `EconomyConfig` - rather than growing
// this struct forever.
/// Simple, general settings Ember adds on top of upstream Pumpkin.
///
/// Kept out of `pumpkin.toml`/`AdvancedConfiguration` on purpose: that file
/// should stay recognizable against vanilla Pumpkin's own docs/examples.
#[derive(Deserialize, Serialize, Default)]
#[serde(default)]
pub struct EmberConfiguration {
    /// Performance/concurrency tuning for shared, process-wide resources.
    pub performance: PerformanceConfig,
}

impl LoadConfiguration for EmberConfiguration {
    fn get_path() -> &'static Path {
        Path::new("ember.toml")
    }

    fn validate(&self) {}
}
// EMBER end

/// Basic configuration for core server settings.
///
/// Covers edition support, world, networking, gameplay rules, and security options.
#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct BasicConfiguration {
    /// The seed for the world generation.
    pub seed: Seed,
    /// The default game difficulty.
    pub default_difficulty: Difficulty,
    /// The op level assigned by the /op command.
    pub op_permission_level: PermissionLvl,
    /// Whether the Nether dimension is enabled.
    pub allow_nether: bool,
    /// Whether the End dimension is enabled.
    pub allow_end: bool,
    /// Whether the server is in hardcore mode.
    pub hardcore: bool,
    /// The server's ticks per second.
    pub tps: f32,
    /// The default gamemode for players.
    pub default_gamemode: GameMode,
    /// If the server forces the gamemode on-join.
    pub force_gamemode: bool,
    /// Whether to remove IPs from logs or not.
    pub scrub_ips: bool,
    /// Whether to use a server favicon.
    pub use_favicon: bool,
    /// Path to optional server favicon.
    pub favicon_path: Option<String>,
    /// The default level name
    pub default_level_name: String,
    /// Whether chat messages should be signed or not.
    pub allow_chat_reports: bool,
    /// Whether to enable the whitelist.
    pub white_list: bool,
    /// Whether to enforce the whitelist.
    pub enforce_whitelist: bool,
}

impl Default for BasicConfiguration {
    fn default() -> Self {
        Self {
            seed: Seed(random::get_seed()),
            default_difficulty: Difficulty::Normal,
            op_permission_level: PermissionLvl::Four,
            allow_nether: true,
            allow_end: true,
            hardcore: false,
            tps: 20.0,
            default_gamemode: GameMode::Survival,
            force_gamemode: false,
            scrub_ips: true,
            use_favicon: true,
            favicon_path: None,
            default_level_name: "world".to_string(),
            allow_chat_reports: false,
            white_list: false,
            enforce_whitelist: false,
        }
    }
}

impl BasicConfiguration {
    /// Returns the path to the server's default world folder.
    #[must_use]
    pub fn get_world_path(&self) -> PathBuf {
        PathBuf::from(&self.default_level_name)
    }

    pub const fn validate(&self) {}
}

impl AdvancedConfiguration {
    pub const fn validate(&self) {
        //self.resource_pack.validate();
    }
}

/// Trait for loading and validating configuration from a TOML file.
///
/// Provides default implementations for loading, merging with defaults,
/// and writing missing values back to disk. Also requires validation logic.
pub trait LoadConfiguration {
    /// Load configuration from the given directory.
    ///
    /// Creates the directory if it doesn't exist, reads the TOML file,
    /// merges it with defaults, writes missing fields, and validates the result.
    #[must_use]
    // NOTE: Logger may not be ready.
    #[expect(clippy::print_stdout)]
    fn load(config_dir: &Path) -> Self
    where
        Self: Sized + Default + Serialize + DeserializeOwned,
    {
        let path = config_dir.join(Self::get_path());
        // EMBER: `create_dir_all` (not just `config_dir` itself) so a
        // `get_path()` naming a subfolder - e.g. `economy/economy.toml` for a
        // feature with its own config folder - creates that folder too.
        if let Some(parent) = path.parent()
            && !parent.exists()
        {
            debug!("creating new config folder: {}", parent.display());
            fs::create_dir_all(parent).expect("Failed to create config folder");
        }

        let config = if path.exists() {
            let file_content = fs::read_to_string(&path).unwrap_or_else(|_| {
                panic!("Couldn't read configuration file at {}", path.display())
            });

            let parsed_toml_value: toml::Value = toml::from_str(&file_content)
                .unwrap_or_else(|err| {
                    panic!(
                        "Couldn't parse TOML at {}. Reason: {}. This is probably caused by invalid TOML syntax",
                        path.display(), err
                    )
                });

            let (merged_config, changed) = Self::merge_with_default_toml(parsed_toml_value);

            if changed {
                println!(
                    "{} changed because values were missing. The missing values were filled with default values.",
                    path.file_name().unwrap().display()
                );
                if let Err(err) = fs::write(&path, toml::to_string(&merged_config).unwrap()) {
                    warn!(
                        "Couldn't write merged config to {}. Reason: {}",
                        path.display(),
                        err
                    );
                }
            }

            merged_config
        } else {
            let content = Self::default();
            if let Err(err) = fs::write(&path, toml::to_string(&content).unwrap()) {
                warn!(
                    "Couldn't write default config to {:?}. Reason: {}",
                    path.display(),
                    err
                );
            }

            content
        };

        config.validate();
        config
    }

    /// Merge a parsed TOML value with the default configuration.
    ///
    /// Returns the merged configuration and a flag indicating if any values were filled.
    #[must_use]
    fn merge_with_default_toml(parsed_toml: toml::Value) -> (Self, bool)
    where
        Self: Sized + Default + Serialize + DeserializeOwned,
    {
        let default_config = Self::default();

        let default_toml_value =
            toml::Value::try_from(default_config).expect("Failed to parse default config");

        let (merged_value, changed) = Self::merge_toml_values(default_toml_value, parsed_toml);

        let config = merged_value
            .try_into()
            .expect("Failed to convert merged config");

        (config, changed)
    }

    /// Merge two TOML values recursively.
    ///
    /// Base is treated as default; overlay overwrites values.
    #[must_use]
    fn merge_toml_values(base: toml::Value, overlay: toml::Value) -> (toml::Value, bool) {
        match (base, overlay) {
            (toml::Value::Table(mut base_table), toml::Value::Table(overlay_table)) => {
                let mut changed = false;

                for key in base_table.keys() {
                    if !overlay_table.contains_key(key) {
                        changed = true;
                        break;
                    }
                }

                for (key, overlay_value) in overlay_table {
                    if let Some(base_value) = base_table.get(&key).cloned() {
                        let (merged_value, value_changed) =
                            Self::merge_toml_values(base_value, overlay_value);
                        base_table.insert(key, merged_value);
                        if value_changed {
                            changed = true;
                        }
                    } else {
                        base_table.insert(key, overlay_value);
                    }
                }
                (toml::Value::Table(base_table), changed)
            }
            (_, overlay) => (overlay, false),
        }
    }

    /// Returns the path to the configuration file relative to the config directory.
    fn get_path() -> &'static Path;

    /// Validates the configuration after loading or merging.
    fn validate(&self);

    // EMBER start - explicit re-save for runtime-mutated TOML configs
    /// Writes the current value back to disk at `config_dir`/`get_path()`.
    ///
    /// `load` already self-heals missing fields on its own, but that's a
    /// one-shot merge at startup - this is for configs a manager keeps
    /// mutating afterward at runtime (e.g. a placed/broken instance list),
    /// which need to persist each change themselves.
    fn save(&self, config_dir: &Path)
    where
        Self: Serialize,
    {
        let path = config_dir.join(Self::get_path());
        if let Some(parent) = path.parent()
            && !parent.exists()
        {
            let _ = fs::create_dir_all(parent);
        }
        if let Err(err) = fs::write(&path, toml::to_string(self).unwrap()) {
            warn!(
                "Couldn't save config to {}. Reason: {}",
                path.display(),
                err
            );
        }
    }
    // EMBER end
}
