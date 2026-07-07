// Ember - synthesized "generated" chunks for non-seed generation modes.
//
// Worlds whose `ember-world.toml` sets a non-seed `generate` mode never run
// the vanilla terrain generator. Instead, chunks that were never stored are
// *synthesized* on the fly:
//
//  * `Void`  -> an all-air chunk (identical to `easy_instance::empty_chunk`).
//  * `Ocean` -> a flat ocean base: bedrock floor, stone body and two layers
//    of still water topping out at sea level (63).
//
// The synthesized chunk carries `ChunkStatus::Full` and populated light, so
// the chunk system treats it as finished terrain and the generator is never
// invoked.
//
// [`GenFillIO`] is a thin [`FileIO`] decorator that wraps a world's real
// chunk store: every chunk the inner store reports as `Missing` is replaced
// with a synthesized chunk; `Loaded`/`Error` results pass through untouched.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32};

use pumpkin_config::ember_world::GenerateMode;
use pumpkin_data::Block;
use pumpkin_data::chunk::ChunkStatus;
use pumpkin_util::math::vector2::Vector2;
use tokio::sync::mpsc;

use crate::chunk::io::{BoxFuture, FileIO, LoadedData};
use crate::chunk::{
    ChunkData, ChunkHeightmaps, ChunkLight, ChunkReadingError, ChunkSections, ChunkWritingError,
    format::LightContainer,
    palette::{BiomePalette, BlockPalette},
};
use crate::level::LevelFolder;
use crate::tick::scheduler::ChunkTickScheduler;

/// Vanilla sea level. Ocean synthesis tops water out here.
const SEA_LEVEL: i32 = 63;

// ─── Chunk synthesis ───────────────────────────────────────────────────

