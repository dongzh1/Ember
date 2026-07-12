// EMBER - /home: player-facing command to return to your personal home
// world. Each player's home lives in a `home_<uuid>` world: loaded straight
// from disk if it already exists, otherwise cloned from the operator-
// configured template world (`home/home.toml`'s `template_world`, see
// `HomeManager`) the first time the player ever visits.

use std::sync::Arc;

use pumpkin_data::dimension::Dimension;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::permission::{Permission, PermissionDefault, PermissionRegistry};
use pumpkin_util::text::{TextComponent, color::NamedColor};
use uuid::Uuid;

use crate::command::argument_builder::{ArgumentBuilder, command};
use crate::command::context::command_context::CommandContext;
use crate::command::node::dispatcher::CommandDispatcher;
use crate::command::node::{CommandExecutor, CommandExecutorResult};
use crate::entity::EntityBase;
use crate::server::{HomeManager, Server};
use crate::world::World;

const DESCRIPTION: &str = "Teleports you to your personal home world.";
const PERMISSION: &str = "ember:command.home";

fn err_text(msg: impl Into<String>) -> TextComponent {
    TextComponent::text(msg.into()).color_named(NamedColor::Red)
}

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

/// Loads (or, on a player's first visit, clones from the template) their
/// home world and returns it, ready to teleport into.
async fn resolve_home_world(server: &Arc<Server>, player_uuid: Uuid) -> Result<Arc<World>, String> {
    let home_name = HomeManager::world_name_for(player_uuid);

    if let Some(world) = find_world(server, &home_name) {
        return Ok(world);
    }
    if server.is_world_unloading(&home_name) {
        return Err("你的家园世界正在卸载中，请稍后再试。".to_string());
    }
    if server.list_world_folders().iter().any(|n| n == &home_name) {
        return Ok(server.create_world(home_name, Dimension::OVERWORLD).await);
    }

    // First visit: no home world on disk yet, so clone it from the
    // template. `clone_world` requires its source to be loaded, so load
    // the template first if it's only sitting on disk.
    let template_name = server.home_manager.template_world().to_string();
    if find_world(server, &template_name).is_none() {
        if server
            .list_world_folders()
            .iter()
            .any(|n| n == &template_name)
        {
            server
                .create_world(template_name.clone(), Dimension::OVERWORLD)
                .await;
        } else {
            return Err(format!(
                "家园模板世界 '{template_name}' 还不存在，请联系管理员创建。"
            ));
        }
    }
    server.clone_world(&template_name, &home_name).await
}

struct HomeExecutor;

impl CommandExecutor for HomeExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let Some(player) = context.source.output.as_player() else {
                context
                    .source
                    .send_feedback(err_text("只有玩家可以使用 /home。"), false)
                    .await;
                return Ok(0);
            };
            let server = context.server().clone();

            match resolve_home_world(&server, player.gameprofile.id).await {
                Ok(world) => {
                    let spawn = spawn_of(&world);
                    player.teleport(spawn, None, None, world).await;
                    context
                        .source
                        .send_feedback(
                            TextComponent::text("已传送到你的家园。")
                                .color_named(NamedColor::Green),
                            false,
                        )
                        .await;
                    Ok(1)
                }
                Err(e) => {
                    context.source.send_feedback(err_text(e), false).await;
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
        command("home", DESCRIPTION)
            .requires(PERMISSION)
            .executes(HomeExecutor),
    );
}
