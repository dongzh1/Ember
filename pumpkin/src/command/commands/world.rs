// EMBER - /world command: runtime world management
//
//   /world list           - list loaded worlds with player counts
//   /world load <name>    - load (or create) a world at runtime
//   /world unload <name>  - evacuate players, save and unload a world
//   /world tp <name>      - teleport yourself to a world's spawn

use std::sync::Arc;

use pumpkin_data::dimension::Dimension;
use pumpkin_util::PermissionLvl;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::permission::{Permission, PermissionDefault, PermissionRegistry};
use pumpkin_util::text::{TextComponent, color::NamedColor};

use crate::command::argument_builder::{ArgumentBuilder, argument, command, literal};
use crate::command::argument_types::core::string::StringArgumentType;
use crate::command::context::command_context::CommandContext;
use crate::command::node::dispatcher::CommandDispatcher;
use crate::command::node::{CommandExecutor, CommandExecutorResult};
use crate::server::Server;
use crate::world::World;

const DESCRIPTION: &str = "Manage worlds at runtime: list, load, unload, teleport.";
const PERMISSION: &str = "ember:command.world";
const ARG_NAME: &str = "name";

fn find_world(server: &Server, name: &str) -> Option<Arc<World>> {
    server
        .worlds
        .load()
        .iter()
        .find(|w| w.get_world_name() == name)
        .cloned()
}

fn spawn_of(world: &World) -> Vector3<f64> {
    let info = world.level_info.load();
    Vector3::new(
        f64::from(info.spawn_x) + 0.5,
        f64::from(info.spawn_y),
        f64::from(info.spawn_z) + 0.5,
    )
}

async fn feedback(context: &CommandContext<'_>, msg: TextComponent) {
    context.source.send_feedback(msg, false).await;
}

fn err_text(msg: impl Into<String>) -> TextComponent {
    TextComponent::text(msg.into()).color_named(NamedColor::Red)
}

struct WorldListExecutor;

impl CommandExecutor for WorldListExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let worlds = context.server().worlds.load();
            let mut lines = vec![format!("Loaded worlds ({}):", worlds.len())];
            for w in worlds.iter() {
                lines.push(format!(
                    "  {} [{}] - {} player(s)",
                    w.get_world_name(),
                    w.dimension.minecraft_name,
                    w.players.load().len(),
                ));
            }
            feedback(context, TextComponent::text(lines.join("\n"))).await;
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            Ok(worlds.len() as i32)
        })
    }
}

struct WorldLoadExecutor;

impl CommandExecutor for WorldLoadExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let server = context.server().clone();

            if find_world(&server, &name).is_some() {
                feedback(
                    context,
                    err_text(format!("World '{name}' is already loaded.")),
                )
                .await;
                return Ok(0);
            }

            let world = server
                .create_world(name.clone(), Dimension::OVERWORLD)
                .await;
            feedback(
                context,
                TextComponent::text(format!(
                    "World '{}' loaded ({}).",
                    world.get_world_name(),
                    world.dimension.minecraft_name,
                ))
                .color_named(NamedColor::Green),
            )
            .await;
            Ok(1)
        })
    }
}

struct WorldUnloadExecutor;

impl CommandExecutor for WorldUnloadExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let server = context.server().clone();

            let Some(world) = find_world(&server, &name) else {
                feedback(context, err_text(format!("World '{name}' is not loaded."))).await;
                return Ok(0);
            };
            let Some(fallback) = server.worlds.load().first().cloned() else {
                feedback(context, err_text("No fallback world available.")).await;
                return Ok(0);
            };

            match server.unload_world(&world, &fallback).await {
                Ok(()) => {
                    feedback(
                        context,
                        TextComponent::text(format!("World '{name}' saved and unloaded."))
                            .color_named(NamedColor::Green),
                    )
                    .await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, err_text(format!("Cannot unload '{name}': {e}"))).await;
                    Ok(0)
                }
            }
        })
    }
}

struct WorldTpExecutor;

impl CommandExecutor for WorldTpExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();

            let Some(player) = context.source.output.as_player() else {
                feedback(context, err_text("Only players can use /world tp.")).await;
                return Ok(0);
            };
            let Some(world) = find_world(context.server(), &name) else {
                feedback(context, err_text(format!("World '{name}' is not loaded."))).await;
                return Ok(0);
            };

            let spawn = spawn_of(&world);
            player.teleport_world(world, spawn, None, None).await;
            feedback(
                context,
                TextComponent::text(format!("Teleported to world '{name}'."))
                    .color_named(NamedColor::Green),
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
        PermissionDefault::Op(PermissionLvl::Three),
    ));

    dispatcher.register(
        command("world", DESCRIPTION)
            .requires(PERMISSION)
            .then(literal("list").executes(WorldListExecutor))
            .then(literal("load").then(
                argument(ARG_NAME, StringArgumentType::SingleWord).executes(WorldLoadExecutor),
            ))
            .then(literal("unload").then(
                argument(ARG_NAME, StringArgumentType::SingleWord).executes(WorldUnloadExecutor),
            ))
            .then(literal("tp").then(
                argument(ARG_NAME, StringArgumentType::SingleWord).executes(WorldTpExecutor),
            )),
    );
}
