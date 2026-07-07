// EMBER - world storage format detection & conversion
//
// Two jobs:
//
//  1. `detect_on_disk_config` — called when a world opens. If the region
//     folder stores files of a DIFFERENT format than the configured one,
//     the on-disk format wins (with a loud error log). Without this, a
//     format switch (e.g. changing the global default) would make every
//     stored chunk read as `Missing` and the generator would silently
//     regenerate the world's terrain over the players' builds.
//
//  2. `convert_world` — explicit migration between formats (backing
//     `/world convert`). All formats share the same chunk NBT payload, so
//     conversion is read-all + save-all; source files are renamed to
//     `*.bak` afterwards.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pumpkin_config::chunk::{AnvilChunkConfig, ChunkConfig, EasyBackend, EasyConfig};
use pumpkin_util::math::vector2::Vector2;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::chunk::easy_mysql::EasyMysqlStorage;
use crate::chunk::format::anvil::AnvilChunkFile;
use crate::chunk::format::easy::EasyWorldFile;
use crate::chunk::format::linear::LinearV2File;
use crate::chunk::format::pump::PumpFile;
use crate::chunk::io::{FileIO, LoadedData, file_manager::ChunkFileManager};
use crate::chunk::{ChunkData, ChunkEntityData};
use crate::level::{LevelFolder, SyncChunk, SyncEntityChunk};

/// Region-file extension of a file-backed format (`None` for the `MySQL`
/// backend, which stores no region files).
#[must_use]
pub const fn extension_of(config: &ChunkConfig) -> Option<&'static str> {
    match config {
        ChunkConfig::Anvil(_) => Some("mca"),
        ChunkConfig::Linear => Some("linear"),
        ChunkConfig::Pump => Some("pump"),
        ChunkConfig::Easy(cfg) => match cfg.backend {
            EasyBackend::File => Some("easy"),
            EasyBackend::Mysql => None,
        },
    }
}

/// Default-config counterpart of a region-file extension.
#[must_use]
pub fn config_for_extension(ext: &str) -> Option<ChunkConfig> {
    match ext {
        "mca" => Some(ChunkConfig::Anvil(AnvilChunkConfig::default())),
        "linear" => Some(ChunkConfig::Linear),
        "pump" => Some(ChunkConfig::Pump),
        "easy" => Some(ChunkConfig::Easy(EasyConfig::default())),
        _ => None,
    }
}

/// Parses an operator-facing format name (`/world convert`).
#[must_use]
pub fn config_for_name(name: &str) -> Option<ChunkConfig> {
    match name {
        "anvil" => Some(ChunkConfig::Anvil(AnvilChunkConfig::default())),
        "linear" => Some(ChunkConfig::Linear),
        "pump" => Some(ChunkConfig::Pump),
        "easy" => Some(ChunkConfig::Easy(EasyConfig::default())),
        _ => None,
    }
}

/// The entity-chunk file extension a config stores. The `MySQL` backend
/// keeps entities in `.easy` files, so it maps to `easy`.
#[must_use]
pub fn entity_extension_of(config: &ChunkConfig) -> Option<&'static str> {
    match config {
        ChunkConfig::Easy(cfg) if cfg.backend == EasyBackend::Mysql => Some("easy"),
        other => extension_of(other),
    }
}

