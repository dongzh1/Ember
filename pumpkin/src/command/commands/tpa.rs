// EMBER - /tpa, /tpahere, /tpaaccept, /tpadeny: player-facing teleport
// requests. `/tpa <player>` asks to teleport to them; `/tpahere <player>`
// asks them to teleport to you. The recipient gets a chat message with
// clickable [accept]/[deny] buttons (`ClickEvent::RunCommand`, same
// mechanism `help.rs` already uses for its own clickable text) alongside
// the plain `/tpaaccept`/`/tpadeny` commands. Requests are tracked by
// `Server::tpa_manager` and expire after `tpa::REQUEST_TIMEOUT_SECS`.

use pumpkin_util::permission::{Permission, PermissionDefault, PermissionRegistry};
use pumpkin_util::text::click::ClickEvent;
use pumpkin_util::text::hover::HoverEvent;
use pumpkin_util::text::{TextComponent, color::NamedColor};

use crate::command::argument_builder::{ArgumentBuilder, argument, command};
use crate::command::argument_types::entity::EntityArgumentType;
use crate::command::context::command_context::CommandContext;
use crate::command::errors::command_syntax_error::CommandSyntaxError;
use crate::command::node::dispatcher::CommandDispatcher;
use crate::command::node::{CommandExecutor, CommandExecutorResult};
use crate::entity::EntityBase;
use crate::server::tpa::TpaKind;

const ARG_PLAYER: &str = "player";
const PERM_TPA: &str = "ember:command.tpa";
const PERM_TPAHERE: &str = "ember:command.tpahere";
const PERM_TPAACCEPT: &str = "ember:command.tpaaccept";
const PERM_TPADENY: &str = "ember:command.tpadeny";

fn err_text(msg: impl Into<String>) -> TextComponent {
    TextComponent::text(msg.into()).color_named(NamedColor::Red)
}

fn action_button(label: &str, color: NamedColor, command: &str, hover: &str) -> TextComponent {
    TextComponent::text(label.to_string())
        .color_named(color)
        .click_event(ClickEvent::RunCommand {
            command: command.to_string().into(),
        })
        .hover_event(HoverEvent::ShowText {
            value: vec![TextComponent::text(hover.to_string()).0],
        })
}

/// Shared body of `/tpa` and `/tpahere`: resolve the requester and target,
/// record the request, and message the target with clickable buttons.
async fn send_request(
    context: &CommandContext<'_>,
    kind: TpaKind,
) -> Result<i32, CommandSyntaxError> {
    let Some(requester) = context.source.output.as_player() else {
        context
            .source
            .send_feedback(err_text("只有玩家可以发起传送请求。"), false)
            .await;
        return Ok(0);
    };
    let target = EntityArgumentType::get_player(context, ARG_PLAYER).await?;

    if target.gameprofile.id == requester.gameprofile.id {
        context
            .source
            .send_feedback(err_text("不能向自己发起传送请求。"), false)
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

    let (request_line, accept_hover) = match kind {
        TpaKind::To => (
            format!("{} 请求传送到你这里。", requester.gameprofile.name),
            "接受后，对方将传送到你身边",
        ),
        TpaKind::Here => (
            format!("{} 请求你传送到TA那里。", requester.gameprofile.name),
            "接受后，你将传送到对方身边",
        ),
    };

    let message = TextComponent::text(request_line)
        .color_named(NamedColor::Gold)
        .add_child(TextComponent::text("  "))
        .add_child(action_button(
            "[接受]",
            NamedColor::Green,
            "/tpaaccept",
            accept_hover,
        ))
        .add_child(TextComponent::text(" "))
        .add_child(action_button(
            "[拒绝]",
            NamedColor::Red,
            "/tpadeny",
            "拒绝这个传送请求",
        ));
    target.send_system_message(&message).await;

    context
        .source
        .send_feedback(
            TextComponent::text(format!(
                "已向 {} 发送传送请求，{} 秒内未回应将自动失效。",
                target.gameprofile.name,
                crate::server::tpa::REQUEST_TIMEOUT_SECS
            ))
            .color_named(NamedColor::Green),
            false,
        )
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
            let Some(accepter) = context.source.output.as_player() else {
                context
                    .source
                    .send_feedback(err_text("只有玩家可以使用 /tpaaccept。"), false)
                    .await;
                return Ok(0);
            };
            let server = context.server();

            let Some((from_uuid, from_name, kind)) =
                server.tpa_manager.take(accepter.gameprofile.id).await
            else {
                context
                    .source
                    .send_feedback(err_text("你目前没有待处理的传送请求。"), false)
                    .await;
                return Ok(0);
            };
            let Some(from_player) = server.get_player_by_uuid(from_uuid) else {
                context
                    .source
                    .send_feedback(err_text(format!("{from_name} 已经不在线了。")), false)
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

            context
                .source
                .send_feedback(
                    TextComponent::text(format!("已接受 {from_name} 的传送请求。"))
                        .color_named(NamedColor::Green),
                    false,
                )
                .await;
            from_player
                .send_system_message(
                    &TextComponent::text(format!(
                        "{} 接受了你的传送请求。",
                        accepter.gameprofile.name
                    ))
                    .color_named(NamedColor::Green),
                )
                .await;
            Ok(1)
        })
    }
}

struct TpaDenyExecutor;

impl CommandExecutor for TpaDenyExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let Some(denier) = context.source.output.as_player() else {
                context
                    .source
                    .send_feedback(err_text("只有玩家可以使用 /tpadeny。"), false)
                    .await;
                return Ok(0);
            };
            let server = context.server();

            let Some((from_uuid, from_name, _kind)) =
                server.tpa_manager.take(denier.gameprofile.id).await
            else {
                context
                    .source
                    .send_feedback(err_text("你目前没有待处理的传送请求。"), false)
                    .await;
                return Ok(0);
            };

            context
                .source
                .send_feedback(
                    TextComponent::text(format!("已拒绝 {from_name} 的传送请求。"))
                        .color_named(NamedColor::Yellow),
                    false,
                )
                .await;
            if let Some(from_player) = server.get_player_by_uuid(from_uuid) {
                from_player
                    .send_system_message(
                        &TextComponent::text(format!(
                            "{} 拒绝了你的传送请求。",
                            denier.gameprofile.name
                        ))
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
