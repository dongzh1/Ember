// EMBER - packet-only NPC commands
//
//   /npc create <name>              - spawn an NPC at your position, using your own skin
//   /npc create <name> <player>     - ...using an online player's skin instead
//   /npc create <name> as <type>          - spawn as any entity type (default look)
//   /npc create <name> as <type> <extra>  - ...with a per-type extra: a player name
//                                            (player/mannequin skin), a block name
//                                            (falling_block), or an item name (item)
//   /npc remove <name>              - delete an NPC
//   /npc list                       - list all NPCs (clickable -> /npc info)
//   /npc info <name>                - clickable property viewer/editor
//   /npc move <name>                - move an existing NPC to your position
//   /npc skin <name> <player>       - re-copy an NPC's skin from an online player
//   /npc setaction <name> <command> - run a console command on click (%player% placeholder)
//   /npc clearaction <name>         - make the NPC purely decorative again
//   /npc lookat <name> on|off       - continuously face the nearest visible player
//   /npc gravity <name> on|off      - fall when the block underneath isn't solid
//   /npc sneak <name> on|off        - client-side crouch pose
//   /npc swing <name>               - play the swing-main-arm animation once
//   /npc moveto <name>              - walk (not teleport) to your position
//   /npc wander <name> on <radius>  - randomly wander within <radius> blocks of home
//   /npc wander <name> off          - stop wandering
//   /npc hide <name> <player>       - hide from a specific player regardless of distance
//   /npc show <name> <player>       - undo /npc hide
//   /npc distance <name> [blocks]   - override view distance (omit to reset to default)
//   /npc escort <name> <player>     - follow <player> indefinitely
//   /npc escort <name> <player> here - lead <player> to your position; ends on arrival
//   /npc escort <name> stop         - stop escorting

use pumpkin_data::Block;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_util::PermissionLvl;
use pumpkin_util::permission::{Permission, PermissionDefault, PermissionRegistry};
use pumpkin_util::text::click::ClickEvent;
use pumpkin_util::text::hover::HoverEvent;
use pumpkin_util::text::{TextComponent, color::NamedColor};

use crate::command::argument_builder::{ArgumentBuilder, argument, command, literal};
use crate::command::argument_types::core::integer::IntegerArgumentType;
use crate::command::argument_types::core::string::StringArgumentType;
use crate::command::argument_types::entity::EntityArgumentType;
use crate::command::argument_types::game_profile::GameProfileArgumentType;
use crate::command::context::command_context::CommandContext;
use crate::command::node::dispatcher::CommandDispatcher;
use crate::command::node::{CommandExecutor, CommandExecutorResult};
use crate::command::suggestion::provider::{SuggestionProvider, SuggestionProviderResult};
use crate::command::suggestion::suggestions::SuggestionsBuilder;
use crate::data::npc::NpcEntry;
use crate::entity::EntityBase;
use crate::server::npc::{resolve_entity_type, supports_skin};

const DESCRIPTION: &str = "Manage packet-only NPCs: create, remove, list, move, re-skin.";
const PERMISSION: &str = "ember:command.npc";

const ARG_NAME: &str = "name";
const ARG_SKIN_PLAYER: &str = "player";
const ARG_COMMAND: &str = "command";
const ARG_ENTITY_TYPE: &str = "entity_type";
const ARG_EXTRA: &str = "extra";
const ARG_RADIUS: &str = "radius";
const ARG_TARGET: &str = "target";
const ARG_DISTANCE: &str = "distance";
const ARG_ESCORT_PLAYER: &str = "player";

async fn feedback(context: &CommandContext<'_>, msg: TextComponent) {
    context.source.send_feedback(msg, false).await;
}

fn err_text(msg: impl Into<String>) -> TextComponent {
    TextComponent::text(msg.into()).color_named(NamedColor::Red)
}

fn ok_text(msg: impl Into<String>) -> TextComponent {
    TextComponent::text(msg.into()).color_named(NamedColor::Green)
}

// EMBER start - NPC info (clickable property viewer/editor)
/// A clickable `[label]` button that runs `command` immediately on click -
/// same mechanism `tpa.rs`'s accept/deny buttons and `help.rs`'s command
/// links already use. For instant toggles, not for anything destructive
/// enough to want a second chance before it fires.
fn run_button(label: &str, color: NamedColor, command: String, hover: &str) -> TextComponent {
    TextComponent::text(format!("[{label}]"))
        .color_named(color)
        .click_event(ClickEvent::RunCommand {
            command: command.into(),
        })
        .hover_event(HoverEvent::show_text(TextComponent::text(
            hover.to_string(),
        )))
}

