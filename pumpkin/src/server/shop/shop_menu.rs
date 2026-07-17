// EMBER start - built-in shop/bank/market/lottery system
//! The `/shop <name>` GUI.
//!
//! A `Generic9x3` container where the top two rows are configured items
//! (left-click buys 1) and the bottom row holds a "sell held item" slot and
//! a "redeem" slot. Never allows real item movement in the container area -
//! every click is intercepted in `ShopScreenHandler::on_slot_click` and
//! dispatched to shop business logic instead of the default pickup/place
//! behavior.

use std::any::Any;
use std::str::FromStr;
use std::sync::Arc;

use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::screen::WindowType;
use pumpkin_inventory::screen_handler::{
    InventoryPlayer, ItemStackFuture, ScreenHandler, ScreenHandlerBehaviour, ScreenHandlerFactory,
    ScreenHandlerFuture, SharedScreenHandler,
};
use pumpkin_inventory::slot::NormalSlot;
use pumpkin_protocol::java::server::play::SlotActionType;
use pumpkin_util::text::TextComponent;
use pumpkin_util::translation::{Locale, get_translation_text};
use pumpkin_world::inventory::Inventory;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::entity::player::Player;
use crate::plugin::api::gui::PluginInventory;
use crate::server::economy::EconomyManager;

use super::basic_shop::ShopManager;
use super::gui::{ClickKind, classify_click};

const ITEM_SLOTS: usize = 18;
const SELL_SLOT: usize = 18;
const REDEEM_SLOT: usize = 22;
const TOTAL_SLOTS: usize = 27;

/// Builds one shop item's slot label - the item name plus whichever of
/// "Buy"/"Sell" prices are configured for it (mirrors the presence checks
/// `build_inventory` already did), localized to `locale`.
///
/// # Note
/// Fixes a pre-existing display bug in the helper this replaced: the old
/// `price_name(base, price, currency, verb)` always re-appended
/// `: {buy_price} {currency}` after `verb` (which already contained
/// "Buy: X, Sell: Y"), so a fully-priced item showed the buy price twice
/// (e.g. "Diamond - Buy: 100, Sell: 50: 100 ") with a stray trailing space
/// from the always-empty `currency` argument. No price/label data is lost
/// here versus the original intent - just the accidental duplication.
fn item_label(
    label: &str,
    buy_price: i64,
    sell_price: i64,
    has_buy: bool,
    has_sell: bool,
    currency: &str,
    locale: Locale,
) -> String {
    let mut parts = Vec::new();
    if has_buy {
        parts.push(get_translation_text(
            "ember:commands.shop.item_buy_price",
            locale,
            vec![TextComponent::text(buy_price.to_string()).0],
        ));
    }
    if has_sell {
        parts.push(get_translation_text(
            "ember:commands.shop.item_sell_price",
            locale,
            vec![TextComponent::text(sell_price.to_string()).0],
        ));
    }
    if parts.is_empty() {
        return label.to_string();
    }
    get_translation_text(
        "ember:commands.shop.item_label",
        locale,
        vec![
            TextComponent::text(label.to_string()).0,
            TextComponent::text(parts.join(", ")).0,
            TextComponent::text(currency.to_string()).0,
        ],
    )
}

async fn build_inventory(
    shop: &ShopManager,
    shop_name: &str,
    player: Uuid,
    economy: &EconomyManager,
    locale: Locale,
) -> Arc<PluginInventory> {
    let inventory = Arc::new(PluginInventory::new(TOTAL_SLOTS));
    let Some(config) = shop.find_shop(shop_name) else {
        return inventory;
    };

    for (i, entry) in config.items.iter().take(ITEM_SLOTS).enumerate() {
        let Some(item) = pumpkin_data::item::Item::from_registry_key(&entry.item) else {
            continue;
        };
        let (sell_price, buy_price) = shop.prices(shop_name, entry).await.unwrap_or((0, 0));
        let mut stack = ItemStack::new(1, item);
        let label = entry.item.replace('_', " ");
        let currency = entry
            .currency
            .as_deref()
            .unwrap_or_else(|| economy.default_currency());
        stack.set_custom_name(item_label(
            &label,
            buy_price,
            sell_price,
            entry.base_buy_price.is_some(),
            entry.base_sell_price.is_some(),
            currency,
            locale,
        ));
        inventory.set_stack(i, stack).await;
    }

    if let Ok(Some(redeemable)) = shop.redeemable(player).await
        && let Some(item) = pumpkin_data::item::Item::from_registry_key(&redeemable.item)
    {
        let mut stack = ItemStack::new(u8::try_from(redeemable.amount.min(64)).unwrap_or(64), item);
        stack.set_custom_name(get_translation_text(
            "ember:commands.shop.redeem_label",
            locale,
            vec![
                TextComponent::text(redeemable.amount.to_string()).0,
                TextComponent::text(redeemable.item.replace('_', " ")).0,
                TextComponent::text(redeemable.currency).0,
            ],
        ));
        inventory.set_stack(REDEEM_SLOT, stack).await;
    }

    inventory
}

