// EMBER start - built-in shop/bank/market/lottery system
//! Market/auction: player listings, atomic buy via `DELETE ... LIMIT 1`,
//! commission.
//!
//! `PixelShop` needs a separate "offline seller income mailbox" table because
//! its economy plugin (Vault) is keyed to online sessions. Ember's own
//! [`EconomyManager`] always writes straight to `MySQL` regardless of
//! whether the player is connected, so a sold listing's payment just
//! deposits directly - there's no offline-seller problem to solve here, and
//! no mailbox table needed.

use std::sync::Arc;

use pumpkin_config::{MarketSettings, MarketSlotTier};
use sqlx::Row;
use uuid::Uuid;

use super::{ShopError, ShopPool, db_err};
use crate::server::economy::EconomyManager;

const CREATE_LISTINGS_TABLE: &str = concat!(
    "CREATE TABLE IF NOT EXISTS ember_market_listings (",
    "id BIGINT AUTO_INCREMENT PRIMARY KEY,",
    "seller_uuid CHAR(36) NOT NULL,",
    "seller_name VARCHAR(16) NOT NULL,",
    "item VARCHAR(64) NOT NULL,",
    "amount INT NOT NULL,",
    "currency VARCHAR(32) NOT NULL,",
    "price BIGINT NOT NULL,",
    "listed_at BIGINT NOT NULL,",
    "INDEX idx_seller (seller_uuid)",
    ")"
);

const INSERT_LISTING: &str = concat!(
    "INSERT INTO ember_market_listings ",
    "(seller_uuid, seller_name, item, amount, currency, price, listed_at) ",
    "VALUES (?, ?, ?, ?, ?, ?, ?)"
);

const REINSERT_LISTING: &str = concat!(
    "INSERT INTO ember_market_listings ",
    "(id, seller_uuid, seller_name, item, amount, currency, price, listed_at) ",
    "VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
);

const SELECT_ACTIVE: &str = "SELECT id, seller_uuid, seller_name, item, amount, currency, price, listed_at \
     FROM ember_market_listings ORDER BY listed_at DESC LIMIT 200";

const SELECT_ONE: &str = "SELECT id, seller_uuid, seller_name, item, amount, currency, price, listed_at \
     FROM ember_market_listings WHERE id = ?";

const COUNT_ACTIVE_FOR_SELLER: &str =
    "SELECT COUNT(*) AS n FROM ember_market_listings WHERE seller_uuid = ?";

const DELETE_BY_ID: &str = "DELETE FROM ember_market_listings WHERE id = ? LIMIT 1";

const DELETE_OWNED: &str =
    "DELETE FROM ember_market_listings WHERE id = ? AND seller_uuid = ? LIMIT 1";

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64
}

pub struct Listing {
    pub id: i64,
    pub seller_uuid: Uuid,
    pub seller_name: String,
    pub item: String,
    pub amount: u32,
    pub currency: String,
    pub price: i64,
    pub listed_at: i64,
}

fn row_to_listing(row: &sqlx::mysql::MySqlRow) -> Listing {
    Listing {
        id: row.get("id"),
        seller_uuid: row
            .get::<String, _>("seller_uuid")
            .parse()
            .unwrap_or(Uuid::nil()),
        seller_name: row.get("seller_name"),
        #[expect(
            clippy::cast_sign_loss,
            reason = "amount is always stored non-negative"
        )]
        amount: row.get::<i32, _>("amount") as u32,
        item: row.get("item"),
        currency: row.get("currency"),
        price: row.get("price"),
        listed_at: row.get("listed_at"),
    }
}

pub struct MarketManager {
    pool: ShopPool,
    settings: MarketSettings,
}

/// Bundles `create_listing`'s parameters under clippy's argument-count limit.
pub struct NewListing<'a> {
    pub seller: Uuid,
    pub seller_name: &'a str,
    pub item: &'a str,
    pub amount: u32,
    pub currency: &'a str,
    pub price: i64,
    pub max_listings: u32,
}

impl MarketManager {
    #[must_use]
    pub const fn new(settings: MarketSettings, pool: ShopPool) -> Self {
        Self { pool, settings }
    }

    async fn ensure_tables(&self) -> Result<Arc<sqlx::MySqlPool>, ShopError> {
        let pool = self.pool.get().await?;
        sqlx::query(CREATE_LISTINGS_TABLE)
            .execute(pool.as_ref())
            .await
            .map_err(db_err)?;
        Ok(pool)
    }

    #[must_use]
    pub fn slot_tiers(&self) -> &[MarketSlotTier] {
        &self.settings.slot_tiers
    }

    #[must_use]
    pub fn resolve_slot_tier(&self, held: &dyn Fn(&str) -> bool) -> u32 {
        self.settings
            .slot_tiers
            .iter()
            .filter(|t| t.permission.as_deref().is_none_or(held))
            .map(|t| t.max_listings)
            .max()
            .unwrap_or(0)
    }

    fn commission(&self, price: i64) -> i64 {
        let pct = (price as f64 * self.settings.commission_rate).floor() as i64;
        pct.max(self.settings.min_commission)
    }

