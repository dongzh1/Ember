// EMBER - EasyShard region format (.ezs)
//
// EasyWorld v3 for write-heavy worlds (resource worlds): every group of
// `group_chunks` chunks is its own zstd blob, so writing one chunk
// recompresses ONE group instead of the whole region (the whole-region
// recompress is `easy`'s biggest cost on scattered edits).
//
//  * `group_chunks = 1` (default): per-chunk blobs — minimal write cost,
//    Anvil-style random access, Pump-style residency (compressed bytes).
//  * larger groups trade write cost for cross-chunk compression ratio,
//    up to `1024` = whole-region (`easy`'s ratio).
//
// Flushes rewrite the file through an atomic temp+fsync+rename, but only
// the groups touched since the last flush are recompressed — clean groups
// re-emit their already-compressed bytes. A clean region writes nothing.
// Unlike other formats, dirty *watched* regions also flush on autosave
// (`should_write(true)`), closing the crash-loss window for worlds where
// players keep regions loaded for hours while mining.
//
// File naming: r.{region_x}.{region_z}.ezs

use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use bytes::Bytes;
use pumpkin_config::chunk::EasyShardConfig;
use pumpkin_util::math::vector2::Vector2;
use ruzstd::decoding::StreamingDecoder;
use ruzstd::encoding::{CompressionLevel, compress_to_vec};
use serde::{Deserialize, Serialize};
use tracing::trace;

use crate::chunk::format::anvil::SingleChunkDataSerializer;
use crate::chunk::format::easy::try_prune_chunk_any;
use crate::chunk::io::{ChunkSerializer, LoadedData};
use crate::chunk::{ChunkReadingError, ChunkWritingError};

/// Magic bytes: "EZW\x03" (`EasyShard` v3)
const SHARD_MAGIC: u32 = 0x455a_5703;

/// A group's decompressed contents: `(region-relative chunk index, raw NBT)`.
type UnitEntries = Vec<(u16, Vec<u8>)>;

fn read_err(e: impl std::fmt::Display) -> ChunkReadingError {
    ChunkReadingError::IoError(std::io::Error::other(e.to_string()))
}

fn decode_unit(blob: &[u8]) -> Result<UnitEntries, ChunkReadingError> {
    let mut decoder = StreamingDecoder::new(blob).map_err(read_err)?;
    let mut decompressed = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut decompressed)
        .map_err(ChunkReadingError::IoError)?;
    postcard::from_bytes(&decompressed).map_err(read_err)
}

fn encode_unit(entries: &UnitEntries) -> Result<Vec<u8>, ChunkWritingError> {
    let raw = postcard::to_allocvec(entries)
        .map_err(|e| ChunkWritingError::ChunkSerializingError(e.to_string()))?;
    Ok(compress_to_vec(&*raw, CompressionLevel::Fastest))
}

/// On-disk (and in-memory) shard container. Unit blobs stay compressed in
/// memory — a resident region costs its compressed size, not tens of MB of
/// raw NBT.
#[derive(Serialize, Deserialize)]
pub(crate) struct ShardData {
    magic: u32,
    region_x: i32,
    region_z: i32,
    /// Chunks per compression unit, fixed when the file is first written.
    group_chunks: u16,
    /// unit index -> zstd blob of that unit's [`UnitEntries`].
    units: BTreeMap<u16, Vec<u8>>,
}

impl ShardData {
    const fn new(group_chunks: u16) -> Self {
        Self {
            magic: SHARD_MAGIC,
            region_x: 0,
            region_z: 0,
            group_chunks,
            units: BTreeMap::new(),
        }
    }

    fn is_valid(&self) -> bool {
        self.magic == SHARD_MAGIC && (1..=1024).contains(&self.group_chunks)
    }

    const fn unit_of(&self, chunk_index: u16) -> u16 {
        chunk_index / self.group_chunks
    }
}

pub struct EasyShardFile<D> {
    data: ShardData,
    /// `group_chunks` was taken from the config yet (files loaded from disk
    /// keep their own value for stability).
    group_resolved: bool,
    /// Set on the first actual mutation; clean regions skip the flush.
    dirty: AtomicBool,
    _phantom: PhantomData<D>,
}