pub struct ShopScreenHandler {
    inventory: Arc<PluginInventory>,
    behaviour: ScreenHandlerBehaviour,
    shop_manager: Arc<ShopManager>,
    economy: Arc<EconomyManager>,
    shop_name: String,
    player_uuid: Uuid,
    locale: Locale,
}

impl ShopScreenHandler {
    async fn new(
        sync_id: u8,
        shop_manager: Arc<ShopManager>,
        economy: Arc<EconomyManager>,
        shop_name: String,
        player_uuid: Uuid,
        locale: Locale,
        player_inventory: &Arc<pumpkin_inventory::player::player_inventory::PlayerInventory>,
    ) -> Self {
        let inventory =
            build_inventory(&shop_manager, &shop_name, player_uuid, &economy, locale).await;
        let mut behaviour = ScreenHandlerBehaviour::new(sync_id, Some(WindowType::Generic9x3));
        behaviour.container_slots = TOTAL_SLOTS;

        let mut handler = Self {
            inventory: inventory.clone(),
            behaviour,
            shop_manager,
            economy,
            shop_name,
            player_uuid,
            locale,
        };

        for i in 0..TOTAL_SLOTS {
            handler.add_slot(Arc::new(NormalSlot::new(inventory.clone(), i)));
        }
        let player_inv_dyn: Arc<dyn pumpkin_world::inventory::Inventory> = player_inventory.clone();
        handler.add_player_slots(&player_inv_dyn);

        handler
    }

    /// Rebuilds and re-syncs the inventory. Re-derives `self.locale` from
    /// `player`'s *current* config first - the locale was only ever a
    /// snapshot taken when the menu was opened, and a player can change
    /// their client language while it stays open (`handle_client_information`
    /// applies that live, with no notion of "close any open screen"), so
    /// reusing the stale snapshot here would leave item labels stuck in the
    /// old language until the menu is closed and reopened.
    async fn refresh(&mut self, player: &dyn InventoryPlayer) {
        if let Some(p) = player.as_any().downcast_ref::<Player>() {
            self.locale = Locale::from_str(&p.config.load().locale).unwrap_or(Locale::EnUs);
        }
        let inventory = build_inventory(
            &self.shop_manager,
            &self.shop_name,
            self.player_uuid,
            &self.economy,
            self.locale,
        )
        .await;
        for i in 0..ITEM_SLOTS.max(REDEEM_SLOT + 1) {
            let stack = inventory.get_stack(i).await.lock().await.clone();
            self.inventory.set_stack(i, stack).await;
        }
        self.sync_state().await;
    }

    async fn handle_buy(&mut self, slot: usize, player: &dyn InventoryPlayer) {
        let Some(item_name) = self
            .shop_manager
            .find_shop(&self.shop_name)
            .and_then(|s| s.items.get(slot))
            .map(|i| i.item.clone())
        else {
            return;
        };
        match self
            .shop_manager
            .buy(
                self.player_uuid,
                &self.shop_name,
                &item_name,
                1,
                &self.economy,
            )
            .await
        {
            Ok((item, _total)) => {
                let stack = ItemStack::new(1, item);
                player
                    .get_inventory()
                    .offer_or_drop_stack(stack, player)
                    .await;
            }
            Err(_e) => {
                // Insufficient funds / limit reached / etc. - silently no-op
                // for now; the price shown in the slot name already reflects
                // reality, and a chat error channel isn't wired to this menu
                // yet. Left as a known follow-up.
            }
        }
        self.refresh(player).await;
    }