    /// Creates a new listing, rejecting if the seller already has
    /// `max_listings` active (their resolved slot-tier limit).
    pub async fn create_listing(&self, new: NewListing<'_>) -> Result<i64, ShopError> {
        let pool = self.ensure_tables().await?;
        let count: i64 = sqlx::query(COUNT_ACTIVE_FOR_SELLER)
            .bind(new.seller.hyphenated().to_string())
            .fetch_one(pool.as_ref())
            .await
            .map_err(db_err)?
            .get("n");
        if count >= i64::from(new.max_listings) {
            return Err(ShopError::Other(format!(
                "you already have {count}/{} active listings",
                new.max_listings
            )));
        }

        let listed_at = now_unix();
        let result = sqlx::query(INSERT_LISTING)
            .bind(new.seller.hyphenated().to_string())
            .bind(new.seller_name)
            .bind(new.item)
            .bind(new.amount)
            .bind(new.currency)
            .bind(new.price)
            .bind(listed_at)
            .execute(pool.as_ref())
            .await
            .map_err(db_err)?;
        Ok(result.last_insert_id() as i64)
    }

    /// Every currently active listing (browse-all, not just online sellers -
    /// unlike `PixelShop`, Ember reads straight from the database so an
    /// offline seller's listing is just as buyable).
    pub async fn active_listings(&self) -> Result<Vec<Listing>, ShopError> {
        let pool = self.ensure_tables().await?;
        let rows = sqlx::query(SELECT_ACTIVE)
            .fetch_all(pool.as_ref())
            .await
            .map_err(db_err)?;
        Ok(rows.iter().map(row_to_listing).collect())
    }

    /// Atomically "claims" a listing (`DELETE ... LIMIT 1`, the same
    /// `rows_affected()`-checked primitive `EconomyManager::withdraw` uses)
    /// before charging the buyer - safe even if multiple Ember instances
    /// share this `MySQL` database, unlike `PixelShop`'s in-JVM
    /// `synchronized` lock. Re-inserts the listing as compensation if the
    /// buyer's payment then fails.
    pub async fn buy_listing(
        &self,
        listing_id: i64,
        buyer: Uuid,
        economy: &EconomyManager,
    ) -> Result<(String, u32), ShopError> {
        let pool = self.ensure_tables().await?;
        let Some(row) = sqlx::query(SELECT_ONE)
            .bind(listing_id)
            .fetch_optional(pool.as_ref())
            .await
            .map_err(db_err)?
        else {
            return Err(ShopError::Other("that listing is gone".to_string()));
        };
        let listing = row_to_listing(&row);

        let claimed = sqlx::query(DELETE_BY_ID)
            .bind(listing_id)
            .execute(pool.as_ref())
            .await
            .map_err(db_err)?;
        if claimed.rows_affected() != 1 {
            return Err(ShopError::Other(
                "someone else just bought that listing".to_string(),
            ));
        }

        if let Err(e) = economy
            .withdraw(buyer, Some(&listing.currency), listing.price)
            .await
        {
            // Compensate: put the listing back exactly as it was.
            let _ = sqlx::query(REINSERT_LISTING)
                .bind(listing.id)
                .bind(listing.seller_uuid.hyphenated().to_string())
                .bind(&listing.seller_name)
                .bind(&listing.item)
                .bind(listing.amount)
                .bind(&listing.currency)
                .bind(listing.price)
                .bind(listing.listed_at)
                .execute(pool.as_ref())
                .await;
            return Err(match e {
                crate::server::economy::EconomyError::InsufficientFunds { have, need } => {
                    ShopError::InsufficientFunds { have, need }
                }
                other => ShopError::Other(other.to_string()),
            });
        }

        let commission = self.commission(listing.price);
        let seller_proceeds = (listing.price - commission).max(0);
        if seller_proceeds > 0 {
            let _ = economy
                .deposit(
                    listing.seller_uuid,
                    Some(&listing.currency),
                    seller_proceeds,
                )
                .await;
        }

        Ok((listing.item, listing.amount))
    }

    /// Cancels the caller's own listing, returning the item info to give
    /// back. No fee for delisting.
    pub async fn cancel_listing(
        &self,
        listing_id: i64,
        owner: Uuid,
    ) -> Result<(String, u32), ShopError> {
        let pool = self.ensure_tables().await?;
        let Some(row) = sqlx::query(SELECT_ONE)
            .bind(listing_id)
            .fetch_optional(pool.as_ref())
            .await
            .map_err(db_err)?
        else {
            return Err(ShopError::Other("that listing is gone".to_string()));
        };
        let listing = row_to_listing(&row);
        if listing.seller_uuid != owner {
            return Err(ShopError::Other("that isn't your listing".to_string()));
        }

        let removed = sqlx::query(DELETE_OWNED)
            .bind(listing_id)
            .bind(owner.hyphenated().to_string())
            .execute(pool.as_ref())
            .await
            .map_err(db_err)?;
        if removed.rows_affected() != 1 {
            return Err(ShopError::Other("that listing is gone".to_string()));
        }

        Ok((listing.item, listing.amount))
    }
}
// EMBER end
