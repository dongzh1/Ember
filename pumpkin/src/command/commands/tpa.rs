// EMBER - /tpa, /tpahere, /tpaaccept, /tpadeny: player-facing teleport
// requests. `/tpa <player>` asks to teleport to them; `/tpahere <player>`
// asks them to teleport to you. The recipient gets a chat message with
// clickable [accept]/[deny] buttons (`ClickEvent::RunCommand`, same
// mechanism `help.rs` already uses for its own clickable text) alongside
// the plain `/tpaaccept`/`/tpadeny` commands. Requests are tracked by
// `Server::tpa_manager` and expire after `tpa::REQUEST_TIMEOUT_SECS`.

use std::str::FromStr;

use pumpkin_util::permission::{Permission, PermissionDefault, PermissionRegistry};
use pumpkin_util::text::click::ClickEvent;
use pumpkin_util::text::hover::HoverEvent;
use pumpkin_util::text::{TextComponent, color::NamedColor};
use pumpkin_util::translation::{Locale, get_translation_text};

use crate::command::argument_builder::{ArgumentBuilder, argument, command};
use crate::command::argument_types::entity::EntityArgumentType;
use crate::command::context::command_context::CommandContext;
use crate::command::errors::command_syntax_error::CommandSyntaxError;
use crate::command::node::dispatcher::CommandDispatcher;
use crate::command::node::{CommandExecutor, CommandExecutorResult};
use crate::entity::EntityBase;
use crate::entity::player::Player;
use crate::server::tpa::TpaKind;

const ARG_PLAYER: &str = "player";
const PERM_TPA: &str = "ember:command.tpa";
const PERM_TPAHERE: &str = "ember:command.tpahere";
const PERM_TPAACCEPT: &str = "ember:command.tpaaccept";
const PERM_TPADENY: &str = "ember:command.tpadeny";

/// Colors an already-translated message as plain error feedback.
fn err_text(msg: TextComponent) -> TextComponent {
    msg.color_named(NamedColor::Red)
}

/// Resolves a player's configured locale directly. Mirrors
/// `CommandSender::get_locale`, which only covers command *senders* - the
/// *other* party in a teleport request is just a `Player`, not necessarily
/// whoever is running the command, so messages addressed to them need
/// their own locale looked up independently.
fn locale_of(player: &Player) -> Locale {
    Locale::from_str(&player.config.load().locale).unwrap_or(Locale::EnUs)
}

/// Builds a clickable action button (the `[accept]`/`[deny]` buttons on an
/// incoming teleport request). `label` and `hover` arrive already styled
/// and translated by the caller - this only wires up the click-to-run
/// command and the hover tooltip.
fn action_button(label: TextComponent, command: &str, hover: TextComponent) -> TextComponent {
    label
        .click_event(ClickEvent::RunCommand {
            command: command.to_string().into(),
        })
        .hover_event(HoverEvent::show_text(hover))
}

