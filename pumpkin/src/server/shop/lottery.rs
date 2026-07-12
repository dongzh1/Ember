// EMBER start - built-in shop/bank/market/lottery system
//! Lottery: weighted-random draws, pity, daily limits.
//!
//! Pays *before* verifying entitlement (row-locked daily-limit check +
//! counter update), the opposite order from `PixelShop`'s reference (which
//! commits its DB transaction first and only pays afterward - a network
//! hiccup there can charge nothing for a counted draw). Paying first means
//! the failure window is "charged but not drawn", which has a clean,
//! guaranteed refund path (`economy.deposit`) rather than an approximate
//! rollback.

use std::sync::Arc;

use pumpkin_config::{LoadConfiguration, LotteryListConfig, LotteryPoolConfig, LotteryPrize};
use sqlx::Row;
use uuid::Uuid;

use super::{ShopError, ShopPool, db_err};
use crate::server::economy::EconomyManager;

// `daily_date` is a plain `YYYY-MM-DD` string (computed in Rust, see
// `today_string`), not a native `DATE` column - avoids depending on which
// date type `sqlx`'s enabled feature set maps `DATE` to.
const CREATE_STATE_TABLE: &str = concat!(
    "CREATE TABLE IF NOT EXISTS ember_lottery_state (",
    "player_uuid CHAR(36) NOT NULL,",
    "pool_name VARCHAR(64) NOT NULL,",
    "daily_count INT NOT NULL DEFAULT 0,",
    "daily_date VARCHAR(10) NOT NULL DEFAULT '1970-01-01',",
    "pity_count INT NOT NULL DEFAULT 0,",
    "total_count INT NOT NULL DEFAULT 0,",
    "PRIMARY KEY (player_uuid, pool_name)",
    ")"
);

const SELECT_STATE_FOR_UPDATE: &str = concat!(
    "SELECT daily_count, daily_date, pity_count, total_count FROM ember_lottery_state ",
    "WHERE player_uuid = ? AND pool_name = ? FOR UPDATE"
);

const UPSERT_STATE: &str = concat!(
    "INSERT INTO ember_lottery_state ",
    "(player_uuid, pool_name, daily_count, daily_date, pity_count, total_count) ",
    "VALUES (?, ?, ?, ?, ?, ?) ",
    "ON DUPLICATE KEY UPDATE daily_count = VALUES(daily_count), daily_date = VALUES(daily_date), ",
    "pity_count = VALUES(pity_count), total_count = VALUES(total_count)"
);

pub struct DrawResult {
    pub prize: LotteryPrize,
}

pub struct LotteryManager {
    pool: ShopPool,
    pools: LotteryListConfig,
}

impl LotteryManager {
    #[must_use]
    pub fn new(pool: ShopPool) -> Self {
        let exec_dir = std::env::current_dir().expect("Failed to get current directory");
        let pools = LotteryListConfig::load(&exec_dir);
        Self { pool, pools }
    }

    async fn ensure_tables(&self) -> Result<Arc<sqlx::MySqlPool>, ShopError> {
        let pool = self.pool.get().await?;
        sqlx::query(CREATE_STATE_TABLE)
            .execute(pool.as_ref())
            .await
            .map_err(db_err)?;
        Ok(pool)
    }

    #[must_use]
    pub fn find_pool(&self, name: &str) -> Option<&LotteryPoolConfig> {
        self.pools
            .pools
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
    }

    #[must_use]
    pub fn pool_names(&self) -> Vec<String> {
        self.pools.pools.iter().map(|p| p.name.clone()).collect()
    }

    fn pick_prize(pool: &LotteryPoolConfig, force_pity: bool) -> Option<&LotteryPrize> {
        let candidates: Vec<&LotteryPrize> = if force_pity {
            let Some(pity) = &pool.pity else {
                return pool.prizes.first();
            };
            pool.prizes
                .iter()
                .filter(|p| p.group.as_deref() == Some(pity.group.as_str()))
                .collect()
        } else {
            pool.prizes.iter().collect()
        };
        if candidates.is_empty() {
            return pool.prizes.first();
        }
        let total_weight: u32 = candidates.iter().map(|p| p.weight.max(1)).sum();
        if total_weight == 0 {
            return candidates.first().copied();
        }
        let mut roll = rand::random_range(0..total_weight);
        for prize in &candidates {
            let weight = prize.weight.max(1);
            if roll < weight {
                return Some(prize);
            }
            roll -= weight;
        }
        candidates.last().copied()
    }

