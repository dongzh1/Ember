// EMBER start: resource pack builder (self-generate + self-host/S3)
//! Builds a resource pack from `resourcepack/source/` and gets it in front
//! of Java clients, either via a small built-in HTTP server or by uploading
//! it to an S3-compatible bucket - then overwrites
//! `AdvancedConfiguration.resource_pack.java`'s `url`/`sha1`/`enabled` so the
//! *existing*, unmodified resource-pack push in `net/java/login.rs` (and the
//! response handling in `net/java/config.rs`) picks it up exactly like a
//! manually-configured external URL would. Neither of those needs to know
//! (or change) whether the pack came from here or from a hand-configured
//! external host.
use std::io::{Cursor, Write as _};
use std::path::Path;
use std::time::SystemTime;

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{SignableBody, SignableRequest, SigningSettings, sign};
use aws_sigv4::sign::v4;
use pumpkin_config::{
    HostingMode, LoadConfiguration, ResourcePackBuilderConfig, S3Config, SelfHostedConfig,
};
use sha1::Sha1;
use tracing::{error, info, warn};
use zip::write::{SimpleFileOptions, ZipWriter};

/// Loads `resourcepack/resourcepack.toml` and, if enabled, runs the whole
/// pipeline (build -> host or upload). Returns `(url, sha1)` to write into
/// `JavaResourcePackConfig` - `None` if disabled or on any failure (logged;
/// the server still boots without a resource pack rather than refusing to
/// start over this).
#[must_use]
pub fn init(exec_dir: &Path) -> Option<(String, String)> {
    let config = ResourcePackBuilderConfig::load(exec_dir);
    if !config.enabled {
        return None;
    }

    let source_dir = exec_dir.join(&config.source_dir);
    if let Err(e) = ensure_default_pack(&source_dir) {
        error!(
            "Resource pack builder: failed to prepare '{}': {e}",
            source_dir.display()
        );
        return None;
    }

    let zip_bytes = match build_zip(&source_dir) {
        Ok(bytes) => bytes,
        Err(e) => {
            error!(
                "Resource pack builder: failed to build a pack from '{}': {e}",
                source_dir.display()
            );
            return None;
        }
    };
    info!(
        "Resource pack builder: built {} bytes from '{}'",
        zip_bytes.len(),
        source_dir.display()
    );
    let sha1_hex = hex_encode(sha1_digest(&zip_bytes));

    let url = match config.hosting {
        HostingMode::SelfHosted => serve_self_hosted(zip_bytes, &config.self_hosted),
        HostingMode::S3 => match upload_to_s3(&zip_bytes, &config.s3) {
            Ok(url) => Some(url),
            Err(e) => {
                error!("Resource pack builder: S3 upload failed: {e}");
                None
            }
        },
    }?;

    Some((url, sha1_hex))
}

fn ensure_default_pack(source_dir: &Path) -> std::io::Result<()> {
    if source_dir.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(source_dir.join("assets"))?;
    // EMBER: a placeholder pack_format - the operator is expected to drop
    // their own `pack.mcmeta` in here; a mismatched format only produces a
    // client-side warning, never a hard failure.
    std::fs::write(
        source_dir.join("pack.mcmeta"),
        r#"{"pack":{"pack_format":55,"description":"Ember resource pack"}}"#,
    )?;
    Ok(())
}

fn build_zip(source_dir: &Path) -> std::io::Result<Vec<u8>> {
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
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
/// runtime) serving `bytes` to every request. Returns the URL to hand to
/// clients, or `None` if the port couldn't be bound.
fn serve_self_hosted(bytes: Vec<u8>, config: &SelfHostedConfig) -> Option<String> {
    let bind = format!("{}:{}", config.bind_addr, config.port);
    let server = match tiny_http::Server::http(&bind) {
        Ok(server) => server,
        Err(e) => {
            error!("Resource pack builder: failed to bind self-hosted server on '{bind}': {e}");
            return None;
        }
    };

    let spawn_result = std::thread::Builder::new()
        .name("resourcepack-http".to_string())
        .spawn(move || {
            for request in server.incoming_requests() {
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
        return None;
    }

    info!("Resource pack builder: self-hosting on {bind}");
    Some(if config.public_url.is_empty() {
        format!("http://{bind}/pack.zip")
    } else {
        config.public_url.clone()
    })
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
// EMBER end