/// A clickable `[label]` button that pre-fills `command` into the chat box
/// instead of running it - for edits that need a typed value, and for
/// anything irreversible (still one click away, but not a single misclick).
fn suggest_button(label: &str, command: String, hover: &str) -> TextComponent {
    TextComponent::text(format!("[{label}]"))
        .color_named(NamedColor::Aqua)
        .click_event(ClickEvent::SuggestCommand {
            command: command.into(),
        })
        .hover_event(HoverEvent::show_text(TextComponent::text(
            hover.to_string(),
        )))
}

/// A plain `label: value` line, no button - for read-only info.
fn info_line(label: &str, value: impl Into<String>) -> TextComponent {
    TextComponent::text(format!("{label}: "))
        .color_named(NamedColor::Gray)
        .add_child(
            TextComponent::text(format!("{}\n", value.into())).color_named(NamedColor::White),
        )
}

/// A `label: 开/关 [switch]` line for a per-NPC boolean toggle, where
/// `subcommand` is the `/npc <subcommand> <name> on|off` command that flips
/// it (`lookat`/`sneak`/`gravity` today).
fn toggle_line(label: &str, npc_name: &str, subcommand: &str, enabled: bool) -> TextComponent {
    let (state_text, state_color) = if enabled {
        ("开", NamedColor::Green)
    } else {
        ("关", NamedColor::Red)
    };
    let (next_label, next_state) = if enabled {
        ("关闭", "off")
    } else {
        ("开启", "on")
    };
    TextComponent::text(format!("{label}: "))
        .color_named(NamedColor::Gray)
        .add_child(TextComponent::text(state_text).color_named(state_color))
        .add_child(TextComponent::text(" "))
        .add_child(run_button(
            next_label,
            NamedColor::Yellow,
            format!("/npc {subcommand} {npc_name} {next_state}"),
            &format!("点击{next_label} {label}"),
        ))
        .add_child(TextComponent::text("\n"))
}
// EMBER end

/// An NPC's name doubles as its fake tab-list username (see
/// `server::npc::NpcManager::send_spawn`), so it's held to the same charset
/// Minecraft enforces for real usernames.
fn validate_npc_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 16 {
        return Err("NPC names must be 1-16 characters.".to_string());
    }
    if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Ok(())
    } else {
        Err("NPC names may only contain letters, digits and underscores.".to_string())
    }
}

struct NpcCreateExecutor {
    has_skin_player: bool,
}
impl CommandExecutor for NpcCreateExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let sender = context.source.player_or_err()?;
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            if let Err(e) = validate_npc_name(&name) {
                feedback(context, err_text(e)).await;
                return Ok(0);
            }

            let entity = sender.get_entity();
            let pos = entity.pos.load();
            let entry = NpcEntry {
                name: name.clone(),
                world: sender.world().get_world_name().to_string(),
                x: pos.x,
                y: pos.y,
                z: pos.z,
                yaw: entity.yaw.load(),
                pitch: entity.pitch.load(),
                skin: None,
                click_command: None,
                entity_type: "player".to_string(),
                block: None,
                item: None,
                look_at_nearest_player: false,
                sneaking: false,
                wander_radius: None,
                hidden_from: std::collections::HashSet::new(),
                visible_distance: None,
                gravity: false,
            };

            let server = context.server();
            if let Err(e) = server.npc_manager.create(entry).await {
                feedback(context, err_text(e)).await;
                return Ok(0);
            }

            let skin_result = if self.has_skin_player {
                let skin_source = EntityArgumentType::get_player(context, ARG_SKIN_PLAYER).await?;
                server
                    .npc_manager
                    .set_skin(server, &name, &skin_source)
                    .await
            } else {
                server.npc_manager.set_skin(server, &name, sender).await
            };
            if let Err(e) = skin_result {
                feedback(context, err_text(e)).await;
                return Ok(0);
            }

            feedback(context, ok_text(format!("NPC '{name}' created."))).await;
            Ok(1)
        })
    }
}

