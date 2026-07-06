// EMBER - EasyWorld MySQL storage backend v2
//
// Stores world region data in MySQL.  Uses the same EasyRegionData format
// as .easy files, imported from `crate::chunk::format::easy`.
//
// Table (auto-created; MEDIUMBLOB tables from older versions are upgraded):
//   CREATE TABLE easyworld_regions (
//       world_key  VARCHAR(512) NOT NULL,
//       region_x   INT NOT NULL,
//       region_z   INT NOT NULL,
//       data       LONGBLOB NOT NULL,
//       PRIMARY KEY (world_key, region_x, region_z)
//   );
//
// Concurrency: region writes are read-modify-write cycles serialized by a
// per-region async lock, so concurrent saves within one server are safe.
// A database must NOT be shared by multiple server instances at once —
// there is no cross-process locking.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use futures::future::join_all;
use pumpkin_util::math::vector2::Vector2;
use tokio::sync::{RwLock, mpsc};
use tracing::{debug, error, info, trace};

use crate::chunk::format::anvil::SingleChunkDataSerializer;
use crate::chunk::format::easy::EasyRegionData;
use crate::chunk::io::{BoxFuture, Dirtiable};
use crate::chunk::{ChunkReadingError, ChunkWritingError};
use crate::level::LevelFolder;
use pumpkin_config::chunk::EasyMysqlConfig;

use super::io::{FileIO, LoadedData};

/// SQL statements.
const CREATE_TABLE: &str = concat!(
    "CREATE TABLE IF NOT EXISTS easyworld_regions (",
    "world_key VARCHAR(512) NOT NULL,",
    "region_x INT NOT NULL,",
    "region_z INT NOT NULL,",
    "data LONGBLOB NOT NULL,",
    "PRIMARY KEY (world_key, region_x, region_z)",
    ")"
);

const SELECT_REGION: &str =
    "SELECT data FROM easyworld_regions WHERE world_key = ? AND region_x = ? AND region_z = ?";

const UPSERT_REGION: &str = concat!(
    "INSERT INTO easyworld_regions (world_key, region_x, region_z, data) ",
    "VALUES (?, ?, ?, ?) ",
    "ON DUPLICATE KEY UPDATE data = VALUES(data)"
);

// ─── Serialization helpers ─────────────────────────────────────────────

fn serialize_region(region: &EasyRegionData) -> Result<Vec<u8>, ChunkWritingError> {
    let raw = postcard::to_allocvec(region)
        .map_err(|e| ChunkWritingError::ChunkSerializingError(e.to_string()))?;
    Ok(ruzstd::encoding::compress_to_vec(
        &*raw,
        ruzstd::encoding::CompressionLevel::Default,
    ))
}

fn deserialize_region(data: &[u8]) -> Result<EasyRegionData, ChunkReadingError> {
    let mut decoder = ruzstd::decoding::StreamingDecoder::new(data).map_err(|e| {
        ChunkReadingError::Compression(crate::chunk::CompressionError::ZstdError(
            std::io::Error::other(e.to_string()),
        ))
    })?;
    let mut decompressed = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut decompressed)
        .map_err(ChunkReadingError::IoError)?;
    let region: EasyRegionData = postcard::from_bytes(&decompressed).map_err(|e| {
        ChunkReadingError::ParsingError(crate::chunk::ChunkParsingError::ErrorDeserializingChunk(
            e.to_string(),
        ))
    })?;
    // Reject corrupted rows up front instead of panicking on a bad slice later.
    if !region.is_consistent() {
        return Err(ChunkReadingError::ParsingError(
            crate::chunk::ChunkParsingError::ErrorDeserializingChunk(
                "inconsistent easyworld region data".to_string(),
            ),
        ));
    }
    Ok(region)
}

// ─── MySQL pool wrapper ────────────────────────────────────────────────

struct MysqlPool {
    pool: sqlx::mysql::MySqlPool,
}

impl MysqlPool {
    async fn new(url: &str) -> Result<Self, ChunkReadingError> {
        let pool = sqlx::mysql::MySqlPoolOptions::new()
            .max_connections(8)
            .connect(url)
            .await
            .map_err(|e| ChunkReadingError::IoError(std::io::Error::other(e.to_string())))?;
        Ok(Self { pool })
    }

