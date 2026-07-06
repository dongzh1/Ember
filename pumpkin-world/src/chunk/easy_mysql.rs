// EMBER - EasyWorld MySQL storage backend v2
//
// Stores world region data in MySQL.  Uses the same EasyRegionData format
// as .easy files, imported from `crate::chunk::format::easy`.
//
// Table (auto-created):
//   CREATE TABLE easyworld_regions (
//       world_key  VARCHAR(512) NOT NULL,
//       region_x   INT NOT NULL,
//       region_z   INT NOT NULL,
//       data       MEDIUMBLOB NOT NULL,
//       PRIMARY KEY (world_key, region_x, region_z)
//   );

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
    "data MEDIUMBLOB NOT NULL,",
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
    postcard::from_bytes(&decompressed).map_err(|e| {
        ChunkReadingError::ParsingError(crate::chunk::ChunkParsingError::ErrorDeserializingChunk(
            e.to_string(),
        ))
    })
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

/// Pending chunk updates for one region: (chunk index, raw NBT bytes).
type RegionUpdates = Vec<(u32, Vec<u8>)>;

pub struct EasyMysqlStorage {
    config: EasyMysqlConfig,
    pool: tokio::sync::OnceCell<Arc<MysqlPool>>,
    region_cache: RwLock<BTreeMap<(String, i32, i32), EasyRegionData>>,
    watchers: RwLock<BTreeMap<(String, i32, i32), usize>>,
}

impl EasyMysqlStorage {
    #[must_use]
    pub fn new(config: &EasyMysqlConfig) -> Self {
        Self {
            config: config.clone(),
            pool: tokio::sync::OnceCell::new(),
            region_cache: RwLock::new(BTreeMap::new()),
            watchers: RwLock::new(BTreeMap::new()),
        }
    }

    async fn ensure_pool(&self) -> Result<&Arc<MysqlPool>, ChunkReadingError> {
        self.pool
            .get_or_try_init(|| async {
                let p = MysqlPool::new(&self.config.url).await?;
                p.ensure_table().await?;
                info!("EasyWorld MySQL pool initialized (table: easyworld_regions)");
                Ok(Arc::new(p))
            })
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
        let mut cache = self.region_cache.write().await;
        cache.insert(key, region.clone());
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
                            let _ = stream.send(LoadedData::Error((coords[0], e))).await;
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

            let mut region_dirty: BTreeMap<(i32, i32), RegionUpdates> = BTreeMap::new();
            for (pos, chunk) in &chunks_data {
                if !chunk.is_dirty() {
                    continue;
                }
                // ChunkPruner: skip empty chunks.
                if crate::chunk::format::easy::is_prunable_chunk(chunk) {
                    chunk.mark_dirty(false);
                    continue;
                }

                chunk.mark_dirty(false);
                let bytes = chunk
                    .to_bytes()
                    .await
                    .map_err(|e| ChunkWritingError::ChunkSerializingError(e.to_string()))?;
                let index = Self::chunk_index(*pos);
                region_dirty
                    .entry(Self::region_for(*pos))
                    .or_default()
                    .push((index, bytes.to_vec()));
            }

            let tasks = region_dirty.into_iter().map(|((rx, rz), updates)| {
                let world_key = world_key.clone();
                async move {
                    let mut region = match self.get_region(&world_key, rx, rz).await {
                        Ok(r) => r,
                        Err(e) => {
                            error!("Failed to load region ({rx},{rz}) for write: {e}");
                            return Err(ChunkWritingError::IoError(std::io::Error::other(
                                e.to_string(),
                            )));
                        }
                    };

                    for (index, raw_nbt) in updates {
                        region.upsert_chunk(index, &raw_nbt);
                    }

                    let pool = self.ensure_pool().await.map_err(|e| {
                        ChunkWritingError::IoError(std::io::Error::other(e.to_string()))
                    })?;
                    pool.save_region(&world_key, &region).await?;

                    let key = (world_key.clone(), rx, rz);
                    let mut cache = self.region_cache.write().await;
                    cache.insert(key, region);

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
