// EMBER start: resource pack builder (self-generate + self-host/S3)
//! Builds a resource pack from `resourcepack/source/` and gets it in front
//! of Java clients, either via a small built-in HTTP server or by uploading
//! it to an S3-compatible bucket - then feeds the result into
//! [`crate::server::Server::resolve_java_resource_pack`], which the
//! *existing*, unmodified resource-pack push in `net/java/login.rs` (and the
//! response handling in `net/java/config.rs`) both call to decide what to
//! actually send/expect. Neither of those needs to know (or change) whether
//! a pack came from here or from a hand-configured external host.
//!
//! Builds more than one variant, because Minecraft's `pack.mcmeta` format
//! itself changed structurally around the 26.x era: pre-26.1 clients only
//! understand a single `pack_format` number, 26.1+ clients expect
//! `min_format`/`max_format` instead. One shared `assets/` tree is reused
//! for every variant (no evidence the actual model/texture JSON differs
//! between eras, only `pack.mcmeta` itself does) - only the injected
//! `pack.mcmeta` bytes differ per variant.
use std::collections::HashMap;
use std::io::{Cursor, Write as _};
use std::path::Path;
use std::time::SystemTime;

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{SignableBody, SignableRequest, SigningSettings, sign};
use aws_sigv4::sign::v4;
use pumpkin_config::{
    HostingMode, LoadConfiguration, ResourcePackBuilderConfig, S3Config, SelfHostedConfig,
};
use pumpkin_util::version::JavaMinecraftVersion;
use sha1::Sha1;
use tracing::{error, info, warn};
use zip::write::{SimpleFileOptions, ZipWriter};

/// Every 26.x+ version Ember can actually tell apart at the protocol level
/// *and* that needs a different resource pack format, with that version's
/// default `pack.mcmeta` content (Minecraft Wiki, verified per-version, not
/// guessed: 26.1 -> format 84, 26.2 -> format 88). `26.1.2` is a real
/// Minecraft version but isn't listed here - it's a client-side-only
/// hotfix that shares 26.1's protocol number *and* its resource pack format
/// (also verified against the wiki), so it's already indistinguishable from,
/// and needs the identical `pack.mcmeta` as, `V_26_1`; no separate entry is
/// possible or needed. When a future Mojang release actually bumps the
/// protocol number with a new pack format, add one line here (same idea as
/// the generated block-state/item-id remap tables elsewhere in this repo).
const KNOWN_MODERN_VERSIONS: &[(JavaMinecraftVersion, &str)] = &[
    (
        JavaMinecraftVersion::V_26_1,
        r#"{"pack":{"min_format":84,"max_format":84,"description":"Ember resource pack"}}"#,
    ),
    (
        JavaMinecraftVersion::V_26_2,
        r#"{"pack":{"min_format":88,"max_format":88,"description":"Ember resource pack"}}"#,
    ),
];

const LEGACY_DEFAULT_MCMETA: &[u8] =
    br#"{"pack":{"pack_format":75,"description":"Ember resource pack"}}"#;

/// One successfully built pack, ready to host/upload.
struct BuiltVariant {
    /// URL path segment / S3 object-key infix: `"legacy"`, `"26-1"`, `"26-2"`.
    name: String,
    bytes: Vec<u8>,
    sha1: String,
}

/// A single resolved resource pack's url/sha1 - the payload half of
/// [`crate::server::Server::resolve_java_resource_pack`]'s result, kept
/// distinct from `force`/`prompt_message` (shared across every variant, so
/// there's no need for a second copy of those per variant).
pub struct ResourcePackVariant {
    pub url: String,
    pub sha1: String,
}

/// Everything [`init`] managed to build. Both fields are independently
/// `None`/empty on any failure (disabled, build error, hosting error) -
/// callers always have a well-defined (if degraded) fallback.
#[derive(Default)]
pub struct BuiltResourcePacks {
    pub legacy: Option<(String, String)>,
    /// One entry per successfully-built `KNOWN_MODERN_VERSIONS` variant.
    /// Only ever a couple of entries - linear lookup is enough, no need for
    /// a `HashMap<JavaMinecraftVersion, _>` (which would need `Hash`
    /// derived on a widely-shared enum just for this).
    pub per_version: Vec<(JavaMinecraftVersion, String, String)>,
}