/// Builds a finished, all-air chunk with `ChunkStatus::Full` and populated
/// light (sky light 15, block light 0). This is the exact construction used
/// by `easy_instance::empty_chunk` and is the base every mode starts from.
fn base_chunk(pos: Vector2<i32>, min_y: i32, height: i32) -> ChunkData {
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

/// Fills a base (all-air) chunk with a simple ocean profile:
///
///  * bedrock at the very bottom layer (`y = min_y`),
///  * stone from there up to just below the water,
///  * still water at `y = 62` and `y = 63` (sea level),
///  * air above.
///
/// Blocks are placed column by column with [`ChunkData::set_block_absolute_y`],
/// so the heightmaps (and random-tick caches) are kept correct automatically.
/// Out-of-range writes (a `y` above the world height) are ignored by that
/// method, so odd `min_y`/`height` values can never panic here.
fn ocean_chunk(pos: Vector2<i32>, min_y: i32, height: i32) -> ChunkData {
    let chunk = base_chunk(pos, min_y, height);

    let bedrock = Block::BEDROCK.default_state.id;
    let stone = Block::STONE.default_state.id;
    let water = Block::WATER.default_state.id;

    for x in 0..16usize {
        for z in 0..16usize {
            // Bedrock floor.
            chunk.set_block_absolute_y(x, min_y, z, bedrock);
            // Stone body up to just below the water.
            for y in (min_y + 1)..=(SEA_LEVEL - 2) {
                chunk.set_block_absolute_y(x, y, z, stone);
            }
            // Two layers of still water, surface at sea level.
            chunk.set_block_absolute_y(x, SEA_LEVEL - 1, z, water);
            chunk.set_block_absolute_y(x, SEA_LEVEL, z, water);
        }
    }

    // base_chunk starts fully sky-lit (light_populated = true); after filling
    // the solids/water the interior and floor would stay lit as if under open
    // sky. Mark lighting unpopulated so the engine relights the chunk on load
    // (Default lighting mode) instead of baking in a uniform sky_light = 15.
    chunk
        .light_populated
        .store(false, std::sync::atomic::Ordering::Relaxed);
    chunk
}

/// Synthesizes a finished chunk for a non-seed generation `mode`, replacing
/// the vanilla terrain generator entirely.
///
/// `Void` (and `Seed` as a fallback) yield an all-air `Full` chunk; `Ocean`
/// yields the flat ocean base described on [`ocean_chunk`].
#[must_use]
pub fn synthesize_chunk(
    pos: Vector2<i32>,
    min_y: i32,
    height: i32,
    mode: GenerateMode,
) -> ChunkData {
    match mode {
        GenerateMode::Ocean => ocean_chunk(pos, min_y, height),
        // Void produces a void world; Seed is not normally wrapped, but if it
        // is we fall back to an all-air chunk rather than terrain.
        GenerateMode::Void | GenerateMode::Seed => base_chunk(pos, min_y, height),
    }
}

// ─── FileIO decorator ──────────────────────────────────────────────────

/// A [`FileIO`] decorator that turns `Missing` chunks from `inner` into
/// synthesized chunks (see [`synthesize_chunk`]). Every other operation is
/// delegated straight to `inner`.
///
/// Callers skip wrapping for `GenerateMode::Seed` (the vanilla generator is
/// wanted there); the decorator still works if wrapped, synthesizing all-air.
pub struct GenFillIO {
    /// The world's real chunk store.
    pub inner: Arc<dyn FileIO<Data = Arc<ChunkData>>>,
    /// How missing chunks are synthesized.
    pub mode: GenerateMode,
    /// World bottom (block Y of the lowest section).
    pub min_y: i32,
    /// World height in blocks.
    pub height: i32,
}

impl FileIO for GenFillIO {
    type Data = Arc<ChunkData>;

    fn fetch_chunks<'a>(
        &'a self,
        folder: &'a LevelFolder,
        chunk_coords: &'a [Vector2<i32>],
        stream: mpsc::Sender<LoadedData<Self::Data, ChunkReadingError>>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let mode = self.mode;
            let min_y = self.min_y;
            let height = self.height;

            // A bounded channel of 1 keeps backpressure between the inner
            // store and the caller (mirrors `ChunkFileManager::fetch_chunks`).
            let (send, mut recv) = mpsc::channel::<LoadedData<Self::Data, ChunkReadingError>>(1);

            // Forward the inner store's output, turning every `Missing` chunk
            // into a synthesized one. `Loaded`/`Error` pass through unchanged.
            let forward = async move {
                while let Some(data) = recv.recv().await {
                    let mapped = match data {
                        LoadedData::Missing(pos) => {
                            LoadedData::Loaded(Arc::new(synthesize_chunk(pos, min_y, height, mode)))
                        }
                        other => other,
                    };
                    if stream.send(mapped).await.is_err() {
                        // Receiver dropped; abort early to avoid wasted work.
                        return;
                    }
                }
            };

            let inner = self.inner.fetch_chunks(folder, chunk_coords, send);

            tokio::join!(forward, inner);
        })
    }

    fn save_chunks<'a>(
        &'a self,
        folder: &'a LevelFolder,
        chunks_data: Vec<(Vector2<i32>, Self::Data)>,
    ) -> BoxFuture<'a, Result<(), ChunkWritingError>> {
        self.inner.save_chunks(folder, chunks_data)
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
    use pumpkin_data::Block;

    #[test]
    fn void_is_full_all_air() {
        let chunk = synthesize_chunk(Vector2::new(3, -4), -64, 384, GenerateMode::Void);
        assert_eq!(chunk.x, 3);
        assert_eq!(chunk.z, -4);
        assert_eq!(chunk.section.count, 24);
        assert_eq!(chunk.section.min_y, -64);
        assert!(matches!(chunk.status, ChunkStatus::Full));
        // Every section is air.
        let sections = chunk.section.block_sections.read().unwrap();
        assert!(sections.iter().all(BlockPalette::has_only_air));
    }

    #[test]
    fn seed_falls_back_to_all_air() {
        let chunk = synthesize_chunk(Vector2::new(0, 0), -64, 384, GenerateMode::Seed);
        assert_eq!(chunk.section.count, 24);
        assert!(matches!(chunk.status, ChunkStatus::Full));
        let sections = chunk.section.block_sections.read().unwrap();
        assert!(sections.iter().all(BlockPalette::has_only_air));
    }

    #[test]
    fn ocean_is_full_with_expected_sections() {
        let chunk = synthesize_chunk(Vector2::new(1, 2), -64, 384, GenerateMode::Ocean);
        assert_eq!(chunk.section.count, 24);
        assert_eq!(chunk.section.min_y, -64);
        assert!(matches!(chunk.status, ChunkStatus::Full));

        // Self-consistent profile check: bottom is bedrock, sea level is water,
        // above sea level is air. (Uses the same ids the synthesizer placed,
        // so this holds regardless of the concrete state-id values.)
        let bottom = chunk.section.get_block_absolute_y(0, -64, 0);
        assert_eq!(bottom, Some(Block::BEDROCK.default_state.id));
        let sea = chunk.section.get_block_absolute_y(0, SEA_LEVEL, 0);
        assert_eq!(sea, Some(Block::WATER.default_state.id));
        let above = chunk.section.get_block_absolute_y(0, SEA_LEVEL + 1, 0);
        assert_eq!(above, Some(Block::AIR.default_state.id));
    }
}
