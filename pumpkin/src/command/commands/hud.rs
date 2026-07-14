// EMBER - HUD system command
//
//   /hud toggle   - turn your own boss-bar HUD on/off
//   /hud reload   - re-read hud/hud.toml (admin)

use pumpkin_util::PermissionLvl;
use pumpkin_util::permission::{Permission, PermissionDefault, PermissionRegistry};
use pumpkin_util::text::{TextComponent, color::NamedColor};

use crate::command::argument_builder::{ArgumentBuilder, command, literal};
use crate::command::context::command_context::CommandContext;
use crate::command::node::dispatcher::CommandDispatcher;
use crate::command::node::{CommandExecutor, CommandExecutorResult};

const DESCRIPTION: &str = "Toggle or reload the boss-bar HUD.";
const PERMISSION_TOGGLE: &str = "ember:command.hud.toggle";
const PERMISSION_RELOAD: &str = "ember:command.hud.reload";

fn ok_text(msg: impl Into<String>) -> TextComponent {
    TextComponent::text(msg.into()).color_named(NamedColor::Green)
}

struct HudToggleExecutor;
impl CommandExecutor for HudToggleExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let player = context.source.player_or_err()?;
            let server = context.server();
            let Some(player_arc) = server.get_player_by_uuid(player.gameprofile.id) else {
                return Ok(0);
            };

            // Read the last-known state (defaulting the same way the
            // ticker does) to decide which way to flip it - there's no
            // separate "is this player's HUD currently on" query, this
            // mirrors it inline rather than adding one just for this.
            let currently_on = server
                .hud_manager
                .is_enabled_for_command(player.gameprofile.id)
                .await;
            let now_enabled = !currently_on;
            let took_effect = server
                .hud_manager
                .set_enabled(&player_arc, now_enabled)
                .await;

            let message = if !took_effect {
                "HUD preference saved, but the HUD feature is currently disabled server-wide \
                 (hud.toml's `enabled` is false) - it'll show once an admin turns that on."
            } else if now_enabled {
                "HUD enabled."
            } else {
                "HUD disabled."
            };
            context.source.send_feedback(ok_text(message), false).await;
            Ok(1)
        })
    }
}

struct HudReloadExecutor;
impl CommandExecutor for HudReloadExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            context.server().hud_manager.reload().await;
            context
                .source
                .send_feedback(ok_text("HUD config reloaded."), false)
                .await;
            Ok(1)
        })
    }
}

pub fn register(dispatcher: &mut CommandDispatcher, registry: &mut PermissionRegistry) {
    registry.register_permission_or_panic(Permission::new(
        PERMISSION_TOGGLE,
        "Toggle your own boss-bar HUD on/off.",
        PermissionDefault::Allow,
    ));
    registry.register_permission_or_panic(Permission::new(
        PERMISSION_RELOAD,
        "Reload the HUD's hud.toml.",
        PermissionDefault::Op(PermissionLvl::Three),
    ));

    dispatcher.register(
        command("hud", DESCRIPTION)
            .then(
                literal("toggle")
                    .requires(PERMISSION_TOGGLE)
                    .executes(HudToggleExecutor),
            )
            .then(
                literal("reload")
                    .requires(PERMISSION_RELOAD)
                    .executes(HudReloadExecutor),
            ),
    );
}
// EMBER end
