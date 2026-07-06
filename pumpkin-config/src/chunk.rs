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
    /// EasyWorld region-level zstd compressed format (.easy files).
    #[serde(rename = "easy")]
    Easy,
    /// EasyWorld format stored in MySQL database.
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
/// Configuration for EasyWorld MySQL storage.
#[derive(Deserialize, Serialize, Clone)]
pub struct EasyMysqlConfig {
    /// MySQL connection URL, e.g. "mysql://user:pass@localhost:3306/ember"
    pub url: String,
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