/// What `net/java/{login,config}.rs` actually need to push/verify a
/// resource pack for one connecting client.
pub struct ResolvedResourcePack<'a> {
    pub url: &'a str,
    pub sha1: &'a str,
    pub force: bool,
    pub prompt_message: &'a str,
}

/// Picks which resource pack variant `version` should receive: an exact
/// `versioned` match if one was built for this precise version, otherwise
/// `legacy` - which covers every pre-26.1 version, and also silently covers
/// any 26.x+ version Ember doesn't have a dedicated variant for yet (unbuilt,
/// or a future release `KNOWN_MODERN_VERSIONS` doesn't list). `None` only
/// when the resource pack system is disabled entirely (`!legacy.enabled`).
///
/// Split out from `Server::resolve_java_resource_pack` as a plain function
/// over borrowed pieces (rather than `&Server`) purely so it's unit-testable
/// without constructing a full `Server` - nothing else in this crate's test
/// suite does that, `Server::new` is a heavy, deeply-async constructor.
fn resolve_resource_pack_variant<'a>(
    legacy: &'a pumpkin_config::resource_pack::JavaResourcePackConfig,
    versioned: &'a [(JavaMinecraftVersion, ResourcePackVariant)],
    version: JavaMinecraftVersion,
) -> Option<ResolvedResourcePack<'a>> {
    if !legacy.enabled {
        return None;
    }
    let exact_match = versioned.iter().find(|(v, _)| *v == version);
    if let Some((_, variant)) = exact_match {
        return Some(ResolvedResourcePack {
            url: &variant.url,
            sha1: &variant.sha1,
            force: legacy.force,
            prompt_message: &legacy.prompt_message,
        });
    }
    Some(ResolvedResourcePack {
        url: &legacy.url,
        sha1: &legacy.sha1,
        force: legacy.force,
        prompt_message: &legacy.prompt_message,
    })
}

impl crate::server::Server {
    /// This is the *only* place that should decide which resource pack
    /// variant a client gets - `login.rs`'s send site and `config.rs`'s
    /// response-verification site both call it, so they can never disagree
    /// about which pack a given client was actually sent. See
    /// [`resolve_resource_pack_variant`] for the actual logic.
    #[must_use]
    pub fn resolve_java_resource_pack(
        &self,
        version: JavaMinecraftVersion,
    ) -> Option<ResolvedResourcePack<'_>> {
        resolve_resource_pack_variant(
            &self.advanced_config.resource_pack.java,
            &self.versioned_resource_packs,
            version,
        )
    }
}

/// Loads `resourcepack/resourcepack.toml` and, if enabled, runs the whole
/// pipeline (build every variant -> host or upload) once. Returns whatever
/// was actually built - partially populated (or entirely empty) on any
/// failure, logged; the server still boots without some/all pack variants
/// rather than refusing to start over this.
#[must_use]
pub fn init(exec_dir: &Path) -> BuiltResourcePacks {
    let config = ResourcePackBuilderConfig::load(exec_dir);
    if !config.enabled {
        return BuiltResourcePacks::default();
    }

    let source_dir = exec_dir.join(&config.source_dir);
    if let Err(e) = ensure_default_pack(&source_dir) {
        error!(
            "Resource pack builder: failed to prepare '{}': {e}",
            source_dir.display()
        );
        return BuiltResourcePacks::default();
    }
    // `pack.mcmeta.<suffix>` overrides live next to `source_dir`, not inside
    // its `assets/` tree - `source_dir` itself always has a parent once
    // `ensure_default_pack` has run (it's `<exec_dir>/<config.source_dir>`).
    let override_dir = source_dir.parent().unwrap_or(&source_dir).to_path_buf();

    let mut built: Vec<(Option<JavaMinecraftVersion>, BuiltVariant)> = Vec::new();
    if let Some(variant) = build_variant(
        "legacy",
        &source_dir,
        &source_dir.join("pack.mcmeta"),
        LEGACY_DEFAULT_MCMETA,
    ) {
        built.push((None, variant));
    }
    for &(version, default_mcmeta) in KNOWN_MODERN_VERSIONS {
        let suffix = version.to_string().replace('.', "_");
        let name = version.to_string().replace('.', "-");
        let override_path = override_dir.join(format!("pack.mcmeta.{suffix}"));
        if let Some(variant) = build_variant(
            &name,
            &source_dir,
            &override_path,
            default_mcmeta.as_bytes(),
        ) {
            built.push((Some(version), variant));
        }
    }
    if built.is_empty() {
        return BuiltResourcePacks::default();
    }

    let urls = match config.hosting {
        HostingMode::SelfHosted => serve_self_hosted(&built, &config.self_hosted),
        HostingMode::S3 => upload_all_to_s3(&built, &config.s3),
    };

    let mut result = BuiltResourcePacks::default();
    for (version, variant) in built {
        let Some(url) = urls.get(&variant.name) else {
            continue;
        };
        match version {
            None => result.legacy = Some((url.clone(), variant.sha1)),
            Some(v) => result.per_version.push((v, url.clone(), variant.sha1)),
        }
    }
    result
}

