// EMBER start - dedicated MySQL database for Ember's own auxiliary storage
//! Connects to Ember's own dedicated `MySQL` database, creating it first if
//! it doesn't exist yet.
//!
//! `server::furniture`/`server::custom_block` persist their placed-instance
//! data to `MySQL` for any world whose own chunk storage backend is `MySQL`
//! (so every server sharing that world sees the same placements). Early on
//! this reused that world's own configured database directly — which meant
//! Ember's own tables landed inside whatever database the admin already
//! uses for chunk data, uninvited. This connects to a separate, dedicated
//! database instead, so Ember never writes into a database it wasn't
//! explicitly given.
use std::str::FromStr;

use sqlx::mysql::{MySqlConnectOptions, MySqlPool, MySqlPoolOptions};
use tracing::info;

/// Dedicated database name for all of Ember's own MySQL-backed auxiliary
/// storage. Not user-configurable (yet) — if this collides with something
/// real, rename it here and reconnect.
pub const EMBER_DATABASE_NAME: &str = "ember";

/// Parses `world_mysql_url` and swaps in [`EMBER_DATABASE_NAME`] as the
/// target database, keeping everything else (host/port/credentials/tls)
/// as given. Split out from [`connect_ember_database`] so the override
/// itself — the actual point of this module — is unit-testable without a
/// real server.
fn ember_connect_options(world_mysql_url: &str) -> Result<MySqlConnectOptions, String> {
    MySqlConnectOptions::from_str(world_mysql_url)
        .map(|opts| opts.database(EMBER_DATABASE_NAME))
        .map_err(|e| format!("couldn't parse mysql url: {e}"))
}

/// Connects to [`EMBER_DATABASE_NAME`] on the same `MySQL` server
/// `world_mysql_url` points at, best-effort creating the database first if
/// it looks like it might not exist yet. `world_mysql_url`'s own database
/// name is intentionally discarded — only its host/port/credentials are
/// reused.
///
/// The create-database step is deliberately best-effort, not required to
/// succeed: a mysql account scoped to only the [`EMBER_DATABASE_NAME`]
/// database it was already given (a normal way to hand Ember credentials
/// without granting it server-wide `CREATE`) can't run `CREATE DATABASE`
/// at all, even with `IF NOT EXISTS`, even though the database is right
/// there and fully usable. So this never treats that step's failure as
/// fatal — it just goes ahead and tries the real connection below, and
/// only reports an error if that one fails too.
pub async fn connect_ember_database(world_mysql_url: &str) -> Result<MySqlPool, String> {
    let ember_options = ember_connect_options(world_mysql_url)?;

    // Best-effort: any database guaranteed to already exist, purely to try
    // `CREATE DATABASE IF NOT EXISTS` for Ember's own one. `information_schema`
    // is a standard, always-present, never-user-owned MySQL/MariaDB system
    // schema — deliberately not the caller's own world database, so this
    // never even transiently touches that. Both connecting here and the
    // `CREATE DATABASE` itself are allowed to fail silently (see doc comment).
    if let Ok(admin_pool) = MySqlPoolOptions::new()
        .max_connections(1)
        .connect_with(ember_options.clone().database("information_schema"))
        .await
    {
        let _ = sqlx::query(&format!(
            "CREATE DATABASE IF NOT EXISTS `{EMBER_DATABASE_NAME}`"
        ))
        .execute(&admin_pool)
        .await;
        admin_pool.close().await;
    }

    let pool = MySqlPoolOptions::new()
        .max_connections(4)
        .connect_with(ember_options)
        .await
        .map_err(|e| {
            format!(
                "couldn't connect to the '{EMBER_DATABASE_NAME}' database: {e} — if it doesn't \
                 exist yet and this mysql user can't create it itself, ask an admin to run \
                 `CREATE DATABASE {EMBER_DATABASE_NAME};` manually once, then restart the server."
            )
        })?;
    info!(
        "Connected to the dedicated '{EMBER_DATABASE_NAME}' database for Ember's own mysql storage."
    );
    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_whatever_database_the_world_url_specifies() {
        let opts = ember_connect_options("mysql://user:pass@127.0.0.1:3306/chunkworld")
            .expect("valid url should parse");
        assert_eq!(opts.get_database(), Some(EMBER_DATABASE_NAME));
    }

    #[test]
    fn rejects_an_unparseable_url() {
        assert!(ember_connect_options("not a url").is_err());
    }
}
// EMBER end
