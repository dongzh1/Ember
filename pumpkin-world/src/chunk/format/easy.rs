// EMBER - EasyWorld region format v2
//
// Region-level zstd compression with:
//  1. ChunkPruner — empty chunks (all air, no tile entities) are not stored
//  2. Bitmap + flat array — replaces BTreeMap, eliminates serialization overhead
//  3. All stored chunk NBT concatenated into one contiguous buffer before zstd
//
// File extension: .easy
// File naming:    r.{region_x}.{region_z}.easy

use std::{marker::PhantomData, path::PathBuf};

use bytes::Bytes;
use pumpkin_data::block_properties::is_air;
use pumpkin_util::math::vector2::Vector2;
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

    /// Returns true if the bit for chunk `index` (0..1023) is set.
    fn has_chunk(&self, index: u32) -> bool {
        let byte = self.chunk_bitmap[(index / 8) as usize];
        (byte >> (index % 8)) & 1 == 1
    }

    /// Set the bit for chunk `index`.
    fn set_chunk(&mut self, index: u32) {
        self.chunk_bitmap[(index / 8) as usize] |= 1 << (index % 8);
    }

    /// Clear the bit for chunk `index`.
    fn clear_chunk(&mut self, index: u32) {
        self.chunk_bitmap[(index / 8) as usize] &= !(1u8 << (index % 8));
    }

    /// Number of stored chunks with an index lower than `index`.
    /// This is the position of chunk `index` inside `chunk_sizes`
    /// (bitmap order), whether or not it is stored itself.
    fn stored_before(&self, index: u32) -> usize {
        (0..index).filter(|&i| self.has_chunk(i)).count()
    }

    /// Byte offset into `chunks_data` where the chunk at `stored_idx` starts.
    fn offset_of_stored(&self, stored_idx: usize) -> usize {
        self.chunk_sizes[..stored_idx]
            .iter()
            .map(|&s| s as usize)
            .sum()
    }

    /// Returns (`byte_offset`, `size`, `stored_index`) for a stored chunk.
    /// Returns `None` if the chunk is not stored or the region data is
    /// internally inconsistent (defensive against corrupted input).
    fn chunk_info(&self, index: u32) -> Option<(usize, u32, usize)> {
        if !self.has_chunk(index) {
            return None;
        }
        let stored_idx = self.stored_before(index);
        let size = *self.chunk_sizes.get(stored_idx)?;
        let offset = self.offset_of_stored(stored_idx);
        if offset + size as usize > self.chunks_data.len() {
            return None;
        }
        Some((offset, size, stored_idx))
    }

    /// Get a chunk's raw NBT bytes by its region-relative index.
    /// Returns `None` if the chunk is not stored (pruned or missing).
    pub(crate) fn get_chunk_bytes(&self, index: u32) -> Option<Vec<u8>> {
        let (offset, size, _) = self.chunk_info(index)?;
        Some(self.chunks_data[offset..offset + size as usize].to_vec())
    }

    /// Insert or update a chunk.  Called during `update_chunk`.
    ///
    /// `chunk_sizes`/`chunks_data` are kept in bitmap (index) order, so new
    /// chunks must be spliced in at their ordered position — appending would
    /// desync the size table from the bitmap for every later lookup.
    pub(crate) fn upsert_chunk(&mut self, index: u32, raw_nbt: &[u8]) {
        let new_size = raw_nbt.len() as u32;

        if let Some((offset, old_size, stored_idx)) = self.chunk_info(index) {
            // Replace the existing data range in place.
            self.chunks_data
                .splice(offset..offset + old_size as usize, raw_nbt.iter().copied());
            self.chunk_sizes[stored_idx] = new_size;
        } else {
            // Insert at the bitmap-ordered position.
            let stored_idx = self.stored_before(index);
            let offset = self.offset_of_stored(stored_idx);
            self.set_chunk(index);
            self.chunk_sizes.insert(stored_idx, new_size);
            self.chunks_data
                .splice(offset..offset, raw_nbt.iter().copied());
        }
    }

    /// Remove a stored chunk (used by the `ChunkPruner` so an emptied chunk
    /// does not resurrect its old contents on the next load).
    /// Returns `true` if the chunk existed.
    pub(crate) fn remove_chunk(&mut self, index: u32) -> bool {
        match self.chunk_info(index) {
            Some((offset, size, stored_idx)) => {
                self.chunks_data.drain(offset..offset + size as usize);
                self.chunk_sizes.remove(stored_idx);
                self.clear_chunk(index);
                true
            }
            None => false,
        }
    }

    /// Number of chunks currently stored.
    const fn stored_count(&self) -> usize {
        self.chunk_sizes.len()
    }

    /// Byte spans `(offset, size)` of every stored chunk, indexed by
    /// region-relative chunk index. One O(1024) prefix-sum pass replaces the
    /// O(N)-per-lookup scans of `stored_before`/`offset_of_stored` on
    /// read-heavy paths (batch loads, template building).
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
    data: EasyRegionData,
    /// Set on the first actual mutation; a clean region skips the
    /// whole-region recompress + rewrite entirely on flush.
    dirty: std::sync::atomic::AtomicBool,
    _phantom: PhantomData<D>,
}

