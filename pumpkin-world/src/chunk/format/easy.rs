// EMBER - EasyWorld region format v2
//
// Region-level zstd compression with:
//  1. ChunkPruner — empty chunks (all air, no tile entities) are not stored
//  2. Bitmap + flat array — replaces BTreeMap, eliminates serialization overhead
//  3. All stored chunk NBT concatenated into one contiguous buffer before zstd
//
// `EasyRegionData` below is the *wire* layout (what gets postcard-encoded and
// zstd-compressed): optimized for compression ratio, not point updates — a
// single-chunk change means splicing the shared `chunks_data` buffer.
// `EasyWorldFile` keeps the *live* copy as a plain index -> bytes map instead
// (O(1) update/remove), and only pays the O(region bytes) cost of building
// the wire layout once, in `write()`, instead of once per updated chunk.
//
// File extension: .easy
// File naming:    r.{region_x}.{region_z}.easy

use std::{marker::PhantomData, path::PathBuf};

use bytes::Bytes;
use pumpkin_data::block_properties::is_air;
use pumpkin_util::math::vector2::Vector2;
use rustc_hash::FxHashMap;
use ruzstd::{
    decoding::StreamingDecoder,
    encoding::{CompressionLevel, compress_to_vec},
};
use serde::{Deserialize, Serialize};
use tracing::{debug, trace};

use crate::chunk::{
    ChunkReadingError, ChunkWritingError,
    format::anvil::SingleChunkDataSerializer,
    io::{ChunkSerializer, LoadedData},
};

/// Magic bytes: "EZW\x02" (`EasyWorld` v2)
const EASY_MAGIC: u32 = 0x45_5a_57_02;

// ─── Serde-compatible region data ─────────────────────────────────────

/// Serializable region data for `EasyWorld` v2.
///
/// The bitmap marks which chunk indices (0..1023) are stored.
/// `chunk_sizes` (one `u32` per stored chunk) allows random access into `chunks_data`.
/// `chunks_data` is the concatenation of all stored chunks' raw NBT bytes — this
/// contiguous layout gives zstd the best cross-chunk dictionary sharing.
#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct EasyRegionData {
    magic: u32,
    pub(crate) region_x: i32,
    pub(crate) region_z: i32,
    /// Bitmap: bit i set -> chunk with region-relative index i is stored.
    chunk_bitmap: Vec<u8>,
    /// Sizes of each stored chunk's NBT data, in bitmap order.
    chunk_sizes: Vec<u32>,
    /// Concatenated NBT bytes of all stored chunks.
    chunks_data: Vec<u8>,
}

impl EasyRegionData {
    pub(crate) fn new(region_x: i32, region_z: i32) -> Self {
        Self {
            magic: EASY_MAGIC,
            region_x,
            region_z,
            chunk_bitmap: vec![0u8; 128],
            chunk_sizes: Vec::new(),
            chunks_data: Vec::new(),
        }
    }

    /// Builds a fresh wire-format region from a live `index -> bytes` map in
    /// one O(total stored bytes) pass — the batch counterpart to calling
    /// `upsert_chunk` once per chunk, which is O(region bytes) *each* call
    /// because it keeps the shared `chunks_data` buffer in bitmap order.
    pub(crate) fn from_chunks(
        region_x: i32,
        region_z: i32,
        chunks: &FxHashMap<u32, Bytes>,
    ) -> Self {
        let mut region = Self::new(region_x, region_z);
        let mut indices: Vec<u32> = chunks.keys().copied().collect();
        indices.sort_unstable();
        for index in indices {
            let bytes = &chunks[&index];
            region.set_chunk(index);
            region.chunk_sizes.push(bytes.len() as u32);
            region.chunks_data.extend_from_slice(bytes);
        }
        region
    }

    /// Returns true if the bit for chunk `index` (0..1023) is set.
    fn has_chunk(&self, index: u32) -> bool {
        let byte = self.chunk_bitmap[(index / 8) as usize];
        (byte >> (index % 8)) & 1 == 1
    }

    /// Set the bit for chunk `index`.
    fn set_chunk(&mut self, index: u32) {
        self.chunk_bitmap[(index / 8) as usize] |= 1 << (index % 8);
    }

