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
// Concurrency (SlimeWorld-style one-writer/many-readers):
//   - Within one server, region writes are read-modify-write cycles
//     serialized by a per-region async lock.
//   - Across servers, a world may be loaded read_only by any number of
//     servers, but only ONE server at a time may hold it read_write.
//     The writer registers in `easyworld_locks` and refreshes a heartbeat;
//     a lock whose heartbeat expired (crashed server) can be taken over.
//     A second read_write server degrades to read-only with a loud error.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use futures::future::join_all;
use pumpkin_util::math::vector2::Vector2;
use tokio::sync::{RwLock, mpsc};
use tracing::{debug, error, info, trace, warn};

use crate::chunk::format::anvil::SingleChunkDataSerializer;
use crate::chunk::format::easy::EasyRegionData;
use crate::chunk::io::{BoxFuture, Dirtiable};
use crate::chunk::{ChunkReadingError, ChunkWritingError};
use crate::level::LevelFolder;
use pumpkin_config::chunk::{EasyMysqlConfig, EasyWorldMode};

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

// ─── World write lock (one writer, many readers) ───────────────────────

/// A writer whose heartbeat is older than this is considered crashed and
/// its lock may be taken over.
const LOCK_TTL_SECS: i64 = 60;
/// How often the heartbeat task refreshes held locks (and retries denied ones).
const LOCK_HEARTBEAT_SECS: u64 = 20;

const CREATE_LOCK_TABLE: &str = concat!(
    "CREATE TABLE IF NOT EXISTS easyworld_locks (",
    "world_key VARCHAR(512) NOT NULL,",
    "owner VARCHAR(128) NOT NULL,",
    "heartbeat BIGINT NOT NULL,",
    "PRIMARY KEY (world_key)",
    ")"
);

/// Atomically takes the lock when the row is already ours or the previous
/// holder's heartbeat expired; a live foreign row is left untouched.
/// (`MySQL` evaluates the assignments left to right, so the heartbeat IF
/// already sees the updated owner.)
const ACQUIRE_LOCK: &str = concat!(
    "INSERT INTO easyworld_locks (world_key, owner, heartbeat) VALUES (?, ?, ?) ",
    "ON DUPLICATE KEY UPDATE ",
    "owner = IF(owner = VALUES(owner) OR heartbeat < ?, VALUES(owner), owner), ",
    "heartbeat = IF(owner = VALUES(owner), VALUES(heartbeat), heartbeat)"
);

const SELECT_LOCK_OWNER: &str = "SELECT owner FROM easyworld_locks WHERE world_key = ?";

const REFRESH_LOCK: &str =
    "UPDATE easyworld_locks SET heartbeat = ? WHERE world_key = ? AND owner = ?";

const RELEASE_LOCK: &str = "DELETE FROM easyworld_locks WHERE world_key = ? AND owner = ?";

