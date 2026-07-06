use std::str;

use serde::{Deserialize, Serialize};

/// Configuration for chunk storage format.
///
/// Supports multiple chunk formats, currently `Anvil`, `Linear`, `Pump`,
/// `Easy`, and `EasyMysql`.
#[derive(Deserialize, Default, Serialize, Clone)]
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
    #[default]
    Pump,
    // EMBER start - easyworld format
    /// `EasyWorld` region-level zstd compressed format (.easy files).
    #[serde(rename = "easy")]
    Easy,
    /// `EasyWorld` format stored in `MySQL` database.
    #[serde(rename = "easy_mysql")]
    EasyMysql(EasyMysqlConfig),
    // EMBER end
}

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
}

/// Access mode for a shared `EasyWorld` database.
#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Default)]
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
