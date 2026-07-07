// EMBER - per-world configuration sidecar
//
// A world folder may carry an `ember-world.toml` that overrides the global
// `[world]` config for that world only. This is what lets a small map
// (loaded whole into memory, cloned instantly) and a big map (loaded region
// by region) coexist on one server without the operator picking a storage
// format — EasyWorld decides by size.
//
// Example `<world folder>/ember-world.toml`:
//
// ```toml
// border   = 512            # max world size in blocks; <=512 -> small map
// generate = "void"         # seed (default) | void | ocean
// mode     = "read_write"   # read_write (default) | read_only
// source   = "arena"        # read-only clone: read another world's data
// ```
//
// Every key is optional; missing keys fall back to the global config.

use std::path::Path;

use serde::{Deserialize, Serialize};
use tracing::{error, info};

use crate::{
    chunk::{ChunkConfig, EasyWorldMode},
    world::LevelConfig,
};

/// File name of the per-world configuration sidecar.
pub const SIDECAR_FILE: &str = "ember-world.toml";

/// Small-map border threshold (block side length).
///
/// A world whose border is at or below this is loaded whole into memory and
/// cloned by sharing that memory; larger (or borderless) worlds load region
/// by region.
pub const SMALL_MAP_MAX_BORDER: i32 = 512;

/// A centered 512-block border can straddle up to a 2x2 region grid, so a
/// small map is prewarmed up to this many regions.
pub const SMALL_MAP_REGIONS: usize = 4;

/// Hard ceiling for an explicit `/world prewarm`, protecting against
/// prewarming an unbounded world into memory.
pub const MAX_PREWARM_REGIONS: usize = 64;

/// How the world's terrain is produced for chunks that have never been
/// stored.
#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum GenerateMode {
    /// Normal terrain generation from the world seed (default). Untouched
    /// worlds keep generating exactly as before.
    #[default]
    #[serde(rename = "seed")]
    Seed,
    /// Empty world: ungenerated chunks are all air. Good for build/dungeon
    /// maps that supply their own structure.
    #[serde(rename = "void")]
    Void,
    /// Ocean floor: ungenerated chunks are bedrock + stone + water up to sea
    /// level, a blank canvas with a basic base layer.
    #[serde(rename = "ocean")]
    Ocean,
}

/// Per-world runtime behaviour, resolved from the sidecar and carried on
/// [`LevelConfig`] into world construction. Defaults reproduce a normal
/// seed-generated, read-write, unbounded world.
#[derive(Clone, Default, Debug)]
pub struct EmberRuntime {
    /// Storage access mode (`read_write` / `read_only`).
    pub mode: EasyWorldMode,
    /// Read-only clone source world name (reads that world's stored data).
    pub source: Option<String>,
    /// Terrain generation mode.
    pub generate: GenerateMode,
    /// Max world border in blocks (side length); `None` = unbounded.
    pub border: Option<i32>,
}

/// Contents of a world's `ember-world.toml` sidecar.
#[derive(Deserialize, Serialize, Clone, Default)]
#[serde(default)]
pub struct EmberWorldConfig {
    /// Max world border in blocks (side length). When set, the world border
    /// is clamped to it (players cannot build past it) and a value
    /// `<= SMALL_MAP_MAX_BORDER` marks the world as a small map. `None`
    /// leaves the border unbounded (a big map).
    pub border: Option<i32>,
    /// Terrain generation mode (see [`GenerateMode`]).
    pub generate: GenerateMode,
    /// Storage access mode: `read_write` (default) or `read_only`
    /// (never persists; changes are discarded on unload).
    pub mode: EasyWorldMode,
    /// Read-only clone source: the name of another world whose stored data
    /// this world reads. Combined with `mode = "read_only"` it is an
    /// in-memory instance of that world.
    pub source: Option<String>,
    /// Storage override for this world; `None` uses the global
    /// `[world.chunk]` value (usually just to switch backend).
    pub chunk: Option<ChunkConfig>,
    /// Autosave override for this world; `None` uses the global value.
    pub autosave_ticks: Option<u64>,
}

