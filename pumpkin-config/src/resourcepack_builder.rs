use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::LoadConfiguration;

// EMBER start - resource pack builder (self-generate + self-host/S3)
/// Builds a resource pack from a local folder and gets it in front of
/// clients, either by self-hosting it (a small built-in HTTP server) or by
/// uploading it to an S3-compatible bucket.
///
/// Layered on top of the existing `AdvancedConfiguration.resource_pack.java`
/// (manual external-URL) config, not a replacement for it: if this is
/// enabled, `Server::new` computes and overwrites that config's
/// `url`/`sha1`/`enabled` at startup; if disabled, manually configuring
/// that section for an already-externally-hosted pack still works exactly
/// as before.
#[derive(Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct ResourcePackBuilderConfig {
    pub enabled: bool,
    /// Folder holding the pack source (`pack.mcmeta` + `assets/...`),
    /// relative to the server's working directory.
    pub source_dir: String,
    pub hosting: HostingMode,
    pub self_hosted: SelfHostedConfig,
    pub s3: S3Config,
    /// Shown to the player when prompted to accept the pack.
    pub prompt_message: String,
    /// Whether players are forced to accept (matches
    /// `JavaResourcePackConfig::force`).
    pub force: bool,
}

impl Default for ResourcePackBuilderConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            source_dir: "resourcepack/source".to_string(),
            hosting: HostingMode::default(),
            self_hosted: SelfHostedConfig::default(),
            s3: S3Config::default(),
            prompt_message: String::new(),
            force: false,
        }
    }
}

impl LoadConfiguration for ResourcePackBuilderConfig {
    fn get_path() -> &'static Path {
        Path::new("resourcepack/resourcepack.toml")
    }

    fn validate(&self) {}
}

#[derive(Deserialize, Serialize, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostingMode {
    #[default]
    SelfHosted,
    S3,
}

/// Only consulted when `hosting = "self_hosted"`.
#[derive(Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct SelfHostedConfig {
    pub bind_addr: String,
    pub port: u16,
    /// Overrides the auto-built `http://<bind_addr>:<port>/pack-legacy.zip`
    /// URL for the legacy (pre-26.1) pack variant - needed whenever the
    /// server's public address differs from `bind_addr` (behind NAT/a
    /// reverse proxy/a domain name), which this process has no way to
    /// detect on its own.
    pub public_url: String,
    /// Same idea as `public_url`, but for the per-version 26.1+ variants -
    /// there are multiple of these (one per known 26.x release) so a single
    /// fixed URL can't stand in for all of them. Must contain the literal
    /// `{version}` placeholder (replaced with e.g. `26-1`/`26-2`); empty
    /// auto-builds `http://<bind_addr>:<port>/pack-{version}.zip`.
    pub public_url_modern: String,
}

impl Default for SelfHostedConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0".to_string(),
            port: 25566,
            public_url: String::new(),
            public_url_modern: String::new(),
        }
    }
}

/// Only consulted when `hosting = "s3"`. Works against any S3-compatible
/// provider (Cloudflare R2, `MinIO`, real AWS S3, ...), not just AWS.
#[derive(Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct S3Config {
    /// Custom endpoint for S3-compatible providers. Leave blank for real AWS
    /// S3 (the endpoint is derived from `region` in that case).
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    pub object_key: String,
    /// Prefixes the final download URL sent to players - the bucket's own
    /// public endpoint, or a CDN/custom domain placed in front of it.
    pub public_url_base: String,
}

impl Default for S3Config {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            bucket: String::new(),
            region: String::new(),
            access_key: String::new(),
            secret_key: String::new(),
            object_key: "resourcepack.zip".to_string(),
            public_url_base: String::new(),
        }
    }
}
// EMBER end
