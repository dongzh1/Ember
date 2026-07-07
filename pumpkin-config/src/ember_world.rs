// EMBER - per-world configuration sidecar
//
// A world folder may contain an `ember-world.toml` file overriding the
// global `[world]` configuration for that world only. This is what lets
// four very different world archetypes (hub, personal, resource, dungeon)
// coexist on one running server, each with its own storage format,
// residency policy and autosave cadence.
//
// Example `<world folder>/ember-world.toml`:
//
// ```toml
// archetype = "hub"          # default | personal | hub | resource | dungeon
// residency = "auto"         # auto | full | lazy
// autosave_ticks = 24000
//
// [chunk]
// type = "easy_shard"        # any [world.chunk] value is accepted here
// group_chunks = 1
// ```
//
// Every key is optional; missing keys fall back to the global config.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::{error, info};

use crate::{chunk::ChunkConfig, world::LevelConfig};

/// File name of the per-world configuration sidecar.
pub const SIDECAR_FILE: &str = "ember-world.toml";

/// Hard ceiling on fully-resident regions even for `residency = "full"`.
///
/// Protects a misconfigured unbounded world from eating all RAM.
/// 64 regions = a 32768x32768-block area, tens of MB to a few GB resident.
pub const MAX_RESIDENT_REGIONS: usize = 64;

/// What kind of world this is. Drives the `auto` residency decision and
/// serves as operator documentation; it never changes data on disk.
#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum WorldArchetype {
    /// No special treatment (the global defaults).
    #[default]
    #[serde(rename = "default")]
    Default,
    /// Per-player world: small, must persist, many exist but few are loaded.
    #[serde(rename = "personal")]
    Personal,
    /// Main city: small, near-static, many players gathered — read-heavy.
    #[serde(rename = "hub")]
    Hub,
    /// Infinite mining/exploration world: write-heavy, periodically reset.
    #[serde(rename = "resource")]
    Resource,
    /// Ephemeral instance world: template-based, changes are discarded.
    #[serde(rename = "dungeon")]
    Dungeon,
}

/// How much of the world's stored region data is kept resident in memory.
#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Residency {
    /// Decide from the world's stored size: one region or less (a 512x512
    /// world) is fully prewarmed and kept resident; `hub`/`dungeon` worlds
    /// stay resident up to 4 regions (1024x1024); everything larger loads
    /// lazily.
    #[default]
    #[serde(rename = "auto")]
    Auto,
    /// Prewarm and keep every stored region resident
    /// (capped at [`MAX_RESIDENT_REGIONS`]).
    #[serde(rename = "full")]
    Full,
    /// Regions load on demand only (the pre-sidecar behaviour).
    #[serde(rename = "lazy")]
    Lazy,
}

/// Contents of a world's `ember-world.toml` sidecar.
#[derive(Deserialize, Serialize, Clone, Default)]
#[serde(default)]
pub struct EmberWorldConfig {
    /// World archetype (see [`WorldArchetype`]).
    pub archetype: WorldArchetype,
    /// Region residency policy (see [`Residency`]).
    pub residency: Residency,
    /// Chunk-storage override for this world; `None` uses the global
    /// `[world.chunk]` value.
    pub chunk: Option<ChunkConfig>,
    /// Autosave override for this world; `None` uses the global value.
    pub autosave_ticks: Option<u64>,
}

impl EmberWorldConfig {
    /// Loads `<world_root>/ember-world.toml`. Returns `None` when the file
    /// is absent; a present-but-invalid file is reported loudly and treated
    /// as absent so a typo can never silently switch storage formats.
    #[must_use]
    pub fn load(world_root: &Path) -> Option<Self> {
        let path = world_root.join(SIDECAR_FILE);
        let text = std::fs::read_to_string(&path).ok()?;
        match toml::from_str::<Self>(&text) {
            Ok(config) => {
                info!(
                    "EasyWorld: loaded sidecar {} (archetype {:?}, residency {:?})",
                    path.display(),
                    config.archetype,
                    config.residency,
                );
                Some(config)
            }
            Err(e) => {
                error!(
                    "EasyWorld: IGNORING invalid sidecar {}: {e}",
                    path.display()
                );
                None
            }
        }
    }

