// EMBER - offline-mode login verification admin command
//
//   /auth reset <player> - deletes a player's login account so their next
//                          join starts fresh registration (forgot-password
//                          recovery; there is no other way to unlock them)

use pumpkin_util::PermissionLvl;
use pumpkin_util::permission::{Permission, PermissionDefault, PermissionRegistry};
use pumpkin_util::text::{TextComponent, color::NamedColor};

use crate::command::argument_builder::{ArgumentBuilder, argument, command, literal};
use crate::command::argument_types::game_profile::GameProfileArgumentType;
use crate::command::context::command_context::CommandContext;
use crate::command::node::dispatcher::CommandDispatcher;
use crate::command::node::{CommandExecutor, CommandExecutorResult};

const DESCRIPTION: &str = "Manage the offline-mode login wall.";
const PERMISSION: &str = "ember:command.auth";
const ARG_TARGET: &str = "target";

async fn feedback(context: &CommandContext<'_>, msg: TextComponent) {
    context.source.send_feedback(msg, false).await;
}

fn err_text(msg: impl Into<String>) -> TextComponent {
    TextComponent::text(msg.into()).color_named(NamedColor::Red)
}

struct AuthResetExecutor;
impl CommandExecutor for AuthResetExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let profiles = GameProfileArgumentType::get(context, ARG_TARGET).await?;
            if profiles.is_empty() {
                feedback(context, err_text("No matching player.")).await;
                return Ok(0);
            }

            // A selector like `@a` can resolve to more than one profile - reset every
            // match instead of silently acting on only the first, same as other
            // selector-driven commands in this codebase.
            let mut reset_count = 0;
            for target in &profiles {
                match context.server().login_manager.reset(target.id).await {
                    Ok(true) => {
                        reset_count += 1;
                        feedback(
                            context,
                            TextComponent::text(format!(
                                "{}'s login account was reset; they'll register fresh next join.",
                                target.name
                            ))
                            .color_named(NamedColor::Green),
                        )
                        .await;
                    }
                    Ok(false) => {
                        feedback(
                            context,
                            err_text(format!("{} has no login account.", target.name)),
                        )
                        .await;
                    }
                    Err(e) => {
                        feedback(
                            context,
                            err_text(format!("{}: login database error: {e}", target.name)),
                        )
                        .await;
                    }
                }
            }
            Ok(reset_count)
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
        command("auth", DESCRIPTION).requires(PERMISSION).then(
            literal("reset")
                .then(argument(ARG_TARGET, GameProfileArgumentType).executes(AuthResetExecutor)),
        ),
    );
}
// EMBER end