    /// Byte spans `(offset, size)` of every stored chunk, indexed by
    /// region-relative chunk index. One O(1024) prefix-sum pass over the
    /// bitmap, used by every read path (batch loads, template building).
    pub(crate) fn chunk_spans(&self) -> Box<[Option<(usize, usize)>]> {
        let mut spans = vec![None; 1024].into_boxed_slice();
        let mut stored_idx = 0usize;
        let mut offset = 0usize;
        for index in 0..1024u32 {
            if self.has_chunk(index) {
                let Some(&size) = self.chunk_sizes.get(stored_idx) else {
                    break;
                };
                let size = size as usize;
                if offset + size > self.chunks_data.len() {
                    break;
                }
                spans[index as usize] = Some((offset, size));
                offset += size;
                stored_idx += 1;
            }
        }
        spans
    }

    /// Iterates `(chunk index, raw NBT bytes)` over every stored chunk.
    /// Used to build shared instance templates.
    pub(crate) fn stored_chunks(&self) -> impl Iterator<Item = (u32, &[u8])> + '_ {
        let spans = self.chunk_spans();
        (0..1024u32).filter_map(move |i| {
            spans[i as usize].map(|(offset, size)| (i, &self.chunks_data[offset..offset + size]))
        })
    }

    /// Inverse of `from_chunks`: materializes this wire-format region into a
    /// live `index -> bytes` map, the representation every read path (file
    /// load, `MySQL` region-cache fill) actually works against — O(1) lookup
    /// per chunk instead of re-walking the bitmap for each one.
    pub(crate) fn to_chunks_map(&self) -> FxHashMap<u32, Bytes> {
        self.stored_chunks()
            .map(|(index, bytes)| (index, Bytes::copy_from_slice(bytes)))
            .collect()
    }

    /// Structural consistency check for data loaded from disk or database.
    /// Guards every later slice/index operation against corrupted input.
    pub(crate) fn is_consistent(&self) -> bool {
        if self.magic != EASY_MAGIC || self.chunk_bitmap.len() != 128 {
            return false;
        }
        let stored: usize = self
            .chunk_bitmap
            .iter()
            .map(|b| b.count_ones() as usize)
            .sum();
        let total: usize = self.chunk_sizes.iter().map(|&s| s as usize).sum();
        stored == self.chunk_sizes.len() && total == self.chunks_data.len()
    }
}

// ─── ChunkPruner ───────────────────────────────────────────────────────

/// Returns `true` when the chunk contains only air blocks and has no pending
/// block entities — i.e. it can be reconstructed as an empty chunk on load.
pub(crate) fn is_prunable_chunk(chunk: &crate::chunk::ChunkData) -> bool {
    // Check block palette: every section must be all-air.
    let sections = chunk.section.block_sections.read().unwrap();
    let all_air = sections.iter().all(|section| match section {
        crate::chunk::palette::PalettedContainer::Homogeneous(state_id) => is_air(*state_id),
        crate::chunk::palette::PalettedContainer::Heterogeneous(data) => {
            data.palette.iter().all(|&state_id| is_air(state_id))
        }
    });
    if !all_air {
        return false;
    }

    // Check for pending block entities (tile entities that haven't been placed yet).
    let block_entities = chunk.pending_block_entities.lock().unwrap();
    if !block_entities.is_empty() {
        return false;
    }

    // EMBER start - chunk-embedded storage for ember custom blocks/furniture
    // Furniture in particular never touches block state (packet-only display
    // entity), so an all-air chunk holding only furniture must not be pruned.
    if !chunk.ember_custom_blocks.lock().unwrap().is_empty()
        || !chunk.ember_furniture.lock().unwrap().is_empty()
    {
        return false;
    }
    // EMBER end

    true
}

/// Decompresses and validates a serialized region blob (the shared decode
/// path for `.easy` files, `MySQL` rows and instance templates).
pub(crate) fn decode_region_bytes(raw: &[u8]) -> Result<EasyRegionData, ChunkReadingError> {
    let mut decoder = StreamingDecoder::new(raw).map_err(|e| {
        ChunkReadingError::Compression(crate::chunk::CompressionError::ZstdError(
            std::io::Error::other(e.to_string()),
        ))
    })?;
    let mut decompressed = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut decompressed)
        .map_err(ChunkReadingError::IoError)?;

    let data: EasyRegionData = postcard::from_bytes(&decompressed).map_err(|e| {
        ChunkReadingError::ParsingError(crate::chunk::ChunkParsingError::ErrorDeserializingChunk(
            e.to_string(),
        ))
    })?;

    if !data.is_consistent() {
        return Err(ChunkReadingError::InvalidHeader);
    }
    Ok(data)
}

// ─── ChunkSerializer implementation ────────────────────────────────────

