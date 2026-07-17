// EMBER - custom item command
//
//   /customitem give <player> <id>          - give 1 of a configured custom item
//   /customitem give <player> <id> <count>  - give a specific count
//   /customitem list                        - list configured custom item ids

use pumpkin_util::PermissionLvl;
use pumpkin_util::permission::{Permission, PermissionDefault, PermissionRegistry};
use pumpkin_util::text::{TextComponent, color::NamedColor};
use pumpkin_util::translation::get_translation_text;

use crate::command::argument_builder::{ArgumentBuilder, argument, command, literal};
use crate::command::argument_types::core::integer::IntegerArgumentType;
use crate::command::argument_types::core::string::StringArgumentType;
use crate::command::argument_types::entity::EntityArgumentType;
use crate::command::context::command_context::CommandContext;
use crate::command::node::dispatcher::CommandDispatcher;
use crate::command::node::{CommandExecutor, CommandExecutorResult};
use crate::command::suggestion::provider::{SuggestionProvider, SuggestionProviderResult};
use crate::command::suggestion::suggestions::SuggestionsBuilder;

const DESCRIPTION: &str = "Gives configured custom items (resourcepack/items.toml).";
const PERMISSION: &str = "ember:command.customitem";
const ARG_TARGET: &str = "target";
const ARG_ID: &str = "id";
const ARG_COUNT: &str = "count";

struct CustomItemGiveExecutor {
    has_count: bool,
}

impl CommandExecutor for CustomItemGiveExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let target = EntityArgumentType::get_player(context, ARG_TARGET).await?;
            let id = StringArgumentType::get(context, ARG_ID)?;
            let count = if self.has_count {
                IntegerArgumentType::get(context, ARG_COUNT)?
            } else {
                1
            };
            let locale = context.source.output.get_locale();

            let server = context.server();
            let Some(mut stack) = server.custom_item_manager.build_stack(id, 1).await else {
                context
                    .source
                    .send_feedback(
                        TextComponent::custom(
                            "ember",
                            "commands.custom_item.not_found",
                            locale,
                            vec![TextComponent::text(id.to_string())],
                        )
                        .color_named(NamedColor::Red),
                        false,
                    )
                    .await;
                return Ok(0);
            };

            let max_stack = i32::from(stack.get_max_stack_size());
            let mut remaining = count;
            while remaining > 0 {
                let take = remaining.min(max_stack);
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "take is clamped to max_stack (u8 range)"
                )]
                let take_u8 = take as u8;
                stack.item_count = take_u8;
                target
                    .inventory
                    .offer_or_drop_stack(stack.clone(), target.as_ref())
                    .await;
                remaining -= take;
            }

            let given_text = get_translation_text(
                "ember:commands.custom_item.given",
                locale,
                vec![
                    TextComponent::text(count.to_string()).0,
                    TextComponent::text(id.to_string()).0,
                    TextComponent::text(target.gameprofile.name.clone()).0,
                ],
            );
            context
                .source
                .send_feedback(TextComponent::text_ember(given_text), false)
                .await;
            Ok(1)
        })
    }
}

struct CustomItemListExecutor;

impl CommandExecutor for CustomItemListExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let ids = context.server().custom_item_manager.list_ids().await;
            let locale = context.source.output.get_locale();
            if ids.is_empty() {
                context
                    .source
                    .send_feedback(
                        TextComponent::custom(
                            "ember",
                            "commands.custom_item.list_empty",
                            locale,
                            vec![],
                        ),
                        false,
                    )
                    .await;
                return Ok(0);
            }
            context
                .source
                .send_feedback(
                    TextComponent::custom(
                        "ember",
                        "commands.custom_item.list",
                        locale,
                        vec![TextComponent::text(ids.join(", "))],
                    ),
                    false,
                )
                .await;
            #[expect(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            Ok(ids.len() as i32)
        })
    }
}

/// Suggests configured custom item ids.
struct CustomItemIdSuggestionProvider;

impl SuggestionProvider for CustomItemIdSuggestionProvider {
    fn suggest<'a>(
        &'a self,
        context: &'a CommandContext,
        builder: SuggestionsBuilder,
    ) -> SuggestionProviderResult<'a> {
        Box::pin(async move {
            let ids = context.server().custom_item_manager.list_ids().await;
            builder.filter_and_suggest_iter(ids).build()
        })
    }
}

pub fn register(dispatcher: &mut CommandDispatcher, registry: &mut PermissionRegistry) {
    registry.register_permission_or_panic(Permission::new(
        PERMISSION,
        DESCRIPTION,
        PermissionDefault::Op(PermissionLvl::Three),
    ));

    dispatcher.register(
        command("customitem", DESCRIPTION)
            .requires(PERMISSION)
            .then(literal("list").executes(CustomItemListExecutor))
            .then(
                literal("give").then(
                    argument(ARG_TARGET, EntityArgumentType::Player).then(
                        argument(ARG_ID, StringArgumentType::SingleWord)
                            .suggests(CustomItemIdSuggestionProvider)
                            .executes(CustomItemGiveExecutor { has_count: false })
                            .then(
                                argument(ARG_COUNT, IntegerArgumentType::with_min(1))
                                    .executes(CustomItemGiveExecutor { has_count: true }),
                            ),
                    ),
                ),
            ),
    );
}
// EMBER end