/// Shared body of `/tpa` and `/tpahere`: resolve the requester and target,
/// record the request, and message the target with clickable buttons.
async fn send_request(
    context: &CommandContext<'_>,
    kind: TpaKind,
) -> Result<i32, CommandSyntaxError> {
    let locale = context.source.output.get_locale();
    let Some(requester) = context.source.output.as_player() else {
        context
            .source
            .send_feedback(
                err_text(TextComponent::custom(
                    "ember",
                    "commands.tpa.players_only",
                    locale,
                    vec![],
                )),
                false,
            )
            .await;
        return Ok(0);
    };
    let target = EntityArgumentType::get_player(context, ARG_PLAYER).await?;

    if target.gameprofile.id == requester.gameprofile.id {
        context
            .source
            .send_feedback(
                err_text(TextComponent::custom(
                    "ember",
                    "commands.tpa.cannot_target_self",
                    locale,
                    vec![],
                )),
                false,
            )
            .await;
        return Ok(0);
    }

    let server = context.server();
    server
        .tpa_manager
        .request(
            requester.gameprofile.id,
            requester.gameprofile.name.clone(),
            target.gameprofile.id,
            kind,
        )
        .await;

    // The message goes to `target`, not the requester, so it must be
    // rendered in target's own locale rather than `locale` above.
    let target_locale = locale_of(&target);

    // The request headline is the branded/prominent part of this
    // notification, so it gets Ember's signature gradient - which requires
    // an already-resolved plain string (see the comment on the /home and
    // /spawn success messages for why `ember_gradient` can't be applied
    // directly to a live `Custom` translation component). The accept/deny
    // buttons deliberately keep plain green/red instead, for clear click
    // affordance.
    let (request_text, accept_hover) = match kind {
        TpaKind::To => (
            get_translation_text(
                "ember:commands.tpa.request_to",
                target_locale,
                vec![TextComponent::text(requester.gameprofile.name.clone()).0],
            ),
            TextComponent::custom(
                "ember",
                "commands.tpa.accept_hover_to",
                target_locale,
                vec![],
            ),
        ),
        TpaKind::Here => (
            get_translation_text(
                "ember:commands.tpa.request_here",
                target_locale,
                vec![TextComponent::text(requester.gameprofile.name.clone()).0],
            ),
            TextComponent::custom(
                "ember",
                "commands.tpa.accept_hover_here",
                target_locale,
                vec![],
            ),
        ),
    };
    let deny_hover =
        TextComponent::custom("ember", "commands.tpa.deny_hover", target_locale, vec![]);
    let accept_label =
        TextComponent::custom("ember", "commands.tpa.accept_button", target_locale, vec![])
            .color_named(NamedColor::Green);
    let deny_label =
        TextComponent::custom("ember", "commands.tpa.deny_button", target_locale, vec![])
            .color_named(NamedColor::Red);

    let message = TextComponent::text_ember(request_text)
        .add_child(TextComponent::text("  "))
        .add_child(action_button(accept_label, "/tpaaccept", accept_hover))
        .add_child(TextComponent::text(" "))
        .add_child(action_button(deny_label, "/tpadeny", deny_hover));
    target.send_system_message(&message).await;

    // Branded success confirmation back to the requester: their request
    // was sent. Same resolved-string-then-gradient pattern as above.
    let sent_text = get_translation_text(
        "ember:commands.tpa.request_sent",
        locale,
        vec![
            TextComponent::text(target.gameprofile.name.clone()).0,
            TextComponent::text(crate::server::tpa::REQUEST_TIMEOUT_SECS.to_string()).0,
        ],
    );
    context
        .source
        .send_feedback(TextComponent::text_ember(sent_text), false)
        .await;
    Ok(1)
}

struct TpaExecutor;

impl CommandExecutor for TpaExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move { send_request(context, TpaKind::To).await })
    }
}

struct TpaHereExecutor;

impl CommandExecutor for TpaHereExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move { send_request(context, TpaKind::Here).await })
    }
}

struct TpaAcceptExecutor;

impl CommandExecutor for TpaAcceptExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let locale = context.source.output.get_locale();
            let Some(accepter) = context.source.output.as_player() else {
                context
                    .source
                    .send_feedback(
                        err_text(TextComponent::custom(
                            "ember",
                            "commands.tpa.accept_players_only",
                            locale,
                            vec![],
                        )),
                        false,
                    )
                    .await;
                return Ok(0);
            };
            let server = context.server();

            let Some((from_uuid, from_name, kind)) =
                server.tpa_manager.take(accepter.gameprofile.id).await
            else {
                context
                    .source
                    .send_feedback(
                        err_text(TextComponent::custom(
                            "ember",
                            "commands.tpa.no_pending_request",
                            locale,
                            vec![],
                        )),
                        false,
                    )
                    .await;
                return Ok(0);
            };
            let Some(from_player) = server.get_player_by_uuid(from_uuid) else {
                context
                    .source
                    .send_feedback(
                        err_text(TextComponent::custom(
                            "ember",
                            "commands.tpa.target_offline",
                            locale,
                            vec![TextComponent::text(from_name)],
                        )),
                        false,
                    )
                    .await;
                return Ok(0);
            };

            match kind {
                TpaKind::To => {
                    from_player
                        .clone()
                        .teleport(accepter.position(), None, None, accepter.world().clone())
                        .await;
                }
                TpaKind::Here => {
                    accepter
                        .clone()
                        .teleport(
                            from_player.position(),
                            None,
                            None,
                            from_player.world().clone(),
                        )
                        .await;
                }
            }

            // Both parties get a branded confirmation - this is the
            // celebratory "the teleport happened" moment for the accepter
            // and the requester alike. Each is rendered in its own
            // recipient's locale.
            let accepted_text = get_translation_text(
                "ember:commands.tpa.accepted_self",
                locale,
                vec![TextComponent::text(from_name).0],
            );
            context
                .source
                .send_feedback(TextComponent::text_ember(accepted_text), false)
                .await;

            let requester_locale = locale_of(&from_player);
            let notify_text = get_translation_text(
                "ember:commands.tpa.accepted_notify",
                requester_locale,
                vec![TextComponent::text(accepter.gameprofile.name.clone()).0],
            );
            from_player
                .send_system_message(&TextComponent::text_ember(notify_text))
                .await;
            Ok(1)
        })
    }
}