pub struct EasyWorldFile<D> {
    region_x: i32,
    region_z: i32,
    /// Live per-chunk storage, keyed by region-relative index (0..1024).
    /// `update_chunk` is a plain O(1) map insert/remove; the wire-format
    /// `EasyRegionData` (bitmap + concatenated bytes) is only built when
    /// actually serializing in `write()`.
    chunks: FxHashMap<u32, Bytes>,
    /// Set on the first actual mutation; a clean region skips the
    /// whole-region recompress + rewrite entirely on flush.
    dirty: std::sync::atomic::AtomicBool,
    _phantom: PhantomData<D>,
}

impl<D> Default for EasyWorldFile<D> {
    fn default() -> Self {
        Self {
            region_x: 0,
            region_z: 0,
            chunks: FxHashMap::default(),
            dirty: std::sync::atomic::AtomicBool::new(false),
            _phantom: PhantomData,
        }
    }
}

/// Region-relative chunk index (0..1024) for a chunk at world position `(x, z)`.
/// Shared with `easy_mysql.rs` so both backends compute it identically.
pub(crate) const fn region_relative_index(x: i32, z: i32) -> u32 {
    let rel_x = x.rem_euclid(32);
    let rel_z = z.rem_euclid(32);
    (rel_x + rel_z * 32) as u32
}

impl<D> ChunkSerializer for EasyWorldFile<D>
where
    D: SingleChunkDataSerializer + Send + Sync + Sized + 'static,
{
    type Data = D;
    type WriteBackend = PathBuf;
    type ChunkConfig = ();

    fn get_chunk_key(chunk: &Vector2<i32>) -> String {
        let region_x = chunk.x >> 5;
        let region_z = chunk.y >> 5;
        format!("r.{region_x}.{region_z}.easy")
    }

    fn should_write(&self, is_watched: bool) -> bool {
        // Watched regions defer to the unload/unwatch flush, like Anvil.
        !is_watched
    }

    async fn write(&self, backend: &Self::WriteBackend) -> Result<(), std::io::Error> {
        // A region that was never mutated has nothing to say: skip the
        // whole-region recompress and leave the on-disk file untouched.
        if !self.dirty.load(std::sync::atomic::Ordering::Acquire) {
            trace!("EasyWorld v2: skipping clean region {}", backend.display());
            return Ok(());
        }

        let region = EasyRegionData::from_chunks(self.region_x, self.region_z, &self.chunks);
        let serialized = postcard::to_allocvec(&region)
            .map_err(|e| std::io::Error::other(format!("postcard serialize: {e}")))?;
        let raw_len = serialized.len();

        // Region compression is CPU-bound — keep it off the async workers.
        // EMBER: `ruzstd` only implements `Uncompressed`/`Fastest` — any other
        // level (including `Default`, used here until this fix) hits
        // `unimplemented!()` and panics on every call. Match pump.rs/linear.rs,
        // which already use the only working compressing level.
        let compressed = tokio::task::spawn_blocking(move || {
            compress_to_vec(&*serialized, CompressionLevel::Fastest)
        })
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))?;
        debug!(
            "EasyWorld v2: {} chunks → {} B raw → {} B zstd for {}",
            self.chunks.len(),
            raw_len,
            compressed.len(),
            backend.display(),
        );

        // Atomic replace: write a temp file, fsync, then rename over the
        // target so a crash mid-write can never truncate the region.
        let tmp = backend.with_extension("easy.tmp");
        let mut file = tokio::fs::File::create(&tmp).await?;
        tokio::io::AsyncWriteExt::write_all(&mut file, &compressed).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&tmp, backend).await?;
        self.dirty
            .store(false, std::sync::atomic::Ordering::Release);
        Ok(())
    }

    fn read(r: Bytes) -> Result<Self, ChunkReadingError> {
        let region = decode_region_bytes(&r)?;
        Ok(Self {
            region_x: region.region_x,
            region_z: region.region_z,
            chunks: region.to_chunks_map(),
            dirty: std::sync::atomic::AtomicBool::new(false),
            _phantom: PhantomData,
        })
    }

    async fn update_chunk(
        &mut self,
        chunk_data: &Self::Data,
        _chunk_config: &Self::ChunkConfig,
    ) -> Result<(), ChunkWritingError> {
        let (x, z) = chunk_data.position();
        self.region_x = x >> 5;
        self.region_z = z >> 5;
        let index = region_relative_index(x, z);

        // ChunkPruner: skip chunks that are entirely air with no block entities.
        // We downcast via Any to check the concrete type.  This only applies when
        // Data = ChunkData; for ChunkEntityData the check is a no-op.
        let should_skip = Self::try_prune(chunk_data);

        if should_skip {
            trace!("EasyWorld: pruning empty chunk ({x},{z}) index {index}");
            // Remove any previously stored version, otherwise a chunk that was
            // mined out to all air would resurrect its old contents on reload.
            if self.chunks.remove(&index).is_some() {
                self.dirty.store(true, std::sync::atomic::Ordering::Release);
            }
            return Ok(());
        }

        let bytes = chunk_data
            .to_bytes()
            .await
            .map_err(|e| ChunkWritingError::ChunkSerializingError(e.to_string()))?;

        self.chunks.insert(index, bytes);
        self.dirty.store(true, std::sync::atomic::Ordering::Release);

        Ok(())
    }

    async fn get_chunks(
        &self,
        chunks: Vec<Vector2<i32>>,
        stream: tokio::sync::mpsc::Sender<LoadedData<Self::Data, ChunkReadingError>>,
    ) {
        // Direct O(1) map lookup per requested chunk — no region-wide pass needed.
        for pos in chunks {
            let index = region_relative_index(pos.x, pos.y);
            match self.chunks.get(&index) {
                Some(bytes) => match D::from_bytes(bytes, pos) {
                    Ok(data) => {
                        let _ = stream.send(LoadedData::Loaded(data)).await;
                    }
                    Err(e) => {
                        let _ = stream.send(LoadedData::Error((pos, e))).await;
                    }
                },
                None => {
                    let _ = stream.send(LoadedData::Missing(pos)).await;
                }
            }
        }
    }
}