impl<D> Default for EasyShardFile<D> {
    fn default() -> Self {
        Self {
            data: ShardData::new(1),
            group_resolved: false,
            dirty: AtomicBool::new(false),
            _phantom: PhantomData,
        }
    }
}

impl<D> ChunkSerializer for EasyShardFile<D>
where
    D: SingleChunkDataSerializer + Send + Sync + Sized + 'static,
{
    type Data = D;
    type WriteBackend = PathBuf;
    type ChunkConfig = EasyShardConfig;

    fn get_chunk_key(chunk: &Vector2<i32>) -> String {
        let region_x = chunk.x >> 5;
        let region_z = chunk.y >> 5;
        format!("r.{region_x}.{region_z}.ezs")
    }

    fn should_write(&self, is_watched: bool) -> bool {
        // Dirty regions flush even while watched (durable autosave for
        // write-heavy worlds); clean regions never do.
        !is_watched || self.dirty.load(Ordering::Acquire)
    }

    async fn write(&self, backend: &Self::WriteBackend) -> Result<(), std::io::Error> {
        if !self.dirty.load(Ordering::Acquire) {
            trace!("EasyShard: skipping clean region {}", backend.display());
            return Ok(());
        }

        // Units are already compressed — serializing the container is a
        // plain memory copy, no CPU-heavy work here.
        let serialized = postcard::to_allocvec(&self.data)
            .map_err(|e| std::io::Error::other(format!("postcard serialize: {e}")))?;

        // Atomic replace: temp file + fsync + rename, a crash can never
        // truncate the region.
        let tmp = backend.with_extension("ezs.tmp");
        let mut file = tokio::fs::File::create(&tmp).await?;
        tokio::io::AsyncWriteExt::write_all(&mut file, &serialized).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&tmp, backend).await?;
        self.dirty.store(false, Ordering::Release);
        Ok(())
    }

    fn read(r: Bytes) -> Result<Self, ChunkReadingError> {
        let data: ShardData = postcard::from_bytes(&r).map_err(|e| {
            ChunkReadingError::ParsingError(
                crate::chunk::ChunkParsingError::ErrorDeserializingChunk(e.to_string()),
            )
        })?;
        if !data.is_valid() {
            return Err(ChunkReadingError::InvalidHeader);
        }
        Ok(Self {
            data,
            group_resolved: true,
            dirty: AtomicBool::new(false),
            _phantom: PhantomData,
        })
    }

    async fn update_chunk(
        &mut self,
        chunk_data: &Self::Data,
        chunk_config: &Self::ChunkConfig,
    ) -> Result<(), ChunkWritingError> {
        // New files take the configured group size (clamped); files loaded
        // from disk keep the size they were written with.
        if !self.group_resolved {
            self.data.group_chunks = chunk_config.group_chunks.clamp(1, 1024);
            self.group_resolved = true;
        }

        let (x, z) = chunk_data.position();
        self.data.region_x = x >> 5;
        self.data.region_z = z >> 5;
        let rel_x = x.rem_euclid(32);
        let rel_z = z.rem_euclid(32);
        let index = (rel_x + rel_z * 32) as u16;
        let unit_idx = self.data.unit_of(index);

        // Decompress only the touched unit (one chunk when group_chunks=1).
        let mut entries = match self.data.units.get(&unit_idx) {
            Some(blob) => decode_unit(blob)
                .map_err(|e| ChunkWritingError::ChunkSerializingError(e.to_string()))?,
            None => Vec::new(),
        };

        if try_prune_chunk_any(chunk_data) {
            // ChunkPruner: an emptied chunk is removed so its old contents
            // cannot resurrect on the next load.
            let before = entries.len();
            entries.retain(|(i, _)| *i != index);
            if before == entries.len() {
                return Ok(()); // was not stored — nothing changed
            }
        } else {
            let bytes = chunk_data
                .to_bytes()
                .await
                .map_err(|e| ChunkWritingError::ChunkSerializingError(e.to_string()))?;
            if let Some((_, existing)) = entries.iter_mut().find(|(i, _)| *i == index) {
                *existing = bytes.to_vec();
            } else {
                entries.push((index, bytes.to_vec()));
                entries.sort_unstable_by_key(|(i, _)| *i);
            }
        }

        if entries.is_empty() {
            self.data.units.remove(&unit_idx);
        } else {
            self.data.units.insert(unit_idx, encode_unit(&entries)?);
        }
        self.dirty.store(true, Ordering::Release);
        Ok(())
    }

    async fn get_chunks(
        &self,
        chunks: Vec<Vector2<i32>>,
        stream: tokio::sync::mpsc::Sender<LoadedData<Self::Data, ChunkReadingError>>,
    ) {
        // Decompress each touched unit once per batch.
        let mut decoded: BTreeMap<u16, Option<UnitEntries>> = BTreeMap::new();

        for pos in chunks {
            let rel_x = pos.x.rem_euclid(32);
            let rel_z = pos.y.rem_euclid(32);
            let index = (rel_x + rel_z * 32) as u16;
            let unit_idx = self.data.unit_of(index);

            let entries = decoded.entry(unit_idx).or_insert_with(|| {
                self.data
                    .units
                    .get(&unit_idx)
                    .and_then(|blob| decode_unit(blob).ok())
            });

            let raw = entries
                .as_ref()
                .and_then(|e| e.iter().find(|(i, _)| *i == index))
                .map(|(_, nbt)| nbt);

            match raw {
                Some(nbt) => {
                    let bytes = Bytes::copy_from_slice(nbt);
                    match D::from_bytes(&bytes, pos) {
                        Ok(data) => {
                            let _ = stream.send(LoadedData::Loaded(data)).await;
                        }
                        Err(e) => {
                            let _ = stream.send(LoadedData::Error((pos, e))).await;
                        }
                    }
                }
                None => {
                    let _ = stream.send(LoadedData::Missing(pos)).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::ChunkSerializingError;
    use crate::chunk::io::Dirtiable;
    use std::future::Future;
    use std::pin::Pin;
    use temp_dir::TempDir;

    #[derive(Debug, Serialize, Deserialize, Clone)]
    struct MockChunk {
        x: i32,
        z: i32,
        data: Vec<u8>,
    }

    impl Dirtiable for MockChunk {
        fn is_dirty(&self) -> bool {
            true
        }
        fn mark_dirty(&self, _: bool) {}
    }

    impl SingleChunkDataSerializer for MockChunk {
        fn to_bytes(
            &self,
        ) -> Pin<Box<dyn Future<Output = Result<Bytes, ChunkSerializingError>> + Send + '_>>
        {
            let mut buf = Vec::new();
            pumpkin_nbt::to_bytes_unnamed(self, &mut buf).unwrap();
            let bytes = Bytes::from(buf);
            Box::pin(async move { Ok(bytes) })
        }
        fn from_bytes(bytes: &Bytes, pos: Vector2<i32>) -> Result<Self, ChunkReadingError> {
            let mut mock: Self = pumpkin_nbt::from_bytes_unnamed(std::io::Cursor::new(bytes))
                .map_err(|e| {
                    ChunkReadingError::ParsingError(
                        crate::chunk::ChunkParsingError::ErrorDeserializingChunk(e.to_string()),
                    )
                })?;
            mock.x = pos.x;
            mock.z = pos.y;
            Ok(mock)
        }
        fn position(&self) -> (i32, i32) {
            (self.x, self.z)
        }
    }

    fn mock(x: i32, z: i32, data: Vec<u8>) -> MockChunk {
        MockChunk { x, z, data }
    }

    async fn collect(
        file: &EasyShardFile<MockChunk>,
        coords: Vec<Vector2<i32>>,
    ) -> Vec<LoadedData<MockChunk, ChunkReadingError>> {
        let (tx, mut rx) = tokio::sync::mpsc::channel(coords.len().max(1));
        file.get_chunks(coords, tx).await;
        let mut out = Vec::new();
        while let Some(v) = rx.recv().await {
            out.push(v);
        }
        out
    }

    #[tokio::test]
    async fn shard_roundtrip_and_missing() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.child("r.0.0.ezs");
        let config = EasyShardConfig { group_chunks: 1 };

        let mut file: EasyShardFile<MockChunk> = EasyShardFile::default();
        file.update_chunk(&mock(0, 0, vec![1, 2, 3]), &config)
            .await
            .unwrap();
        file.update_chunk(&mock(31, 31, vec![9]), &config)
            .await
            .unwrap();
        file.write(&path).await.unwrap();

        let bytes = tokio::fs::read(&path).await.unwrap();
        let read = EasyShardFile::<MockChunk>::read(Bytes::from(bytes)).unwrap();
        assert_eq!(read.data.units.len(), 2); // group=1 -> one unit per chunk

        let loaded = collect(
            &read,
            vec![Vector2::new(0, 0), Vector2::new(31, 31), Vector2::new(5, 5)],
        )
        .await;
        assert!(matches!(&loaded[0], LoadedData::Loaded(c) if c.data == vec![1, 2, 3]));
        assert!(matches!(&loaded[1], LoadedData::Loaded(c) if c.data == vec![9]));
        assert!(matches!(&loaded[2], LoadedData::Missing(_)));
    }

    #[tokio::test]
    async fn clean_region_writes_nothing() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.child("r.0.0.ezs");
        let file: EasyShardFile<MockChunk> = EasyShardFile::default();
        // Never mutated -> write is a no-op, no file is created.
        file.write(&path).await.unwrap();
        assert!(!path.exists());
        assert!(!file.should_write(true));
        assert!(file.should_write(false));
    }

    #[tokio::test]
    async fn grouping_packs_chunks_together() {
        let config = EasyShardConfig { group_chunks: 32 };
        let mut file: EasyShardFile<MockChunk> = EasyShardFile::default();
        // Chunks (0,0) and (5,0) -> indices 0 and 5 -> same unit 0.
        file.update_chunk(&mock(0, 0, vec![1]), &config)
            .await
            .unwrap();
        file.update_chunk(&mock(5, 0, vec![2]), &config)
            .await
            .unwrap();
        // Chunk (0,1) -> index 32 -> unit 1.
        file.update_chunk(&mock(0, 1, vec![3]), &config)
            .await
            .unwrap();
        assert_eq!(file.data.units.len(), 2);

        let loaded = collect(
            &file,
            vec![Vector2::new(0, 0), Vector2::new(5, 0), Vector2::new(0, 1)],
        )
        .await;
        assert!(loaded.iter().all(|l| matches!(l, LoadedData::Loaded(_))));
    }

    #[tokio::test]
    async fn loaded_file_keeps_its_group_size() {
        let temp_dir = TempDir::new().unwrap();
        let path = temp_dir.child("r.0.0.ezs");
        let mut file: EasyShardFile<MockChunk> = EasyShardFile::default();
        file.update_chunk(&mock(0, 0, vec![1]), &EasyShardConfig { group_chunks: 8 })
            .await
            .unwrap();
        file.write(&path).await.unwrap();

        let bytes = tokio::fs::read(&path).await.unwrap();
        let mut read = EasyShardFile::<MockChunk>::read(Bytes::from(bytes)).unwrap();
        // A different configured group size must not re-shard the file.
        read.update_chunk(&mock(1, 0, vec![2]), &EasyShardConfig { group_chunks: 512 })
            .await
            .unwrap();
        assert_eq!(read.data.group_chunks, 8);
    }

    #[test]
    fn corrupt_or_foreign_files_are_rejected() {
        assert!(EasyShardFile::<MockChunk>::read(Bytes::from_static(b"nonsense")).is_err());
        let wrong_magic = ShardData {
            magic: 0xdead_beef,
            region_x: 0,
            region_z: 0,
            group_chunks: 1,
            units: BTreeMap::new(),
        };
        let bytes = postcard::to_allocvec(&wrong_magic).unwrap();
        assert!(EasyShardFile::<MockChunk>::read(Bytes::from(bytes)).is_err());
    }
}