/// `/npc create <name> as <entity-type> [extra]` — any entity type, not just
/// a fake player. `extra`'s meaning depends on the resolved type: a player
/// name (skin source) for `player`/`mannequin`, a block name for
/// `falling_block`, an item name for `item`; any other type doesn't take one.
struct NpcCreateAsExecutor {
    has_extra: bool,
}
impl CommandExecutor for NpcCreateAsExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let sender = context.source.player_or_err()?;
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            if let Err(e) = validate_npc_name(&name) {
                feedback(context, err_text(e)).await;
                return Ok(0);
            }

            let type_name = StringArgumentType::get(context, ARG_ENTITY_TYPE)?.to_string();
            let Some(entity_type) = EntityType::from_name(&type_name.to_lowercase()) else {
                feedback(
                    context,
                    err_text(format!("Unknown entity type '{type_name}'.")),
                )
                .await;
                return Ok(0);
            };

            let mut skin = None;
            let mut block = None;
            let mut item = None;

            if self.has_extra {
                let extra = StringArgumentType::get(context, ARG_EXTRA)?.to_string();
                if supports_skin(entity_type) {
                    let Some(source) = context.server().get_player_by_name(&extra) else {
                        feedback(
                            context,
                            err_text(format!("Player '{extra}' is not online.")),
                        )
                        .await;
                        return Ok(0);
                    };
                    skin = source
                        .gameprofile
                        .properties
                        .load()
                        .iter()
                        .find(|p| &*p.name == "textures")
                        .cloned();
                } else if entity_type == &EntityType::FALLING_BLOCK {
                    if Block::from_name(&extra.to_lowercase()).is_none() {
                        feedback(context, err_text(format!("Unknown block '{extra}'."))).await;
                        return Ok(0);
                    }
                    block = Some(extra);
                } else if entity_type == &EntityType::ITEM {
                    if Item::from_registry_key(&extra.to_lowercase()).is_none() {
                        feedback(context, err_text(format!("Unknown item '{extra}'."))).await;
                        return Ok(0);
                    }
                    item = Some(extra);
                } else {
                    feedback(
                        context,
                        err_text(format!(
                            "Entity type '{type_name}' doesn't take an extra argument."
                        )),
                    )
                    .await;
                    return Ok(0);
                }
            }

            let entity = sender.get_entity();
            let pos = entity.pos.load();
            let entry = NpcEntry {
                name: name.clone(),
                world: sender.world().get_world_name().to_string(),
                x: pos.x,
                y: pos.y,
                z: pos.z,
                yaw: entity.yaw.load(),
                pitch: entity.pitch.load(),
                skin,
                click_command: None,
                entity_type: entity_type.resource_name.to_string(),
                block,
                item,
                look_at_nearest_player: false,
                sneaking: false,
                wander_radius: None,
                hidden_from: std::collections::HashSet::new(),
                visible_distance: None,
                gravity: false,
            };

            if let Err(e) = context.server().npc_manager.create(entry).await {
                feedback(context, err_text(e)).await;
                return Ok(0);
            }

            feedback(
                context,
                ok_text(format!(
                    "NPC '{name}' created as '{}'.",
                    entity_type.resource_name
                )),
            )
            .await;
            Ok(1)
        })
    }
}

struct NpcRemoveExecutor;
impl CommandExecutor for NpcRemoveExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let server = context.server();
            match server.npc_manager.remove(server, &name).await {
                Ok(()) => {
                    feedback(context, ok_text(format!("NPC '{name}' removed."))).await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, err_text(e)).await;
                    Ok(0)
                }
            }
        })
    }
}

struct NpcListExecutor;
impl CommandExecutor for NpcListExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let npcs = context.server().npc_manager.list().await;
            if npcs.is_empty() {
                feedback(context, TextComponent::text("No NPCs exist.")).await;
                return Ok(0);
            }
            let mut message = TextComponent::text(format!("NPCs ({}):\n", npcs.len()))
                .color_named(NamedColor::Gray);
            for npc in &npcs {
                message = message
                    .add_child(
                        TextComponent::text(format!(
                            "  {} @ {} ({:.1}, {:.1}, {:.1}) ",
                            npc.name, npc.world, npc.x, npc.y, npc.z
                        ))
                        .color_named(NamedColor::White),
                    )
                    .add_child(run_button(
                        "详情",
                        NamedColor::Aqua,
                        format!("/npc info {}", npc.name),
                        "查看/修改这个NPC的属性",
                    ))
                    .add_child(TextComponent::text("\n"));
            }
            feedback(context, message).await;
            #[expect(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            Ok(npcs.len() as i32)
        })
    }
}

// EMBER start - NPC info (clickable property viewer/editor)
fn info_header_and_position(entry: &NpcEntry) -> TextComponent {
    let name = entry.name.as_str();
    TextComponent::text(format!("=== NPC '{name}' ===\n"))
        .color_named(NamedColor::Gold)
        .add_child(info_line("类型", entry.entity_type.clone()))
        .add_child(
            TextComponent::text("位置: ")
                .color_named(NamedColor::Gray)
                .add_child(
                    TextComponent::text(format!(
                        "{} ({:.1}, {:.1}, {:.1}) yaw={:.0} ",
                        entry.world, entry.x, entry.y, entry.z, entry.yaw
                    ))
                    .color_named(NamedColor::White),
                )
                .add_child(suggest_button(
                    "移到我的位置",
                    format!("/npc move {name}"),
                    "先站到目标位置,再点击执行",
                ))
                .add_child(TextComponent::text("\n")),
        )
}