impl<D: 'static> EasyWorldFile<D> {
    /// Try to prune: returns `true` if the chunk should be skipped.
    fn try_prune(chunk_data: &D) -> bool {
        try_prune_chunk_any(chunk_data)
    }
}

/// Returns `true` when the (type-erased) chunk should be pruned from
/// storage. Uses `Any` downcasting so it compiles for both `ChunkData` and
/// `ChunkEntityData`; only all-air `ChunkData` is ever pruned.
pub(crate) fn try_prune_chunk_any<D: 'static>(chunk_data: &D) -> bool {
    let any = chunk_data as &dyn std::any::Any;
    if let Some(chunk) = any.downcast_ref::<crate::chunk::ChunkData>() {
        return is_prunable_chunk(chunk);
    }
    // For ChunkEntityData, never prune (entities are always meaningful).
    false
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::{EasyRegionData, FxHashMap};

    /// Builds a region the same way production code does — via `from_chunks`
    /// — instead of point mutation, since the wire format is now write-once
    /// per flush (no more incremental `upsert_chunk`/`remove_chunk`).
    fn region_from(region_x: i32, region_z: i32, entries: &[(u32, &[u8])]) -> EasyRegionData {
        let map: FxHashMap<u32, Bytes> = entries
            .iter()
            .map(|&(index, data)| (index, Bytes::copy_from_slice(data)))
            .collect();
        EasyRegionData::from_chunks(region_x, region_z, &map)
    }

    #[test]
    fn postcard_roundtrip() {
        let r = region_from(-3, 7, &[(0, &[42u8; 16]), (1023, &[43u8; 32])]);
        let bytes = postcard::to_allocvec(&r).unwrap();
        let back: EasyRegionData = postcard::from_bytes(&bytes).unwrap();
        assert!(back.is_consistent());
        assert_eq!(back.region_x, -3);
        assert_eq!(back.region_z, 7);
        let stored: FxHashMap<u32, &[u8]> = back.stored_chunks().collect();
        assert_eq!(stored[&0], &[42u8; 16][..]);
        assert_eq!(stored[&1023], &[43u8; 32][..]);
    }

    #[test]
    fn chunk_spans_and_stored_chunks_agree() {
        // Out-of-order insertion: sizes/data must land in bitmap order.
        let r = region_from(0, 0, &[(900, &[9u8; 10]), (300, &[3u8; 7]), (5, &[5u8; 4])]);
        assert!(r.is_consistent());

        let spans = r.chunk_spans();
        assert!(spans[0].is_none());
        assert!(spans[1023].is_none());
        for (index, bytes) in r.stored_chunks() {
            let (offset, size) = spans[index as usize].expect("span must exist for a stored chunk");
            assert_eq!(&r.chunks_data[offset..offset + size], bytes);
        }
        let stored: Vec<u32> = r.stored_chunks().map(|(i, _)| i).collect();
        assert_eq!(stored, vec![5, 300, 900]);
    }

    #[test]
    fn corrupted_data_is_rejected_not_panicking() {
        let mut r = region_from(0, 0, &[(4, &[1u8; 4])]);
        // Corrupt: drop the size table entry while the bit stays set.
        r.chunk_sizes.clear();
        assert!(!r.is_consistent());
        assert!(r.chunk_spans()[4].is_none()); // must not panic
        assert!(r.stored_chunks().next().is_none()); // must not panic
    }

    #[test]
    fn from_chunks_empty_map_is_consistent_and_empty() {
        let empty = FxHashMap::default();
        let region = EasyRegionData::from_chunks(0, 0, &empty);
        assert!(region.is_consistent());
        assert!(region.stored_chunks().next().is_none());
    }
}

