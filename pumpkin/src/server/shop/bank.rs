// EMBER start - built-in shop/bank/market/lottery system
//! Bank: deposit/withdraw, permission-tiered compound interest, transaction
//! log.

use pumpkin_config::{BankSettings, BankTier};
use sqlx::Row;
use uuid::Uuid;

use super::{ShopError, ShopPool, db_err};
use crate::server::economy::EconomyManager;

const CREATE_ACCOUNTS_TABLE: &str = concat!(
    "CREATE TABLE IF NOT EXISTS ember_bank_accounts (",
    "player_uuid CHAR(36) NOT NULL,",
    "currency VARCHAR(32) NOT NULL,",
    "balance BIGINT NOT NULL DEFAULT 0,",
    "last_settled_at BIGINT NOT NULL,",
    "PRIMARY KEY (player_uuid, currency)",
    ")"
);

const CREATE_TRANSACTIONS_TABLE: &str = concat!(
    "CREATE TABLE IF NOT EXISTS ember_bank_transactions (",
    "id BIGINT AUTO_INCREMENT PRIMARY KEY,",
    "player_uuid CHAR(36) NOT NULL,",
    "currency VARCHAR(32) NOT NULL,",
    "amount BIGINT NOT NULL,",
    "is_interest BOOLEAN NOT NULL DEFAULT FALSE,",
    "occurred_at BIGINT NOT NULL,",
    "INDEX idx_player (player_uuid, occurred_at)",
    ")"
);

const SELECT_ACCOUNT: &str = "SELECT balance, last_settled_at FROM ember_bank_accounts WHERE player_uuid = ? AND currency = ?";

const UPSERT_BALANCE: &str = concat!(
    "INSERT INTO ember_bank_accounts (player_uuid, currency, balance, last_settled_at) ",
    "VALUES (?, ?, ?, ?) ",
    "ON DUPLICATE KEY UPDATE balance = VALUES(balance), last_settled_at = VALUES(last_settled_at)"
);

const INSERT_TRANSACTION: &str = concat!(
    "INSERT INTO ember_bank_transactions (player_uuid, currency, amount, is_interest, occurred_at) ",
    "VALUES (?, ?, ?, ?, ?)"
);

const SELECT_RECENT_TRANSACTIONS: &str = concat!(
    "SELECT amount, is_interest, occurred_at FROM ember_bank_transactions ",
    "WHERE player_uuid = ? AND currency = ? ORDER BY occurred_at DESC LIMIT 10"
);

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64
}

pub struct BankAccount {
    pub balance: i64,
    pub tier: BankTier,
}

pub struct Transaction {
    pub amount: i64,
    pub is_interest: bool,
    pub occurred_at: i64,
}

pub struct BankManager {
    pool: ShopPool,
    settings: BankSettings,
}

impl BankManager {
    #[must_use]
    pub const fn new(settings: BankSettings, pool: ShopPool) -> Self {
        Self { pool, settings }
    }

    async fn ensure_tables(&self) -> Result<std::sync::Arc<sqlx::MySqlPool>, ShopError> {
        let pool = self.pool.get().await?;
        for stmt in [CREATE_ACCOUNTS_TABLE, CREATE_TRANSACTIONS_TABLE] {
            sqlx::query(stmt)
                .execute(pool.as_ref())
                .await
                .map_err(db_err)?;
        }
        Ok(pool)
    }

    /// All configured tiers, for the caller to check permissions against
    /// before calling [`Self::resolve_tier`].
    #[must_use]
    pub fn tiers(&self) -> &[BankTier] {
        &self.settings.tiers
    }

    /// The highest-`max_balance` tier among the ones a player with `held`
    /// (permissions they hold) qualifies for - always at least one tier
    /// applies, since the config guarantees a no-`permission` default.
    #[must_use]
    pub fn resolve_tier(&self, held: &dyn Fn(&str) -> bool) -> BankTier {
        self.settings
            .tiers
            .iter()
            .filter(|t| t.permission.as_deref().is_none_or(held))
            .max_by_key(|t| t.max_balance)
            .cloned()
            .unwrap_or(BankTier {
                permission: None,
                max_balance: 0,
                daily_rate: 0.0,
                max_interest_per_settlement: 0,
            })
    }

