// EMBER start - built-in economy system
//! Multi-currency, `MySQL`-backed economy system. Deliberately server-builtin
//! rather than a plugin — see `EMBER.md`'s "服务端 vs 插件边界" section.
//!
//! Balances are the single source of truth in `MySQL` (no in-process cache),
//! so multiple servers can share one database safely. Balance-changing
//! queries use `UPDATE ... WHERE balance >= ?` (checked via `rows_affected`)
//! rather than a read-then-write round trip, so concurrent withdrawals on
//! the same account can never over-draw it.

use std::collections::HashSet;
use std::sync::Arc;

use pumpkin_config::EconomyConfig;
use sqlx::mysql::MySqlPoolOptions;
use tracing::error;
use uuid::Uuid;

const CREATE_TABLE: &str = concat!(
    "CREATE TABLE IF NOT EXISTS ember_economy_balances (",
    "player_uuid CHAR(36) NOT NULL,",
    "currency VARCHAR(32) NOT NULL,",
    "balance BIGINT NOT NULL DEFAULT 0,",
    "PRIMARY KEY (player_uuid, currency)",
    ")"
);

const SELECT_BALANCE: &str =
    "SELECT balance FROM ember_economy_balances WHERE player_uuid = ? AND currency = ?";

/// No-op upsert: guarantees the row exists (at `starting_balance`) without
/// touching an existing balance, so a conditional `UPDATE` always has a row
/// to match against even for a player who has never held this currency.
const ENSURE_ROW: &str = concat!(
    "INSERT INTO ember_economy_balances (player_uuid, currency, balance) VALUES (?, ?, ?) ",
    "ON DUPLICATE KEY UPDATE balance = balance"
);

/// Atomic conditional deduction: `rows_affected() == 1` iff `balance >= ?`
/// held at the moment this ran, regardless of anything read earlier.
const WITHDRAW: &str = concat!(
    "UPDATE ember_economy_balances SET balance = balance - ? ",
    "WHERE player_uuid = ? AND currency = ? AND balance >= ?"
);

const DEPOSIT: &str = concat!(
    "INSERT INTO ember_economy_balances (player_uuid, currency, balance) VALUES (?, ?, ?) ",
    "ON DUPLICATE KEY UPDATE balance = balance + VALUES(balance)"
);

const SET_BALANCE: &str = concat!(
    "INSERT INTO ember_economy_balances (player_uuid, currency, balance) VALUES (?, ?, ?) ",
    "ON DUPLICATE KEY UPDATE balance = VALUES(balance)"
);

#[derive(thiserror::Error, Debug)]
pub enum EconomyError {
    #[error("the economy system is not enabled ([economy] enabled = true in the config)")]
    Disabled,
    #[error("unknown currency: {0}")]
    UnknownCurrency(String),
    #[error("insufficient funds: has {have}, needs {need}")]
    InsufficientFunds { have: i64, need: i64 },
    #[error("economy database error: {0}")]
    Database(String),
}

fn db_err(e: impl std::fmt::Display) -> EconomyError {
    EconomyError::Database(e.to_string())
}

pub struct EconomyManager {
    enabled: bool,
    url: String,
    pool: Arc<tokio::sync::OnceCell<Arc<sqlx::MySqlPool>>>,
    currencies: HashSet<String>,
    default_currency: String,
    starting_balance: i64,
}

impl EconomyManager {
    #[must_use]
    pub fn new(config: &EconomyConfig) -> Self {
        let manager = Self {
            enabled: config.enabled,
            url: config.url.clone(),
            pool: Arc::new(tokio::sync::OnceCell::new()),
            currencies: config.currencies.iter().map(|c| c.id.clone()).collect(),
            default_currency: config.default_currency.clone(),
            starting_balance: config.starting_balance,
        };

        if manager.enabled
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            // Eagerly connect (and create the table) in the background so a
            // bad URL/unreachable database fails loudly at startup instead
            // of on the first command that touches it.
            let pool_cell = manager.pool.clone();
            let url = manager.url.clone();
            handle.spawn(async move {
                if let Err(e) = pool_cell.get_or_try_init(|| Self::init_pool(&url)).await {
                    error!("Economy MySQL eager init failed (check [economy] url): {e}");
                }
            });
        }

