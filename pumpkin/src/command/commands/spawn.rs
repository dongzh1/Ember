// EMBER - /spawn: player-facing command to return to the main hub world.
//
// The hub is the server's default world (`server.worlds.load().first()`,
// the same world `WorldUnloadExecutor`'s fallback and `delete_world`'s
// "cannot delete the default world" guard already treat as the primary
// world). Unlike `/world tp` (op-only, teleports to an arbitrary named
// world), `/spawn` is intended for every player and always targets that
// one world's own spawn point.

use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::permission::{Permission, PermissionDefault, PermissionRegistry};
use pumpkin_util::text::{TextComponent, color::NamedColor};
use pumpkin_util::translation::get_translation_text;

use crate::command::argument_builder::{ArgumentBuilder, command};
use crate::command::context::command_context::CommandContext;
use crate::command::node::dispatcher::CommandDispatcher;
use crate::command::node::{CommandExecutor, CommandExecutorResult};
use crate::entity::EntityBase;
use crate::world::World;

const DESCRIPTION: &str = "Teleports you to the main hub world's spawn point.";
const PERMISSION: &str = "ember:command.spawn";

fn spawn_of(world: &World) -> Vector3<f64> {
    let info = world.level_info.load();
    Vector3::new(
        f64::from(info.spawn_x) + 0.5,
        f64::from(info.spawn_y),
        f64::from(info.spawn_z) + 0.5,
    )
}

struct SpawnExecutor;

impl CommandExecutor for SpawnExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let locale = context.source.output.get_locale();
            let Some(player) = context.source.output.as_player() else {
                context
                    .source
                    .send_feedback(
                        TextComponent::custom(
                            "ember",
                            "commands.spawn.players_only",
                            locale,
                            vec![],
                        )
                        .color_named(NamedColor::Red),
                        false,
                    )
                    .await;
                return Ok(0);
            };
            let Some(hub) = context.server().worlds.load().first().cloned() else {
                context
                    .source
                    .send_feedback(
                        TextComponent::custom(
                            "ember",
                            "commands.spawn.hub_not_loaded",
                            locale,
                            vec![],
                        )
                        .color_named(NamedColor::Red),
                        false,
                    )
                    .await;
                return Ok(0);
            };

            let spawn = spawn_of(&hub);
            player.teleport(spawn, None, None, hub).await;
            // Branded success confirmation, same reasoning as /home: the
            // gradient is applied to a pre-resolved plain string (not a
            // live `Custom` component), since `ember_gradient` bakes in
            // per-character colors off the component's resolved text and
            // would otherwise ignore the player's actual locale.
            context
                .source
                .send_feedback(
                    TextComponent::text_ember(get_translation_text(
                        "ember:commands.spawn.teleported",
                        locale,
                        vec![],
                    )),
                    false,
                )
                .await;
            Ok(1)
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
        command("spawn", DESCRIPTION)
            .requires(PERMISSION)
            .executes(SpawnExecutor),
    );
}