impl<D> Default for EasyWorldFile<D> {
    fn default() -> Self {
        Self {
            data: EasyRegionData::new(0, 0),
            dirty: std::sync::atomic::AtomicBool::new(false),
            _phantom: PhantomData,
        }
    }
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

        let serialized = postcard::to_allocvec(&self.data)
            .map_err(|e| std::io::Error::other(format!("postcard serialize: {e}")))?;
        let raw_len = serialized.len();

        // Region compression is CPU-bound — keep it off the async workers.
        let compressed = tokio::task::spawn_blocking(move || {
            compress_to_vec(&*serialized, CompressionLevel::Default)
        })
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))?;
        debug!(
            "EasyWorld v2: {} chunks → {} B raw → {} B zstd for {}",
            self.data.stored_count(),
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
        let data = decode_region_bytes(&r)?;
        Ok(Self {
            data,
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
        self.data.region_x = x >> 5;
        self.data.region_z = z >> 5;
        let rel_x = x.rem_euclid(32);
        let rel_z = z.rem_euclid(32);
        let index = (rel_x + rel_z * 32) as u32;

        // ChunkPruner: skip chunks that are entirely air with no block entities.
        // We downcast via Any to check the concrete type.  This only applies when
        // Data = ChunkData; for ChunkEntityData the check is a no-op.
        let should_skip = Self::try_prune(chunk_data);

        if should_skip {
            trace!("EasyWorld: pruning empty chunk ({x},{z}) index {index}");
            // Remove any previously stored version, otherwise a chunk that was
            // mined out to all air would resurrect its old contents on reload.
            if self.data.remove_chunk(index) {
                self.dirty.store(true, std::sync::atomic::Ordering::Release);
            }
            return Ok(());
        }

        let bytes = chunk_data
            .to_bytes()
            .await
            .map_err(|e| ChunkWritingError::ChunkSerializingError(e.to_string()))?;

        self.data.upsert_chunk(index, &bytes);
        self.dirty.store(true, std::sync::atomic::Ordering::Release);

        Ok(())
    }

    async fn get_chunks(
        &self,
        chunks: Vec<Vector2<i32>>,
        stream: tokio::sync::mpsc::Sender<LoadedData<Self::Data, ChunkReadingError>>,
    ) {
        // One O(1024) pass, then O(1) per requested chunk.
        let spans = self.data.chunk_spans();
        for pos in chunks {
            let rel_x = pos.x.rem_euclid(32);
            let rel_z = pos.y.rem_euclid(32);
            let index = (rel_x + rel_z * 32) as usize;

            if let Some((offset, size)) = spans[index] {
                let bytes = Bytes::copy_from_slice(&self.data.chunks_data[offset..offset + size]);
                match D::from_bytes(&bytes, pos) {
                    Ok(data) => {
                        let _ = stream.send(LoadedData::Loaded(data)).await;
                    }
                    Err(e) => {
                        let _ = stream.send(LoadedData::Error((pos, e))).await;
                    }
                }
            } else {
                let _ = stream.send(LoadedData::Missing(pos)).await;
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
    use super::EasyRegionData;

    #[test]
    fn upsert_out_of_order_keeps_bitmap_order() {
        let mut r = EasyRegionData::new(0, 0);
        // Insert with descending indices: sizes/data must stay in bitmap order.
        r.upsert_chunk(900, &[9u8; 10]);
        r.upsert_chunk(300, &[3u8; 7]);
        r.upsert_chunk(5, &[5u8; 4]);
        assert!(r.is_consistent());
        assert_eq!(r.get_chunk_bytes(5).unwrap(), vec![5u8; 4]);
        assert_eq!(r.get_chunk_bytes(300).unwrap(), vec![3u8; 7]);
        assert_eq!(r.get_chunk_bytes(900).unwrap(), vec![9u8; 10]);
        assert!(r.get_chunk_bytes(0).is_none());
        assert!(r.get_chunk_bytes(1023).is_none());
    }

    #[test]
    fn upsert_existing_resizes_in_place() {
        let mut r = EasyRegionData::new(0, 0);
        r.upsert_chunk(10, &[1u8; 8]);
        r.upsert_chunk(20, &[2u8; 8]);
        // Grow the first chunk, shrink the second: neighbours must survive.
        r.upsert_chunk(10, &[7u8; 20]);
        r.upsert_chunk(20, &[8u8; 2]);
        assert!(r.is_consistent());
        assert_eq!(r.get_chunk_bytes(10).unwrap(), vec![7u8; 20]);
        assert_eq!(r.get_chunk_bytes(20).unwrap(), vec![8u8; 2]);
    }

    #[test]
    fn remove_chunk_shifts_later_chunks() {
        let mut r = EasyRegionData::new(0, 0);
        r.upsert_chunk(1, &[1u8; 3]);
        r.upsert_chunk(2, &[2u8; 5]);
        r.upsert_chunk(3, &[3u8; 7]);
        assert!(r.remove_chunk(2));
        assert!(!r.remove_chunk(2)); // already gone
        assert!(r.is_consistent());
        assert!(r.get_chunk_bytes(2).is_none());
        assert_eq!(r.get_chunk_bytes(1).unwrap(), vec![1u8; 3]);
        assert_eq!(r.get_chunk_bytes(3).unwrap(), vec![3u8; 7]);
    }

    #[test]
    fn postcard_roundtrip() {
        let mut r = EasyRegionData::new(-3, 7);
        r.upsert_chunk(0, &[42u8; 16]);
        r.upsert_chunk(1023, &[43u8; 32]);
        let bytes = postcard::to_allocvec(&r).unwrap();
        let back: EasyRegionData = postcard::from_bytes(&bytes).unwrap();
        assert!(back.is_consistent());
        assert_eq!(back.region_x, -3);
        assert_eq!(back.region_z, 7);
        assert_eq!(back.get_chunk_bytes(0).unwrap(), vec![42u8; 16]);
        assert_eq!(back.get_chunk_bytes(1023).unwrap(), vec![43u8; 32]);
    }

    #[test]
    fn spans_agree_with_lookup() {
        let mut r = EasyRegionData::new(0, 0);
        r.upsert_chunk(900, &[9u8; 10]);
        r.upsert_chunk(300, &[3u8; 7]);
        r.upsert_chunk(5, &[5u8; 4]);
        let spans = r.chunk_spans();
        for i in 0..1024u32 {
            match (spans[i as usize], r.get_chunk_bytes(i)) {
                (Some((offset, size)), Some(bytes)) => {
                    assert_eq!(&r.chunks_data[offset..offset + size], &bytes[..]);
                }
                (None, None) => {}
                _ => panic!("span/lookup disagree at index {i}"),
            }
        }
        let stored: Vec<u32> = r.stored_chunks().map(|(i, _)| i).collect();
        assert_eq!(stored, vec![5, 300, 900]);
    }

    #[test]
    fn corrupted_data_is_rejected_not_panicking() {
        let mut r = EasyRegionData::new(0, 0);
        r.upsert_chunk(4, &[1u8; 4]);
        // Corrupt: drop the size table entry while the bit stays set.
        r.chunk_sizes.clear();
        assert!(!r.is_consistent());
        assert!(r.get_chunk_bytes(4).is_none()); // must not panic
    }
}