        manager
    }

    async fn init_pool(url: &str) -> Result<Arc<sqlx::MySqlPool>, EconomyError> {
        let pool = MySqlPoolOptions::new()
            .max_connections(8)
            .connect(url)
            .await
            .map_err(db_err)?;
        sqlx::query(CREATE_TABLE)
            .execute(&pool)
            .await
            .map_err(db_err)?;
        Ok(Arc::new(pool))
    }

    async fn ensure_pool(&self) -> Result<&Arc<sqlx::MySqlPool>, EconomyError> {
        if !self.enabled {
            return Err(EconomyError::Disabled);
        }
        self.pool
            .get_or_try_init(|| Self::init_pool(&self.url))
            .await
    }

    fn check_currency<'a>(&'a self, currency: Option<&'a str>) -> Result<&'a str, EconomyError> {
        let currency = currency.unwrap_or(&self.default_currency);
        if self.currencies.contains(currency) {
            Ok(currency)
        } else {
            Err(EconomyError::UnknownCurrency(currency.to_string()))
        }
    }

    /// The default currency (used when a caller doesn't specify one).
    #[must_use]
    pub fn default_currency(&self) -> &str {
        &self.default_currency
    }

    /// All configured currency ids.
    pub fn currencies(&self) -> impl Iterator<Item = &str> {
        self.currencies.iter().map(String::as_str)
    }

    /// Current balance, or the configured starting balance if the player has
    /// never held this currency (accounts are created lazily on first write).
    pub async fn get_balance(&self, id: Uuid, currency: Option<&str>) -> Result<i64, EconomyError> {
        let currency = self.check_currency(currency)?;
        let pool = self.ensure_pool().await?;
        let row: Option<(i64,)> = sqlx::query_as(SELECT_BALANCE)
            .bind(id.hyphenated().to_string())
            .bind(currency)
            .fetch_optional(pool.as_ref())
            .await
            .map_err(db_err)?;
        Ok(row.map_or(self.starting_balance, |(balance,)| balance))
    }

    /// Adds `amount` (must be positive) to `id`'s balance. Returns the new
    /// balance.
    pub async fn deposit(
        &self,
        id: Uuid,
        currency: Option<&str>,
        amount: i64,
    ) -> Result<i64, EconomyError> {
        debug_assert!(amount > 0, "deposit amount must be positive");
        let currency = self.check_currency(currency)?;
        let pool = self.ensure_pool().await?;
        let uuid = id.hyphenated().to_string();
        sqlx::query(DEPOSIT)
            .bind(&uuid)
            .bind(currency)
            .bind(amount)
            .execute(pool.as_ref())
            .await
            .map_err(db_err)?;
        self.get_balance(id, Some(currency)).await
    }

    /// Atomically verifies `id` has at least `amount` of `currency` and
    /// deducts it in the same statement. Returns the new balance, or
    /// `InsufficientFunds` (never lets the balance go negative) without
    /// changing anything.
    pub async fn withdraw(
        &self,
        id: Uuid,
        currency: Option<&str>,
        amount: i64,
    ) -> Result<i64, EconomyError> {
        debug_assert!(amount > 0, "withdraw amount must be positive");
        let currency = self.check_currency(currency)?;
        let pool = self.ensure_pool().await?;
        let uuid = id.hyphenated().to_string();

        sqlx::query(ENSURE_ROW)
            .bind(&uuid)
            .bind(currency)
            .bind(self.starting_balance)
            .execute(pool.as_ref())
            .await
            .map_err(db_err)?;

        let result = sqlx::query(WITHDRAW)
            .bind(amount)
            .bind(&uuid)
            .bind(currency)
            .bind(amount)
            .execute(pool.as_ref())
            .await
            .map_err(db_err)?;

        if result.rows_affected() == 1 {
            self.get_balance(id, Some(currency)).await
        } else {
            let have = self.get_balance(id, Some(currency)).await?;
            Err(EconomyError::InsufficientFunds { have, need: amount })
        }
    }

    /// Directly sets `id`'s balance (admin operation; not conditional on the
    /// current balance).
    pub async fn set_balance(
        &self,
        id: Uuid,
        currency: Option<&str>,
        amount: i64,
    ) -> Result<(), EconomyError> {
        debug_assert!(amount >= 0, "balance cannot be set negative");
        let currency = self.check_currency(currency)?;
        let pool = self.ensure_pool().await?;
        sqlx::query(SET_BALANCE)
            .bind(id.hyphenated().to_string())
            .bind(currency)
            .bind(amount)
            .execute(pool.as_ref())
            .await
            .map_err(db_err)?;
        Ok(())
    }

    /// Resets `id`'s balance back to the configured starting balance.
    pub async fn reset_balance(
        &self,
        id: Uuid,
        currency: Option<&str>,
    ) -> Result<(), EconomyError> {
        let starting_balance = self.starting_balance;
        self.set_balance(id, currency, starting_balance).await
    }

    /// Moves `amount` from `from` to `to` atomically: either both sides
    /// happen or neither does (a real `sqlx` transaction — the one place in
    /// this system that needs cross-row atomicity a single statement can't
    /// express).
    pub async fn transfer(
        &self,
        from: Uuid,
        to: Uuid,
        currency: Option<&str>,
        amount: i64,
    ) -> Result<(), EconomyError> {
        debug_assert!(amount > 0, "transfer amount must be positive");
        let currency = self.check_currency(currency)?;
        let pool = self.ensure_pool().await?;
        let from_uuid = from.hyphenated().to_string();
        let to_uuid = to.hyphenated().to_string();

        let mut tx = pool.begin().await.map_err(db_err)?;

        sqlx::query(ENSURE_ROW)
            .bind(&from_uuid)
            .bind(currency)
            .bind(self.starting_balance)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;

        let result = sqlx::query(WITHDRAW)
            .bind(amount)
            .bind(&from_uuid)
            .bind(currency)
            .bind(amount)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;

        if result.rows_affected() != 1 {
            tx.rollback().await.map_err(db_err)?;
            let have = self.get_balance(from, Some(currency)).await?;
            return Err(EconomyError::InsufficientFunds { have, need: amount });
        }

        sqlx::query(DEPOSIT)
            .bind(&to_uuid)
            .bind(currency)
            .bind(amount)
            .execute(&mut *tx)
            .await
            .map_err(db_err)?;

        tx.commit().await.map_err(db_err)?;
        Ok(())
    }
}