    async fn handle_sell(&mut self, player: &dyn InventoryPlayer) {
        let held = player.get_inventory().held_item();
        let mut stack = held.lock().await.clone();
        if stack.is_empty() {
            return;
        }
        let item_name = stack.item.registry_key.to_string();
        let quantity = u32::from(stack.item_count);
        if self
            .shop_manager
            .sell(
                self.player_uuid,
                &self.shop_name,
                &item_name,
                quantity,
                &self.economy,
            )
            .await
            .is_ok()
        {
            stack.decrement(u8::try_from(quantity).unwrap_or(u8::MAX));
            *held.lock().await = stack;
        }
        self.refresh(player).await;
    }

    async fn handle_redeem(&mut self, player: &dyn InventoryPlayer) {
        if let Ok((item, amount, _paid)) = self
            .shop_manager
            .redeem(self.player_uuid, &self.economy)
            .await
        {
            let stack = ItemStack::new(u8::try_from(amount.min(64)).unwrap_or(64), item);
            player
                .get_inventory()
                .offer_or_drop_stack(stack, player)
                .await;
        }
        self.refresh(player).await;
    }
}

impl ScreenHandler for ShopScreenHandler {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn get_behaviour(&self) -> &ScreenHandlerBehaviour {
        &self.behaviour
    }

    fn get_behaviour_mut(&mut self) -> &mut ScreenHandlerBehaviour {
        &mut self.behaviour
    }

    fn quick_move<'a>(
        &'a mut self,
        _player: &'a dyn InventoryPlayer,
        _slot_index: i32,
    ) -> ItemStackFuture<'a> {
        Box::pin(async move { ItemStack::EMPTY.clone() })
    }

    fn on_slot_click<'a>(
        &'a mut self,
        slot_index: i32,
        button: i32,
        action_type: SlotActionType,
        player: &'a dyn InventoryPlayer,
    ) -> ScreenHandlerFuture<'a, ()> {
        Box::pin(async move {
            #[expect(clippy::cast_sign_loss, reason = "checked non-negative just above")]
            if slot_index < 0 || slot_index as usize >= TOTAL_SLOTS {
                self.internal_on_slot_click(slot_index, button, action_type, player)
                    .await;
                return;
            }
            if matches!(classify_click(&action_type, button), ClickKind::Ignored) {
                return;
            }

            #[expect(clippy::cast_sign_loss, reason = "checked non-negative just above")]
            let slot = slot_index as usize;
            if slot < ITEM_SLOTS {
                self.handle_buy(slot, player).await;
            } else if slot == SELL_SLOT {
                self.handle_sell(player).await;
            } else if slot == REDEEM_SLOT {
                self.handle_redeem(player).await;
            }
        })
    }
}

pub struct ShopMenuFactory {
    pub shop_manager: Arc<ShopManager>,
    pub economy: Arc<EconomyManager>,
    pub shop_name: String,
    pub title: String,
}

impl ScreenHandlerFactory for ShopMenuFactory {
    fn create_screen_handler<'a>(
        &'a self,
        sync_id: u8,
        player_inventory: &'a Arc<pumpkin_inventory::player::player_inventory::PlayerInventory>,
        player: &'a dyn InventoryPlayer,
    ) -> ScreenHandlerFuture<'a, Option<SharedScreenHandler>> {
        Box::pin(async move {
            let player = player.as_any().downcast_ref::<Player>()?;
            let locale = Locale::from_str(&player.config.load().locale).unwrap_or(Locale::EnUs);
            let handler = ShopScreenHandler::new(
                sync_id,
                self.shop_manager.clone(),
                self.economy.clone(),
                self.shop_name.clone(),
                player.gameprofile.id,
                locale,
                player_inventory,
            )
            .await;
            Some(Arc::new(Mutex::new(handler)) as SharedScreenHandler)
        })
    }

    fn get_display_name(&self) -> TextComponent {
        // The title itself is admin-authored (`shop/shops.toml`), not one of
        // Ember's own hardcoded strings, so there's no translation key for
        // it - just the signature ember-glow treatment applied on top of
        // whatever text the admin chose, matching the "branded GUI titles"
        // guidance.
        TextComponent::text(self.title.clone()).ember_gradient()
    }
}
// EMBER end