    async fn ensure_table(&self) -> Result<(), ChunkReadingError> {
        sqlx::query(CREATE_TABLE)
            .execute(&self.pool)
            .await
            .map_err(|e| ChunkReadingError::IoError(std::io::Error::other(e.to_string())))?;

        // Upgrade tables created by older versions: MEDIUMBLOB caps a region
        // at 16 MiB, which a densely built full region can exceed.
        let col: Option<(String,)> = sqlx::query_as(
            "SELECT DATA_TYPE FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = DATABASE() \
             AND TABLE_NAME = 'easyworld_regions' AND COLUMN_NAME = 'data'",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| ChunkReadingError::IoError(std::io::Error::other(e.to_string())))?;
        if let Some((data_type,)) = col
            && data_type.eq_ignore_ascii_case("mediumblob")
        {
            sqlx::query("ALTER TABLE easyworld_regions MODIFY data LONGBLOB NOT NULL")
                .execute(&self.pool)
                .await
                .map_err(|e| ChunkReadingError::IoError(std::io::Error::other(e.to_string())))?;
            info!("EasyWorld MySQL: upgraded data column MEDIUMBLOB -> LONGBLOB");
        }
        Ok(())
    }

    async fn load_region(
        &self,
        world_key: &str,
        rx: i32,
        rz: i32,
    ) -> Result<Option<EasyRegionData>, ChunkReadingError> {
        let row: Option<(Vec<u8>,)> = sqlx::query_as(SELECT_REGION)
            .bind(world_key)
            .bind(rx)
            .bind(rz)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| ChunkReadingError::IoError(std::io::Error::other(e.to_string())))?;

        match row {
            Some((data,)) => deserialize_region(&data).map(Some),
            None => Ok(None),
        }
    }

    async fn save_region(
        &self,
        world_key: &str,
        region: &EasyRegionData,
    ) -> Result<(), ChunkWritingError> {
        let blob = serialize_region(region)?;
        sqlx::query(UPSERT_REGION)
            .bind(world_key)
            .bind(region.region_x)
            .bind(region.region_z)
            .bind(&blob)
            .execute(&self.pool)
            .await
            .map_err(|e| ChunkWritingError::IoError(std::io::Error::other(e.to_string())))?;
        Ok(())
    }
}

// ─── FileIO implementation ─────────────────────────────────────────────

/// (world key, region x, region z)
type RegionKey = (String, i32, i32);

/// Chunks to persist into one region, with their handles so the dirty flag
/// can be cleared after a successful write.
type RegionSaveBatch = Vec<(Vector2<i32>, Arc<crate::chunk::ChunkData>)>;

pub struct EasyMysqlStorage {
    config: EasyMysqlConfig,
    pool: Arc<tokio::sync::OnceCell<Arc<MysqlPool>>>,
    region_cache: RwLock<BTreeMap<RegionKey, EasyRegionData>>,
    watchers: RwLock<BTreeMap<RegionKey, usize>>,
    /// Serializes the read-modify-write save cycle per region so concurrent
    /// saves of the same region cannot overwrite each other's chunks.
    region_locks: RwLock<BTreeMap<RegionKey, Arc<tokio::sync::Mutex<()>>>>,
}

impl EasyMysqlStorage {
    #[must_use]
    pub fn new(config: &EasyMysqlConfig) -> Self {
        let storage = Self {
            config: config.clone(),
            pool: Arc::new(tokio::sync::OnceCell::new()),
            region_cache: RwLock::new(BTreeMap::new()),
            watchers: RwLock::new(BTreeMap::new()),
            region_locks: RwLock::new(BTreeMap::new()),
        };

        // Eagerly connect in the background so a bad URL or unreachable
        // database fails loudly at startup instead of on the first chunk IO
        // (which without players may never happen).
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let pool_cell = storage.pool.clone();
            let url = storage.config.url.clone();
            handle.spawn(async move {
                if let Err(e) = pool_cell.get_or_try_init(|| Self::init_pool(&url)).await {
                    error!("EasyWorld MySQL eager init failed (check [world.chunk] url): {e}");
                }
            });
        }