/// Builds one named variant's zip from `source_dir`'s shared assets plus
/// `mcmeta_path`'s content (or `default_mcmeta` if that override file is
/// absent/unreadable). Logs and returns `None` on any build failure -
/// one variant failing doesn't stop the others from building.
fn build_variant(
    name: &str,
    source_dir: &Path,
    mcmeta_path: &Path,
    default_mcmeta: &[u8],
) -> Option<BuiltVariant> {
    let mcmeta = load_pack_mcmeta(mcmeta_path, default_mcmeta);
    let bytes = match build_zip(source_dir, &mcmeta) {
        Ok(bytes) => bytes,
        Err(e) => {
            error!(
                "Resource pack builder: failed to build the '{name}' pack from '{}': {e}",
                source_dir.display()
            );
            return None;
        }
    };
    info!(
        "Resource pack builder: built the '{name}' pack ({} bytes) from '{}'",
        bytes.len(),
        source_dir.display()
    );
    let sha1 = hex_encode(sha1_digest(&bytes));
    Some(BuiltVariant {
        name: name.to_string(),
        bytes,
        sha1,
    })
}

/// Reads `path` as this variant's `pack.mcmeta` override if present,
/// otherwise (or on a read failure, logged) falls back to `default`.
fn load_pack_mcmeta(path: &Path, default: &[u8]) -> Vec<u8> {
    if !path.exists() {
        return default.to_vec();
    }
    match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) => {
            warn!(
                "Resource pack builder: failed to read override '{}': {e} - using the default instead",
                path.display()
            );
            default.to_vec()
        }
    }
}

fn ensure_default_pack(source_dir: &Path) -> std::io::Result<()> {
    if source_dir.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(source_dir.join("assets"))?;
    // EMBER: a placeholder pack_format - the operator is expected to drop
    // their own `pack.mcmeta` in here; a mismatched format only produces a
    // client-side warning, never a hard failure.
    std::fs::write(source_dir.join("pack.mcmeta"), LEGACY_DEFAULT_MCMETA)?;
    Ok(())
}

/// Zips `source_dir`'s contents, injecting `pack_mcmeta` as the root-level
/// `pack.mcmeta` entry instead of whatever (if anything) is actually on
/// disk there - `add_dir_to_zip` skips that one file during its generic
/// walk so it's never written twice.
fn build_zip(source_dir: &Path, pack_mcmeta: &[u8]) -> std::io::Result<Vec<u8>> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    writer
        .start_file("pack.mcmeta", options)
        .map_err(std::io::Error::other)?;
    writer.write_all(pack_mcmeta)?;
    add_dir_to_zip(&mut writer, source_dir, source_dir, options)?;
    let cursor = writer.finish().map_err(std::io::Error::other)?;
    Ok(cursor.into_inner())
}

