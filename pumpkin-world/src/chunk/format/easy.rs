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

    /// Get a chunk's raw NBT bytes by its region-relative index.
    /// Returns `None` if the chunk is not stored (pruned or missing).
    pub(crate) fn get_chunk_bytes(&self, index: u32) -> Option<Vec<u8>> {
        if !self.has_chunk(index) {
            return None;
        }
        // Count how many stored chunks come before this one.
        let mut offset: usize = 0;
        let mut stored_idx: usize = 0;
        for i in 0..index {
            if self.has_chunk(i) {
                offset += self.chunk_sizes[stored_idx] as usize;
                stored_idx += 1;
            }
        }
        let size = self.chunk_sizes[stored_idx] as usize;
        Some(self.chunks_data[offset..offset + size].to_vec())
    }

    /// Insert or update a chunk.  Called during `update_chunk`.
    pub(crate) fn upsert_chunk(&mut self, index: u32, raw_nbt: &[u8]) {
        let new_size = raw_nbt.len() as u32;

        // If the chunk already exists, remove its old data.
        if self.has_chunk(index) {
            // Find its offset and size, then splice it out.
            let (old_offset, old_size, stored_idx) = self.chunk_info(index);
            // Remove old data range.
            let new_data_len = self.chunks_data.len() - old_size as usize + new_size as usize;
            let mut new_data = Vec::with_capacity(new_data_len);
            new_data.extend_from_slice(&self.chunks_data[..old_offset]);
            new_data.extend_from_slice(raw_nbt);
            new_data.extend_from_slice(&self.chunks_data[old_offset + old_size as usize..]);
            self.chunks_data = new_data;
            self.chunk_sizes[stored_idx] = new_size;
        } else {
            // Append new chunk at the end.
            self.set_chunk(index);
            self.chunk_sizes.push(new_size);
            self.chunks_data.extend_from_slice(raw_nbt);
        }
    }

    /// Returns (`byte_offset`, `size`, `stored_index`) for an existing chunk.
    fn chunk_info(&self, index: u32) -> (usize, u32, usize) {
        let mut offset: usize = 0;
        let mut stored_idx: usize = 0;
        for i in 0..index {
            if self.has_chunk(i) {
                offset += self.chunk_sizes[stored_idx] as usize;
                stored_idx += 1;
            }
        }
        let size = self.chunk_sizes[stored_idx];
        (offset, size, stored_idx)
    }

    /// Number of chunks currently stored.
    const fn stored_count(&self) -> usize {
        self.chunk_sizes.len()
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

// ─── ChunkSerializer implementation ────────────────────────────────────

pub struct EasyWorldFile<D> {
    data: EasyRegionData,
    _phantom: PhantomData<D>,
}

impl<D> Default for EasyWorldFile<D> {
    fn default() -> Self {
        Self {
            data: EasyRegionData::new(0, 0),
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

    fn should_write(&self, _is_watched: bool) -> bool {
        true
    }

    async fn write(&self, backend: &Self::WriteBackend) -> Result<(), std::io::Error> {
        let serialized = postcard::to_allocvec(&self.data)
            .map_err(|e| std::io::Error::other(format!("postcard serialize: {e}")))?;

        let compressed = compress_to_vec(&*serialized, CompressionLevel::Default);
        debug!(
            "EasyWorld v2: {} chunks → {} B raw → {} B zstd for {}",
            self.data.stored_count(),
            serialized.len(),
            compressed.len(),
            backend.display(),
        );

        tokio::fs::write(backend, compressed).await
    }

    fn read(r: Bytes) -> Result<Self, ChunkReadingError> {
        let mut decoder = StreamingDecoder::new(&r[..]).map_err(|e| {
            ChunkReadingError::Compression(crate::chunk::CompressionError::ZstdError(
                std::io::Error::other(e.to_string()),
            ))
        })?;
        let mut decompressed = Vec::new();
        std::io::Read::read_to_end(&mut decoder, &mut decompressed)
            .map_err(ChunkReadingError::IoError)?;

        let data: EasyRegionData = postcard::from_bytes(&decompressed).map_err(|e| {
            ChunkReadingError::ParsingError(
                crate::chunk::ChunkParsingError::ErrorDeserializingChunk(e.to_string()),
            )
        })?;

        if data.magic != EASY_MAGIC {
            return Err(ChunkReadingError::InvalidHeader);
        }

        Ok(Self {
            data,
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
            // Don't store — if it existed before, we leave it.  On next write it'll
            // disappear because we didn't update it.  For now, just skip.
            return Ok(());
        }

        let bytes = chunk_data
            .to_bytes()
            .await
            .map_err(|e| ChunkWritingError::ChunkSerializingError(e.to_string()))?;

        self.data.upsert_chunk(index, &bytes);

        Ok(())
    }

    async fn get_chunks(
        &self,
        chunks: Vec<Vector2<i32>>,
        stream: tokio::sync::mpsc::Sender<LoadedData<Self::Data, ChunkReadingError>>,
    ) {
        for pos in chunks {
            let rel_x = pos.x.rem_euclid(32);
            let rel_z = pos.y.rem_euclid(32);
            let index = (rel_x + rel_z * 32) as u32;

            if let Some(raw_bytes) = self.data.get_chunk_bytes(index) {
                let bytes = Bytes::from(raw_bytes);
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
    /// Uses `Any` downcasting so this compiles for both `ChunkData` and `ChunkEntityData`.
    fn try_prune(chunk_data: &D) -> bool {
        // SAFETY: We only downcast to ChunkData; if D is ChunkEntityData, this is a no-op.
        let any = chunk_data as &dyn std::any::Any;
        if let Some(chunk) = any.downcast_ref::<crate::chunk::ChunkData>() {
            return is_prunable_chunk(chunk);
        }
        // For ChunkEntityData, never prune (entities are always meaningful).
        false
    }
}