        storage
    }

    async fn init_pool(url: &str) -> Result<Arc<MysqlPool>, ChunkReadingError> {
        let p = MysqlPool::new(url).await?;
        p.ensure_table().await?;
        info!("EasyWorld MySQL pool initialized (table: easyworld_regions)");
        Ok(Arc::new(p))
    }

    async fn region_lock(&self, world_key: &str, rx: i32, rz: i32) -> Arc<tokio::sync::Mutex<()>> {
        let key = (world_key.to_string(), rx, rz);
        let mut locks = self.region_locks.write().await;
        locks
            .entry(key)
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    async fn ensure_pool(&self) -> Result<&Arc<MysqlPool>, ChunkReadingError> {
        self.pool
            .get_or_try_init(|| Self::init_pool(&self.config.url))
            .await
    }

    fn world_key(folder: &LevelFolder) -> String {
        folder
            .root_folder
            .to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_string()
    }

    const fn region_for(chunk: Vector2<i32>) -> (i32, i32) {
        (chunk.x >> 5, chunk.y >> 5)
    }

    /// Compute the region-relative chunk index (0..1023).
    const fn chunk_index(pos: Vector2<i32>) -> u32 {
        let rx = pos.x.rem_euclid(32);
        let rz = pos.y.rem_euclid(32);
        (rx + rz * 32) as u32
    }

    async fn get_region(
        &self,
        world_key: &str,
        rx: i32,
        rz: i32,
    ) -> Result<EasyRegionData, ChunkReadingError> {
        let key = (world_key.to_string(), rx, rz);
        {
            let cache = self.region_cache.read().await;
            if let Some(region) = cache.get(&key) {
                return Ok(region.clone());
            }
        }
        let pool = self.ensure_pool().await?;
        let region = pool
            .load_region(world_key, rx, rz)
            .await?
            .unwrap_or_else(|| EasyRegionData::new(rx, rz));
        // Cache only watched regions, otherwise one-off fetches would leak
        // cache entries that no unwatch ever cleans up.
        let watched = self.watchers.read().await.contains_key(&key);
        if watched {
            let mut cache = self.region_cache.write().await;
            cache.insert(key, region.clone());
        }
        Ok(region)
    }
}

impl FileIO for EasyMysqlStorage {
    type Data = Arc<crate::chunk::ChunkData>;

    fn fetch_chunks<'a>(
        &'a self,
        folder: &'a LevelFolder,
        chunk_coords: &'a [Vector2<i32>],
        stream: mpsc::Sender<LoadedData<Self::Data, ChunkReadingError>>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let world_key = Self::world_key(folder);
            let mut regions_chunks: BTreeMap<(i32, i32), Vec<Vector2<i32>>> = BTreeMap::new();
            for coord in chunk_coords {
                regions_chunks
                    .entry(Self::region_for(*coord))
                    .or_default()
                    .push(*coord);
            }

            let tasks = regions_chunks.into_iter().map(|((rx, rz), coords)| {
                let stream = stream.clone();
                let world_key = world_key.clone();
                async move {
                    let region = match self.get_region(&world_key, rx, rz).await {
                        Ok(r) => r,
                        Err(e) => {
                            // Every requested chunk needs a response, or the
                            // consumer waits forever for the missing ones.
                            let msg = e.to_string();
                            for pos in coords {
                                let err = ChunkReadingError::IoError(std::io::Error::other(
                                    msg.clone(),
                                ));
                                let _ = stream.send(LoadedData::Error((pos, err))).await;
                            }
                            return;
                        }
                    };

                    for pos in coords {
                        let index = Self::chunk_index(pos);
                        match region.get_chunk_bytes(index) {
                            Some(raw_bytes) => {
                                let bytes = Bytes::from(raw_bytes);
                                match <crate::chunk::ChunkData as SingleChunkDataSerializer>::from_bytes(&bytes, pos) {
                                    Ok(data) => {
                                        let _ =
                                            stream.send(LoadedData::Loaded(Arc::new(data))).await;
                                    }
                                    Err(e) => {
                                        let _ = stream
                                            .send(LoadedData::Error((
                                                pos,
                                                e,
                                            )))
                                            .await;
                                    }
                                }
                            }
                            None => {
                                let _ = stream.send(LoadedData::Missing(pos)).await;
                            }
                        }
                    }
                }
            });