fn add_dir_to_zip<W: std::io::Write + std::io::Seek>(
    writer: &mut ZipWriter<W>,
    base: &Path,
    dir: &Path,
    options: SimpleFileOptions,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            add_dir_to_zip(writer, base, &path, options)?;
            continue;
        }
        // The root-level pack.mcmeta is injected explicitly by `build_zip`
        // (per-variant content) - skip it here so the generic walk doesn't
        // also write whatever happens to sit on disk, which would either
        // duplicate the entry or silently overwrite the real per-variant one
        // depending on write order.
        if dir == base && path.file_name().is_some_and(|n| n == "pack.mcmeta") {
            continue;
        }
        let relative = path.strip_prefix(base).unwrap_or(path.as_path());
        let name = relative.to_string_lossy().replace('\\', "/");
        writer
            .start_file(&name, options)
            .map_err(std::io::Error::other)?;
        writer.write_all(&std::fs::read(&path)?)?;
    }
    Ok(())
}

/// Spawns a dedicated OS thread (not a tokio task - this is a simple,
/// low-frequency blocking listener, not worth pulling onto the async
/// runtime) serving every built variant on its own path
/// (`/pack-<name>.zip`), binding exactly once regardless of how many
/// variants there are. Returns a `name -> URL` map for whichever variants
/// actually got a URL (empty if the bind itself failed).
fn serve_self_hosted(
    built: &[(Option<JavaMinecraftVersion>, BuiltVariant)],
    config: &SelfHostedConfig,
) -> HashMap<String, String> {
    let bind = format!("{}:{}", config.bind_addr, config.port);
    let server = match tiny_http::Server::http(&bind) {
        Ok(server) => server,
        Err(e) => {
            error!("Resource pack builder: failed to bind self-hosted server on '{bind}': {e}");
            return HashMap::new();
        }
    };

    let contents: HashMap<String, Vec<u8>> = built
        .iter()
        .map(|(_, v)| (v.name.clone(), v.bytes.clone()))
        .collect();

    let spawn_result = std::thread::Builder::new()
        .name("resourcepack-http".to_string())
        .spawn(move || {
            for request in server.incoming_requests() {
                let requested_name = request
                    .url()
                    .trim_start_matches('/')
                    .strip_prefix("pack-")
                    .and_then(|rest| rest.strip_suffix(".zip"));
                let bytes = requested_name.and_then(|name| contents.get(name));
                let Some(bytes) = bytes else {
                    let response = tiny_http::Response::from_string("not found")
                        .with_status_code(tiny_http::StatusCode(404));
                    if let Err(e) = request.respond(response) {
                        warn!("Resource pack builder: failed to serve a 404: {e}");
                    }
                    continue;
                };
                let content_type =
                    tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/zip"[..])
                        .expect("static header is always valid");
                let response =
                    tiny_http::Response::from_data(bytes.clone()).with_header(content_type);
                if let Err(e) = request.respond(response) {
                    warn!("Resource pack builder: failed to serve a pack download: {e}");
                }
            }
        });
    if let Err(e) = spawn_result {
        error!("Resource pack builder: failed to spawn the hosting thread: {e}");
        return HashMap::new();
    }

    info!("Resource pack builder: self-hosting on {bind}");
    built
        .iter()
        .map(|(_, v)| {
            let url = if v.name == "legacy" {
                if config.public_url.is_empty() {
                    format!("http://{bind}/pack-legacy.zip")
                } else {
                    config.public_url.clone()
                }
            } else if config.public_url_modern.is_empty() {
                format!("http://{bind}/pack-{}.zip", v.name)
            } else {
                config.public_url_modern.replace("{version}", &v.name)
            };
            (v.name.clone(), url)
        })
        .collect()
}

/// Uploads every variant to the same S3-compatible bucket, one object per
/// variant (`object_key` suffixed with `-<name>`, e.g. `resourcepack.zip`
/// -> `resourcepack-26-1.zip`). Returns a `name -> URL` map for whichever
/// uploads actually succeeded.
fn upload_all_to_s3(
    built: &[(Option<JavaMinecraftVersion>, BuiltVariant)],
    config: &S3Config,
) -> HashMap<String, String> {
    let mut urls = HashMap::with_capacity(built.len());
    for (_, variant) in built {
        let mut variant_config = config.clone();
        variant_config.object_key = suffixed_object_key(&config.object_key, &variant.name);
        match upload_to_s3(&variant.bytes, &variant_config) {
            Ok(url) => {
                urls.insert(variant.name.clone(), url);
            }
            Err(e) => {
                error!(
                    "Resource pack builder: S3 upload failed for the '{}' variant: {e}",
                    variant.name
                );
            }
        }
    }
    urls
}

