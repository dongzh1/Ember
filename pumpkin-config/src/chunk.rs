use std::str;

use serde::{Deserialize, Serialize};

/// Configuration for chunk storage format.
///
/// Supports the upstream `Anvil`, `Linear` and `Pump` formats, plus Ember's
/// `Easy` (the default), which stores worlds as region-level zstd either on
/// disk or in `MySQL` — the backend is the only knob most operators need.
#[derive(Deserialize, Serialize, Clone)]
#[serde(tag = "type")]
pub enum ChunkConfig {
    /// Standard Anvil chunk storage.
    #[serde(rename = "anvil")]
    Anvil(AnvilChunkConfig),
    /// Linear chunk storage format.
    #[serde(rename = "linear")]
    Linear,
    /// Pumpkin's own optimized world format.
    #[serde(rename = "pump")]
    Pump,
    // EMBER start - easyworld format
    /// `EasyWorld`: region-level zstd with empty-chunk pruning and atomic
    /// writes. Ember's default. One format, two backends (file / `MySQL`).
    /// Per-world behaviour (size limit, generation, read-only, clone source)
    /// lives in the world's `ember-world.toml` sidecar, not here.
    #[serde(rename = "easy")]
    Easy(EasyConfig),
    // EMBER end
}

// EMBER: Easy is the default chunk format.
impl Default for ChunkConfig {
    fn default() -> Self {
        Self::Easy(EasyConfig::default())
    }
}

// EMBER start - unified easy config
/// Configuration for the `EasyWorld` format.
#[derive(Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct EasyConfig {
    /// Where chunk data is stored: on disk (`file`, default) or in a shared
    /// `MySQL` database (`mysql`).
    pub backend: EasyBackend,
    /// `MySQL` connection URL (backend = `mysql` only),
    /// e.g. `mysql://user:pass@localhost:3306/ember`.
    pub url: String,
    /// Optional namespace prepended to every database world key
    /// (`<key_prefix>/<world folder path>`). Servers sharing one database
    /// must use the same prefix and the same relative world folder layout.
    pub key_prefix: String,
    /// Maximum number of decompressed regions kept resident per world (LRU),
    /// for the `mysql` backend. Default 32.
    #[serde(default = "default_max_cached_regions")]
    pub max_cached_regions: usize,
}

// A manual `Default` (not derived) so the config-merge base — built from
// `Default::default()`, not from a deserialized file — carries the documented
// `max_cached_regions = 32`. A derived Default would yield 0 (usize default),
// which the merge then persists and clamps to 4, silently giving globally
// configured mysql worlds an 8x-smaller region cache than the sidecar path.
impl Default for EasyConfig {
    fn default() -> Self {
        Self {
            backend: EasyBackend::default(),
            url: String::new(),
            key_prefix: String::new(),
            max_cached_regions: default_max_cached_regions(),
        }
    }
}

/// Storage backend for the `EasyWorld` format.
#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum EasyBackend {
    /// On-disk `.easy` region files (default).
    #[default]
    #[serde(rename = "file")]
    File,
    /// Region data stored in a shared `MySQL` database.
    #[serde(rename = "mysql")]
    Mysql,
}

impl EasyConfig {
    /// Builds the `MySQL` connection parameters for this world, with the
    /// per-world access `mode` resolved from the sidecar.
    #[must_use]
    pub fn mysql(&self, mode: EasyWorldMode) -> EasyMysqlConfig {
        EasyMysqlConfig {
            url: self.url.clone(),
            mode,
            key_prefix: self.key_prefix.clone(),
            max_cached_regions: self.max_cached_regions,
        }
    }
}
// EMBER end

/// Configuration for Anvil chunk storage.
#[derive(Deserialize, Serialize, Default, Clone)]
#[serde(default)]
pub struct AnvilChunkConfig {
    /// Compression settings for chunk data.
    pub compression: ChunkCompression,
    /// Whether chunks should be written in place.
    pub write_in_place: bool,
}

// EMBER start - easyworld mysql config
/// Configuration for `EasyWorld` `MySQL` storage.
///
/// Like `SlimeWorldManager`: any number of servers may load a world in
/// `read_only` mode, but only one server at a time may hold it in
/// `read_write` mode. The writer is protected by a heartbeat lock in the
/// database; a second writer automatically degrades to read-only.
#[derive(Deserialize, Serialize, Clone)]
pub struct EasyMysqlConfig {
    /// `MySQL` connection URL, e.g. `mysql://user:pass@localhost:3306/ember`
    pub url: String,
    /// Access mode: `read_write` (default, takes the world lock) or
    /// `read_only` (never writes; safe on any number of servers).
    #[serde(default)]
    pub mode: EasyWorldMode,
    /// Optional namespace prepended to every database world key
    /// (`<key_prefix>/<world folder path>`). Servers sharing one database
    /// must use the same prefix and the same relative world folder layout
    /// (the default `world` layout qualifies).
    #[serde(default)]
    pub key_prefix: String,
    /// Maximum number of decompressed regions kept resident per world
    /// (LRU). A dense region can take tens of MB of memory; lower this on
    /// small servers, raise it for many concurrent players. Default 32.
    #[serde(default = "default_max_cached_regions")]
    pub max_cached_regions: usize,
}

const fn default_max_cached_regions() -> usize {
    32
}

/// Access mode for a shared `EasyWorld` database.
#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum EasyWorldMode {
    /// Full access: loads, generates, and saves chunks. Requires the
    /// world lock; only one `read_write` server per database at a time.
    #[default]
    #[serde(rename = "read_write")]
    ReadWrite,
    /// Never writes to the database. Chunks missing from storage are
    /// generated in memory and discarded on unload/shutdown.
    #[serde(rename = "read_only")]
    ReadOnly,
}
// EMBER end

/// Compression settings for chunk data.
#[derive(Deserialize, Serialize, Clone)]
pub struct ChunkCompression {
    /// Compression algorithm to use.
    pub algorithm: Compression,
    /// Compression level (algorithm-specific).
    pub level: u32,
}

impl Default for ChunkCompression {
    fn default() -> Self {
        Self {
            algorithm: Compression::LZ4,
            level: 6,
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Copy)]
pub enum Compression {
    /// `GZip` Compression.
    GZip,
    /// `ZLib` Compression.
    ZLib,
    /// LZ4 Compression (since 24w04a).
    LZ4,
    /// Custom compression algorithm (since 24w05a).
    Custom,
}