fn info_skin_line(entry: &NpcEntry) -> Option<TextComponent> {
    if !supports_skin(resolve_entity_type(entry)) {
        return None;
    }
    let name = entry.name.as_str();
    let skin_desc = entry
        .skin
        .as_ref()
        .map_or_else(|| "默认".to_string(), |_| "已自定义".to_string());
    Some(
        TextComponent::text("皮肤: ")
            .color_named(NamedColor::Gray)
            .add_child(TextComponent::text(skin_desc).color_named(NamedColor::White))
            .add_child(TextComponent::text(" "))
            .add_child(suggest_button(
                "改",
                format!("/npc skin {name} "),
                "输入一个在线玩家名,复制其皮肤",
            ))
            .add_child(TextComponent::text("\n")),
    )
}

fn info_action_line(entry: &NpcEntry) -> TextComponent {
    let name = entry.name.as_str();
    let action_desc = entry
        .click_command
        .clone()
        .unwrap_or_else(|| "无(纯装饰)".to_string());
    TextComponent::text("点击指令: ")
        .color_named(NamedColor::Gray)
        .add_child(TextComponent::text(action_desc).color_named(NamedColor::White))
        .add_child(TextComponent::text(" "))
        .add_child(suggest_button(
            "改",
            format!("/npc setaction {name} "),
            "输入玩家点击这个NPC时执行的控制台命令,%player%会替换成点击者",
        ))
        .add_child(run_button(
            "清除",
            NamedColor::Red,
            format!("/npc clearaction {name}"),
            "清除点击指令,NPC变回纯装饰",
        ))
        .add_child(TextComponent::text("\n"))
}

fn info_wander_line(entry: &NpcEntry) -> TextComponent {
    let name = entry.name.as_str();
    let wander_desc = entry
        .wander_radius
        .map_or_else(|| "未启用".to_string(), |r| format!("{r:.0} 格"));
    TextComponent::text("漫游半径: ")
        .color_named(NamedColor::Gray)
        .add_child(TextComponent::text(wander_desc).color_named(NamedColor::White))
        .add_child(TextComponent::text(" "))
        .add_child(suggest_button(
            "改",
            format!("/npc wander {name} on "),
            "输入漫游半径(格数)",
        ))
        .add_child(run_button(
            "关闭",
            NamedColor::Yellow,
            format!("/npc wander {name} off"),
            "停止漫游,原地停下",
        ))
        .add_child(TextComponent::text("\n"))
}

fn info_distance_line(entry: &NpcEntry) -> TextComponent {
    let name = entry.name.as_str();
    let distance_desc = entry.visible_distance.map_or_else(
        || "默认(跟随观察者客户端视距)".to_string(),
        |d| format!("{d:.0} 格"),
    );
    TextComponent::text("可见距离: ")
        .color_named(NamedColor::Gray)
        .add_child(TextComponent::text(distance_desc).color_named(NamedColor::White))
        .add_child(TextComponent::text(" "))
        .add_child(suggest_button(
            "改",
            format!("/npc distance {name} "),
            "输入可见距离(格数)",
        ))
        .add_child(TextComponent::text("\n"))
}

fn info_footer_line(entry: &NpcEntry) -> TextComponent {
    let name = entry.name.as_str();
    TextComponent::text("其他: ")
        .color_named(NamedColor::Gray)
        .add_child(run_button(
            "挥手",
            NamedColor::Yellow,
            format!("/npc swing {name}"),
            "播放一次挥手动画",
        ))
        .add_child(TextComponent::text(" "))
        .add_child(suggest_button(
            "移除NPC",
            format!("/npc remove {name}"),
            "彻底删除这个NPC,不可撤销 - 点击后请再确认一次按回车",
        ))
}

