// EMBER start - built-in shop/bank/market/lottery system
//! Bank: deposit/withdraw, permission-tiered compound interest, transaction
//! log.

use pumpkin_config::{BankSettings, BankTier};
use pumpkin_util::text::TextComponent;
use pumpkin_util::translation::{Locale, get_translation_text};
use sqlx::Row;
use uuid::Uuid;

use super::{ShopError, ShopPool, db_err};
use crate::server::economy::{EconomyError, EconomyManager};

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

/// Mirrors `commands::economy`'s translated wording (reusing the identical
/// `ember:commands.economy.*`/`ember:commands.bank.*` keys) for the rare
/// case where an underlying `EconomyManager` call fails for a reason other
/// than insufficient funds while moving money into/out of a bank account -
/// keeps the message properly localized instead of leaking `EconomyError`'s
/// internal English `Display` text to the player. `InsufficientFunds` can't
/// actually reach here today (both call sites below intercept it before
/// falling back to this), but is still handled for exhaustiveness and to
/// stay correct if that ever changes.
fn translate_economy_error(e: EconomyError, currency: &str, locale: Locale) -> String {
    match e {
        EconomyError::Disabled => {
            get_translation_text("ember:commands.economy.disabled", locale, vec![])
        }
        EconomyError::UnknownCurrency(c) => get_translation_text(
            "ember:commands.economy.unknown_currency",
            locale,
            vec![TextComponent::text(c).0],
        ),
        EconomyError::InsufficientFunds { have, need } => get_translation_text(
            "ember:commands.bank.insufficient_funds",
            locale,
            vec![
                TextComponent::text(currency.to_string()).0,
                TextComponent::text(have.to_string()).0,
                TextComponent::text(need.to_string()).0,
            ],
        ),
        EconomyError::Database(e) => get_translation_text(
            "ember:commands.economy.database_error",
            locale,
            vec![TextComponent::text(e).0],
        ),
    }
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
    /// Serializes the settle-then-mutate sequence for bank accounts.
    /// `settle_and_get`/`deposit`/`withdraw` are each a read-then-write
    /// against `MySQL` with no DB-side atomic guard (unlike
    /// `EconomyManager::withdraw`'s conditional `UPDATE`), and every chat
    /// command runs as its own unserialized tokio task
    /// (`net/java/play.rs`'s `handle_chat_command`) - without this, two
    /// concurrent commands against the same account could both read the
    /// same stale balance before either writes, duplicating or losing
    /// money. One coarse lock (rather than per-account striping) is used
    /// since bank commands are infrequent, human-paced operations where
    /// full serialization has no perceptible cost.
    lock: tokio::sync::Mutex<()>,
}

impl BankManager {
    #[must_use]
    pub fn new(settings: BankSettings, pool: ShopPool) -> Self {
        Self {
            pool,
            settings,
            lock: tokio::sync::Mutex::new(()),
        }
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
        let _guard = self.lock.lock().await;
        self.settle_and_get_locked(player, currency, tier).await
    }

