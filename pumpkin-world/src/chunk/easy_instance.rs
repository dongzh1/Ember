// EMBER - EasyWorld instance storage (shared-template ephemeral worlds)
//
// SlimeWorld-style template instancing for dungeon/minigame worlds:
//
//  * One immutable template (the region data of a normal `easy` or
//    `easy_mysql` world) is decompressed ONCE into a process-global
//    registry and shared by every instance via `Arc` — creating another
//    instance clones a pointer, not the data.
//  * Each instance keeps its own tiny RAM overlay of edited/removed
//    chunks. Saves go to the overlay only and are DISCARDED when the
//    instance world unloads; the template can never be modified.
//  * Chunk indices missing from the template are served as void (all-air,
//    `ChunkStatus::Full`) chunks instead of `Missing`, so the vanilla
//    terrain generator never runs inside an instance world.
//  * Entity data uses [`DiscardEntityIO`]: instances start with no stored
//    entities and never persist any.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::{Arc, LazyLock};

use bytes::Bytes;
use pumpkin_config::chunk::EasyMysqlConfig;
use pumpkin_util::math::vector2::Vector2;
use tokio::sync::{OnceCell, RwLock, mpsc};
use tracing::{error, info, warn};

use crate::chunk::format::anvil::SingleChunkDataSerializer;
use crate::chunk::format::easy::{decode_region_bytes, is_prunable_chunk};
use crate::chunk::io::{BoxFuture, Dirtiable, FileIO, LoadedData};
use crate::chunk::{
    ChunkData, ChunkEntityData, ChunkHeightmaps, ChunkLight, ChunkReadingError, ChunkSections,
    ChunkWritingError,
    format::LightContainer,
    palette::{BiomePalette, BlockPalette},
};
use crate::level::LevelFolder;
use crate::tick::scheduler::ChunkTickScheduler;
use pumpkin_data::chunk::ChunkStatus;

/// `"namespace/name"` path segment of a dimension inside a world folder
/// (mirrors `Level::from_root_folder`'s layout).
#[must_use]
pub fn dimension_path(minecraft_name: &str) -> String {
    match minecraft_name.split_once(':') {
        Some((ns, n)) => format!("{ns}/{n}"),
        None => format!("minecraft/{minecraft_name}"),
    }
}

/// Where a read-only instance reads its source world's stored data.
#[derive(Clone)]
pub enum TemplateSource {
    /// A world folder containing `.easy` region files.
    File { root: PathBuf },
    /// An `EasyWorld` `MySQL` database, keyed by the source world folder.
    Mysql {
        root: PathBuf,
        config: EasyMysqlConfig,
    },
}

// ─── Template ──────────────────────────────────────────────────────────

/// One region of a template: 1024 pre-sliced chunk blobs. Cloning a chunk
/// out of it is a refcount bump (`Bytes::clone`), shared by all instances.
struct TemplateRegion {
    chunks: Box<[Option<Bytes>]>,
}

/// A fully decompressed, immutable template world.
pub struct EasyTemplate {
    regions: HashMap<(i32, i32), TemplateRegion>,
    /// Total stored chunks (for diagnostics).
    chunk_count: usize,
}

impl EasyTemplate {
    fn chunk_bytes(&self, region: (i32, i32), index: u32) -> Option<Bytes> {
        self.regions
            .get(&region)?
            .chunks
            .get(index as usize)?
            .clone()
    }
}