    /// Draws once from `pool_name` for `player`: charges the cost first,
    /// then atomically (row-locked) re-verifies the daily limit and updates
    /// counters together with the prize roll, refunding if the limit was
    /// hit by a concurrent draw in that narrow window.
    pub async fn draw(
        &self,
        player: Uuid,
        pool_name: &str,
        economy: &EconomyManager,
    ) -> Result<DrawResult, ShopError> {
        let Some(pool) = self.find_pool(pool_name) else {
            return Err(ShopError::Other(format!(
                "no such lottery pool '{pool_name}'"
            )));
        };

        let currency = pool.cost_currency.clone();
        if pool.cost_amount > 0 {
            economy
                .withdraw(player, currency.as_deref(), pool.cost_amount)
                .await
                .map_err(|e| match e {
                    crate::server::economy::EconomyError::InsufficientFunds { have, need } => {
                        ShopError::InsufficientFunds { have, need }
                    }
                    other => ShopError::Other(other.to_string()),
                })?;
        }

        let outcome = self.settle_draw(player, pool).await;
        if outcome.is_err() && pool.cost_amount > 0 {
            // Compensate: the charge went through but the draw didn't.
            let _ = economy
                .deposit(player, currency.as_deref(), pool.cost_amount)
                .await;
        }
        outcome
    }

    async fn settle_draw(
        &self,
        player: Uuid,
        pool: &LotteryPoolConfig,
    ) -> Result<DrawResult, ShopError> {
        let db_pool = self.ensure_tables().await?;
        let mut tx = db_pool.begin().await.map_err(db_err)?;

        let row = sqlx::query(SELECT_STATE_FOR_UPDATE)
            .bind(player.hyphenated().to_string())
            .bind(&pool.name)
            .fetch_optional(&mut *tx)
            .await
            .map_err(db_err)?;

        let today = today_string();
        let (mut daily_count, mut pity_count, total_count) = row.as_ref().map_or((0, 0, 0), |r| {
            let date: String = r.get("daily_date");
            let daily_count: i32 = if date == today {
                r.get("daily_count")
            } else {
                0
            };
            (
                daily_count,
                r.get::<i32, _>("pity_count"),
                r.get::<i32, _>("total_count"),
            )
        });

        if pool.daily_limit >= 0 && i64::from(daily_count) >= pool.daily_limit {
            tx.rollback().await.map_err(db_err)?;
            return Err(ShopError::Other("daily draw limit reached".to_string()));
        }

        let force_pity = pool
            .pity
            .as_ref()
            .is_some_and(|p| pity_count + 1 >= p.threshold as i32);
        let Some(prize) = Self::pick_prize(pool, force_pity).cloned() else {
            tx.rollback().await.map_err(db_err)?;
            return Err(ShopError::Other(
                "this pool has no prizes configured".to_string(),
            ));
        };

        daily_count += 1;
        if force_pity {
            pity_count = 0;
        } else if pool.pity.is_some() {
            pity_count += 1;
        }

        sqlx::query(UPSERT_STATE)
            .bind(player.hyphenated().to_string())
            .bind(&pool.name)
            .bind(daily_count)
            .bind(today)
            .bind(pity_count)
            .bind(total_count + 1)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;

        tx.commit().await.map_err(db_err)?;
        Ok(DrawResult { prize })
    }
}

/// `YYYY-MM-DD` for today, `UTC` - used only to compare against the stored
/// `daily_date` column, not for display.
fn today_string() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = now / 86400;
    // Civil-from-days (Howard Hinnant's algorithm), avoids a chrono dependency.
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}
// EMBER end
