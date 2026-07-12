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
            let Some(player) = context.source.output.as_player() else {
                context
                    .source
                    .send_feedback(
                        TextComponent::text("只有玩家可以使用 /spawn。")
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
                        TextComponent::text("主城世界未加载。").color_named(NamedColor::Red),
                        false,
                    )
                    .await;
                return Ok(0);
            };

            let spawn = spawn_of(&hub);
            player.teleport(spawn, None, None, hub).await;
            context
                .source
                .send_feedback(
                    TextComponent::text("已传送到主城。").color_named(NamedColor::Green),
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
