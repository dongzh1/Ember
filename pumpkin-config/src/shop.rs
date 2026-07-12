use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::LoadConfiguration;

// EMBER start - built-in shop/bank/market/lottery system
/// Configuration for Ember's built-in shop system.
///
/// Shops, bank, market, and lottery all share one `MySQL` connection and one
/// config file - the same "own folder" convention as `EconomyConfig` (see
/// `EMBER.md`'s "服务端 vs 插件边界" section for why this lives server-side
/// rather than as a plugin). Per-shop item listings live in
/// `shop/shops.toml` and lottery pools in `shop/lottery.toml` - both
/// separate files since they're arbitrarily-long named lists, not fixed
/// settings.
#[derive(Deserialize, Serialize, Clone, Default)]
#[serde(default)]
pub struct ShopSystemConfig {
    /// Whether the shop system is active. Off by default, like the economy
    /// system - this feature requires `MySQL`.
    pub enabled: bool,
    /// `MySQL` connection URL, shared by shop/bank/market/lottery.
    pub url: String,
    pub shop: ShopSettings,
    pub bank: BankSettings,
    pub market: MarketSettings,
}

impl LoadConfiguration for ShopSystemConfig {
    fn get_path() -> &'static Path {
        Path::new("shop/shop.toml")
    }

    fn validate(&self) {}
}

/// Dynamic-pricing and redemption knobs, shared by every shop in `shops.toml`.
#[derive(Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct ShopSettings {
    /// Buy price = sell price + `max(1, ceil(sell_price * buy_markup_pct))`.
    pub buy_markup_pct: f64,
    /// Every this many units sold, the sell price decays by `price_decay_pct`.
    pub decay_every_n_sold: u32,
    /// Fraction knocked off the current sell price at each decay step (e.g.
    /// `0.01` = 1%).
    pub price_decay_pct: f64,
    /// Floor for a decayed price, as a fraction of the item's configured
    /// base sell price (e.g. `0.1` = never decays below 10% of base).
    pub min_price_pct: f64,
    /// Daily recovery: a decayed price moves toward `base_sell_price` by
    /// this multiple of its current (decayed) price, capped at the base.
    pub daily_recovery_multiplier: f64,
    /// How long a sold item stays redeemable, in hours.
    pub redeemable_expiry_hours: i64,
}

impl Default for ShopSettings {
    fn default() -> Self {
        Self {
            buy_markup_pct: 0.10,
            decay_every_n_sold: 10,
            price_decay_pct: 0.01,
            min_price_pct: 0.1,
            daily_recovery_multiplier: 1.5,
            redeemable_expiry_hours: 24,
        }
    }
}

/// One interest tier.
///
/// A player qualifies for every tier whose `permission` they hold, plus the
/// one tier with no `permission` at all (the default); the tier with the
/// highest `max_balance` among those wins.
#[derive(Deserialize, Serialize, Clone)]
pub struct BankTier {
    pub permission: Option<String>,
    pub max_balance: i64,
    pub daily_rate: f64,
    pub max_interest_per_settlement: i64,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct BankSettings {
    pub tiers: Vec<BankTier>,
}

impl Default for BankSettings {
    fn default() -> Self {
        Self {
            tiers: vec![BankTier {
                permission: None,
                max_balance: 100_000,
                daily_rate: 0.001,
                max_interest_per_settlement: 500,
            }],
        }
    }
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct MarketSettings {
    pub commission_rate: f64,
    pub min_commission: i64,
    /// Max active listings per player; same permission-tier resolution as
    /// `BankTier` (highest-limit tier the player qualifies for wins).
    pub slot_tiers: Vec<MarketSlotTier>,
}

impl Default for MarketSettings {
    fn default() -> Self {
        Self {
            commission_rate: 0.05,
            min_commission: 1,
            slot_tiers: vec![MarketSlotTier {
                permission: None,
                max_listings: 5,
            }],
        }
    }
}

#[derive(Deserialize, Serialize, Clone)]
pub struct MarketSlotTier {
    pub permission: Option<String>,
    pub max_listings: u32,
}

/// The list of configured shops, `shop/shops.toml`. A separate file from
/// `shop/shop.toml` since it's an arbitrarily-long named list, not fixed
/// settings - same reasoning as `NpcConfig`/`npc/npcs.json`.
#[derive(Deserialize, Serialize, Default, Clone)]
#[serde(transparent)]
pub struct ShopListConfig {
    pub shops: Vec<ShopConfig>,
}

impl LoadConfiguration for ShopListConfig {
    fn get_path() -> &'static Path {
        Path::new("shop/shops.toml")
    }

    fn validate(&self) {}
}

#[derive(Deserialize, Serialize, Clone)]
pub struct ShopConfig {
    pub name: String,
    pub title: String,
    pub items: Vec<ShopItem>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct ShopItem {
    /// Vanilla item resource name (e.g. `diamond_sword`).
    pub item: String,
    /// `None` uses the economy system's default currency.
    #[serde(default)]
    pub currency: Option<String>,
    /// `None`/absent means this item can't be bought.
    #[serde(default)]
    pub base_sell_price: Option<i64>,
    /// `None`/absent means this item can't be sold to the shop.
    #[serde(default)]
    pub base_buy_price: Option<i64>,
    /// Daily purchase cap per player. `-1` (the default) means unlimited.
    #[serde(default = "default_limit")]
    pub limit: i64,
    /// Hides this item's slot from players without this permission.
    #[serde(default)]
    pub permission: Option<String>,
}

const fn default_limit() -> i64 {
    -1
}

/// The list of configured lottery pools, `shop/lottery.toml` - same
/// "own file, arbitrarily-long named list" reasoning as `ShopListConfig`.
#[derive(Deserialize, Serialize, Default, Clone)]
#[serde(transparent)]
pub struct LotteryListConfig {
    pub pools: Vec<LotteryPoolConfig>,
}

impl LoadConfiguration for LotteryListConfig {
    fn get_path() -> &'static Path {
        Path::new("shop/lottery.toml")
    }

    fn validate(&self) {}
}

#[derive(Deserialize, Serialize, Clone)]
pub struct LotteryPoolConfig {
    pub name: String,
    pub title: String,
    #[serde(default)]
    pub permission: Option<String>,
    /// `-1` means unlimited draws per player per day.
    #[serde(default = "default_limit")]
    pub daily_limit: i64,
    pub cost_currency: Option<String>,
    pub cost_amount: i64,
    #[serde(default)]
    pub pity: Option<LotteryPity>,
    pub prizes: Vec<LotteryPrize>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct LotteryPity {
    /// Draws since the last pity-group win before one is forced.
    pub threshold: u32,
    /// Only prizes tagged with this `group` are eligible once pity triggers.
    pub group: String,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct LotteryPrize {
    /// Vanilla item resource name.
    pub item: String,
    pub amount: u32,
    /// Weighted-random weight; not a percentage, just relative to the pool's
    /// other prizes.
    pub weight: u32,
    /// Tag used by [`LotteryPity`]'s `group` to mark this as a pity prize.
    #[serde(default)]
    pub group: Option<String>,
    /// Server-wide broadcast on a win, `{player}`/`{prize}` placeholders.
    #[serde(default)]
    pub broadcast_message: Option<String>,
}
// EMBER end