    /// Settles any owed compound interest (lazily, since `last_settled_at`)
    /// and returns the account's up-to-date state. Call this before every
    /// deposit/withdraw/GUI-open so the balance a player sees/acts on is
    /// always current.
    pub async fn settle_and_get(
        &self,
        player: Uuid,
        currency: &str,
        tier: &BankTier,
    ) -> Result<BankAccount, ShopError> {
        let pool = self.ensure_tables().await?;
        let row = sqlx::query(SELECT_ACCOUNT)
            .bind(player.hyphenated().to_string())
            .bind(currency)
            .fetch_optional(pool.as_ref())
            .await
            .map_err(db_err)?;

        let now = now_unix();
        let Some(row) = row else {
            return Ok(BankAccount {
                balance: 0,
                tier: tier.clone(),
            });
        };

        let balance: i64 = row.get("balance");
        let last_settled_at: i64 = row.get("last_settled_at");
        let days_elapsed = (now - last_settled_at) / 86400;
        if days_elapsed <= 0 || balance <= 0 {
            return Ok(BankAccount {
                balance,
                tier: tier.clone(),
            });
        }

        let compounded = (balance as f64) * (1.0 + tier.daily_rate).powi(days_elapsed as i32);
        let interest = (compounded - balance as f64)
            .round()
            .min(tier.max_interest_per_settlement as f64) as i64;
        let new_balance = balance + interest.max(0);
        let new_settled_at = last_settled_at + days_elapsed * 86400;

        sqlx::query(UPSERT_BALANCE)
            .bind(player.hyphenated().to_string())
            .bind(currency)
            .bind(new_balance)
            .bind(new_settled_at)
            .execute(pool.as_ref())
            .await
            .map_err(db_err)?;

        if interest > 0 {
            sqlx::query(INSERT_TRANSACTION)
                .bind(player.hyphenated().to_string())
                .bind(currency)
                .bind(interest)
                .bind(true)
                .bind(now)
                .execute(pool.as_ref())
                .await
                .map_err(db_err)?;
        }

        Ok(BankAccount {
            balance: new_balance,
            tier: tier.clone(),
        })
    }

    /// Moves `amount` from the player's wallet (via `economy`) into their
    /// bank balance. Rejects (without touching either balance) if it would
    /// push the bank balance over the resolved tier's `max_balance`.
    pub async fn deposit(
        &self,
        player: Uuid,
        currency: &str,
        amount: i64,
        tier: &BankTier,
        economy: &EconomyManager,
    ) -> Result<i64, ShopError> {
        let account = self.settle_and_get(player, currency, tier).await?;
        if account.balance + amount > tier.max_balance {
            return Err(ShopError::Other(format!(
                "deposit would exceed this account's cap of {}",
                tier.max_balance
            )));
        }

        economy
            .withdraw(player, Some(currency), amount)
            .await
            .map_err(|e| match e {
                crate::server::economy::EconomyError::InsufficientFunds { have, need } => {
                    ShopError::InsufficientFunds { have, need }
                }
                other => ShopError::Other(other.to_string()),
            })?;

        let pool = self.ensure_tables().await?;
        let new_balance = account.balance + amount;
        sqlx::query(UPSERT_BALANCE)
            .bind(player.hyphenated().to_string())
            .bind(currency)
            .bind(new_balance)
            .bind(now_unix())
            .execute(pool.as_ref())
            .await
            .map_err(db_err)?;
        sqlx::query(INSERT_TRANSACTION)
            .bind(player.hyphenated().to_string())
            .bind(currency)
            .bind(amount)
            .bind(false)
            .bind(now_unix())
            .execute(pool.as_ref())
            .await
            .map_err(db_err)?;

        Ok(new_balance)
    }

    /// Moves `amount` from the player's bank balance back into their wallet.
    /// Rejects (without touching either balance) if it exceeds the current
    /// bank balance - no overdraft, unlike a plain wallet-to-wallet pay.
    pub async fn withdraw(
        &self,
        player: Uuid,
        currency: &str,
        amount: i64,
        tier: &BankTier,
        economy: &EconomyManager,
    ) -> Result<i64, ShopError> {
        let account = self.settle_and_get(player, currency, tier).await?;
        if amount > account.balance {
            return Err(ShopError::InsufficientFunds {
                have: account.balance,
                need: amount,
            });
        }

        let pool = self.ensure_tables().await?;
        let new_balance = account.balance - amount;
        sqlx::query(UPSERT_BALANCE)
            .bind(player.hyphenated().to_string())
            .bind(currency)
            .bind(new_balance)
            .bind(now_unix())
            .execute(pool.as_ref())
            .await
            .map_err(db_err)?;
        sqlx::query(INSERT_TRANSACTION)
            .bind(player.hyphenated().to_string())
            .bind(currency)
            .bind(-amount)
            .bind(false)
            .bind(now_unix())
            .execute(pool.as_ref())
            .await
            .map_err(db_err)?;

        economy
            .deposit(player, Some(currency), amount)
            .await
            .map_err(|e| ShopError::Other(e.to_string()))?;

        Ok(new_balance)
    }

    /// The player's last 10 transactions (deposits/withdrawals/interest),
    /// most recent first - a self-check display, not an audit ledger.
    pub async fn recent_transactions(
        &self,
        player: Uuid,
        currency: &str,
    ) -> Result<Vec<Transaction>, ShopError> {
        let pool = self.ensure_tables().await?;
        let rows = sqlx::query(SELECT_RECENT_TRANSACTIONS)
            .bind(player.hyphenated().to_string())
            .bind(currency)
            .fetch_all(pool.as_ref())
            .await
            .map_err(db_err)?;
        Ok(rows
            .into_iter()
            .map(|r| Transaction {
                amount: r.get("amount"),
                is_interest: r.get("is_interest"),
                occurred_at: r.get("occurred_at"),
            })
            .collect())
    }
}
// EMBER end