fn read_err(e: impl std::fmt::Display) -> ChunkReadingError {
    ChunkReadingError::IoError(std::io::Error::other(e.to_string()))
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

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
            .map_err(read_err)?;
        sqlx::query(CREATE_LOCK_TABLE)
            .execute(&self.pool)
            .await
            .map_err(read_err)?;

        // Upgrade tables created by older versions: MEDIUMBLOB caps a region
        // at 16 MiB, which a densely built full region can exceed.
        // NOTE: compare inside SQL — sqlx returns information_schema string
        // columns as VARBINARY, which does not decode into Rust String.
        let (mediumblob,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = DATABASE() \
             AND TABLE_NAME = 'easyworld_regions' AND COLUMN_NAME = 'data' \
             AND DATA_TYPE = 'mediumblob'",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(read_err)?;
        if mediumblob > 0 {
            sqlx::query("ALTER TABLE easyworld_regions MODIFY data LONGBLOB NOT NULL")
                .execute(&self.pool)
                .await
                .map_err(read_err)?;
            info!("EasyWorld MySQL: upgraded data column MEDIUMBLOB -> LONGBLOB");
        }
        Ok(())
    }

    /// Whether the region table exists (read-only servers must not create it).
    async fn table_exists(&self) -> Result<bool, ChunkReadingError> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM information_schema.TABLES \
             WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = 'easyworld_regions'",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(read_err)?;
        Ok(count > 0)
    }

    /// Try to take (or keep) the write lock for `world_key`.
    /// Returns `true` when this `owner` now holds the lock.
    async fn try_acquire_lock(
        &self,
        world_key: &str,
        owner: &str,
    ) -> Result<bool, ChunkReadingError> {
        let now = unix_now();
        sqlx::query(ACQUIRE_LOCK)
            .bind(world_key)
            .bind(owner)
            .bind(now)
            .bind(now - LOCK_TTL_SECS)
            .execute(&self.pool)
            .await
            .map_err(read_err)?;
        let row: Option<(String,)> = sqlx::query_as(SELECT_LOCK_OWNER)
            .bind(world_key)
            .fetch_optional(&self.pool)
            .await
            .map_err(read_err)?;
        Ok(row.is_some_and(|(o,)| o == owner))
    }

    /// Refresh the heartbeat of a held lock. Returns `false` if the lock
    /// is no longer ours (expired and taken over).
    async fn refresh_lock(&self, world_key: &str, owner: &str) -> Result<bool, ChunkReadingError> {
        let res = sqlx::query(REFRESH_LOCK)
            .bind(unix_now())
            .bind(world_key)
            .bind(owner)
            .execute(&self.pool)
            .await
            .map_err(read_err)?;
        Ok(res.rows_affected() > 0)
    }

    /// Best-effort lock release on graceful shutdown; heartbeat expiry
    /// covers crashes.
    async fn release_lock(&self, world_key: &str, owner: &str) {
        let _ = sqlx::query(RELEASE_LOCK)
            .bind(world_key)
            .bind(owner)
            .execute(&self.pool)
            .await;
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
    /// Identity of this server process in `easyworld_locks`.
    owner_id: String,
    /// Write-lock state per world key: `true` = held, `false` = denied
    /// (another live server is the writer).
    world_write_locks: Arc<RwLock<BTreeMap<String, bool>>>,
    /// Set on drop; stops the heartbeat task.
    shutdown: Arc<AtomicBool>,
}

impl EasyMysqlStorage {
    #[must_use]
    pub fn new(config: &EasyMysqlConfig) -> Self {
        let owner_id = format!(
            "pid{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos())
        );
        let storage = Self {
            config: config.clone(),
            pool: Arc::new(tokio::sync::OnceCell::new()),
            region_cache: RwLock::new(BTreeMap::new()),
            watchers: RwLock::new(BTreeMap::new()),
            region_locks: RwLock::new(BTreeMap::new()),
            owner_id,
            world_write_locks: Arc::new(RwLock::new(BTreeMap::new())),
            shutdown: Arc::new(AtomicBool::new(false)),
        };

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            // Eagerly connect in the background so a bad URL or unreachable
            // database fails loudly at startup instead of on the first chunk
            // IO (which without players may never happen).
            let pool_cell = storage.pool.clone();
            let url = storage.config.url.clone();
            let mode = storage.config.mode;
            handle.spawn(async move {
                if let Err(e) = pool_cell
                    .get_or_try_init(|| Self::init_pool(&url, mode))
                    .await
                {
                    error!("EasyWorld MySQL eager init failed (check [world.chunk] url): {e}");
                }
            });

            // Heartbeat task: keeps held world locks alive and retries denied
            // ones, so a reader-degraded server takes over automatically once
            // the previous writer shuts down or crashes.
            if storage.config.mode == EasyWorldMode::ReadWrite {
                let pool_cell = storage.pool.clone();
                let locks = storage.world_write_locks.clone();
                let owner = storage.owner_id.clone();
                let shutdown = storage.shutdown.clone();
                handle.spawn(async move {
                    let mut interval =
                        tokio::time::interval(Duration::from_secs(LOCK_HEARTBEAT_SECS));
                    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                    loop {
                        interval.tick().await;
                        if shutdown.load(Ordering::Relaxed) {
                            break;
                        }
                        let snapshot: Vec<(String, bool)> = locks
                            .read()
                            .await
                            .iter()
                            .map(|(k, &v)| (k.clone(), v))
                            .collect();
                        if snapshot.is_empty() {
                            continue;
                        }
                        let Some(pool) = pool_cell.get().cloned() else {
                            continue;
                        };
                        for (key, held) in snapshot {
                            if held {
                                match pool.refresh_lock(&key, &owner).await {
                                    Ok(true) => {}
                                    Ok(false) => {
                                        error!(
                                            "EasyWorld: lost write lock for {key} — writes are now rejected"
                                        );
                                        locks.write().await.insert(key, false);
                                    }
                                    Err(e) => {
                                        warn!("EasyWorld: heartbeat for {key} failed: {e}");
                                    }
                                }
                            } else if matches!(
                                pool.try_acquire_lock(&key, &owner).await,
                                Ok(true)
                            ) {
                                info!(
                                    "EasyWorld: took over write lock for {key} (previous writer gone)"
                                );
                                locks.write().await.insert(key, true);
                            }
                        }
                    }
                });
            }
        }

