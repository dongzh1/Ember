// EMBER - HUD system command
//
//   /hud toggle   - turn your own boss-bar HUD on/off
//   /hud reload   - re-read hud/hud.toml (admin)

use pumpkin_util::PermissionLvl;
use pumpkin_util::permission::{Permission, PermissionDefault, PermissionRegistry};
use pumpkin_util::text::{TextComponent, color::NamedColor};
use pumpkin_util::translation::get_translation_text;

use crate::command::argument_builder::{ArgumentBuilder, command, literal};
use crate::command::context::command_context::CommandContext;
use crate::command::node::dispatcher::CommandDispatcher;
use crate::command::node::{CommandExecutor, CommandExecutorResult};

const DESCRIPTION: &str = "Toggle or reload the boss-bar HUD.";
const PERMISSION_TOGGLE: &str = "ember:command.hud.toggle";
const PERMISSION_RELOAD: &str = "ember:command.hud.reload";

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

            let locale = context.source.output.get_locale();
            // The server-wide-disabled caveat is an informational aside, not
            // a celebratory confirmation, so it keeps a plain color (as
            // before) instead of picking up the ember gradient. The
            // enabled/disabled confirmations, in contrast, are direct
            // success feedback for the command the player just ran, so they
            // get the branded gradient treatment - applied to the already
            // *resolved* string (not the lazy `Custom` component) since
            // `ember_gradient` flattens its input via `get_text(Locale::EnUs)`
            // internally, which would silently discard localization.
            let message = if !took_effect {
                TextComponent::custom("ember", "commands.hud.master_switch_off", locale, vec![])
                    .color_named(NamedColor::Green)
            } else if now_enabled {
                TextComponent::text_ember(get_translation_text(
                    "ember:commands.hud.enabled",
                    locale,
                    vec![],
                ))
            } else {
                TextComponent::text_ember(get_translation_text(
                    "ember:commands.hud.disabled",
                    locale,
                    vec![],
                ))
            };
            context.source.send_feedback(message, false).await;
            Ok(1)
        })
    }
}

struct HudReloadExecutor;
impl CommandExecutor for HudReloadExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            context.server().hud_manager.reload().await;
            let locale = context.source.output.get_locale();
            let message = TextComponent::text_ember(get_translation_text(
                "ember:commands.hud.reloaded",
                locale,
                vec![],
            ));
            context.source.send_feedback(message, false).await;
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