/// Builds the full `/npc info` message. Split out from the executor itself
/// purely to keep it under clippy's line-count lint - same reasoning
/// `server::npc::NpcManager::send_spawn`/`send_spawn_metadata` already
/// split on.
fn build_info_message(entry: &NpcEntry) -> TextComponent {
    let mut message = info_header_and_position(entry);
    if let Some(skin_line) = info_skin_line(entry) {
        message = message.add_child(skin_line);
    }
    message = message
        .add_child(toggle_line(
            "看向玩家",
            &entry.name,
            "lookat",
            entry.look_at_nearest_player,
        ))
        .add_child(toggle_line("潜行", &entry.name, "sneak", entry.sneaking))
        .add_child(toggle_line("重力", &entry.name, "gravity", entry.gravity))
        .add_child(info_action_line(entry))
        .add_child(info_wander_line(entry))
        .add_child(info_distance_line(entry));
    if !entry.hidden_from.is_empty() {
        message = message.add_child(info_line(
            "对指定玩家隐藏",
            format!("{} 人", entry.hidden_from.len()),
        ));
    }
    message.add_child(info_footer_line(entry))
}

struct NpcInfoExecutor;
impl CommandExecutor for NpcInfoExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let requested_name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let npcs = context.server().npc_manager.list().await;
            let Some(entry) = npcs
                .iter()
                .find(|n| n.name.eq_ignore_ascii_case(&requested_name))
            else {
                feedback(
                    context,
                    err_text(format!("No NPC named '{requested_name}' exists.")),
                )
                .await;
                return Ok(0);
            };

            feedback(context, build_info_message(entry)).await;
            Ok(1)
        })
    }
}
// EMBER end

struct NpcMoveExecutor;
impl CommandExecutor for NpcMoveExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let sender = context.source.player_or_err()?;
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let entity = sender.get_entity();
            let pos = entity.pos.load();
            let (yaw, pitch) = (entity.yaw.load(), entity.pitch.load());
            let world = sender.world().get_world_name().to_string();

            let server = context.server();
            let result = server
                .npc_manager
                .move_to(server, &name, world, pos, yaw, pitch)
                .await;
            match result {
                Ok(()) => {
                    feedback(context, ok_text(format!("NPC '{name}' moved."))).await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, err_text(e)).await;
                    Ok(0)
                }
            }
        })
    }
}

// EMBER start - NPC movement (moveto/wander)
struct NpcMoveToExecutor;
impl CommandExecutor for NpcMoveToExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let sender = context.source.player_or_err()?;
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let target = sender.get_entity().pos.load();

            match context.server().npc_manager.walk_to(&name, target).await {
                Ok(()) => {
                    feedback(context, ok_text(format!("NPC '{name}' is walking over."))).await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, err_text(e)).await;
                    Ok(0)
                }
            }
        })
    }
}

struct NpcWanderExecutor {
    enabled: bool,
}
impl CommandExecutor for NpcWanderExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let radius = if self.enabled {
                Some(f64::from(IntegerArgumentType::get(context, ARG_RADIUS)?))
            } else {
                None
            };
            let server = context.server();
            let result = server
                .npc_manager
                .set_wander_radius(server, &name, radius)
                .await;
            match result {
                Ok(()) => {
                    let message = radius.map_or_else(
                        || format!("NPC '{name}' stopped wandering."),
                        |radius| format!("NPC '{name}' wandering within {radius} blocks of home."),
                    );
                    feedback(context, ok_text(message)).await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, err_text(e)).await;
                    Ok(0)
                }
            }
        })
    }
}
// EMBER end

// EMBER start - per-player NPC visibility control
struct NpcHideExecutor;
impl CommandExecutor for NpcHideExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let profiles = GameProfileArgumentType::get(context, ARG_TARGET).await?;
            let Some(target) = profiles.into_iter().next() else {
                feedback(context, err_text("No matching player.")).await;
                return Ok(0);
            };
            let server = context.server();
            match server.npc_manager.hide_from(server, &name, target.id).await {
                Ok(()) => {
                    feedback(
                        context,
                        ok_text(format!("NPC '{name}' is now hidden from {}.", target.name)),
                    )
                    .await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, err_text(e)).await;
                    Ok(0)
                }
            }
        })
    }
}

struct NpcShowExecutor;
impl CommandExecutor for NpcShowExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let profiles = GameProfileArgumentType::get(context, ARG_TARGET).await?;
            let Some(target) = profiles.into_iter().next() else {
                feedback(context, err_text("No matching player.")).await;
                return Ok(0);
            };
            let server = context.server();
            match server.npc_manager.show_to(server, &name, target.id).await {
                Ok(()) => {
                    feedback(
                        context,
                        ok_text(format!("NPC '{name}' is visible to {} again.", target.name)),
                    )
                    .await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, err_text(e)).await;
                    Ok(0)
                }
            }
        })
    }
}

