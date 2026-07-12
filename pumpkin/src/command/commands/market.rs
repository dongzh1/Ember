// EMBER - built-in market/auction command
//
//   /market sell <price> [currency]   - list your held item stack for sale
//   /market list                      - show active listings
//   /market buy <id>                  - buy a listing
//   /market cancel <id>                - cancel your own listing, item returned

use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_util::permission::{Permission, PermissionDefault, PermissionRegistry};
use pumpkin_util::text::{TextComponent, color::NamedColor};

use crate::command::argument_builder::{ArgumentBuilder, argument, command, literal};
use crate::command::argument_types::core::integer::IntegerArgumentType;
use crate::command::argument_types::core::string::StringArgumentType;
use crate::command::context::command_context::CommandContext;
use crate::command::errors::command_syntax_error::CommandSyntaxError;
use crate::command::node::dispatcher::CommandDispatcher;
use crate::command::node::{CommandExecutor, CommandExecutorResult};
use crate::server::Server;
use crate::server::shop::ShopError;

const DESCRIPTION: &str = "Buy/sell items with other players.";
const PERMISSION: &str = "ember:command.market";
const ARG_PRICE: &str = "price";
const ARG_CURRENCY: &str = "currency";
const ARG_ID: &str = "id";

async fn feedback(context: &CommandContext<'_>, msg: TextComponent) {
    context.source.send_feedback(msg, false).await;
}

fn err_text(msg: impl Into<String>) -> TextComponent {
    TextComponent::text(msg.into()).color_named(NamedColor::Red)
}

fn shop_err_text(e: &ShopError) -> TextComponent {
    err_text(e.to_string())
}

fn optional_currency<'a>(
    context: &'a CommandContext,
    has_currency: bool,
) -> Result<Option<&'a str>, CommandSyntaxError> {
    has_currency
        .then(|| StringArgumentType::get(context, ARG_CURRENCY))
        .transpose()
}

fn resolve_currency<'a>(server: &'a Server, currency: Option<&'a str>) -> String {
    currency
        .unwrap_or_else(|| server.economy_manager.default_currency())
        .to_string()
}

struct MarketSellExecutor {
    has_currency: bool,
}
impl CommandExecutor for MarketSellExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let player = context.source.player_or_err()?;
            let server = context.server();
            let price = IntegerArgumentType::get(context, ARG_PRICE)?;
            let currency = optional_currency(context, self.has_currency)?;
            let currency = resolve_currency(server, currency);

            if price < 0 {
                feedback(context, err_text("Price can't be negative.")).await;
                return Ok(0);
            }

            let held = player.inventory.held_item();
            let mut stack = held.lock().await.clone();
            if stack.is_empty() {
                feedback(context, err_text("You aren't holding anything.")).await;
                return Ok(0);
            }
            let item_key = stack.item.registry_key.to_string();
            let amount = u32::from(stack.item_count);

            let mut held_perms = std::collections::HashSet::new();
            if let Some(player_arc) = server.get_player_by_uuid(player.gameprofile.id) {
                for tier in server.market_manager.slot_tiers() {
                    if let Some(node) = &tier.permission
                        && player_arc.has_permission(server, node).await
                    {
                        held_perms.insert(node.clone());
                    }
                }
            }
            let max_listings = server
                .market_manager
                .resolve_slot_tier(&|node: &str| held_perms.contains(node));

            match server
                .market_manager
                .create_listing(crate::server::shop::market::NewListing {
                    seller: player.gameprofile.id,
                    seller_name: &player.gameprofile.name,
                    item: &item_key,
                    amount,
                    currency: &currency,
                    price: i64::from(price),
                    max_listings,
                })
                .await
            {
                Ok(id) => {
                    stack.decrement(stack.item_count);
                    *held.lock().await = stack;
                    feedback(
                        context,
                        TextComponent::text(format!(
                            "Listed {amount}x {item_key} for {price} {currency} (listing #{id})."
                        ))
                        .color_named(NamedColor::Green),
                    )
                    .await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, shop_err_text(&e)).await;
                    Ok(0)
                }
            }
        })
    }
}

