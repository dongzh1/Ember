// EMBER start - built-in shop/bank/market/lottery system
//! Shop system: shops, bank, market/auction, and lottery.
//!
//! All sharing one `MySQL` connection pool. Deliberately server-builtin
//! rather than a plugin (same reasoning as the economy system - see
//! `EMBER.md`'s "服务端 vs 插件边界" section).
//!
//! One [`ShopPool`] is shared by all four sub-managers (`shop`/`bank`/
//! `market`/`lottery`) rather than each opening its own connection - they're
//! one cohesive feature, not four unrelated ones. Each sub-manager still
//! creates and owns its own tables, matching `EconomyManager`'s
//! one-table-per-concern precedent.

pub mod bank;
pub mod basic_shop;
pub mod chat_capture;
pub mod gui;
pub mod lottery;
pub mod market;
pub mod shop_menu;

use std::sync::Arc;

use pumpkin_config::{LoadConfiguration, ShopSystemConfig};
use sqlx::mysql::MySqlPoolOptions;
use tracing::error;

#[derive(thiserror::Error, Debug, Clone)]
pub enum ShopError {
    #[error("the shop system is not enabled ([shop] enabled = true in shop/shop.toml)")]
    Disabled,
    #[error("shop database error: {0}")]
    Database(String),
    #[error("insufficient funds: has {have}, needs {need}")]
    InsufficientFunds { have: i64, need: i64 },
    #[error("{0}")]
    Other(String),
}

fn db_err(e: impl std::fmt::Display) -> ShopError {
    ShopError::Database(e.to_string())
}

/// Loads `shop/shop.toml` and builds the shared, lazily-connected pool.
/// Called once from `Server::new`; the returned config/pool are cloned into
/// each of the four sub-managers.
#[must_use]
pub fn load() -> (ShopSystemConfig, ShopPool) {
    let exec_dir = std::env::current_dir().expect("Failed to get current directory");
    let config = ShopSystemConfig::load(&exec_dir);
    let pool = ShopPool::new(&config);
    (config, pool)
}

/// Lazily-initialized `MySQL` pool shared by every shop-system sub-manager.
///
/// Cheap to clone (an `Arc` around the lazy cell), so each sub-manager just
/// holds its own clone rather than reaching back through a parent struct.
#[derive(Clone)]
pub struct ShopPool {
    enabled: bool,
    url: Arc<str>,
    cell: Arc<tokio::sync::OnceCell<Arc<sqlx::MySqlPool>>>,
}

impl ShopPool {
    fn new(config: &ShopSystemConfig) -> Self {
        let pool = Self {
            enabled: config.enabled,
            url: Arc::from(config.url.as_str()),
            cell: Arc::new(tokio::sync::OnceCell::new()),
        };

        if pool.enabled
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            // Eagerly connect in the background so a bad URL fails loudly at
            // startup instead of on the first shop/bank/market/lottery
            // interaction - matches `EconomyManager::new`'s precedent.
            let cell = pool.cell.clone();
            let url = pool.url.clone();
            handle.spawn(async move {
                if let Err(e) = cell.get_or_try_init(|| Self::connect(&url)).await {
                    error!(
                        "Shop system MySQL eager init failed (check [shop] url in shop/shop.toml): {e}"
                    );
                }
            });
        }

        pool
    }

    async fn connect(url: &str) -> Result<Arc<sqlx::MySqlPool>, ShopError> {
        let pool = MySqlPoolOptions::new()
            .max_connections(8)
            .connect(url)
            .await
            .map_err(db_err)?;
        Ok(Arc::new(pool))
    }

    /// Shared pool accessor. Each sub-manager is responsible for its own
    /// `CREATE TABLE IF NOT EXISTS` on first use - this only owns the
    /// connection itself.
    pub async fn get(&self) -> Result<Arc<sqlx::MySqlPool>, ShopError> {
        if !self.enabled {
            return Err(ShopError::Disabled);
        }
        self.cell
            .get_or_try_init(|| Self::connect(&self.url))
            .await
            .map(Arc::clone)
    }
}
// EMBER end