/// Integration tests against a real `MySQL` instance. Not run by normal
/// `cargo test`/`nextest` (and so not part of CI, matching how `EasyWorld`'s
/// own `MySQL` mode is verified via a separate opt-in script rather than the
/// regular suite) — run explicitly with:
/// `EMBER_ECONOMY_TEST_MYSQL_URL=mysql://user:pass@host:port/db cargo test -p pumpkin --lib server::economy::tests -- --ignored`
#[cfg(test)]
mod tests {
    use super::*;

    fn test_url() -> String {
        std::env::var("EMBER_ECONOMY_TEST_MYSQL_URL").unwrap_or_else(|_| {
            "mysql://root:password@127.0.0.1:3306/ember_economy_test".to_string()
        })
    }

    /// Builds a manager against a freshly-emptied test database (creating
    /// the database itself on first run).
    async fn fresh_manager() -> EconomyManager {
        let url = test_url();
        let (base_url, db_name) = url
            .rsplit_once('/')
            .expect("test MySQL URL must end in /<database>");

        let admin_pool = MySqlPoolOptions::new()
            .max_connections(1)
            .connect(&format!("{base_url}/"))
            .await
            .expect("connect to MySQL server (no database selected) for test setup");
        sqlx::query(&format!("CREATE DATABASE IF NOT EXISTS {db_name}"))
            .execute(&admin_pool)
            .await
            .expect("create test database");
        admin_pool.close().await;

        let config = EconomyConfig {
            enabled: true,
            url,
            ..EconomyConfig::default()
        };
        let manager = EconomyManager::new(&config);
        // Empty out any rows a previous run left behind so tests are repeatable.
        let pool = manager
            .ensure_pool()
            .await
            .expect("manager should connect to the test database");
        sqlx::query("DELETE FROM ember_economy_balances")
            .execute(pool.as_ref())
            .await
            .expect("clear previous test rows");
        manager
    }