/// Loads a template from its source. Runs blocking IO + decompression on
/// the blocking pool.
async fn load_template(
    source: &TemplateSource,
    dim_path: &str,
) -> Result<Arc<EasyTemplate>, String> {
    let regions = match source {
        TemplateSource::File { root } => {
            let region_dir = root.join("dimensions").join(dim_path).join("region");
            tokio::task::spawn_blocking(move || load_file_regions(&region_dir))
                .await
                .map_err(|e| e.to_string())??
        }
        TemplateSource::Mysql { root, config } => {
            crate::chunk::easy_mysql::load_world_regions(config, root).await?
        }
    };

    let mut map = HashMap::new();
    let mut chunk_count = 0usize;
    for region in regions {
        let mut chunks: Vec<Option<Bytes>> = vec![None; 1024];
        for (index, raw) in region.stored_chunks() {
            chunks[index as usize] = Some(Bytes::copy_from_slice(raw));
            chunk_count += 1;
        }
        map.insert(
            (region.region_x, region.region_z),
            TemplateRegion {
                chunks: chunks.into_boxed_slice(),
            },
        );
    }
    if map.is_empty() {
        return Err("template has no stored regions".to_string());
    }
    Ok(Arc::new(EasyTemplate {
        regions: map,
        chunk_count,
    }))
}

/// Reads every `r.X.Z.easy` file of a region folder.
fn load_file_regions(
    region_dir: &Path,
) -> Result<Vec<crate::chunk::format::easy::EasyRegionData>, String> {
    let entries = std::fs::read_dir(region_dir).map_err(|e| {
        format!(
            "cannot read template region folder {}: {e}",
            region_dir.display()
        )
    })?;
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let is_easy = Path::new(name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("easy"));
        if !name.starts_with("r.") || !is_easy {
            continue;
        }
        let raw = std::fs::read(entry.path()).map_err(|e| e.to_string())?;
        let region = decode_region_bytes(&raw)
            .map_err(|e| format!("corrupt template region {name}: {e}"))?;
        out.push(region);
    }
    Ok(out)
}

// ─── Registry ──────────────────────────────────────────────────────────

/// Templates are keyed by `(id, dim_path)`, not by `id` alone: the same clone
/// source resolves to different terrain per dimension (each dim is loaded from
/// its own `dim_path`), so keying by id alone lets a second dimension of the
/// same source collide with — and be served — the first dimension's terrain.
type TemplateKey = (String, String);

struct TemplateRegistry {
    map: RwLock<HashMap<TemplateKey, Arc<OnceCell<Arc<EasyTemplate>>>>>,
}

static REGISTRY: LazyLock<TemplateRegistry> = LazyLock::new(|| TemplateRegistry {
    map: RwLock::new(HashMap::new()),
});

impl TemplateRegistry {
    /// Returns the shared template, loading it exactly once even under
    /// concurrent first use. A failed load is not cached (retried next call).
    async fn get_or_load(
        &self,
        id: &str,
        source: &TemplateSource,
        dim_path: &str,
    ) -> Result<Arc<EasyTemplate>, String> {
        let cell = {
            let mut map = self.map.write().await;
            map.entry((id.to_string(), dim_path.to_string()))
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };
        cell.get_or_try_init(|| async {
            info!("EasyWorld: loading instance template '{id}'");
            let template = load_template(source, dim_path).await?;
            info!(
                "EasyWorld: template '{id}' resident ({} regions, {} chunks)",
                template.regions.len(),
                template.chunk_count,
            );
            Ok::<_, String>(template)
        })
        .await
        .cloned()
    }
}

/// Preloads a template into the shared registry (`/dungeon prewarm`).
/// Returns `(regions, chunks)` of the resident template.
pub async fn prewarm_template(
    id: &str,
    source: &TemplateSource,
    dim_path: &str,
) -> Result<(usize, usize), String> {
    let template = REGISTRY.get_or_load(id, source, dim_path).await?;
    Ok((template.regions.len(), template.chunk_count))
}

/// Drops a template from the registry so the next instance reloads it from
/// its source. Running instances keep serving their existing copy.
/// Returns `true` when the template was resident.
pub async fn reload_template(id: &str) -> bool {
    // Drop every per-dimension entry for this id. The map is keyed by
    // (id, dim_path), so a plain remove(id) would match nothing.
    let mut map = REGISTRY.map.write().await;
    let before = map.len();
    map.retain(|(k_id, _), _| k_id != id);
    map.len() != before
}