struct TpaDenyExecutor;

impl CommandExecutor for TpaDenyExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let locale = context.source.output.get_locale();
            let Some(denier) = context.source.output.as_player() else {
                context
                    .source
                    .send_feedback(
                        err_text(TextComponent::custom(
                            "ember",
                            "commands.tpa.deny_players_only",
                            locale,
                            vec![],
                        )),
                        false,
                    )
                    .await;
                return Ok(0);
            };
            let server = context.server();

            let Some((from_uuid, from_name, _kind)) =
                server.tpa_manager.take(denier.gameprofile.id).await
            else {
                context
                    .source
                    .send_feedback(
                        err_text(TextComponent::custom(
                            "ember",
                            "commands.tpa.no_pending_request",
                            locale,
                            vec![],
                        )),
                        false,
                    )
                    .await;
                return Ok(0);
            };

            // Declining is a neutral/negative outcome, not a celebratory
            // one, so both sides of this keep their original plain
            // (non-gradient) colors - just made translatable.
            context
                .source
                .send_feedback(
                    TextComponent::custom(
                        "ember",
                        "commands.tpa.denied_self",
                        locale,
                        vec![TextComponent::text(from_name)],
                    )
                    .color_named(NamedColor::Yellow),
                    false,
                )
                .await;
            if let Some(from_player) = server.get_player_by_uuid(from_uuid) {
                let requester_locale = locale_of(&from_player);
                from_player
                    .send_system_message(
                        &TextComponent::custom(
                            "ember",
                            "commands.tpa.denied_notify",
                            requester_locale,
                            vec![TextComponent::text(denier.gameprofile.name.clone())],
                        )
                        .color_named(NamedColor::Red),
                    )
                    .await;
            }
            Ok(1)
        })
    }
}

pub fn register(dispatcher: &mut CommandDispatcher, registry: &mut PermissionRegistry) {
    registry.register_permission_or_panic(Permission::new(
        PERM_TPA,
        "Requests to teleport to another player.",
        PermissionDefault::Allow,
    ));
    registry.register_permission_or_panic(Permission::new(
        PERM_TPAHERE,
        "Requests another player to teleport to you.",
        PermissionDefault::Allow,
    ));
    registry.register_permission_or_panic(Permission::new(
        PERM_TPAACCEPT,
        "Accepts a pending teleport request.",
        PermissionDefault::Allow,
    ));
    registry.register_permission_or_panic(Permission::new(
        PERM_TPADENY,
        "Declines a pending teleport request.",
        PermissionDefault::Allow,
    ));

    dispatcher.register(
        command("tpa", "Requests to teleport to another player.")
            .requires(PERM_TPA)
            .then(argument(ARG_PLAYER, EntityArgumentType::Player).executes(TpaExecutor)),
    );
    dispatcher.register(
        command("tpahere", "Requests another player to teleport to you.")
            .requires(PERM_TPAHERE)
            .then(argument(ARG_PLAYER, EntityArgumentType::Player).executes(TpaHereExecutor)),
    );
    dispatcher.register(
        command("tpaaccept", "Accepts a pending teleport request.")
            .requires(PERM_TPAACCEPT)
            .executes(TpaAcceptExecutor),
    );
    dispatcher.register(
        command("tpadeny", "Declines a pending teleport request.")
            .requires(PERM_TPADENY)
            .executes(TpaDenyExecutor),
    );
}