    #[tokio::test]
    #[ignore = "requires a local MySQL instance; see module docs for how to run"]
    async fn deposit_and_withdraw_roundtrip() {
        let manager = fresh_manager().await;
        let player = Uuid::new_v4();

        assert_eq!(manager.get_balance(player, None).await.unwrap(), 0);
        assert_eq!(manager.deposit(player, None, 100).await.unwrap(), 100);
        assert_eq!(manager.withdraw(player, None, 40).await.unwrap(), 60);
        assert_eq!(manager.get_balance(player, None).await.unwrap(), 60);
    }

    #[tokio::test]
    #[ignore = "requires a local MySQL instance; see module docs for how to run"]
    async fn withdraw_insufficient_funds_leaves_balance_unchanged() {
        let manager = fresh_manager().await;
        let player = Uuid::new_v4();
        manager.deposit(player, None, 50).await.unwrap();

        let err = manager.withdraw(player, None, 100).await.unwrap_err();
        assert!(matches!(
            err,
            EconomyError::InsufficientFunds {
                have: 50,
                need: 100
            }
        ));
        // The balance must be untouched - this is the core requirement: a
        // rejected withdrawal must never partially apply.
        assert_eq!(manager.get_balance(player, None).await.unwrap(), 50);
    }

    #[tokio::test]
    #[ignore = "requires a local MySQL instance; see module docs for how to run"]
    async fn concurrent_withdrawals_never_overdraw() {
        let manager = fresh_manager().await;
        let player = Uuid::new_v4();
        manager.deposit(player, None, 100).await.unwrap();

        // Two concurrent withdrawals of 60 against a balance of 100: exactly
        // one may succeed. If the atomic `UPDATE ... WHERE balance >= ?`
        // guard didn't hold, both could succeed and leave a negative balance.
        let (r1, r2) = tokio::join!(
            manager.withdraw(player, None, 60),
            manager.withdraw(player, None, 60),
        );

        let successes = usize::from(r1.is_ok()) + usize::from(r2.is_ok());
        assert_eq!(
            successes, 1,
            "exactly one of two concurrent 60-withdrawals from a balance of \
             100 should succeed, got {r1:?} / {r2:?}"
        );

        let final_balance = manager.get_balance(player, None).await.unwrap();
        assert_eq!(
            final_balance, 40,
            "balance must reflect exactly one successful 60-withdrawal from 100"
        );
    }

    #[tokio::test]
    #[ignore = "requires a local MySQL instance; see module docs for how to run"]
    async fn transfer_moves_money_atomically() {
        let manager = fresh_manager().await;
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        manager.deposit(alice, None, 100).await.unwrap();

        manager.transfer(alice, bob, None, 30).await.unwrap();
        assert_eq!(manager.get_balance(alice, None).await.unwrap(), 70);
        assert_eq!(manager.get_balance(bob, None).await.unwrap(), 30);

        // A transfer that exceeds the sender's balance must fail and leave
        // both balances exactly as they were.
        let err = manager.transfer(alice, bob, None, 1000).await.unwrap_err();
        assert!(matches!(err, EconomyError::InsufficientFunds { .. }));
        assert_eq!(manager.get_balance(alice, None).await.unwrap(), 70);
        assert_eq!(manager.get_balance(bob, None).await.unwrap(), 30);
    }

    #[tokio::test]
    #[ignore = "requires a local MySQL instance; see module docs for how to run"]
    async fn unknown_currency_is_rejected() {
        let manager = fresh_manager().await;
        let player = Uuid::new_v4();
        let err = manager
            .get_balance(player, Some("not_a_real_currency"))
            .await
            .unwrap_err();
        assert!(matches!(err, EconomyError::UnknownCurrency(c) if c == "not_a_real_currency"));
    }
}
// EMBER end