/// `r.<x>.<z>.<ext>` region coordinates present in `dir`.
#[must_use]
pub fn scan_regions(dir: &Path, ext: &str) -> Vec<(i32, i32)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let mut parts = name.split('.');
        if parts.next() != Some("r") {
            continue;
        }
        let (Some(x), Some(z), Some(e)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if e != ext || parts.next().is_some() {
            continue; // wrong extension or trailing suffix (e.g. .bak/.tmp)
        }
        if let (Ok(x), Ok(z)) = (x.parse::<i32>(), z.parse::<i32>()) {
            out.push((x, z));
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

const KNOWN_EXTENSIONS: &[&str] = &["mca", "linear", "pump", "easy"];

/// Guards a world against silent format switches.
///
/// When the region folder already stores files of another known format and
/// none of the configured one, the on-disk format is returned (with a loud
/// error log) so existing terrain keeps loading. `/world convert` performs
/// deliberate migrations.
#[must_use]
pub fn detect_on_disk_config(configured: &ChunkConfig, region_folder: &Path) -> ChunkConfig {
    let Some(configured_ext) = extension_of(configured) else {
        // DB/RAM-backed formats have no files to disagree with.
        return configured.clone();
    };

    let counts: Vec<(&str, usize)> = KNOWN_EXTENSIONS
        .iter()
        .map(|ext| (*ext, scan_regions(region_folder, ext).len()))
        .filter(|(_, n)| *n > 0)
        .collect();

    if counts.is_empty() || counts.iter().any(|(ext, _)| *ext == configured_ext) {
        // Fresh world, or the configured format already has files.
        return configured.clone();
    }

    // Files exist, but none in the configured format: honor the disk.
    let Some((best_ext, n)) = counts.iter().max_by_key(|(_, n)| *n) else {
        return configured.clone();
    };
    let Some(detected) = config_for_extension(best_ext) else {
        return configured.clone();
    };
    error!(
        "World {} stores {n} '.{best_ext}' region file(s) but the configured chunk format is \
         '{configured_ext}'. HONORING THE ON-DISK FORMAT so terrain is not regenerated — \
         run `/world convert <name> {configured_ext}` (world unloaded) to migrate.",
        region_folder.display(),
    );
    detected
}

/// Picks the conversion SOURCE for a dimension folder.
///
/// Chooses the on-disk format with the most region files, excluding the
/// target's own extension — a rerun after a partial conversion never
/// mistakes freshly written target files for "already done". `None` when
/// nothing (else) is stored.
#[must_use]
pub fn detect_source_for_conversion(
    region_folder: &Path,
    target: &ChunkConfig,
) -> Option<ChunkConfig> {
    let target_ext = extension_of(target);
    KNOWN_EXTENSIONS
        .iter()
        .filter(|ext| Some(**ext) != target_ext)
        .map(|ext| (*ext, scan_regions(region_folder, ext).len()))
        .filter(|(_, n)| *n > 0)
        .max_by_key(|(_, n)| *n)
        .and_then(|(ext, _)| config_for_extension(ext))
}

// ─── Conversion ────────────────────────────────────────────────────────

/// Result of a world conversion.
#[derive(Default, Debug)]
pub struct ConvertStats {
    pub regions: usize,
    pub chunks: usize,
    pub entity_chunks: usize,
    /// Chunks that failed to read from the source (corrupt) and were skipped.
    pub skipped: usize,
}

fn chunk_saver_for(config: &ChunkConfig) -> Arc<dyn FileIO<Data = SyncChunk>> {
    match config {
        ChunkConfig::Linear => Arc::new(ChunkFileManager::<LinearV2File<ChunkData>>::new(())),
        ChunkConfig::Anvil(c) => Arc::new(ChunkFileManager::<AnvilChunkFile<ChunkData>>::new(
            c.clone(),
        )),
        ChunkConfig::Pump => Arc::new(ChunkFileManager::<PumpFile<ChunkData>>::new(())),
        ChunkConfig::Easy(cfg) => match cfg.backend {
            EasyBackend::File => Arc::new(ChunkFileManager::<EasyWorldFile<ChunkData>>::new(())),
            EasyBackend::Mysql => Arc::new(EasyMysqlStorage::new(
                &cfg.mysql(pumpkin_config::chunk::EasyWorldMode::ReadWrite),
            )),
        },
    }
}

fn entity_saver_for(config: &ChunkConfig) -> Arc<dyn FileIO<Data = SyncEntityChunk>> {
    match config {
        ChunkConfig::Linear => Arc::new(ChunkFileManager::<LinearV2File<ChunkEntityData>>::new(())),
        ChunkConfig::Anvil(c) => Arc::new(
            ChunkFileManager::<AnvilChunkFile<ChunkEntityData>>::new(c.clone()),
        ),
        ChunkConfig::Pump => Arc::new(ChunkFileManager::<PumpFile<ChunkEntityData>>::new(())),
        // Both easy backends store entities in file-based `.easy` regions.
        ChunkConfig::Easy(_) => {
            Arc::new(ChunkFileManager::<EasyWorldFile<ChunkEntityData>>::new(()))
        }
    }
}

/// All 1024 chunk coordinates of a region.
fn region_coords(rx: i32, rz: i32) -> Vec<Vector2<i32>> {
    let mut coords = Vec::with_capacity(1024);
    for z in 0..32 {
        for x in 0..32 {
            coords.push(Vector2::new((rx << 5) + x, (rz << 5) + z));
        }
    }
    coords
}

/// Fetches every stored chunk of one region from `saver`.
async fn pull_region<D: Send + Sync>(
    saver: &Arc<dyn FileIO<Data = D>>,
    folder: &LevelFolder,
    rx: i32,
    rz: i32,
    pos_of: impl Fn(&D) -> Vector2<i32>,
) -> (Vec<(Vector2<i32>, D)>, usize) {
    let coords = region_coords(rx, rz);
    let (send, mut recv) = mpsc::channel(64);
    let fetch = saver.fetch_chunks(folder, &coords, send);
    let collect = async {
        let mut loaded = Vec::new();
        let mut skipped = 0usize;
        while let Some(data) = recv.recv().await {
            match data {
                LoadedData::Loaded(d) => {
                    let pos = pos_of(&d);
                    loaded.push((pos, d));
                }
                LoadedData::Missing(_) => {}
                LoadedData::Error((pos, e)) => {
                    warn!("convert: skipping corrupt chunk {pos:?}: {e}");
                    skipped += 1;
                }
            }
        }
        (loaded, skipped)
    };
    let ((), result) = tokio::join!(fetch, collect);
    result
}

/// Renames every `r.*.<ext>` file in `dir` to `<name>.bak`.
fn backup_files(dir: &Path, ext: &str) -> usize {
    let mut renamed = 0usize;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let mut parts = name.split('.');
        if parts.next() != Some("r") {
            continue;
        }
        let (Some(_), Some(_), Some(e), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if e != ext {
            continue;
        }
        let src = entry.path();
        let dst = src.with_extension(format!("{ext}.bak"));
        if std::fs::rename(&src, &dst).is_ok() {
            renamed += 1;
        }
    }
    renamed
}

/// Converts one dimension tree of a world from `from` to `to`.
///
/// The world must NOT be loaded. Source region files are renamed to
/// `*.bak` after a successful conversion (database sources are left
/// untouched).
///
/// # Errors
/// Fails when the formats are identical, a saver cannot be constructed, or
/// writing to the target fails. Corrupt source chunks are skipped and
/// counted in [`ConvertStats::skipped`].
pub async fn convert_world(
    folder: &LevelFolder,
    from: &ChunkConfig,
    to: &ChunkConfig,
) -> Result<ConvertStats, String> {
    let from_ext = extension_of(from);
    let to_ext = extension_of(to);
    if from_ext.is_some() && from_ext == to_ext {
        return Err("world already stores that format".to_string());
    }

    let from_chunks = chunk_saver_for(from);
    let to_chunks = chunk_saver_for(to);
    let from_entities = entity_saver_for(from);
    let to_entities = entity_saver_for(to);

    let mut stats = ConvertStats::default();

    // ── Chunk data ──
    let regions = match from_ext {
        Some(ext) => scan_regions(&folder.region_folder, ext),
        None => from_chunks.list_regions(folder).await,
    };
    for (rx, rz) in regions {
        let (mut loaded, skipped) = pull_region(&from_chunks, folder, rx, rz, |c: &SyncChunk| {
            Vector2::new(c.x, c.z)
        })
        .await;
        stats.skipped += skipped;
        if loaded.is_empty() {
            continue;
        }
        for (_, chunk) in &mut loaded {
            chunk
                .dirty
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        stats.chunks += loaded.len();
        stats.regions += 1;
        to_chunks
            .save_chunks(folder, loaded)
            .await
            .map_err(|e| format!("writing region ({rx},{rz}): {e}"))?;
    }

    // ── Entity data ──
    let entity_from_ext = entity_extension_of(from);
    let entity_regions =
        entity_from_ext.map_or_else(Vec::new, |ext| scan_regions(&folder.entities_folder, ext));
    for (rx, rz) in entity_regions {
        let (mut loaded, skipped) =
            pull_region(&from_entities, folder, rx, rz, |c: &SyncEntityChunk| {
                Vector2::new(c.x, c.z)
            })
            .await;
        stats.skipped += skipped;
        if loaded.is_empty() {
            continue;
        }
        for (_, chunk) in &mut loaded {
            chunk
                .dirty
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        stats.entity_chunks += loaded.len();
        to_entities
            .save_chunks(folder, loaded)
            .await
            .map_err(|e| format!("writing entity region ({rx},{rz}): {e}"))?;
    }

    // Drain any in-flight writes before touching the source files.
    to_chunks.block_and_await_ongoing_tasks().await;
    to_entities.block_and_await_ongoing_tasks().await;

    // ── Backup the source files ──
    if let Some(ext) = from_ext {
        let n = backup_files(&folder.region_folder, ext);
        info!("convert: renamed {n} source region file(s) to .bak");
    }
    if let Some(ext) = entity_from_ext {
        // Entity files share the target extension only when formats match;
        // never back up files the target just wrote.
        if entity_extension_of(to) != Some(ext) {
            let n = backup_files(&folder.entities_folder, ext);
            info!("convert: renamed {n} source entity file(s) to .bak");
        }
    }

    Ok(stats)
}

/// Finds every dimension tree of a world folder (the root plus vanilla
/// `DIM-1`/`DIM1` sub-roots), mirroring `Level::from_root_folder`'s layout.
#[must_use]
pub fn discover_dimension_folders(world_root: &Path) -> Vec<LevelFolder> {
    let mut out = Vec::new();
    let candidates = [
        world_root.to_path_buf(),
        world_root.join("DIM-1"),
        world_root.join("DIM1"),
    ];
    for root in candidates {
        let dimensions = root.join("dimensions");
        let Ok(namespaces) = std::fs::read_dir(&dimensions) else {
            continue;
        };
        for ns in namespaces.flatten() {
            let Ok(names) = std::fs::read_dir(ns.path()) else {
                continue;
            };
            for name in names.flatten() {
                let dim_folder: PathBuf = name.path();
                let region_folder = dim_folder.join("region");
                if !region_folder.is_dir() {
                    continue;
                }
                out.push(LevelFolder {
                    root_folder: root.clone(),
                    dim_folder: dim_folder.clone(),
                    region_folder,
                    entities_folder: dim_folder.join("entities"),
                    poi_folder: dim_folder.join("poi"),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_mapping_roundtrips() {
        for ext in KNOWN_EXTENSIONS {
            let config = config_for_extension(ext).unwrap();
            assert_eq!(extension_of(&config), Some(*ext));
        }
        assert!(config_for_extension("dat").is_none());
        assert!(config_for_name("easy").is_some());
        assert!(config_for_name("bogus").is_none());
    }

    #[test]
    fn scan_ignores_bak_and_tmp() {
        let dir = temp_dir::TempDir::new().unwrap();
        std::fs::write(dir.child("r.0.0.pump"), b"x").unwrap();
        std::fs::write(dir.child("r.1.-2.pump"), b"x").unwrap();
        std::fs::write(dir.child("r.0.0.pump.bak"), b"x").unwrap();
        std::fs::write(dir.child("r.0.0.easy.tmp"), b"x").unwrap();
        std::fs::write(dir.child("r.3.3.easy"), b"x").unwrap();
        std::fs::write(dir.child("level.dat"), b"x").unwrap();
        assert_eq!(scan_regions(dir.path(), "pump"), vec![(0, 0), (1, -2)]);
        assert_eq!(scan_regions(dir.path(), "easy"), vec![(3, 3)]);
    }

    fn easy_file() -> ChunkConfig {
        ChunkConfig::Easy(EasyConfig::default())
    }

    fn easy_mysql() -> ChunkConfig {
        ChunkConfig::Easy(EasyConfig {
            backend: EasyBackend::Mysql,
            ..Default::default()
        })
    }

    #[test]
    fn source_detection_excludes_target() {
        let dir = temp_dir::TempDir::new().unwrap();
        std::fs::write(dir.child("r.0.0.pump"), b"x").unwrap();
        // Partial target output must never be mistaken for the source.
        std::fs::write(dir.child("r.0.0.easy"), b"x").unwrap();
        let src = detect_source_for_conversion(dir.path(), &easy_file()).unwrap();
        assert!(matches!(src, ChunkConfig::Pump));

        // Only target-format files present -> nothing to convert.
        let dir2 = temp_dir::TempDir::new().unwrap();
        std::fs::write(dir2.child("r.0.0.easy"), b"x").unwrap();
        assert!(detect_source_for_conversion(dir2.path(), &easy_file()).is_none());
    }

    #[test]
    fn detection_honors_disk_over_config() {
        let dir = temp_dir::TempDir::new().unwrap();
        // Fresh world: config wins.
        assert!(matches!(
            detect_on_disk_config(&easy_file(), dir.path()),
            ChunkConfig::Easy(_)
        ));
        // Disk stores pump, config says easy -> pump wins.
        std::fs::write(dir.child("r.0.0.pump"), b"x").unwrap();
        assert!(matches!(
            detect_on_disk_config(&easy_file(), dir.path()),
            ChunkConfig::Pump
        ));
        // Config format present on disk -> config wins even with strays.
        std::fs::write(dir.child("r.0.0.easy"), b"x").unwrap();
        assert!(matches!(
            detect_on_disk_config(&easy_file(), dir.path()),
            ChunkConfig::Easy(c) if c.backend == EasyBackend::File
        ));
        // DB-backed config has no region files, so it is never overridden.
        assert!(matches!(
            detect_on_disk_config(&easy_mysql(), dir.path()),
            ChunkConfig::Easy(c) if c.backend == EasyBackend::Mysql
        ));
    }
}
