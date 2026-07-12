// EMBER - built-in lottery command
//
//   /lottery                - list available pools
//   /lottery <pool>          - draw once from a pool

use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_util::permission::{Permission, PermissionDefault, PermissionRegistry};
use pumpkin_util::text::{TextComponent, color::NamedColor};

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

fn err_text(msg: impl Into<String>) -> TextComponent {
    TextComponent::text(msg.into()).color_named(NamedColor::Red)
}

struct LotteryListExecutor;
impl CommandExecutor for LotteryListExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let names = context.server().lottery_manager.pool_names();
            if names.is_empty() {
                feedback(
                    context,
                    TextComponent::text("No lottery pools are configured."),
                )
                .await;
                return Ok(0);
            }
            feedback(
                context,
                TextComponent::text(format!("Lottery pools: {}", names.join(", "))),
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

            match server
                .lottery_manager
                .draw(player.gameprofile.id, &name, &server.economy_manager)
                .await
            {
                Ok(result) => {
                    if let Some(item) = Item::from_registry_key(&result.prize.item) {
                        let amount = u8::try_from(result.prize.amount).unwrap_or(u8::MAX);
                        let stack = ItemStack::new(amount.max(1), item);
                        player.inventory.offer_or_drop_stack(stack, player).await;
                    }
                    feedback(
                        context,
                        TextComponent::text(format!(
                            "You won {}x {}!",
                            result.prize.amount,
                            result.prize.item.replace('_', " ")
                        ))
                        .color_named(NamedColor::Green),
                    )
                    .await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, err_text(shop_error_message(&e))).await;
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