        storage
    }

    async fn init_pool(
        url: &str,
        mode: EasyWorldMode,
    ) -> Result<Arc<MysqlPool>, ChunkReadingError> {
        let p = MysqlPool::new(url).await?;
        match mode {
            EasyWorldMode::ReadWrite => {
                p.ensure_table().await?;
                info!("EasyWorld MySQL pool initialized (read_write)");
            }
            EasyWorldMode::ReadOnly => {
                if !p.table_exists().await? {
                    return Err(read_err(
                        "read_only mode but table easyworld_regions does not exist — \
                         start a read_write server against this database first",
                    ));
                }
                info!("EasyWorld MySQL pool initialized (read_only)");
            }
        }
        Ok(Arc::new(p))
    }

    /// Whether this server holds the write lock for `world_key`.
    /// In `read_write` mode the lock is acquired on first use; a failed
    /// acquisition (another live writer) is cached and retried by the
    /// heartbeat task.
    async fn ensure_write_lock(&self, world_key: &str) -> bool {
        if self.config.mode == EasyWorldMode::ReadOnly {
            return false;
        }
        if let Some(&held) = self.world_write_locks.read().await.get(world_key) {
            return held;
        }
        let pool = match self.ensure_pool().await {
            Ok(p) => p.clone(),
            Err(e) => {
                warn!("EasyWorld: cannot reach database to lock {world_key}: {e}");
                return false;
            }
        };
        let acquired = match pool.try_acquire_lock(world_key, &self.owner_id).await {
            Ok(a) => a,
            Err(e) => {
                warn!("EasyWorld: write lock query failed for {world_key}: {e}");
                return false;
            }
        };
        if acquired {
            info!("EasyWorld: acquired write lock for {world_key}");
        } else {
            error!(
                "EasyWorld: world {world_key} is write-locked by another server — \
                 running as READ-ONLY until the writer releases it"
            );
        }
        self.world_write_locks
            .write()
            .await
            .insert(world_key.to_string(), acquired);
        acquired
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
            .get_or_try_init(|| Self::init_pool(&self.config.url, self.config.mode))
            .await
    }

    fn world_key(&self, folder: &LevelFolder) -> String {
        let path = folder
            .root_folder
            .to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_string();
        let prefix = self.config.key_prefix.trim_matches('/');
        if prefix.is_empty() {
            path
        } else {
            format!("{prefix}/{path}")
        }
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
            let world_key = self.world_key(folder);
            // Claim (or verify) the write lock as soon as the world is used,
            // so a locked world is reported at load time, not at first save.
            if self.config.mode == EasyWorldMode::ReadWrite {
                let _ = self.ensure_write_lock(&world_key).await;
            }
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
            let world_key = self.world_key(folder);

            if !self.ensure_write_lock(&world_key).await {
                if self.config.mode == EasyWorldMode::ReadOnly {
                    // Read-only servers intentionally discard saves.
                    for (_, chunk) in &chunks_data {
                        chunk.mark_dirty(false);
                    }
                    debug!(
                        "EasyWorld read_only: discarded {} chunk saves for {world_key}",
                        chunks_data.len()
                    );
                    return Ok(());
                }
                // read_write but another server holds the lock: keep the
                // chunks dirty so they persist once the lock is taken over.
                return Err(ChunkWritingError::IoError(std::io::Error::other(format!(
                    "world {world_key} is write-locked by another server"
                ))));
            }

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
            let world_key = self.world_key(folder);
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
            let world_key = self.world_key(folder);
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

impl Drop for EasyMysqlStorage {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        // Best-effort release of held world locks so another server can take
        // over immediately; heartbeat expiry covers crashes.
        let Some(pool) = self.pool.get().cloned() else {
            return;
        };
        let held: Vec<String> = self.world_write_locks.try_read().map_or_else(
            |_| Vec::new(),
            |map| {
                map.iter()
                    .filter(|&(_, &h)| h)
                    .map(|(k, _)| k.clone())
                    .collect()
            },
        );
        if held.is_empty() {
            return;
        }
        let owner = self.owner_id.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                for key in held {
                    pool.release_lock(&key, &owner).await;
                    info!("EasyWorld: released write lock for {key}");
                }
            });
        }
    }
}