impl EmberWorldConfig {
    /// Loads `<world_root>/ember-world.toml`. Returns `None` when the file
    /// is absent; a present-but-invalid file is reported loudly and treated
    /// as absent so a typo can never silently change a world's behaviour.
    #[must_use]
    pub fn load(world_root: &Path) -> Option<Self> {
        let path = world_root.join(SIDECAR_FILE);
        // Distinguish "absent" from "present but unreadable". Swallowing a
        // read error (permission denied, sharing violation, I/O error) as if
        // the file were absent would silently revert the world to the global
        // config and drop its per-world protective settings (read_only,
        // border, ...) — exactly what the "reported loudly" contract forbids.
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                error!(
                    "EasyWorld: IGNORING unreadable sidecar {} ({e}); falling back to global config",
                    path.display()
                );
                return None;
            }
        };
        match toml::from_str::<Self>(&text) {
            Ok(config) => {
                info!(
                    "EasyWorld: loaded sidecar {} (border {:?}, generate {:?}, mode {:?})",
                    path.display(),
                    config.border,
                    config.generate,
                    config.mode,
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
            ember: self.runtime(),
        }
    }

    /// The runtime behaviour this sidecar declares.
    #[must_use]
    pub fn runtime(&self) -> EmberRuntime {
        EmberRuntime {
            mode: self.mode,
            source: self.source.clone(),
            generate: self.generate,
            border: self.border,
        }
    }

    /// Whether this world is a "small map" (loaded whole into memory).
    #[must_use]
    pub fn is_small_map(&self) -> bool {
        self.border
            .is_some_and(|b| b > 0 && b <= SMALL_MAP_MAX_BORDER)
    }

    /// Maximum number of stored regions to prewarm into memory. Small maps
    /// are loaded whole; big maps load lazily (`0`).
    #[must_use]
    pub fn resident_region_cap(&self) -> usize {
        if self.is_small_map() {
            SMALL_MAP_REGIONS
        } else {
            0
        }
    }
}

/// Resolves the effective [`LevelConfig`] for a world folder: the sidecar
/// overlay when present, the global configuration otherwise.
#[must_use]
pub fn resolve_level_config(global: &LevelConfig, world_root: &Path) -> LevelConfig {
    EmberWorldConfig::load(world_root).map_or_else(|| global.clone(), |s| s.resolve(global))
}

/// Writes a world's sidecar to disk (used when creating a world with an
/// explicit config, e.g. a clone or a dungeon instance).
///
/// # Errors
/// Fails when serialization or the file write fails.
pub fn write_sidecar(world_root: &Path, config: &EmberWorldConfig) -> Result<(), String> {
    let text = toml::to_string_pretty(config).map_err(|e| e.to_string())?;
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
        assert!(matches!(resolved.chunk, ChunkConfig::Easy(_)));
    }

    #[test]
    fn small_map_by_border() {
        let mut c = EmberWorldConfig::default();
        assert!(!c.is_small_map()); // no border -> big
        assert_eq!(c.resident_region_cap(), 0);
        c.border = Some(512);
        assert!(c.is_small_map());
        assert_eq!(c.resident_region_cap(), SMALL_MAP_REGIONS);
        c.border = Some(2048);
        assert!(!c.is_small_map()); // over the threshold -> big
    }

    #[test]
    fn sidecar_toml_roundtrip() {
        let text = r#"
border = 512
generate = "void"
mode = "read_only"
source = "arena"
"#;
        let parsed: EmberWorldConfig = toml::from_str(text).unwrap();
        assert_eq!(parsed.border, Some(512));
        assert_eq!(parsed.generate, GenerateMode::Void);
        assert_eq!(parsed.mode, EasyWorldMode::ReadOnly);
        assert_eq!(parsed.source.as_deref(), Some("arena"));
        assert!(parsed.is_small_map());
    }
}
