// EMBER - native physics status command

use pumpkin_util::PermissionLvl;
use pumpkin_util::permission::{Permission, PermissionDefault, PermissionRegistry};
use pumpkin_util::text::{TextComponent, color::NamedColor};

use crate::command::argument_builder::{ArgumentBuilder, command, literal};
use crate::command::context::command_context::CommandContext;
use crate::command::node::dispatcher::CommandDispatcher;
use crate::command::node::{CommandExecutor, CommandExecutorResult};

const DESCRIPTION: &str = "Show native rigid-body physics status.";
const PERMISSION: &str = "ember:command.physics";

struct PhysicsStatusExecutor;

impl CommandExecutor for PhysicsStatusExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let server = context.server();
            let registry = &server.physics_registry;
            let config = registry.config();
            let state = if registry.enabled() {
                "enabled"
            } else {
                "disabled"
            };
            context
                .source
                .send_feedback(
                    TextComponent::text(format!(
                        "Physics: {state}, {} Hz, limit {} bodies/world",
                        config.simulation_hz, config.max_bodies_per_world
                    ))
                    .color_named(if registry.enabled() {
                        NamedColor::Green
                    } else {
                        NamedColor::Yellow
                    }),
                    false,
                )
                .await;

            for world in server.worlds.load().iter() {
                let (bodies, colliders) = world.physics_manager.counts();
                context
                    .source
                    .send_feedback(
                        TextComponent::text(format!(
                            "  {}: {bodies} bodies, {colliders} colliders",
                            world.get_world_name()
                        )),
                        false,
                    )
                    .await;
            }
            let presets = registry.preset_ids().collect::<Vec<_>>().join(", ");
            context
                .source
                .send_feedback(TextComponent::text(format!("  presets: {presets}")), false)
                .await;
            Ok(1)
        })
    }
}

pub fn register(dispatcher: &mut CommandDispatcher, registry: &mut PermissionRegistry) {
    registry.register_permission_or_panic(Permission::new(
        PERMISSION,
        DESCRIPTION,
        PermissionDefault::Op(PermissionLvl::Two),
    ));
    dispatcher.register(
        command("physics", DESCRIPTION).then(
            literal("status")
                .requires(PERMISSION)
                .executes(PhysicsStatusExecutor),
        ),
    );
}