    /// Overlays this sidecar on the global `[world]` configuration.
    #[must_use]
    pub fn resolve(&self, global: &LevelConfig) -> LevelConfig {
        LevelConfig {
            chunk: self.chunk.clone().unwrap_or_else(|| global.chunk.clone()),
            lighting: global.lighting,
            autosave_ticks: self.autosave_ticks.unwrap_or(global.autosave_ticks),
        }
    }

    /// Maximum number of stored regions this world may keep fully resident.
    /// `0` means "load lazily, never prewarm".
    #[must_use]
    pub const fn resident_region_cap(&self) -> usize {
        match self.residency {
            Residency::Full => MAX_RESIDENT_REGIONS,
            Residency::Lazy => 0,
            Residency::Auto => match self.archetype {
                WorldArchetype::Hub | WorldArchetype::Dungeon => 4,
                _ => 1,
            },
        }
    }
}

/// Resolves the effective [`LevelConfig`] for a world folder: the sidecar
/// overlay when present, the global configuration otherwise.
#[must_use]
pub fn resolve_level_config(global: &LevelConfig, world_root: &Path) -> LevelConfig {
    EmberWorldConfig::load(world_root).map_or_else(|| global.clone(), |s| s.resolve(global))
}

/// Writes (or updates) a world's sidecar so its chunk format is explicit
/// on disk — used by `/world convert` after a migration.
///
/// # Errors
/// Fails when serialization or the file write fails.
pub fn write_sidecar_chunk(world_root: &Path, chunk: ChunkConfig) -> Result<(), String> {
    let mut config = EmberWorldConfig::load(world_root).unwrap_or_default();
    config.chunk = Some(chunk);
    let text = toml::to_string_pretty(&config).map_err(|e| e.to_string())?;
    std::fs::write(world_root.join(SIDECAR_FILE), text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_sidecar_is_none() {
        assert!(EmberWorldConfig::load(Path::new("Z:/definitely/not/here")).is_none());
    }

    #[test]
    fn resolve_overlays_only_present_keys() {
        let global = LevelConfig::default();
        let sidecar = EmberWorldConfig {
            autosave_ticks: Some(1234),
            ..Default::default()
        };
        let resolved = sidecar.resolve(&global);
        assert_eq!(resolved.autosave_ticks, 1234);
        // Chunk stays the global default when the sidecar has no override.
        assert!(matches!(resolved.chunk, crate::chunk::ChunkConfig::Easy));
    }

    #[test]
    fn residency_caps() {
        let mut c = EmberWorldConfig::default();
        assert_eq!(c.resident_region_cap(), 1); // auto + default archetype
        c.archetype = WorldArchetype::Hub;
        assert_eq!(c.resident_region_cap(), 4); // auto + hub
        c.residency = Residency::Lazy;
        assert_eq!(c.resident_region_cap(), 0);
        c.residency = Residency::Full;
        assert_eq!(c.resident_region_cap(), MAX_RESIDENT_REGIONS);
    }

    #[test]
    fn sidecar_toml_roundtrip() {
        let text = r#"
archetype = "resource"
residency = "lazy"
autosave_ticks = 6000

[chunk]
type = "easy_shard"
group_chunks = 4
"#;
        let parsed: EmberWorldConfig = toml::from_str(text).unwrap();
        assert_eq!(parsed.archetype, WorldArchetype::Resource);
        assert_eq!(parsed.residency, Residency::Lazy);
        assert_eq!(parsed.autosave_ticks, Some(6000));
        match parsed.chunk {
            Some(ChunkConfig::EasyShard(cfg)) => assert_eq!(cfg.group_chunks, 4),
            _ => panic!("expected easy_shard chunk override"),
        }
    }
}
