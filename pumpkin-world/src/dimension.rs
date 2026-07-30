use std::{path::PathBuf, sync::Arc};

use pumpkin_config::world::LevelConfig;
use pumpkin_data::dimension::Dimension;

use crate::chunk_system::GenPoolBudget;
use crate::level::Level;

// EMBER start - folderless MySQL worlds
/// Returns the logical database root for a vanilla dimension.
///
/// `MySQL` worlds are independent top-level worlds, not `DIM-1`/`DIM1`
/// children of an on-disk save. Keeping this transformation here makes every
/// caller use the same stable key.
#[must_use]
pub fn mysql_dimension_root(mut base: PathBuf, dimension: &Dimension) -> PathBuf {
    let suffix = if dimension.minecraft_name == Dimension::THE_NETHER.minecraft_name {
        "_nether"
    } else if dimension.minecraft_name == Dimension::THE_END.minecraft_name {
        "_end"
    } else {
        ""
    };
    if suffix.is_empty() {
        return base;
    }
    let Some(name) = base.file_name().and_then(|name| name.to_str()) else {
        return base;
    };
    if !name.ends_with(suffix) {
        base.set_file_name(format!("{name}{suffix}"));
    }
    base
}
// EMBER end

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
    // EMBER start - per-world sidecar config / folderless MySQL worlds
    // A forced MySQL world has no folder from which a sidecar could be read.
    // Its logical name is also the database key, with vanilla dimensions
    // split into independent top-level names.
    let mysql = matches!(
        &level_config.chunk,
        pumpkin_config::chunk::ChunkConfig::Easy(config)
            if config.backend == pumpkin_config::chunk::EasyBackend::Mysql
    );
    let resolved = if mysql {
        level_config.clone()
    } else {
        pumpkin_config::ember_world::resolve_level_config(level_config, &base_directory)
    };
    let level_config = &resolved;
    if mysql {
        base_directory = mysql_dimension_root(base_directory, &dimension);
    } else if dimension.minecraft_name == Dimension::OVERWORLD.minecraft_name {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mysql_vanilla_dimensions_have_independent_roots() {
        let base = PathBuf::from("world");
        assert_eq!(
            mysql_dimension_root(base.clone(), &Dimension::OVERWORLD),
            base
        );
        assert_eq!(
            mysql_dimension_root(PathBuf::from("world"), &Dimension::THE_NETHER),
            PathBuf::from("world_nether")
        );
        assert_eq!(
            mysql_dimension_root(PathBuf::from("world"), &Dimension::THE_END),
            PathBuf::from("world_end")
        );
        assert_eq!(
            mysql_dimension_root(PathBuf::from("world_nether"), &Dimension::THE_NETHER),
            PathBuf::from("world_nether")
        );
    }
}