#[cfg(test)]
mod easy_world_file_tests {
    use std::future::Future;
    use std::pin::Pin;

    use bytes::Bytes;
    use pumpkin_util::math::vector2::Vector2;
    use serde::{Deserialize, Serialize};
    use temp_dir::TempDir;

    use super::EasyWorldFile;
    use crate::chunk::format::anvil::SingleChunkDataSerializer;
    use crate::chunk::io::{ChunkSerializer, Dirtiable, LoadedData};
    use crate::chunk::{ChunkReadingError, ChunkSerializingError};

    #[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
    struct MockChunk {
        x: i32,
        z: i32,
        payload: Vec<u8>,
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

    fn chunk_at(x: i32, z: i32, payload: &[u8]) -> MockChunk {
        MockChunk {
            x,
            z,
            payload: payload.to_vec(),
        }
    }

    async fn collect(
        file: &EasyWorldFile<MockChunk>,
        positions: Vec<Vector2<i32>>,
    ) -> Vec<LoadedData<MockChunk, ChunkReadingError>> {
        let (tx, mut rx) = tokio::sync::mpsc::channel(positions.len().max(1));
        file.get_chunks(positions, tx).await;
        let mut out = Vec::new();
        while let Ok(item) = rx.try_recv() {
            out.push(item);
        }
        out
    }

    #[tokio::test]
    async fn update_chunk_is_visible_before_any_flush() {
        let mut file = EasyWorldFile::<MockChunk>::default();
        file.update_chunk(&chunk_at(1, 2, b"hello"), &())
            .await
            .unwrap();
        file.update_chunk(&chunk_at(3, 4, b"world"), &())
            .await
            .unwrap();

        // No write() has happened yet: reads must still see both chunks
        // (this is the correctness property the O(1) live map has to
        // preserve now that update_chunk no longer mutates a shared,
        // eagerly-serialized wire buffer).
        let loaded = collect(
            &file,
            vec![
                Vector2::new(1, 2),
                Vector2::new(3, 4),
                Vector2::new(9, 9), // never written -> Missing
            ],
        )
        .await;

        assert_eq!(loaded.len(), 3);
        assert!(matches!(&loaded[0], LoadedData::Loaded(c) if c.payload == b"hello"));
        assert!(matches!(&loaded[1], LoadedData::Loaded(c) if c.payload == b"world"));
        assert!(matches!(&loaded[2], LoadedData::Missing(_)));
    }

    #[tokio::test]
    async fn write_then_read_roundtrips_all_chunks() {
        let dir = TempDir::new().unwrap();
        let path = dir.child("r.0.0.easy");

        let mut file = EasyWorldFile::<MockChunk>::default();
        for i in 0..10i32 {
            file.update_chunk(&chunk_at(i, 0, &[i as u8; 3]), &())
                .await
                .unwrap();
        }
        file.write(&path).await.unwrap();

        // `read()` decompresses internally (see `decode_region_bytes`), so the
        // raw on-disk bytes go straight in — matching how `file_manager.rs`
        // and the `pump.rs` equivalent test call it.
        let bytes = tokio::fs::read(&path).await.unwrap();
        let reloaded = EasyWorldFile::<MockChunk>::read(Bytes::from(bytes)).unwrap();

        let positions: Vec<Vector2<i32>> = (0..10i32).map(|i| Vector2::new(i, 0)).collect();
        let loaded = collect(&reloaded, positions).await;
        assert_eq!(loaded.len(), 10);
        for (i, item) in loaded.into_iter().enumerate() {
            match item {
                LoadedData::Loaded(c) => {
                    assert_eq!(c.x, i as i32);
                    assert_eq!(c.payload, vec![i as u8; 3]);
                }
                _ => panic!("expected Loaded at index {i}"),
            }
        }
    }

    #[tokio::test]
    async fn clean_region_skips_write() {
        let dir = TempDir::new().unwrap();
        let path = dir.child("r.0.0.easy");
        let file = EasyWorldFile::<MockChunk>::default(); // never mutated -> not dirty
        file.write(&path).await.unwrap();
        assert!(!path.exists(), "write() must be a no-op on a clean region");
    }
}