    /// Same settlement logic as [`Self::settle_and_get`], but assumes the
    /// caller already holds `self.lock` - used by `deposit`/`withdraw` so
    /// their settle-then-mutate sequence is one atomic critical section
    /// instead of two (re-locking here would deadlock against a
    /// non-reentrant `tokio::sync::Mutex`).
    async fn settle_and_get_locked(
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
        locale: Locale,
    ) -> Result<i64, ShopError> {
        let _guard = self.lock.lock().await;
        let account = self.settle_and_get_locked(player, currency, tier).await?;
        if account.balance + amount > tier.max_balance {
            return Err(ShopError::Other(get_translation_text(
                "ember:commands.bank.deposit_cap_exceeded",
                locale,
                vec![TextComponent::text(tier.max_balance.to_string()).0],
            )));
        }

        economy
            .withdraw(player, Some(currency), amount)
            .await
            .map_err(|e| match e {
                crate::server::economy::EconomyError::InsufficientFunds { have, need } => {
                    ShopError::InsufficientFunds { have, need }
                }
                other => ShopError::Other(translate_economy_error(other, currency, locale)),
            })?;

        let pool = self.ensure_tables().await?;
        let new_balance = account.balance + amount;
        if let Err(e) = sqlx::query(UPSERT_BALANCE)
            .bind(player.hyphenated().to_string())
            .bind(currency)
            .bind(new_balance)
            .bind(now_unix())
            .execute(pool.as_ref())
            .await
        {
            // Compensate: the wallet was already debited above but the bank
            // side never credited - give the money back rather than
            // destroying it.
            let _ = economy.deposit(player, Some(currency), amount).await;
            return Err(db_err(e));
        }
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
        locale: Locale,
    ) -> Result<i64, ShopError> {
        let _guard = self.lock.lock().await;
        let account = self.settle_and_get_locked(player, currency, tier).await?;
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

        if let Err(e) = economy.deposit(player, Some(currency), amount).await {
            // Compensate: the bank side above already committed the debit,
            // but the wallet was never credited - put the bank balance back
            // rather than destroying the money, and log a matching
            // reversal so the transaction log still reconciles with the
            // true final balance.
            let _ = sqlx::query(UPSERT_BALANCE)
                .bind(player.hyphenated().to_string())
                .bind(currency)
                .bind(account.balance)
                .bind(now_unix())
                .execute(pool.as_ref())
                .await;
            let _ = sqlx::query(INSERT_TRANSACTION)
                .bind(player.hyphenated().to_string())
                .bind(currency)
                .bind(amount)
                .bind(false)
                .bind(now_unix())
                .execute(pool.as_ref())
                .await;
            return Err(ShopError::Other(translate_economy_error(
                e, currency, locale,
            )));
        }

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

/// Integration tests against a real `MySQL` instance. Not run by normal
/// `cargo test`/`nextest` (matching `server::economy`'s own `#[ignore]`d
/// `MySQL` tests) - run explicitly with:
/// `EMBER_BANK_TEST_MYSQL_URL=mysql://user:pass@host:port/db cargo test -p pumpkin --lib server::shop::bank::tests -- --ignored`
#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::economy::EconomyManager;
    use pumpkin_config::{EconomyConfig, ShopSystemConfig};

    fn test_url() -> String {
        std::env::var("EMBER_BANK_TEST_MYSQL_URL")
            .unwrap_or_else(|_| "mysql://root:password@127.0.0.1:3306/ember_bank_test".to_string())
    }

    fn test_tier() -> BankTier {
        BankTier {
            permission: None,
            max_balance: 1_000_000,
            daily_rate: 0.0,
            max_interest_per_settlement: 0,
        }
    }

    /// Builds a `BankManager` plus the `EconomyManager` it moves wallet
    /// money through, both pointed at the same freshly-ensured test
    /// database (creating it on first run) - mirrors
    /// `server::economy::tests::fresh_manager`.
    async fn fresh_managers() -> (BankManager, EconomyManager) {
        let url = test_url();
        let (base_url, db_name) = url
            .rsplit_once('/')
            .expect("test MySQL URL must end in /<database>");

        let admin_pool = sqlx::mysql::MySqlPoolOptions::new()
            .max_connections(1)
            .connect(&format!("{base_url}/"))
            .await
            .expect("connect to MySQL server (no database selected) for test setup");
        sqlx::query(&format!("CREATE DATABASE IF NOT EXISTS {db_name}"))
            .execute(&admin_pool)
            .await
            .expect("create test database");
        admin_pool.close().await;

        let shop_config = ShopSystemConfig {
            enabled: true,
            url: url.clone(),
            ..ShopSystemConfig::default()
        };
        let bank = BankManager::new(shop_config.bank.clone(), ShopPool::new(&shop_config));

        let economy_config = EconomyConfig {
            enabled: true,
            url,
            ..EconomyConfig::default()
        };
        let economy = EconomyManager::from_config(&economy_config);

        (bank, economy)
    }

    #[tokio::test]
    #[ignore = "requires a local MySQL instance; see module docs for how to run"]
    async fn concurrent_withdrawals_never_duplicate_money() {
        let (bank, economy) = fresh_managers().await;
        let player = Uuid::new_v4();
        let tier = test_tier();

        economy.deposit(player, None, 100).await.unwrap();
        bank.deposit(player, "coins", 100, &tier, &economy, Locale::EnUs)
            .await
            .unwrap();

        // Two concurrent withdrawals of 80 against a bank balance of 100:
        // exactly one may succeed. Before the `self.lock` fix, both could
        // read the same stale balance=100, both pass the `80 > 100`
        // insufficient-funds check, and both credit the wallet - creating
        // money that was never in the bank ledger to begin with.
        let (r1, r2) = tokio::join!(
            bank.withdraw(player, "coins", 80, &tier, &economy, Locale::EnUs),
            bank.withdraw(player, "coins", 80, &tier, &economy, Locale::EnUs),
        );

        let successes = usize::from(r1.is_ok()) + usize::from(r2.is_ok());
        assert_eq!(
            successes, 1,
            "exactly one of two concurrent 80-withdrawals from a bank balance of \
             100 should succeed, got {r1:?} / {r2:?}"
        );

        let bank_balance = bank
            .settle_and_get(player, "coins", &tier)
            .await
            .unwrap()
            .balance;
        let wallet_balance = economy.get_balance(player, None).await.unwrap();

        assert_eq!(
            bank_balance, 20,
            "bank ledger must reflect exactly one successful 80-withdrawal from 100"
        );
        assert_eq!(
            wallet_balance, 80,
            "wallet must be credited exactly once - a value of 160 here would mean \
             the race duplicated money"
        );
    }
}
// EMBER end