struct NpcDistanceExecutor {
    has_distance: bool,
}
impl CommandExecutor for NpcDistanceExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let blocks = if self.has_distance {
                Some(f64::from(IntegerArgumentType::get(context, ARG_DISTANCE)?))
            } else {
                None
            };
            let server = context.server();
            let result = server
                .npc_manager
                .set_visible_distance(server, &name, blocks)
                .await;
            match result {
                Ok(()) => {
                    let message = blocks.map_or_else(
                        || format!("NPC '{name}' uses each viewer's normal view distance again."),
                        |blocks| format!("NPC '{name}' visible distance set to {blocks} blocks."),
                    );
                    feedback(context, ok_text(message)).await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, err_text(e)).await;
                    Ok(0)
                }
            }
        })
    }
}
// EMBER end

// EMBER start - NPC escort (guide)
struct NpcEscortExecutor {
    lead_to_sender: bool,
}
impl CommandExecutor for NpcEscortExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let target = EntityArgumentType::get_player(context, ARG_ESCORT_PLAYER).await?;
            let destination = if self.lead_to_sender {
                let sender = context.source.player_or_err()?;
                Some(sender.get_entity().pos.load())
            } else {
                None
            };

            let result = context
                .server()
                .npc_manager
                .escort(&name, target.gameprofile.id, destination)
                .await;
            match result {
                Ok(()) => {
                    let message = if self.lead_to_sender {
                        format!("NPC '{name}' is leading {} here.", target.gameprofile.name)
                    } else {
                        format!("NPC '{name}' is now following {}.", target.gameprofile.name)
                    };
                    feedback(context, ok_text(message)).await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, err_text(e)).await;
                    Ok(0)
                }
            }
        })
    }
}

struct NpcEscortStopExecutor;
impl CommandExecutor for NpcEscortStopExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            match context.server().npc_manager.stop_escort(&name).await {
                Ok(()) => {
                    feedback(context, ok_text(format!("NPC '{name}' stopped escorting."))).await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, err_text(e)).await;
                    Ok(0)
                }
            }
        })
    }
}
// EMBER end

struct NpcSkinExecutor;
impl CommandExecutor for NpcSkinExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let skin_source = EntityArgumentType::get_player(context, ARG_SKIN_PLAYER).await?;
            let server = context.server();
            match server
                .npc_manager
                .set_skin(server, &name, &skin_source)
                .await
            {
                Ok(()) => {
                    feedback(context, ok_text(format!("NPC '{name}' re-skinned."))).await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, err_text(e)).await;
                    Ok(0)
                }
            }
        })
    }
}

struct NpcSetActionExecutor;
impl CommandExecutor for NpcSetActionExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let command = StringArgumentType::get(context, ARG_COMMAND)?.to_string();
            let result = context
                .server()
                .npc_manager
                .set_action(&name, Some(command))
                .await;
            match result {
                Ok(()) => {
                    feedback(context, ok_text(format!("NPC '{name}' click action set."))).await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, err_text(e)).await;
                    Ok(0)
                }
            }
        })
    }
}

struct NpcClearActionExecutor;
impl CommandExecutor for NpcClearActionExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let result = context.server().npc_manager.set_action(&name, None).await;
            match result {
                Ok(()) => {
                    feedback(
                        context,
                        ok_text(format!("NPC '{name}' click action cleared.")),
                    )
                    .await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, err_text(e)).await;
                    Ok(0)
                }
            }
        })
    }
}

// EMBER start - basic NPC actions (look-at, sneak, swing)
struct NpcLookAtExecutor {
    enabled: bool,
}
impl CommandExecutor for NpcLookAtExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let result = context
                .server()
                .npc_manager
                .set_look_at_nearest_player(&name, self.enabled)
                .await;
            match result {
                Ok(()) => {
                    let state = if self.enabled { "on" } else { "off" };
                    feedback(
                        context,
                        ok_text(format!("NPC '{name}' look-at-nearest-player {state}.")),
                    )
                    .await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, err_text(e)).await;
                    Ok(0)
                }
            }
        })
    }
}

// EMBER start - NPC gravity
struct NpcGravityExecutor {
    enabled: bool,
}
impl CommandExecutor for NpcGravityExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let result = context
                .server()
                .npc_manager
                .set_gravity(&name, self.enabled)
                .await;
            match result {
                Ok(()) => {
                    let state = if self.enabled { "on" } else { "off" };
                    feedback(context, ok_text(format!("NPC '{name}' gravity {state}."))).await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, err_text(e)).await;
                    Ok(0)
                }
            }
        })
    }
}
// EMBER end