struct MarketListExecutor;
impl CommandExecutor for MarketListExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            match context.server().market_manager.active_listings().await {
                Ok(listings) if listings.is_empty() => {
                    feedback(context, TextComponent::text("No active listings.")).await;
                    Ok(0)
                }
                Ok(listings) => {
                    let mut lines = vec!["Active listings:".to_string()];
                    for l in &listings {
                        lines.push(format!(
                            "  #{}: {}x {} - {} {} (seller: {})",
                            l.id, l.amount, l.item, l.price, l.currency, l.seller_name
                        ));
                    }
                    feedback(context, TextComponent::text(lines.join("\n"))).await;
                    #[expect(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
                    Ok(listings.len() as i32)
                }
                Err(e) => {
                    feedback(context, shop_err_text(&e)).await;
                    Ok(0)
                }
            }
        })
    }
}

async fn give_item(player: &crate::entity::player::Player, item_key: &str, amount: u32) {
    let Some(item) = Item::from_registry_key(item_key) else {
        return;
    };
    let max_stack = u32::from(ItemStack::new(1, item).get_max_stack_size());
    let mut remaining = amount;
    while remaining > 0 {
        let take = remaining.min(max_stack);
        #[expect(
            clippy::cast_possible_truncation,
            reason = "take is clamped to max_stack (u8 range)"
        )]
        let stack = ItemStack::new(take as u8, item);
        player.inventory.offer_or_drop_stack(stack, player).await;
        remaining -= take;
    }
}

struct MarketBuyExecutor;
impl CommandExecutor for MarketBuyExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let player = context.source.player_or_err()?;
            let server = context.server();
            let id = i64::from(IntegerArgumentType::get(context, ARG_ID)?);

            match server
                .market_manager
                .buy_listing(id, player.gameprofile.id, &server.economy_manager)
                .await
            {
                Ok((item_key, amount)) => {
                    give_item(player, &item_key, amount).await;
                    feedback(
                        context,
                        TextComponent::text(format!("Bought listing #{id}."))
                            .color_named(NamedColor::Green),
                    )
                    .await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, shop_err_text(&e)).await;
                    Ok(0)
                }
            }
        })
    }
}

struct MarketCancelExecutor;
impl CommandExecutor for MarketCancelExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let player = context.source.player_or_err()?;
            let server = context.server();
            let id = i64::from(IntegerArgumentType::get(context, ARG_ID)?);

            match server
                .market_manager
                .cancel_listing(id, player.gameprofile.id)
                .await
            {
                Ok((item_key, amount)) => {
                    give_item(player, &item_key, amount).await;
                    feedback(
                        context,
                        TextComponent::text(format!("Cancelled listing #{id}."))
                            .color_named(NamedColor::Green),
                    )
                    .await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, shop_err_text(&e)).await;
                    Ok(0)
                }
            }
        })
    }
}

pub fn register(dispatcher: &mut CommandDispatcher, registry: &mut PermissionRegistry) {
    registry.register_permission_or_panic(Permission::new(
        PERMISSION,
        DESCRIPTION,
        PermissionDefault::Allow,
    ));

    dispatcher.register(
        command("market", DESCRIPTION)
            .requires(PERMISSION)
            .then(
                literal("sell").then(
                    argument(ARG_PRICE, IntegerArgumentType::with_min(0))
                        .executes(MarketSellExecutor {
                            has_currency: false,
                        })
                        .then(
                            argument(ARG_CURRENCY, StringArgumentType::SingleWord)
                                .executes(MarketSellExecutor { has_currency: true }),
                        ),
                ),
            )
            .then(literal("list").executes(MarketListExecutor))
            .then(literal("buy").then(
                argument(ARG_ID, IntegerArgumentType::with_min(1)).executes(MarketBuyExecutor),
            ))
            .then(literal("cancel").then(
                argument(ARG_ID, IntegerArgumentType::with_min(1)).executes(MarketCancelExecutor),
            )),
    );
}
// EMBER end