/// `(id, regions, chunks, live handles)` of every resident template.
/// `live handles` counts `Arc` clones beyond the registry's own (≈ open
/// instances plus in-flight loads).
pub async fn list_templates() -> Vec<(String, usize, usize, usize)> {
    let map = REGISTRY.map.read().await;
    let mut out = Vec::new();
    for ((id, _dim), cell) in map.iter() {
        if let Some(template) = cell.get() {
            out.push((
                id.clone(),
                template.regions.len(),
                template.chunk_count,
                Arc::strong_count(template).saturating_sub(1),
            ));
        }
    }
    out.sort();
    out
}

// ─── Void chunk synthesis ──────────────────────────────────────────────

/// Builds an all-air chunk with `ChunkStatus::Full`, so chunks outside the
/// template bypass the vanilla generator entirely (no terrain leaks into
/// instance worlds).
fn empty_chunk(pos: Vector2<i32>, min_y: i32, height: i32) -> ChunkData {
    let section_count = (height.max(16) / 16) as usize;
    let block_palettes = vec![BlockPalette::default(); section_count];
    let (random_tick_sections, mask) =
        ChunkSections::build_random_tick_sections_cache(&block_palettes);
    ChunkData {
        section: ChunkSections {
            count: section_count,
            block_sections: std::sync::RwLock::new(block_palettes.into_boxed_slice()),
            random_tick_sections: std::sync::RwLock::new(random_tick_sections),
            randomly_ticking_mask: AtomicU32::new(mask),
            biome_sections: std::sync::RwLock::new(
                vec![BiomePalette::default(); section_count].into_boxed_slice(),
            ),
            min_y,
        },
        heightmap: std::sync::Mutex::new(ChunkHeightmaps::default()),
        x: pos.x,
        z: pos.y,
        block_ticks: ChunkTickScheduler::default(),
        fluid_ticks: ChunkTickScheduler::default(),
        pending_block_entities: std::sync::Mutex::new(rustc_hash::FxHashMap::default()),
        light_engine: std::sync::Mutex::new(ChunkLight {
            sky_light: vec![LightContainer::new_empty(15); section_count].into_boxed_slice(),
            block_light: vec![LightContainer::new_empty(0); section_count].into_boxed_slice(),
        }),
        light_populated: AtomicBool::new(true),
        status: ChunkStatus::Full,
        blending_data: None,
        dirty: AtomicBool::new(false),
    }
}

// ─── Instance storage (FileIO) ─────────────────────────────────────────

#[derive(Default)]
struct RegionOverlay {
    /// Chunks edited inside this instance (raw NBT bytes).
    edited: HashMap<u32, Bytes>,
    /// Chunks mined out to all-air inside this instance: served as void
    /// instead of resurrecting the template contents.
    removed: HashSet<u32>,
}

/// Chunk storage of one ephemeral instance world (a read-only clone).
pub struct EasyInstanceStorage {
    template_id: String,
    source: TemplateSource,
    dim_path: String,
    min_y: i32,
    height: i32,
    template: OnceCell<Arc<EasyTemplate>>,
    overlay: RwLock<HashMap<(i32, i32), RegionOverlay>>,
}

impl EasyInstanceStorage {
    #[must_use]
    pub fn new(
        template_id: String,
        source: TemplateSource,
        dim_path: String,
        min_y: i32,
        height: i32,
    ) -> Self {
        Self {
            template_id,
            source,
            dim_path,
            min_y,
            height,
            template: OnceCell::new(),
            overlay: RwLock::new(HashMap::new()),
        }
    }

    async fn template(&self) -> Result<&Arc<EasyTemplate>, String> {
        self.template
            .get_or_try_init(|| async {
                REGISTRY
                    .get_or_load(&self.template_id, &self.source, &self.dim_path)
                    .await
            })
            .await
    }

    const fn region_and_index(pos: Vector2<i32>) -> ((i32, i32), u32) {
        let region = (pos.x >> 5, pos.y >> 5);
        let rel_x = pos.x.rem_euclid(32);
        let rel_z = pos.y.rem_euclid(32);
        (region, (rel_x + rel_z * 32) as u32)
    }
}

impl FileIO for EasyInstanceStorage {
    type Data = Arc<ChunkData>;

