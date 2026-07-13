use std::{path::PathBuf, sync::Arc};

use pumpkin_config::world::LevelConfig;
use pumpkin_data::dimension::Dimension;

use crate::chunk_system::GenPoolBudget;
use crate::level::Level;

#[must_use]
// EMBER: returns the resolved `ChunkConfig` alongside the `Level` - callers
// that need to know the world's chosen chunk backend after this returns
// (furniture/custom block mysql-vs-file storage) have no other way to get
// it, since `Level` itself doesn't retain the `LevelConfig` it was built
// from.
pub fn into_level(
    dimension: Dimension,
    level_config: &LevelConfig,
    mut base_directory: PathBuf,
    seed: i64,
    gen_pool: Option<Arc<rayon::ThreadPool>>,
    // EMBER start - cross-world gen_pool admission control
    gen_budget: Option<Arc<GenPoolBudget>>,
    // EMBER end
) -> (Arc<Level>, pumpkin_config::chunk::ChunkConfig) {
    // EMBER start - per-world sidecar config (ember-world.toml)
    // Resolved at the world root, before any dimension sub-path, so one
    // sidecar governs every dimension of the world.
    let resolved = pumpkin_config::ember_world::resolve_level_config(level_config, &base_directory);
    let level_config = &resolved;
    // EMBER end
    if dimension.minecraft_name == Dimension::OVERWORLD.minecraft_name {
    } else if dimension.minecraft_name == Dimension::THE_NETHER.minecraft_name {
        base_directory.push("DIM-1");
    } else if dimension.minecraft_name == Dimension::THE_END.minecraft_name {
        base_directory.push("DIM1");
    }
    let level = Level::from_root_folder(
        level_config,
        base_directory,
        seed,
        dimension,
        gen_pool,
        gen_budget, // EMBER
    );
    (level, level_config.chunk.clone()) // EMBER
}