struct NpcSneakExecutor {
    sneaking: bool,
}
impl CommandExecutor for NpcSneakExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let server = context.server();
            let result = server
                .npc_manager
                .set_sneaking(server, &name, self.sneaking)
                .await;
            match result {
                Ok(()) => {
                    let state = if self.sneaking { "on" } else { "off" };
                    feedback(context, ok_text(format!("NPC '{name}' sneaking {state}."))).await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, err_text(e)).await;
                    Ok(0)
                }
            }
        })
    }
}

struct NpcSwingExecutor;
impl CommandExecutor for NpcSwingExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let server = context.server();
            match server.npc_manager.swing_arm(server, &name).await {
                Ok(()) => {
                    feedback(context, ok_text(format!("NPC '{name}' swung its arm."))).await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, err_text(e)).await;
                    Ok(0)
                }
            }
        })
    }
}
// EMBER end

/// Suggests names of existing NPCs — used by every subcommand that acts on
/// an already-created NPC (everything except `create`, which names a new one).
struct NpcNameSuggestionProvider;

impl SuggestionProvider for NpcNameSuggestionProvider {
    fn suggest<'a>(
        &'a self,
        context: &'a CommandContext,
        builder: SuggestionsBuilder,
    ) -> SuggestionProviderResult<'a> {
        Box::pin(async move {
            let names = context
                .server()
                .npc_manager
                .list()
                .await
                .into_iter()
                .map(|npc| npc.name);
            builder.filter_and_suggest_iter(names).build()
        })
    }
}

/// Suggests every known entity type's resource name (`player`, `mannequin`,
/// `zombie`, ...). There's no exposed "all entity types" slice in
/// `pumpkin-data`, so this just scans ids past the current known range —
/// harmless since `from_raw` returns `None` for anything unused.
struct EntityTypeSuggestionProvider;

impl SuggestionProvider for EntityTypeSuggestionProvider {
    fn suggest<'a>(
        &'a self,
        _context: &'a CommandContext,
        builder: SuggestionsBuilder,
    ) -> SuggestionProviderResult<'a> {
        Box::pin(async move {
            let names =
                (0u16..512).filter_map(|id| EntityType::from_raw(id).map(|e| e.resource_name));
            builder.filter_and_suggest_iter(names).build()
        })
    }
}

/// Suggests values for `extra`, whose meaning depends on the `entity_type`
/// already typed earlier in the same command (see `NpcCreateAsExecutor`):
/// online player names for skin-supporting types, or every known block name
/// for `falling_block`. `item` has no candidate list — `pumpkin-data` has no
/// "all items" slice or id-based scan the way `Block`/`EntityType` do (only
/// a name-keyed match), so it's left as free text.
struct NpcExtraSuggestionProvider;

impl SuggestionProvider for NpcExtraSuggestionProvider {
    fn suggest<'a>(
        &'a self,
        context: &'a CommandContext,
        builder: SuggestionsBuilder,
    ) -> SuggestionProviderResult<'a> {
        Box::pin(async move {
            let Some(entity_type) = StringArgumentType::get(context, ARG_ENTITY_TYPE)
                .ok()
                .and_then(|type_name| EntityType::from_name(&type_name.to_lowercase()))
            else {
                return builder.build();
            };

            if supports_skin(entity_type) {
                let names = context
                    .server()
                    .get_all_players()
                    .into_iter()
                    .map(|p| p.gameprofile.name.clone());
                builder.filter_and_suggest_iter(names).build()
            } else if entity_type == &EntityType::FALLING_BLOCK {
                let names = (0u16..4096).filter_map(|id| {
                    pumpkin_data::BlockId::new(id).map(|id| Block::from_id(id).name)
                });
                builder.filter_and_suggest_iter(names).build()
            } else {
                builder.build()
            }
        })
    }
}