    fn fetch_chunks<'a>(
        &'a self,
        _folder: &'a LevelFolder,
        chunk_coords: &'a [Vector2<i32>],
        stream: mpsc::Sender<LoadedData<Self::Data, ChunkReadingError>>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let template = match self.template().await {
                Ok(t) => t.clone(),
                Err(e) => {
                    error!(
                        "EasyWorld: template '{}' failed to load: {e}",
                        self.template_id
                    );
                    // Every requested chunk needs a response or the chunk
                    // system waits forever.
                    for pos in chunk_coords {
                        let err = ChunkReadingError::IoError(std::io::Error::other(e.clone()));
                        let _ = stream.send(LoadedData::Error((*pos, err))).await;
                    }
                    return;
                }
            };

            let overlay = self.overlay.read().await;
            for pos in chunk_coords {
                let (region, index) = Self::region_and_index(*pos);
                let ov = overlay.get(&region);

                let raw = if ov.is_some_and(|o| o.removed.contains(&index)) {
                    None // mined out inside this instance -> void
                } else if let Some(bytes) = ov.and_then(|o| o.edited.get(&index)) {
                    Some(bytes.clone())
                } else {
                    template.chunk_bytes(region, index)
                };

                // Outside the template (or removed): a finished void chunk,
                // never Missing -> the generator never runs.
                let loaded = raw.map_or_else(
                    || LoadedData::Loaded(Arc::new(empty_chunk(*pos, self.min_y, self.height))),
                    |bytes| match <ChunkData as SingleChunkDataSerializer>::from_bytes(&bytes, *pos)
                    {
                        Ok(data) => LoadedData::Loaded(Arc::new(data)),
                        Err(e) => LoadedData::Error((*pos, e)),
                    },
                );
                let _ = stream.send(loaded).await;
            }
        })
    }

    fn save_chunks<'a>(
        &'a self,
        _folder: &'a LevelFolder,
        chunks_data: Vec<(Vector2<i32>, Self::Data)>,
    ) -> BoxFuture<'a, Result<(), ChunkWritingError>> {
        Box::pin(async move {
            // Serialize outside the overlay lock, then apply in one pass.
            let mut updates: Vec<((i32, i32), u32, Option<Bytes>)> = Vec::new();
            for (pos, chunk) in &chunks_data {
                if !chunk.is_dirty() {
                    continue;
                }
                // Snapshot-and-clear the dirty flag BEFORE reading the chunk, so a
                // mutation that races in during to_bytes().await re-dirties it and is
                // picked up by the next save instead of being wiped by a blanket clear.
                chunk.mark_dirty(false);
                let (region, index) = Self::region_and_index(*pos);
                if is_prunable_chunk(chunk) {
                    updates.push((region, index, None));
                } else {
                    let bytes = match chunk.to_bytes().await {
                        Ok(bytes) => bytes,
                        Err(e) => {
                            // Serialization failed — re-dirty so the chunk is retried
                            // next save rather than left clean-but-unsaved.
                            chunk.mark_dirty(true);
                            return Err(ChunkWritingError::ChunkSerializingError(e.to_string()));
                        }
                    };
                    updates.push((region, index, Some(bytes)));
                }
            }

            if !updates.is_empty() {
                let mut overlay = self.overlay.write().await;
                for (region, index, bytes) in updates {
                    let ov = overlay.entry(region).or_default();
                    if let Some(bytes) = bytes {
                        ov.removed.remove(&index);
                        ov.edited.insert(index, bytes);
                    } else {
                        ov.edited.remove(&index);
                        ov.removed.insert(index);
                    }
                }
            }

            Ok(())
        })
    }

    fn watch_chunks<'a>(
        &'a self,
        _folder: &'a LevelFolder,
        _chunks: &'a [Vector2<i32>],
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn unwatch_chunks<'a>(
        &'a self,
        _folder: &'a LevelFolder,
        _chunks: &'a [Vector2<i32>],
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn clear_watched_chunks(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn block_and_await_ongoing_tasks(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    /// Regions come from the shared template, not the instance folder.
    fn list_regions<'a>(&'a self, _folder: &'a LevelFolder) -> BoxFuture<'a, Vec<(i32, i32)>> {
        Box::pin(async move {
            self.template()
                .await
                .map_or_else(|_| Vec::new(), |t| t.regions.keys().copied().collect())
        })
    }
}

// ─── Discard entity storage ────────────────────────────────────────────

/// Entity storage for instance worlds: fetches report `Missing` (instances
/// start with no stored entities) and saves are discarded.
pub struct DiscardEntityIO;

impl FileIO for DiscardEntityIO {
    type Data = Arc<ChunkEntityData>;

    fn fetch_chunks<'a>(
        &'a self,
        _folder: &'a LevelFolder,
        chunk_coords: &'a [Vector2<i32>],
        stream: mpsc::Sender<LoadedData<Self::Data, ChunkReadingError>>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            for pos in chunk_coords {
                let _ = stream.send(LoadedData::Missing(*pos)).await;
            }
        })
    }

    fn save_chunks<'a>(
        &'a self,
        _folder: &'a LevelFolder,
        chunks_data: Vec<(Vector2<i32>, Self::Data)>,
    ) -> BoxFuture<'a, Result<(), ChunkWritingError>> {
        Box::pin(async move {
            if !chunks_data.is_empty() {
                warn!(
                    "EasyWorld instance: discarding {} entity chunk saves",
                    chunks_data.len()
                );
            }
            for (_, chunk) in &chunks_data {
                chunk.mark_dirty(false);
            }
            Ok(())
        })
    }

    fn watch_chunks<'a>(
        &'a self,
        _folder: &'a LevelFolder,
        _chunks: &'a [Vector2<i32>],
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn unwatch_chunks<'a>(
        &'a self,
        _folder: &'a LevelFolder,
        _chunks: &'a [Vector2<i32>],
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn clear_watched_chunks(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn block_and_await_ongoing_tasks(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }
}

