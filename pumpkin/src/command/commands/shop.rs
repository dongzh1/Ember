// EMBER - built-in shop system command
//
//   /shop            - list available shops
//   /shop <name>     - open a shop's buy/sell menu

use pumpkin_util::permission::{Permission, PermissionDefault, PermissionRegistry};
use pumpkin_util::text::{TextComponent, color::NamedColor};

use crate::command::argument_builder::{ArgumentBuilder, argument, command};
use crate::command::argument_types::core::string::StringArgumentType;
use crate::command::context::command_context::CommandContext;
use crate::command::node::dispatcher::CommandDispatcher;
use crate::command::node::{CommandExecutor, CommandExecutorResult};
use crate::command::suggestion::provider::{SuggestionProvider, SuggestionProviderResult};
use crate::command::suggestion::suggestions::SuggestionsBuilder;
use crate::server::shop::shop_menu::ShopMenuFactory;

const DESCRIPTION: &str = "Open the shop system's buy/sell menus.";
const PERMISSION: &str = "ember:command.shop";
const ARG_NAME: &str = "name";

async fn feedback(context: &CommandContext<'_>, msg: TextComponent) {
    context.source.send_feedback(msg, false).await;
}

fn err_text(msg: impl Into<String>) -> TextComponent {
    TextComponent::text(msg.into()).color_named(NamedColor::Red)
}

struct ShopListExecutor;
impl CommandExecutor for ShopListExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let names = context.server().shop_manager.shop_names();
            if names.is_empty() {
                feedback(context, TextComponent::text("No shops are configured.")).await;
                return Ok(0);
            }
            feedback(
                context,
                TextComponent::text(format!("Shops: {}", names.join(", "))),
            )
            .await;
            #[expect(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            Ok(names.len() as i32)
        })
    }
}

struct ShopOpenExecutor;
impl CommandExecutor for ShopOpenExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let sender = context.source.player_or_err()?;
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let server = context.server();
            let Some(shop) = server.shop_manager.find_shop(&name) else {
                feedback(context, err_text(format!("No shop named '{name}'."))).await;
                return Ok(0);
            };
            let Some(sender) = server.get_player_by_uuid(sender.gameprofile.id) else {
                return Ok(0);
            };
            let factory = ShopMenuFactory {
                shop_manager: server.shop_manager.clone(),
                economy: server.economy_manager.clone(),
                shop_name: shop.name.clone(),
                title: shop.title.clone(),
            };
            sender.open_handled_screen(&factory, None).await;
            Ok(1)
        })
    }
}

struct ShopNameSuggestionProvider;
impl SuggestionProvider for ShopNameSuggestionProvider {
    fn suggest<'a>(
        &'a self,
        context: &'a CommandContext,
        builder: SuggestionsBuilder,
    ) -> SuggestionProviderResult<'a> {
        Box::pin(async move {
            let names = context.server().shop_manager.shop_names();
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
        command("shop", DESCRIPTION)
            .requires(PERMISSION)
            .executes(ShopListExecutor)
            .then(
                argument(ARG_NAME, StringArgumentType::SingleWord)
                    .suggests(ShopNameSuggestionProvider)
                    .executes(ShopOpenExecutor),
            ),
    );
}
// EMBER end