// EMBER: a long but flat builder chain, not complex logic - splitting it
// would just scatter one command tree across multiple functions.
#[expect(clippy::too_many_lines)]
pub fn register(dispatcher: &mut CommandDispatcher, registry: &mut PermissionRegistry) {
    registry.register_permission_or_panic(Permission::new(
        PERMISSION,
        DESCRIPTION,
        PermissionDefault::Op(PermissionLvl::Three),
    ));

    dispatcher.register(
        command("npc", DESCRIPTION)
            .requires(PERMISSION)
            .then(
                literal("create").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord)
                        .executes(NpcCreateExecutor {
                            has_skin_player: false,
                        })
                        .then(
                            argument(ARG_SKIN_PLAYER, EntityArgumentType::Player).executes(
                                NpcCreateExecutor {
                                    has_skin_player: true,
                                },
                            ),
                        )
                        .then(
                            literal("as").then(
                                argument(ARG_ENTITY_TYPE, StringArgumentType::SingleWord)
                                    .suggests(EntityTypeSuggestionProvider)
                                    .executes(NpcCreateAsExecutor { has_extra: false })
                                    .then(
                                        argument(ARG_EXTRA, StringArgumentType::SingleWord)
                                            .suggests(NpcExtraSuggestionProvider)
                                            .executes(NpcCreateAsExecutor { has_extra: true }),
                                    ),
                            ),
                        ),
                ),
            )
            .then(
                literal("remove").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord)
                        .suggests(NpcNameSuggestionProvider)
                        .executes(NpcRemoveExecutor),
                ),
            )
            .then(literal("list").executes(NpcListExecutor))
            .then(
                literal("info").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord)
                        .suggests(NpcNameSuggestionProvider)
                        .executes(NpcInfoExecutor),
                ),
            )
            .then(
                literal("move").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord)
                        .suggests(NpcNameSuggestionProvider)
                        .executes(NpcMoveExecutor),
                ),
            )
            .then(
                literal("skin").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord)
                        .suggests(NpcNameSuggestionProvider)
                        .then(
                            argument(ARG_SKIN_PLAYER, EntityArgumentType::Player)
                                .executes(NpcSkinExecutor),
                        ),
                ),
            )
            .then(
                literal("setaction").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord)
                        .suggests(NpcNameSuggestionProvider)
                        .then(
                            argument(ARG_COMMAND, StringArgumentType::GreedyPhrase)
                                .executes(NpcSetActionExecutor),
                        ),
                ),
            )
            .then(
                literal("clearaction").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord)
                        .suggests(NpcNameSuggestionProvider)
                        .executes(NpcClearActionExecutor),
                ),
            )
            .then(
                literal("lookat").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord)
                        .suggests(NpcNameSuggestionProvider)
                        .then(literal("on").executes(NpcLookAtExecutor { enabled: true }))
                        .then(literal("off").executes(NpcLookAtExecutor { enabled: false })),
                ),
            )
            .then(
                literal("gravity").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord)
                        .suggests(NpcNameSuggestionProvider)
                        .then(literal("on").executes(NpcGravityExecutor { enabled: true }))
                        .then(literal("off").executes(NpcGravityExecutor { enabled: false })),
                ),
            )
            .then(
                literal("sneak").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord)
                        .suggests(NpcNameSuggestionProvider)
                        .then(literal("on").executes(NpcSneakExecutor { sneaking: true }))
                        .then(literal("off").executes(NpcSneakExecutor { sneaking: false })),
                ),
            )
            .then(
                literal("swing").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord)
                        .suggests(NpcNameSuggestionProvider)
                        .executes(NpcSwingExecutor),
                ),
            )
            .then(
                literal("moveto").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord)
                        .suggests(NpcNameSuggestionProvider)
                        .executes(NpcMoveToExecutor),
                ),
            )
            .then(
                literal("wander").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord)
                        .suggests(NpcNameSuggestionProvider)
                        .then(literal("off").executes(NpcWanderExecutor { enabled: false }))
                        .then(
                            literal("on").then(
                                argument(ARG_RADIUS, IntegerArgumentType::with_min(1))
                                    .executes(NpcWanderExecutor { enabled: true }),
                            ),
                        ),
                ),
            )
            .then(
                literal("hide").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord)
                        .suggests(NpcNameSuggestionProvider)
                        .then(
                            argument(ARG_TARGET, GameProfileArgumentType).executes(NpcHideExecutor),
                        ),
                ),
            )
            .then(
                literal("show").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord)
                        .suggests(NpcNameSuggestionProvider)
                        .then(
                            argument(ARG_TARGET, GameProfileArgumentType).executes(NpcShowExecutor),
                        ),
                ),
            )
            .then(
                literal("distance").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord)
                        .suggests(NpcNameSuggestionProvider)
                        .executes(NpcDistanceExecutor {
                            has_distance: false,
                        })
                        .then(
                            argument(ARG_DISTANCE, IntegerArgumentType::with_min(1))
                                .executes(NpcDistanceExecutor { has_distance: true }),
                        ),
                ),
            )
            .then(
                literal("escort").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord)
                        .suggests(NpcNameSuggestionProvider)
                        .then(literal("stop").executes(NpcEscortStopExecutor))
                        .then(
                            argument(ARG_ESCORT_PLAYER, EntityArgumentType::Player)
                                .executes(NpcEscortExecutor {
                                    lead_to_sender: false,
                                })
                                .then(literal("here").executes(NpcEscortExecutor {
                                    lead_to_sender: true,
                                })),
                        ),
                ),
            ),
    );
}
// EMBER end
