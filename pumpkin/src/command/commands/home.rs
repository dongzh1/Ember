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
use pumpkin_util::translation::get_translation_text;
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

/// Colors an already-translated message as plain error feedback.
fn err_text(msg: TextComponent) -> TextComponent {
    msg.color_named(NamedColor::Red)
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

/// Errors [`resolve_home_world`] can fail with. Carries whatever dynamic
/// data the eventual player-facing message needs - locale (and therefore
/// message rendering) isn't known until back in the command executor, so
/// this can't build a `TextComponent` itself.
enum HomeError {
    /// The player's home world is mid-unload; ask them to retry shortly.
    WorldUnloading,
    /// No home world exists yet, and the operator-configured template world
    /// (`home/home.toml`'s `template_world`) hasn't been created either.
    TemplateMissing(String),
    /// `Server::clone_world` failed. The `String` is its own (already
    /// English, fairly technical) failure reason - shown as-is inside a
    /// translated wrapper so no diagnostic detail is lost.
    CloneFailed(String),
}

/// Loads (or, on a player's first visit, clones from the template) their
/// home world and returns it, ready to teleport into.
async fn resolve_home_world(
    server: &Arc<Server>,
    player_uuid: Uuid,
) -> Result<Arc<World>, HomeError> {
    let home_name = HomeManager::world_name_for(player_uuid);

    if let Some(world) = find_world(server, &home_name) {
        return Ok(world);
    }
    if server.is_world_unloading(&home_name) {
        return Err(HomeError::WorldUnloading);
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
            return Err(HomeError::TemplateMissing(template_name));
        }
    }
    server
        .clone_world(&template_name, &home_name)
        .await
        .map_err(HomeError::CloneFailed)
}

struct HomeExecutor;

impl CommandExecutor for HomeExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let locale = context.source.output.get_locale();
            let Some(player) = context.source.output.as_player() else {
                context
                    .source
                    .send_feedback(
                        err_text(TextComponent::custom(
                            "ember",
                            "commands.home.players_only",
                            locale,
                            vec![],
                        )),
                        false,
                    )
                    .await;
                return Ok(0);
            };
            let server = context.server().clone();

            match resolve_home_world(&server, player.gameprofile.id).await {
                Ok(world) => {
                    let spawn = spawn_of(&world);
                    player.teleport(spawn, None, None, world).await;
                    // Branded success confirmation - the moment the player
                    // actually arrives home, so it gets Ember's signature
                    // gradient. `ember_gradient` distributes color per
                    // character off the component's resolved text (see
                    // `TextComponent::apply_color_effect`), so it must be
                    // applied to an already-resolved plain string (via
                    // `get_translation_text`) rather than a live `Custom`
                    // component - otherwise it would silently render in
                    // English regardless of the player's actual locale.
                    context
                        .source
                        .send_feedback(
                            TextComponent::text_ember(get_translation_text(
                                "ember:commands.home.teleported",
                                locale,
                                vec![],
                            )),
                            false,
                        )
                        .await;
                    Ok(1)
                }
                Err(e) => {
                    let message = match e {
                        HomeError::WorldUnloading => TextComponent::custom(
                            "ember",
                            "commands.home.world_unloading",
                            locale,
                            vec![],
                        ),
                        HomeError::TemplateMissing(template_name) => TextComponent::custom(
                            "ember",
                            "commands.home.template_missing",
                            locale,
                            vec![TextComponent::text(template_name)],
                        ),
                        HomeError::CloneFailed(reason) => TextComponent::custom(
                            "ember",
                            "commands.home.clone_failed",
                            locale,
                            vec![TextComponent::text(reason)],
                        ),
                    };
                    context.source.send_feedback(err_text(message), false).await;
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
