// EMBER start - built-in shop/bank/market/lottery system
//! Basic shop: GUI buy/sell, batch purchase, redemption, dynamic pricing.

use std::cmp::max;
use std::sync::Arc;
use std::time::Duration;

use pumpkin_config::{LoadConfiguration, ShopConfig, ShopItem, ShopListConfig, ShopSettings};
use sqlx::Row;
use uuid::Uuid;

use super::{ShopError, ShopPool, db_err};

const CREATE_PRICES_TABLE: &str = concat!(
    "CREATE TABLE IF NOT EXISTS ember_shop_prices (",
    "shop_name VARCHAR(64) NOT NULL,",
    "item VARCHAR(64) NOT NULL,",
    "currency VARCHAR(32) NOT NULL,",
    "current_sell_price BIGINT NOT NULL,",
    "sold_since_decay INT NOT NULL DEFAULT 0,",
    "last_recovery_date DATE NOT NULL,",
    "PRIMARY KEY (shop_name, item, currency)",
    ")"
);

const CREATE_PURCHASE_LOG_TABLE: &str = concat!(
    "CREATE TABLE IF NOT EXISTS ember_shop_purchase_log (",
    "player_uuid CHAR(36) NOT NULL,",
    "shop_name VARCHAR(64) NOT NULL,",
    "item VARCHAR(64) NOT NULL,",
    "purchase_date DATE NOT NULL,",
    "count INT NOT NULL DEFAULT 0,",
    "PRIMARY KEY (player_uuid, shop_name, item, purchase_date)",
    ")"
);

const CREATE_REDEEMABLE_TABLE: &str = concat!(
    "CREATE TABLE IF NOT EXISTS ember_shop_redeemable (",
    "player_uuid CHAR(36) NOT NULL PRIMARY KEY,",
    "shop_name VARCHAR(64) NOT NULL,",
    "item VARCHAR(64) NOT NULL,",
    "amount INT NOT NULL,",
    "currency VARCHAR(32) NOT NULL,",
    "unit_price BIGINT NOT NULL,",
    "expires_at BIGINT NOT NULL",
    ")"
);

const SELECT_PRICE: &str = "SELECT current_sell_price, sold_since_decay, last_recovery_date \
     FROM ember_shop_prices WHERE shop_name = ? AND item = ? AND currency = ?";

const UPSERT_PRICE: &str = concat!(
    "INSERT INTO ember_shop_prices ",
    "(shop_name, item, currency, current_sell_price, sold_since_decay, last_recovery_date) ",
    "VALUES (?, ?, ?, ?, ?, CURDATE()) ",
    "ON DUPLICATE KEY UPDATE current_sell_price = VALUES(current_sell_price), ",
    "sold_since_decay = VALUES(sold_since_decay)"
);

const SELECT_PURCHASE_COUNT: &str = "SELECT count FROM ember_shop_purchase_log \
     WHERE player_uuid = ? AND shop_name = ? AND item = ? AND purchase_date = CURDATE()";

const BUMP_PURCHASE_COUNT: &str = concat!(
    "INSERT INTO ember_shop_purchase_log (player_uuid, shop_name, item, purchase_date, count) ",
    "VALUES (?, ?, ?, CURDATE(), ?) ",
    "ON DUPLICATE KEY UPDATE count = count + VALUES(count)"
);

const UPSERT_REDEEMABLE: &str = concat!(
    "INSERT INTO ember_shop_redeemable ",
    "(player_uuid, shop_name, item, amount, currency, unit_price, expires_at) ",
    "VALUES (?, ?, ?, ?, ?, ?, ?) ",
    "ON DUPLICATE KEY UPDATE shop_name = VALUES(shop_name), item = VALUES(item), ",
    "amount = VALUES(amount), currency = VALUES(currency), unit_price = VALUES(unit_price), ",
    "expires_at = VALUES(expires_at)"
);

const SELECT_REDEEMABLE: &str = "SELECT shop_name, item, amount, currency, unit_price, expires_at \
     FROM ember_shop_redeemable WHERE player_uuid = ?";

const DELETE_REDEEMABLE: &str = "DELETE FROM ember_shop_redeemable WHERE player_uuid = ?";

/// One player's most recently sold item, still buyable back.
pub struct Redeemable {
    pub shop_name: String,
    pub item: String,
    pub amount: u32,
    pub currency: String,
    /// Sell-side unit price at the moment it was sold - shown for reference
    /// only; redeeming always charges the *current* buy price, not this.
    pub unit_price: i64,
    pub expires_at: i64,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64
}