            join_all(tasks).await;
        })
    }

    fn save_chunks<'a>(
        &'a self,
        folder: &'a LevelFolder,
        chunks_data: Vec<(Vector2<i32>, Self::Data)>,
    ) -> BoxFuture<'a, Result<(), ChunkWritingError>> {
        Box::pin(async move {
            let world_key = Self::world_key(folder);

            // Group chunks by region, keeping the chunk handle so the dirty
            // flag is cleared only after the region is actually persisted.
            let mut by_region: BTreeMap<(i32, i32), RegionSaveBatch> = BTreeMap::new();
            for (pos, chunk) in chunks_data {
                if !chunk.is_dirty() {
                    continue;
                }
                by_region
                    .entry(Self::region_for(pos))
                    .or_default()
                    .push((pos, chunk));
            }

            let tasks = by_region.into_iter().map(|((rx, rz), entries)| {
                let world_key = world_key.clone();
                async move {
                    // Serialize the read-modify-write cycle per region.
                    let lock = self.region_lock(&world_key, rx, rz).await;
                    let _guard = lock.lock().await;

                    let mut region = self.get_region(&world_key, rx, rz).await.map_err(|e| {
                        error!("Failed to load region ({rx},{rz}) for write: {e}");
                        ChunkWritingError::IoError(std::io::Error::other(e.to_string()))
                    })?;

                    let mut changed = false;
                    for (pos, chunk) in &entries {
                        let index = Self::chunk_index(*pos);
                        // ChunkPruner: an emptied chunk is removed instead of
                        // stored, so its old contents cannot resurrect.
                        if crate::chunk::format::easy::is_prunable_chunk(chunk) {
                            changed |= region.remove_chunk(index);
                            continue;
                        }
                        let bytes = chunk
                            .to_bytes()
                            .await
                            .map_err(|e| ChunkWritingError::ChunkSerializingError(e.to_string()))?;
                        region.upsert_chunk(index, &bytes);
                        changed = true;
                    }

                    if changed {
                        let pool = self.ensure_pool().await.map_err(|e| {
                            ChunkWritingError::IoError(std::io::Error::other(e.to_string()))
                        })?;
                        pool.save_region(&world_key, &region).await?;
                    }

                    // Persisted — only now clear the dirty flags, so a failed
                    // write leaves the chunks dirty for the next save attempt.
                    for (_, chunk) in &entries {
                        chunk.mark_dirty(false);
                    }

                    let key = (world_key.clone(), rx, rz);
                    let watched = self.watchers.read().await.contains_key(&key);
                    let mut cache = self.region_cache.write().await;
                    if watched {
                        cache.insert(key, region);
                    } else {
                        cache.remove(&key);
                    }
                    drop(cache);

                    debug!("Saved region ({rx},{rz}) for world {world_key}");
                    Ok(())
                }
            });

            let results: Vec<Result<(), ChunkWritingError>> = join_all(tasks).await;
            results.into_iter().find(Result::is_err).unwrap_or(Ok(()))
        })
    }

    fn watch_chunks<'a>(
        &'a self,
        folder: &'a LevelFolder,
        chunks: &'a [Vector2<i32>],
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let world_key = Self::world_key(folder);
            let mut watchers = self.watchers.write().await;
            for chunk in chunks {
                let key = (world_key.clone(), chunk.x >> 5, chunk.y >> 5);
                *watchers.entry(key).or_insert(0) += 1;
            }
        })
    }

    fn unwatch_chunks<'a>(
        &'a self,
        folder: &'a LevelFolder,
        chunks: &'a [Vector2<i32>],
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let world_key = Self::world_key(folder);
            let mut watchers = self.watchers.write().await;
            for chunk in chunks {
                let key = (world_key.clone(), chunk.x >> 5, chunk.y >> 5);
                if let std::collections::btree_map::Entry::Occupied(mut e) = watchers.entry(key) {
                    let count = e.get_mut();
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        e.remove();
                        let mut cache = self.region_cache.write().await;
                        cache.remove(&(world_key.clone(), chunk.x >> 5, chunk.y >> 5));
                    }
                }
            }
        })
    }

    fn clear_watched_chunks(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.watchers.write().await.clear();
            self.region_cache.write().await.clear();
        })
    }

    fn block_and_await_ongoing_tasks(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            trace!("EasyMysqlStorage: block_and_await_ongoing_tasks (no-op)");
        })
    }
}