// ─── Read-only chunk storage ───────────────────────────────────────────

/// A chunk `FileIO` decorator that makes a `read_only` world persist nothing.
///
/// Reads, watches and region enumeration delegate to the inner store, but
/// saves are dropped (chunks marked clean). This mirrors [`DiscardEntityIO`]
/// so a source-less `read_only` world stays symmetric — neither blocks nor
/// entities are written back — instead of the File backend silently
/// persisting blocks while entities are discarded.
pub struct ReadOnlyChunkIO {
    pub inner: Arc<dyn FileIO<Data = Arc<ChunkData>>>,
}

impl FileIO for ReadOnlyChunkIO {
    type Data = Arc<ChunkData>;

    fn fetch_chunks<'a>(
        &'a self,
        folder: &'a LevelFolder,
        chunk_coords: &'a [Vector2<i32>],
        stream: mpsc::Sender<LoadedData<Self::Data, ChunkReadingError>>,
    ) -> BoxFuture<'a, ()> {
        self.inner.fetch_chunks(folder, chunk_coords, stream)
    }

    fn save_chunks<'a>(
        &'a self,
        _folder: &'a LevelFolder,
        chunks_data: Vec<(Vector2<i32>, Self::Data)>,
    ) -> BoxFuture<'a, Result<(), ChunkWritingError>> {
        Box::pin(async move {
            for (_, chunk) in &chunks_data {
                chunk.mark_dirty(false);
            }
            Ok(())
        })
    }

    fn watch_chunks<'a>(
        &'a self,
        folder: &'a LevelFolder,
        chunks: &'a [Vector2<i32>],
    ) -> BoxFuture<'a, ()> {
        self.inner.watch_chunks(folder, chunks)
    }

    fn unwatch_chunks<'a>(
        &'a self,
        folder: &'a LevelFolder,
        chunks: &'a [Vector2<i32>],
    ) -> BoxFuture<'a, ()> {
        self.inner.unwatch_chunks(folder, chunks)
    }

    fn clear_watched_chunks(&self) -> BoxFuture<'_, ()> {
        self.inner.clear_watched_chunks()
    }

    fn block_and_await_ongoing_tasks(&self) -> BoxFuture<'_, ()> {
        self.inner.block_and_await_ongoing_tasks()
    }

    fn list_regions<'a>(&'a self, folder: &'a LevelFolder) -> BoxFuture<'a, Vec<(i32, i32)>> {
        self.inner.list_regions(folder)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::format::easy::EasyRegionData;

    fn template_from_regions(regions: Vec<EasyRegionData>) -> Arc<EasyTemplate> {
        let mut map = HashMap::new();
        let mut chunk_count = 0;
        for region in regions {
            let mut chunks: Vec<Option<Bytes>> = vec![None; 1024];
            for (index, raw) in region.stored_chunks() {
                chunks[index as usize] = Some(Bytes::copy_from_slice(raw));
                chunk_count += 1;
            }
            map.insert(
                (region.region_x, region.region_z),
                TemplateRegion {
                    chunks: chunks.into_boxed_slice(),
                },
            );
        }
        Arc::new(EasyTemplate {
            regions: map,
            chunk_count,
        })
    }

    fn region_with(region_x: i32, region_z: i32, index: u32, bytes: &[u8]) -> EasyRegionData {
        let mut map = rustc_hash::FxHashMap::default();
        map.insert(index, Bytes::copy_from_slice(bytes));
        EasyRegionData::from_chunks(region_x, region_z, &map)
    }

    #[test]
    fn template_serves_shared_bytes() {
        let region = region_with(0, 0, 7, &[1, 2, 3]);
        let template = template_from_regions(vec![region]);

        // Two "instances" get the same underlying buffer (refcount clone).
        let a = template.chunk_bytes((0, 0), 7).unwrap();
        let b = template.chunk_bytes((0, 0), 7).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.as_ptr(), b.as_ptr());
        assert!(template.chunk_bytes((0, 0), 8).is_none());
        assert!(template.chunk_bytes((1, 0), 7).is_none());
    }

    #[test]
    fn empty_chunk_is_full_status_air() {
        let chunk = empty_chunk(Vector2::new(3, -4), -64, 384);
        assert_eq!(chunk.x, 3);
        assert_eq!(chunk.z, -4);
        assert_eq!(chunk.section.count, 24);
        assert_eq!(chunk.section.min_y, -64);
        assert!(matches!(chunk.status, ChunkStatus::Full));
        assert!(!chunk.dirty.load(std::sync::atomic::Ordering::Relaxed));
        // All sections all air.
        let sections = chunk.section.block_sections.read().unwrap();
        assert!(sections.iter().all(|s| matches!(
            s,
            crate::chunk::palette::PalettedContainer::Homogeneous(id)
                if pumpkin_data::block_properties::is_air(*id)
        )));
    }

    #[tokio::test]
    async fn overlay_edit_and_remove_semantics() {
        let region = region_with(0, 0, 0, &[9u8; 4]);
        let template = template_from_regions(vec![region]);

        let storage = EasyInstanceStorage {
            template_id: "t".into(),
            source: TemplateSource::File { root: ".".into() },
            dim_path: "minecraft/overworld".into(),
            min_y: -64,
            height: 384,
            template: OnceCell::from(template),
            overlay: RwLock::new(HashMap::new()),
        };

        // Overlay edit shadows the template; removal serves void.
        let mut overlay = storage.overlay.write().await;
        let ov = overlay.entry((0, 0)).or_default();
        ov.edited.insert(1, Bytes::from_static(&[7u8; 2]));
        ov.removed.insert(0);
        drop(overlay);

        let overlay = storage.overlay.read().await;
        let ov = overlay.get(&(0, 0)).unwrap();
        assert!(ov.removed.contains(&0));
        assert_eq!(ov.edited.get(&1).unwrap().as_ref(), &[7u8; 2]);
        // The template itself is untouched.
        let template = storage.template.get().unwrap();
        assert_eq!(template.chunk_bytes((0, 0), 0).unwrap().as_ref(), &[9u8; 4]);
    }
}
