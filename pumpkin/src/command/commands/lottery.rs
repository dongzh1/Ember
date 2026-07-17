// EMBER - built-in lottery command
//
//   /lottery                - list available pools
//   /lottery <pool>          - draw once from a pool

use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_util::permission::{Permission, PermissionDefault, PermissionRegistry};
use pumpkin_util::text::{TextComponent, color::NamedColor};
use pumpkin_util::translation::get_translation_text;
use tracing::warn;

use crate::command::argument_builder::{ArgumentBuilder, argument, command};
use crate::command::argument_types::core::string::StringArgumentType;
use crate::command::context::command_context::CommandContext;
use crate::command::node::dispatcher::CommandDispatcher;
use crate::command::node::{CommandExecutor, CommandExecutorResult};
use crate::command::suggestion::provider::{SuggestionProvider, SuggestionProviderResult};
use crate::command::suggestion::suggestions::SuggestionsBuilder;
use crate::server::shop::ShopError;

const DESCRIPTION: &str = "Draw from the lottery.";
const PERMISSION: &str = "ember:command.lottery";
const ARG_NAME: &str = "pool";

async fn feedback(context: &CommandContext<'_>, msg: TextComponent) {
    context.source.send_feedback(msg, false).await;
}

/// Wraps an already-built (and already localized) message in Ember's plain
/// error color - errors stay clearly red/plain rather than picking up the
/// ember gradient, for legibility.
fn err_text(component: TextComponent) -> TextComponent {
    component.color_named(NamedColor::Red)
}

struct LotteryListExecutor;
impl CommandExecutor for LotteryListExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let names = context.server().lottery_manager.pool_names();
            let locale = context.source.output.get_locale();
            if names.is_empty() {
                feedback(
                    context,
                    TextComponent::custom("ember", "commands.lottery.list_empty", locale, vec![]),
                )
                .await;
                return Ok(0);
            }
            feedback(
                context,
                TextComponent::custom(
                    "ember",
                    "commands.lottery.list",
                    locale,
                    vec![TextComponent::text(names.join(", "))],
                ),
            )
            .await;
            #[expect(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            Ok(names.len() as i32)
        })
    }
}

struct LotteryDrawExecutor;
impl CommandExecutor for LotteryDrawExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let player = context.source.player_or_err()?;
            let server = context.server();
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let locale = context.source.output.get_locale();

            match server
                .lottery_manager
                .draw(player.gameprofile.id, &name, &server.economy_manager)
                .await
            {
                Ok(result) => {
                    let Some(item) = Item::from_registry_key(&result.prize.item) else {
                        // The pool's configured prize doesn't resolve to a
                        // real item (typo'd/renamed/removed id in
                        // lottery.toml) - the charge and draw already
                        // succeeded, but nothing can actually be handed
                        // over, so don't tell the player they won something
                        // they didn't receive.
                        warn!(
                            "Lottery pool '{name}' rolled prize item '{}', which does not \
                             resolve to a real item - check its lottery.toml configuration",
                            result.prize.item
                        );
                        feedback(
                            context,
                            err_text(TextComponent::custom(
                                "ember",
                                "commands.lottery.prize_unavailable",
                                locale,
                                vec![],
                            )),
                        )
                        .await;
                        return Ok(0);
                    };
                    let amount = u8::try_from(result.prize.amount).unwrap_or(u8::MAX);
                    let stack = ItemStack::new(amount.max(1), item);
                    player.inventory.offer_or_drop_stack(stack, player).await;
                    let text = get_translation_text(
                        "ember:commands.lottery.win",
                        locale,
                        vec![
                            TextComponent::text(result.prize.amount.to_string()).0,
                            TextComponent::text(result.prize.item.replace('_', " ")).0,
                        ],
                    );
                    feedback(context, TextComponent::text_ember(text)).await;
                    Ok(1)
                }
                Err(e) => {
                    // `ShopError`'s `Display` text is plain, un-localized
                    // English baked in at construction time deep in the
                    // manager layer - out of this pass's scope since fixing
                    // it properly means restructuring that enum (in
                    // `server/shop/mod.rs`, not one of the files this pass
                    // covers) rather than just swapping a string literal.
                    feedback(
                        context,
                        err_text(TextComponent::text(shop_error_message(&e))),
                    )
                    .await;
                    Ok(0)
                }
            }
        })
    }
}

fn shop_error_message(e: &ShopError) -> String {
    e.to_string()
}

struct LotteryPoolSuggestionProvider;
impl SuggestionProvider for LotteryPoolSuggestionProvider {
    fn suggest<'a>(
        &'a self,
        context: &'a CommandContext,
        builder: SuggestionsBuilder,
    ) -> SuggestionProviderResult<'a> {
        Box::pin(async move {
            let names = context.server().lottery_manager.pool_names();
            builder.filter_and_suggest_iter(names).build()
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
        command("lottery", DESCRIPTION)
            .requires(PERMISSION)
            .executes(LotteryListExecutor)
            .then(
                argument(ARG_NAME, StringArgumentType::SingleWord)
                    .suggests(LotteryPoolSuggestionProvider)
                    .executes(LotteryDrawExecutor),
            ),
    );
}
// EMBER end
