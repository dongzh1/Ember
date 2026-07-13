// EMBER - floating packet-only menu command
//
//   /menu           - open the first configured menu (or close it if already open)
//   /menu <name>    - open a specific menu (or close it if that one's already open)

use pumpkin_util::permission::{Permission, PermissionDefault, PermissionRegistry};
use pumpkin_util::text::{TextComponent, color::NamedColor};

use crate::command::argument_builder::{ArgumentBuilder, argument, command};
use crate::command::argument_types::core::string::StringArgumentType;
use crate::command::context::command_context::CommandContext;
use crate::command::node::dispatcher::CommandDispatcher;
use crate::command::node::{CommandExecutor, CommandExecutorResult};
use crate::command::suggestion::provider::{SuggestionProvider, SuggestionProviderResult};
use crate::command::suggestion::suggestions::SuggestionsBuilder;

const DESCRIPTION: &str = "Opens a floating menu (or closes it if already open).";
const PERMISSION: &str = "ember:command.menu";
const ARG_NAME: &str = "name";

fn err_text(msg: impl Into<String>) -> TextComponent {
    TextComponent::text(msg.into()).color_named(NamedColor::Red)
}

struct MenuOpenExecutor {
    has_name: bool,
}

impl CommandExecutor for MenuOpenExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let player = context.source.player_or_err()?;
            let server = context.server();
            let name = if self.has_name {
                Some(StringArgumentType::get(context, ARG_NAME)?)
            } else {
                None
            };

            let Some(player_arc) = server.get_player_by_uuid(player.gameprofile.id) else {
                return Ok(0);
            };

            match server.menu_manager.open(&player_arc, name).await {
                Ok(()) => Ok(1),
                Err(e) => {
                    context.source.send_feedback(err_text(e), false).await;
                    Ok(0)
                }
            }
        })
    }
}

/// Suggests names of configured menus.
struct MenuNameSuggestionProvider;

impl SuggestionProvider for MenuNameSuggestionProvider {
    fn suggest<'a>(
        &'a self,
        context: &'a CommandContext,
        builder: SuggestionsBuilder,
    ) -> SuggestionProviderResult<'a> {
        Box::pin(async move {
            let names = context.server().menu_manager.list_names().await;
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
        command("menu", DESCRIPTION)
            .requires(PERMISSION)
            .executes(MenuOpenExecutor { has_name: false })
            .then(
                argument(ARG_NAME, StringArgumentType::SingleWord)
                    .suggests(MenuNameSuggestionProvider)
                    .executes(MenuOpenExecutor { has_name: true }),
            ),
    );
}
// EMBER end
