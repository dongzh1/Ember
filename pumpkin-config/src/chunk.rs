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
    /// `EasyWorld` shard format (.ezs files): every chunk group is its own
    /// zstd blob, so a single edit recompresses one group instead of the
    /// whole region. Best for write-heavy worlds (resource worlds).
    #[serde(rename = "easy_shard")]
    EasyShard(EasyShardConfig),
    /// `EasyWorld` ephemeral instance storage: any number of worlds share
    /// one immutable in-memory template; per-instance edits live in RAM and
    /// are discarded on unload. Best for dungeon/minigame instances.
    #[serde(rename = "easy_instance")]
    EasyInstance(EasyInstanceConfig),
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
    /// Maximum number of decompressed regions kept resident per world
    /// (LRU). A dense region can take tens of MB of memory; lower this on
    /// small servers, raise it for many concurrent players. Default 32.
    #[serde(default = "default_max_cached_regions")]
    pub max_cached_regions: usize,
}

const fn default_max_cached_regions() -> usize {
    32
}

/// Configuration for the `EasyWorld` shard format.
#[derive(Deserialize, Serialize, Clone, Copy)]
#[serde(default)]
pub struct EasyShardConfig {
    /// Number of chunks per compression unit. `1` (default) compresses each
    /// chunk on its own — cheapest writes, ideal for resource worlds.
    /// Larger groups (up to `1024` = whole region) trade write cost for a
    /// better compression ratio. Clamped to `1..=1024` at load time.
    pub group_chunks: u16,
}

impl Default for EasyShardConfig {
    fn default() -> Self {
        Self { group_chunks: 1 }
    }
}

/// Configuration for `EasyWorld` ephemeral instance storage.
#[derive(Deserialize, Serialize, Clone)]
pub struct EasyInstanceConfig {
    /// Template identifier. Instances created with the same identifier share
    /// one immutable in-memory copy of the template's region data.
    pub template: String,
    /// Where the template's region data is loaded from.
    pub source: InstanceTemplateSource,
}

/// Source of an `EasyWorld` instance template.
#[derive(Deserialize, Serialize, Clone)]
#[serde(tag = "kind")]
pub enum InstanceTemplateSource {
    /// A world folder containing `.easy` region files
    /// (`<path>/dimensions/<ns>/<name>/region/r.X.Z.easy`).
    #[serde(rename = "file")]
    File { path: String },
    /// An `EasyWorld` `MySQL` database; the world key is derived from
    /// `path` exactly like a live `easy_mysql` world.
    #[serde(rename = "mysql")]
    Mysql {
        path: String,
        #[serde(flatten)]
        config: EasyMysqlConfig,
    },
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