pub struct ShopManager {
    pool: ShopPool,
    settings: ShopSettings,
    shops: ShopListConfig,
}

impl ShopManager {
    #[must_use]
    pub fn new(settings: ShopSettings, pool: ShopPool) -> Self {
        let exec_dir = std::env::current_dir().expect("Failed to get current directory");
        let shops = ShopListConfig::load(&exec_dir);
        Self {
            pool,
            settings,
            shops,
        }
    }

    /// Spawns the background loop that calls [`Self::run_daily_recovery`]
    /// once every 24h. Split out from `new()` since it needs `self` already
    /// wrapped in an `Arc` (only available once the caller has done so).
    pub fn spawn_daily_recovery(self: &Arc<Self>) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let this = self.clone();
        handle.spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_hours(24)).await;
                if let Err(e) = this.run_daily_recovery().await {
                    tracing::error!("Shop daily price recovery failed: {e}");
                }
            }
        });
    }

    async fn ensure_tables(&self) -> Result<std::sync::Arc<sqlx::MySqlPool>, ShopError> {
        let pool = self.pool.get().await?;
        for stmt in [
            CREATE_PRICES_TABLE,
            CREATE_PURCHASE_LOG_TABLE,
            CREATE_REDEEMABLE_TABLE,
        ] {
            sqlx::query(stmt)
                .execute(pool.as_ref())
                .await
                .map_err(db_err)?;
        }
        Ok(pool)
    }

    #[must_use]
    pub fn find_shop(&self, name: &str) -> Option<&ShopConfig> {
        self.shops
            .shops
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
    }

    #[must_use]
    pub fn shop_names(&self) -> Vec<String> {
        self.shops.shops.iter().map(|s| s.name.clone()).collect()
    }

    /// Current dynamic sell price for `item` in `shop_name` - `base` if
    /// there's no price-decay row yet (never sold, or fully recovered).
    async fn current_sell_price(
        &self,
        shop_name: &str,
        item: &str,
        currency: &str,
        base: i64,
    ) -> Result<i64, ShopError> {
        let pool = self.ensure_tables().await?;
        let row = sqlx::query(SELECT_PRICE)
            .bind(shop_name)
            .bind(item)
            .bind(currency)
            .fetch_optional(pool.as_ref())
            .await
            .map_err(db_err)?;
        Ok(row.map_or(base, |r| r.get::<i64, _>("current_sell_price")))
    }

    /// Buy price = sell price + `max(1, ceil(sell_price * buy_markup_pct))`.
    #[must_use]
    pub fn buy_price_from_sell(&self, sell_price: i64) -> i64 {
        let markup = (sell_price as f64 * self.settings.buy_markup_pct).ceil() as i64;
        sell_price + max(1, markup)
    }

    /// Resolves both prices for one shop item right now (dynamic sell price
    /// plus the buy price derived from it).
    pub async fn prices(&self, shop_name: &str, entry: &ShopItem) -> Result<(i64, i64), ShopError> {
        let currency = entry.currency.as_deref().unwrap_or_default();
        let base = entry.base_sell_price.unwrap_or(0);
        let sell = self
            .current_sell_price(shop_name, &entry.item, currency, base)
            .await?;
        Ok((sell, self.buy_price_from_sell(sell)))
    }

    /// How many of `item` `player` has already bought today in this shop.
    async fn purchases_today(
        &self,
        player: Uuid,
        shop_name: &str,
        item: &str,
    ) -> Result<i64, ShopError> {
        let pool = self.ensure_tables().await?;
        let row = sqlx::query(SELECT_PURCHASE_COUNT)
            .bind(player.hyphenated().to_string())
            .bind(shop_name)
            .bind(item)
            .fetch_optional(pool.as_ref())
            .await
            .map_err(db_err)?;
        Ok(row.map_or(0, |r| i64::from(r.get::<i32, _>("count"))))
    }

    /// Buys `quantity` of `entry` for `player`. Charges the *current* buy
    /// price at the moment of purchase (not `base_buy_price`), enforces the
    /// daily `limit`, and withdraws via the given [`EconomyManager`]. Buying
    /// doesn't move dynamic prices - only real sales do (matching `PixelShop`:
    /// buy-side has no decay/recovery, just the sell-price-derived markup).
    pub async fn buy(
        &self,
        player: Uuid,
        shop_name: &str,
        item_name: &str,
        quantity: u32,
        economy: &crate::server::economy::EconomyManager,
    ) -> Result<(&'static pumpkin_data::item::Item, i64), ShopError> {
        let Some(shop) = self.find_shop(shop_name) else {
            return Err(ShopError::Other(format!("no such shop '{shop_name}'")));
        };
        let Some(entry) = shop.items.iter().find(|i| i.item == item_name) else {
            return Err(ShopError::Other(format!("'{item_name}' isn't sold here")));
        };
        let Some(item) = pumpkin_data::item::Item::from_registry_key(&entry.item) else {
            return Err(ShopError::Other(format!("unknown item '{}'", entry.item)));
        };

        if entry.limit >= 0 {
            let bought = self.purchases_today(player, shop_name, item_name).await?;
            if bought + i64::from(quantity) > entry.limit {
                return Err(ShopError::Other(format!(
                    "daily limit reached ({bought}/{})",
                    entry.limit
                )));
            }
        }

        let (_, unit_buy_price) = self.prices(shop_name, entry).await?;
        let total = unit_buy_price * i64::from(quantity);
        economy
            .withdraw(player, entry.currency.as_deref(), total)
            .await
            .map_err(|e| match e {
                crate::server::economy::EconomyError::InsufficientFunds { have, need } => {
                    ShopError::InsufficientFunds { have, need }
                }
                other => ShopError::Other(other.to_string()),
            })?;

        if entry.limit >= 0 {
            let pool = self.ensure_tables().await?;
            sqlx::query(BUMP_PURCHASE_COUNT)
                .bind(player.hyphenated().to_string())
                .bind(shop_name)
                .bind(item_name)
                .bind(i64::from(quantity))
                .execute(pool.as_ref())
                .await
                .map_err(db_err)?;
        }

        Ok((item, total))
    }

    /// Sells `quantity` of `item_name` for `player`, decaying the dynamic
    /// price and recording the sale as redeemable. Returns the amount paid.
    pub async fn sell(
        &self,
        player: Uuid,
        shop_name: &str,
        item_name: &str,
        quantity: u32,
        economy: &crate::server::economy::EconomyManager,
    ) -> Result<i64, ShopError> {
        let Some(shop) = self.find_shop(shop_name) else {
            return Err(ShopError::Other(format!("no such shop '{shop_name}'")));
        };
        let Some(entry) = shop.items.iter().find(|i| i.item == item_name) else {
            return Err(ShopError::Other(format!("'{item_name}' isn't sold here")));
        };
        let Some(base) = entry.base_sell_price else {
            return Err(ShopError::Other(format!(
                "'{item_name}' can't be sold here"
            )));
        };

        let currency = entry.currency.clone().unwrap_or_default();
        let sell_price = self
            .current_sell_price(shop_name, item_name, &currency, base)
            .await?;
        let total = sell_price * i64::from(quantity);

        let pool = self.ensure_tables().await?;
        let row = sqlx::query(SELECT_PRICE)
            .bind(shop_name)
            .bind(item_name)
            .bind(&currency)
            .fetch_optional(pool.as_ref())
            .await
            .map_err(db_err)?;
        let sold_since_decay = row
            .as_ref()
            .map_or(0, |r| r.get::<i32, _>("sold_since_decay"));

        let mut new_price = sell_price;
        let mut new_sold_since_decay = sold_since_decay + quantity as i32;
        let decay_every = max(1, self.settings.decay_every_n_sold as i32);
        let min_price = max(
            1,
            (base as f64 * self.settings.min_price_pct).round() as i64,
        );
        while new_sold_since_decay >= decay_every {
            new_sold_since_decay -= decay_every;
            let decayed = (new_price as f64 * (1.0 - self.settings.price_decay_pct)).round() as i64;
            new_price = max(min_price, decayed);
        }

        sqlx::query(UPSERT_PRICE)
            .bind(shop_name)
            .bind(item_name)
            .bind(&currency)
            .bind(new_price)
            .bind(new_sold_since_decay)
            .execute(pool.as_ref())
            .await
            .map_err(db_err)?;

        economy
            .deposit(player, Some(&currency), total)
            .await
            .map_err(|e| ShopError::Other(e.to_string()))?;

        let expires_at = now_unix() + self.settings.redeemable_expiry_hours * 3600;
        sqlx::query(UPSERT_REDEEMABLE)
            .bind(player.hyphenated().to_string())
            .bind(shop_name)
            .bind(item_name)
            .bind(quantity)
            .bind(&currency)
            .bind(sell_price)
            .bind(expires_at)
            .execute(pool.as_ref())
            .await
            .map_err(db_err)?;

        Ok(total)
    }

    /// The player's redeemable item, if any and not yet expired.
    pub async fn redeemable(&self, player: Uuid) -> Result<Option<Redeemable>, ShopError> {
        let pool = self.ensure_tables().await?;
        let Some(row) = sqlx::query(SELECT_REDEEMABLE)
            .bind(player.hyphenated().to_string())
            .fetch_optional(pool.as_ref())
            .await
            .map_err(db_err)?
        else {
            return Ok(None);
        };
        let expires_at: i64 = row.get("expires_at");
        if expires_at < now_unix() {
            return Ok(None);
        }
        Ok(Some(Redeemable {
            shop_name: row.get("shop_name"),
            item: row.get("item"),
            #[expect(
                clippy::cast_sign_loss,
                reason = "amount is always stored non-negative"
            )]
            amount: row.get::<i32, _>("amount") as u32,
            currency: row.get("currency"),
            unit_price: row.get("unit_price"),
            expires_at,
        }))
    }

    /// Buys back the player's redeemable item at the *current* buy price,
    /// clearing the redeemable record on success.
    pub async fn redeem(
        &self,
        player: Uuid,
        economy: &crate::server::economy::EconomyManager,
    ) -> Result<(&'static pumpkin_data::item::Item, u32, i64), ShopError> {
        let Some(redeemable) = self.redeemable(player).await? else {
            return Err(ShopError::Other("nothing to redeem".to_string()));
        };
        let Some(shop) = self.find_shop(&redeemable.shop_name) else {
            return Err(ShopError::Other("that shop no longer exists".to_string()));
        };
        let Some(entry) = shop.items.iter().find(|i| i.item == redeemable.item) else {
            return Err(ShopError::Other(
                "that item is no longer sold there".to_string(),
            ));
        };
        let Some(item) = pumpkin_data::item::Item::from_registry_key(&redeemable.item) else {
            return Err(ShopError::Other("unknown item".to_string()));
        };

        let (_, unit_buy_price) = self.prices(&redeemable.shop_name, entry).await?;
        let total = unit_buy_price * i64::from(redeemable.amount);
        economy
            .withdraw(player, Some(&redeemable.currency), total)
            .await
            .map_err(|e| match e {
                crate::server::economy::EconomyError::InsufficientFunds { have, need } => {
                    ShopError::InsufficientFunds { have, need }
                }
                other => ShopError::Other(other.to_string()),
            })?;

        let pool = self.ensure_tables().await?;
        sqlx::query(DELETE_REDEEMABLE)
            .bind(player.hyphenated().to_string())
            .execute(pool.as_ref())
            .await
            .map_err(db_err)?;

        Ok((item, redeemable.amount, total))
    }

    /// Daily recovery: every decayed price (below its item's configured
    /// `base_sell_price`) moves up by `daily_recovery_multiplier`, capped at
    /// the base; fully-recovered rows are deleted so a later read falls back
    /// to the configured base again. Meant to be called once a day from a
    /// scheduled task, not per-request.
    pub async fn run_daily_recovery(&self) -> Result<(), ShopError> {
        let pool = self.ensure_tables().await?;
        for shop in &self.shops.shops {
            for entry in &shop.items {
                let Some(base) = entry.base_sell_price else {
                    continue;
                };
                let currency = entry.currency.clone().unwrap_or_default();
                let current = self
                    .current_sell_price(&shop.name, &entry.item, &currency, base)
                    .await?;
                if current >= base {
                    continue;
                }
                let recovered =
                    ((current as f64) * self.settings.daily_recovery_multiplier).round() as i64;
                let new_price = recovered.min(base);
                if new_price >= base {
                    sqlx::query(
                        "DELETE FROM ember_shop_prices WHERE shop_name = ? AND item = ? AND currency = ?",
                    )
                    .bind(&shop.name)
                    .bind(&entry.item)
                    .bind(&currency)
                    .execute(pool.as_ref())
                    .await
                    .map_err(db_err)?;
                } else {
                    sqlx::query(UPSERT_PRICE)
                        .bind(&shop.name)
                        .bind(&entry.item)
                        .bind(&currency)
                        .bind(new_price)
                        .bind(0)
                        .execute(pool.as_ref())
                        .await
                        .map_err(db_err)?;
                }
            }
        }
        Ok(())
    }
}
// EMBER end