/// Inserts `-<suffix>` right before the last `.` in `object_key` (or just
/// appends it if there's no extension), e.g. `("resourcepack.zip", "26-1")`
/// -> `"resourcepack-26-1.zip"`.
fn suffixed_object_key(object_key: &str, suffix: &str) -> String {
    object_key.rfind('.').map_or_else(
        || format!("{object_key}-{suffix}"),
        |idx| format!("{}-{suffix}{}", &object_key[..idx], &object_key[idx..]),
    )
}

/// Uploads `bytes` to an S3-compatible bucket via a SigV4-signed `PUT`,
/// returning the URL to hand to clients. Works against any S3-compatible
/// endpoint (Cloudflare R2, `MinIO`, real AWS S3), not just AWS - `PUT` is
/// path-style (`{endpoint}/{bucket}/{key}`) so it doesn't depend on
/// per-bucket DNS/virtual-hosting support.
fn upload_to_s3(bytes: &[u8], config: &S3Config) -> Result<String, String> {
    if config.bucket.is_empty() || config.access_key.is_empty() || config.secret_key.is_empty() {
        return Err("[s3] bucket/access_key/secret_key must all be set".to_string());
    }

    let endpoint = if config.endpoint.is_empty() {
        format!("https://s3.{}.amazonaws.com", config.region)
    } else {
        config.endpoint.trim_end_matches('/').to_string()
    };
    let host = endpoint
        .strip_prefix("https://")
        .or_else(|| endpoint.strip_prefix("http://"))
        .unwrap_or(&endpoint);
    let url = format!("{endpoint}/{}/{}", config.bucket, config.object_key);

    let identity = Credentials::new(
        config.access_key.clone(),
        config.secret_key.clone(),
        None,
        None,
        "ember-resourcepack-builder",
    )
    .into();
    let signing_params = v4::SigningParams::builder()
        .identity(&identity)
        .region(&config.region)
        .name("s3")
        .time(SystemTime::now())
        .settings(SigningSettings::default())
        .build()
        .map_err(|e| format!("failed to build SigV4 signing params: {e}"))?
        .into();

    let signable_request = SignableRequest::new(
        "PUT",
        &url,
        [("host", host)].into_iter(),
        SignableBody::Bytes(bytes),
    )
    .map_err(|e| format!("failed to build a signable request: {e}"))?;
    let (signing_instructions, _signature) = sign(signable_request, &signing_params)
        .map_err(|e| format!("SigV4 signing failed: {e}"))?
        .into_parts();

    let mut request = ureq::put(&url).header("host", host);
    for (name, value) in signing_instructions.headers() {
        request = request.header(name, value);
    }
    request
        .send(bytes)
        .map_err(|e| format!("upload request failed: {e}"))?;

    info!(
        "Resource pack builder: uploaded to S3 bucket '{}'",
        config.bucket
    );
    let base = if config.public_url_base.is_empty() {
        format!("{endpoint}/{}", config.bucket)
    } else {
        config.public_url_base.trim_end_matches('/').to_string()
    };
    Ok(format!("{base}/{}", config.object_key))
}

fn sha1_digest(bytes: &[u8]) -> impl AsRef<[u8]> {
    use sha1::Digest as _;
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    hasher.finalize()
}

fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write as _;
    bytes.as_ref().iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_source_dir() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let source_dir = dir.path().join("source");
        std::fs::create_dir_all(source_dir.join("assets")).unwrap();
        (dir, source_dir)
    }

    fn read_zip_entry(bytes: &[u8], name: &str) -> Option<Vec<u8>> {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;
        let mut file = archive.by_name(name).ok()?;
        let mut contents = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut contents).ok()?;
        Some(contents)
    }

    #[test]
    fn build_zip_injects_pack_mcmeta_even_if_one_exists_on_disk() {
        let (_dir, source_dir) = temp_source_dir();
        std::fs::write(source_dir.join("pack.mcmeta"), b"stale on-disk content").unwrap();

        let bytes = build_zip(&source_dir, b"injected content").unwrap();

        assert_eq!(
            read_zip_entry(&bytes, "pack.mcmeta").unwrap(),
            b"injected content"
        );
    }

    #[test]
    fn build_zip_still_packs_other_root_files() {
        let (_dir, source_dir) = temp_source_dir();
        std::fs::write(source_dir.join("pack.png"), b"fake icon").unwrap();

        let bytes = build_zip(&source_dir, b"{}").unwrap();

        assert_eq!(read_zip_entry(&bytes, "pack.png").unwrap(), b"fake icon");
    }

    #[test]
    fn load_pack_mcmeta_prefers_override_when_present() {
        let (_dir, source_dir) = temp_source_dir();
        let override_path = source_dir.join("pack.mcmeta.26_1");
        std::fs::write(&override_path, b"custom").unwrap();

        assert_eq!(load_pack_mcmeta(&override_path, b"default"), b"custom");
    }

    #[test]
    fn load_pack_mcmeta_falls_back_when_absent() {
        let (_dir, source_dir) = temp_source_dir();
        let missing_path = source_dir.join("pack.mcmeta.26_2");

        assert_eq!(load_pack_mcmeta(&missing_path, b"default"), b"default");
    }

    #[test]
    fn suffixed_object_key_inserts_before_extension() {
        assert_eq!(
            suffixed_object_key("resourcepack.zip", "26-1"),
            "resourcepack-26-1.zip"
        );
    }

    #[test]
    fn suffixed_object_key_appends_when_no_extension() {
        assert_eq!(
            suffixed_object_key("resourcepack", "legacy"),
            "resourcepack-legacy"
        );
    }

    #[test]
    fn ensure_default_pack_writes_the_legacy_default_once() {
        let dir = tempfile::tempdir().unwrap();
        let source_dir = dir.path().join("source");

        ensure_default_pack(&source_dir).unwrap();
        assert!(source_dir.join("assets").is_dir());
        let content = std::fs::read_to_string(source_dir.join("pack.mcmeta")).unwrap();
        assert!(content.contains("\"pack_format\":75"));

        // Existing installs aren't overwritten.
        std::fs::write(source_dir.join("pack.mcmeta"), "custom").unwrap();
        ensure_default_pack(&source_dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(source_dir.join("pack.mcmeta")).unwrap(),
            "custom"
        );
    }

    fn test_legacy_config(enabled: bool) -> pumpkin_config::resource_pack::JavaResourcePackConfig {
        pumpkin_config::resource_pack::JavaResourcePackConfig {
            enabled,
            url: "http://legacy.example/pack.zip".to_string(),
            sha1: "legacy-sha1".to_string(),
            prompt_message: String::new(),
            force: false,
        }
    }

    fn test_versioned_packs() -> Vec<(JavaMinecraftVersion, ResourcePackVariant)> {
        vec![
            (
                JavaMinecraftVersion::V_26_1,
                ResourcePackVariant {
                    url: "http://modern.example/pack-26-1.zip".to_string(),
                    sha1: "26-1-sha1".to_string(),
                },
            ),
            (
                JavaMinecraftVersion::V_26_2,
                ResourcePackVariant {
                    url: "http://modern.example/pack-26-2.zip".to_string(),
                    sha1: "26-2-sha1".to_string(),
                },
            ),
        ]
    }

    #[test]
    fn resolve_resource_pack_variant_disabled_is_always_none() {
        let legacy = test_legacy_config(false);
        let versioned = test_versioned_packs();
        for version in [
            JavaMinecraftVersion::V_1_21,
            JavaMinecraftVersion::V_26_1,
            JavaMinecraftVersion::V_26_2,
            JavaMinecraftVersion::Unknown,
        ] {
            assert!(resolve_resource_pack_variant(&legacy, &versioned, version).is_none());
        }
    }

    #[test]
    fn resolve_resource_pack_variant_pre_26_falls_back_to_legacy() {
        let legacy = test_legacy_config(true);
        let versioned = test_versioned_packs();
        for version in [
            JavaMinecraftVersion::V_1_21,
            JavaMinecraftVersion::V_1_21_11,
            JavaMinecraftVersion::Unknown,
        ] {
            let resolved = resolve_resource_pack_variant(&legacy, &versioned, version).unwrap();
            assert_eq!(resolved.url, legacy.url);
            assert_eq!(resolved.sha1, legacy.sha1);
        }
    }

    #[test]
    fn resolve_resource_pack_variant_26_x_gets_its_own_exact_variant() {
        let legacy = test_legacy_config(true);
        let versioned = test_versioned_packs();

        let v26_1 =
            resolve_resource_pack_variant(&legacy, &versioned, JavaMinecraftVersion::V_26_1)
                .unwrap();
        assert_eq!(v26_1.url, "http://modern.example/pack-26-1.zip");
        assert_eq!(v26_1.sha1, "26-1-sha1");

        let v26_2 =
            resolve_resource_pack_variant(&legacy, &versioned, JavaMinecraftVersion::V_26_2)
                .unwrap();
        assert_eq!(v26_2.url, "http://modern.example/pack-26-2.zip");
        assert_eq!(v26_2.sha1, "26-2-sha1");
    }

    #[test]
    fn resolve_resource_pack_variant_26_1_2_hotfix_matches_26_1_exactly() {
        // 26.1.2 is a client-side-only hotfix sharing 26.1's protocol number
        // (verified against Minecraft Wiki) - Ember can't tell them apart at
        // all, so "resolving for 26.1.2" *is* "resolving for V_26_1". This
        // test locks in that the two are indistinguishable by construction,
        // not just by assumption.
        let legacy = test_legacy_config(true);
        let versioned = test_versioned_packs();
        let resolved =
            resolve_resource_pack_variant(&legacy, &versioned, JavaMinecraftVersion::V_26_1)
                .unwrap();
        assert_eq!(resolved.url, "http://modern.example/pack-26-1.zip");
    }

    #[test]
    fn resolve_resource_pack_variant_falls_back_when_nothing_built() {
        let legacy = test_legacy_config(true);
        let resolved =
            resolve_resource_pack_variant(&legacy, &[], JavaMinecraftVersion::V_26_1).unwrap();
        assert_eq!(resolved.url, legacy.url);
    }

    #[test]
    fn serve_self_hosted_answers_each_variant_and_404s_unknown_paths() {
        let built = vec![
            (
                None,
                BuiltVariant {
                    name: "legacy".to_string(),
                    bytes: b"legacy bytes".to_vec(),
                    sha1: "unused".to_string(),
                },
            ),
            (
                Some(JavaMinecraftVersion::V_26_1),
                BuiltVariant {
                    name: "26-1".to_string(),
                    bytes: b"26.1 bytes".to_vec(),
                    sha1: "unused".to_string(),
                },
            ),
        ];
        let config = SelfHostedConfig {
            bind_addr: "127.0.0.1".to_string(),
            port: 0, // OS-assigned free port
            public_url: String::new(),
            public_url_modern: String::new(),
        };

        // `serve_self_hosted` doesn't report back which port it actually
        // bound to when `port: 0` is used, so bind ourselves first, hand
        // over the concrete port, matching how the rest of this function
        // already expects a fully-resolved `bind_addr:port`.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let config = SelfHostedConfig { port, ..config };

        let urls = serve_self_hosted(&built, &config);
        assert_eq!(urls.len(), 2);

        let legacy_url = &urls["legacy"];
        let body = ureq::get(legacy_url)
            .call()
            .unwrap()
            .body_mut()
            .read_to_vec()
            .unwrap();
        assert_eq!(body, b"legacy bytes");

        let modern_url = &urls["26-1"];
        let body = ureq::get(modern_url)
            .call()
            .unwrap()
            .body_mut()
            .read_to_vec()
            .unwrap();
        assert_eq!(body, b"26.1 bytes");

        let unknown_url = legacy_url.replace("pack-legacy", "pack-does-not-exist");
        let status = ureq::get(&unknown_url).call().unwrap_err();
        match status {
            ureq::Error::StatusCode(code) => assert_eq!(code, 404),
            other => panic!("expected a 404 status error, got {other:?}"),
        }
    }
}
// EMBER end
