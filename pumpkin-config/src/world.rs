use serde::{Deserialize, Serialize};

use crate::{chunk::ChunkConfig, lighting::LightingEngineConfig};

/// Configuration for world and level-specific settings.
///
/// Currently, it includes chunk-related options; more settings may be added later.
#[derive(Deserialize, Serialize, Default, Clone)]
pub struct LevelConfig {
    /// Configuration for chunk behaviour and management.
    pub chunk: ChunkConfig,
    #[serde(default)]
    pub lighting: LightingEngineConfig,
    /// Number of ticks between autosave checks. If 0, autosave is disabled.
    #[serde(default = "default_autosave_ticks")]
    pub autosave_ticks: u64,
    // EMBER start - per-world runtime behaviour resolved from ember-world.toml
    /// Per-world `EasyWorld` runtime settings (generation mode, access mode,
    /// clone source, border). Not read from the global config file — it is
    /// filled from the world's `ember-world.toml` sidecar at load.
    #[serde(skip)]
    pub ember: crate::ember_world::EmberRuntime,
    // EMBER end
    // TODO: More options
}

const fn default_autosave_ticks() -> u64 {
    6000 // Default to 5 minutes at 20 TPS
}
