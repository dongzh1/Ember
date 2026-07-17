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
use pumpkin_util::translation::get_translation_text;

use crate::command::argument_builder::{ArgumentBuilder, argument, command, literal};
use crate::command::argument_types::core::integer::IntegerArgumentType;
use crate::command::argument_types::core::string::StringArgumentType;
use crate::command::context::command_context::CommandContext;
use crate::command::errors::command_syntax_error::CommandSyntaxError;
use crate::command::node::dispatcher::CommandDispatcher;
use crate::command::node::{CommandExecutor, CommandExecutorResult};
use crate::command::suggestion::provider::{SuggestionProvider, SuggestionProviderResult};
use crate::command::suggestion::suggestions::SuggestionsBuilder;
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

/// Wraps an already-built (and already localized) message in Ember's plain
/// error color - errors stay clearly red/plain rather than picking up the
/// ember gradient, for legibility.
fn err_text(component: TextComponent) -> TextComponent {
    component.color_named(NamedColor::Red)
}

/// `ShopError`'s `Display` text (see `server::shop::ShopError`) is plain,
/// un-localized English baked in at construction time deep in the manager
/// layer - out of this pass's scope since fixing it properly means
/// restructuring that enum (in `server/shop/mod.rs`, not one of the files
/// this pass covers) rather than just swapping a string literal.
fn shop_err_text(e: &ShopError) -> TextComponent {
    err_text(TextComponent::text(e.to_string()))
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

/// Suggests every currency id configured in `economy/economy.toml`.
struct CurrencySuggestionProvider;

impl SuggestionProvider for CurrencySuggestionProvider {
    fn suggest<'a>(
        &'a self,
        context: &'a CommandContext,
        builder: SuggestionsBuilder,
    ) -> SuggestionProviderResult<'a> {
        Box::pin(async move {
            let currencies = context.server().economy_manager.currencies();
            builder.filter_and_suggest_iter(currencies).build()
        })
    }
}

/// Suggests every currently active listing id - what `/market buy` accepts.
struct MarketListingIdSuggestionProvider;

impl SuggestionProvider for MarketListingIdSuggestionProvider {
    fn suggest<'a>(
        &'a self,
        context: &'a CommandContext,
        builder: SuggestionsBuilder,
    ) -> SuggestionProviderResult<'a> {
        Box::pin(async move {
            let ids = context
                .server()
                .market_manager
                .active_listings()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|l| l.id.to_string());
            builder.filter_and_suggest_iter(ids).build()
        })
    }
}

/// Suggests only the calling player's own active listing ids - what
/// `/market cancel` actually accepts (cancelling someone else's listing
/// fails), so unlike `MarketListingIdSuggestionProvider` this doesn't just
/// list everything.
struct MarketOwnListingIdSuggestionProvider;

impl SuggestionProvider for MarketOwnListingIdSuggestionProvider {
    fn suggest<'a>(
        &'a self,
        context: &'a CommandContext,
        builder: SuggestionsBuilder,
    ) -> SuggestionProviderResult<'a> {
        Box::pin(async move {
            let Some(player_uuid) = context.source.player_or_none().map(|p| p.gameprofile.id)
            else {
                return builder.build();
            };
            let ids = context
                .server()
                .market_manager
                .active_listings()
                .await
                .unwrap_or_default()
                .into_iter()
                .filter(|l| l.seller_uuid == player_uuid)
                .map(|l| l.id.to_string());
            builder.filter_and_suggest_iter(ids).build()
        })
    }
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
            let locale = context.source.output.get_locale();

            if price < 0 {
                feedback(
                    context,
                    err_text(TextComponent::custom(
                        "ember",
                        "commands.market.negative_price",
                        locale,
                        vec![],
                    )),
                )
                .await;
                return Ok(0);
            }

            let held = player.inventory.held_item();
            let mut stack = held.lock().await.clone();
            if stack.is_empty() {
                feedback(
                    context,
                    err_text(TextComponent::custom(
                        "ember",
                        "commands.market.empty_hand",
                        locale,
                        vec![],
                    )),
                )
                .await;
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
                    let text = get_translation_text(
                        "ember:commands.market.sell_success",
                        locale,
                        vec![
                            TextComponent::text(amount.to_string()).0,
                            TextComponent::text(item_key).0,
                            TextComponent::text(price.to_string()).0,
                            TextComponent::text(currency).0,
                            TextComponent::text(id.to_string()).0,
                        ],
                    );
                    feedback(context, TextComponent::text_ember(text)).await;
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
            let locale = context.source.output.get_locale();
            match context.server().market_manager.active_listings().await {
                Ok(listings) if listings.is_empty() => {
                    feedback(
                        context,
                        TextComponent::custom(
                            "ember",
                            "commands.market.list_empty",
                            locale,
                            vec![],
                        ),
                    )
                    .await;
                    Ok(0)
                }
                Ok(listings) => {
                    let mut message = TextComponent::custom(
                        "ember",
                        "commands.market.list_header",
                        locale,
                        vec![],
                    );
                    for l in &listings {
                        message = message.new_line().add_child(TextComponent::custom(
                            "ember",
                            "commands.market.list_row",
                            locale,
                            vec![
                                TextComponent::text(l.id.to_string()),
                                TextComponent::text(l.amount.to_string()),
                                TextComponent::text(l.item.clone()),
                                TextComponent::text(l.price.to_string()),
                                TextComponent::text(l.currency.clone()),
                                TextComponent::text(l.seller_name.clone()),
                            ],
                        ));
                    }
                    feedback(context, message).await;
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
            let locale = context.source.output.get_locale();

            match server
                .market_manager
                .buy_listing(id, player.gameprofile.id, &server.economy_manager)
                .await
            {
                Ok((item_key, amount)) => {
                    give_item(player, &item_key, amount).await;
                    let text = get_translation_text(
                        "ember:commands.market.buy_success",
                        locale,
                        vec![TextComponent::text(id.to_string()).0],
                    );
                    feedback(context, TextComponent::text_ember(text)).await;
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
            let locale = context.source.output.get_locale();

            match server
                .market_manager
                .cancel_listing(id, player.gameprofile.id)
                .await
            {
                Ok((item_key, amount)) => {
                    give_item(player, &item_key, amount).await;
                    let text = get_translation_text(
                        "ember:commands.market.cancel_success",
                        locale,
                        vec![TextComponent::text(id.to_string()).0],
                    );
                    feedback(context, TextComponent::text_ember(text)).await;
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
                                .suggests(CurrencySuggestionProvider)
                                .executes(MarketSellExecutor { has_currency: true }),
                        ),
                ),
            )
            .then(literal("list").executes(MarketListExecutor))
            .then(
                literal("buy").then(
                    argument(ARG_ID, IntegerArgumentType::with_min(1))
                        .suggests(MarketListingIdSuggestionProvider)
                        .executes(MarketBuyExecutor),
                ),
            )
            .then(
                literal("cancel").then(
                    argument(ARG_ID, IntegerArgumentType::with_min(1))
                        .suggests(MarketOwnListingIdSuggestionProvider)
                        .executes(MarketCancelExecutor),
                ),
            ),
    );
}
// EMBER end
