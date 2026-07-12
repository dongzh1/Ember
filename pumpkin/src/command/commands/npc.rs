// EMBER - packet-only NPC commands
//
//   /npc create <name>              - spawn an NPC at your position, using your own skin
//   /npc create <name> <player>     - ...using an online player's skin instead
//   /npc create <name> as <type>          - spawn as any entity type (default look)
//   /npc create <name> as <type> <extra>  - ...with a per-type extra: a player name
//                                            (player/mannequin skin), a block name
//                                            (falling_block), or an item name (item)
//   /npc remove <name>              - delete an NPC
//   /npc list                       - list all NPCs
//   /npc move <name>                - move an existing NPC to your position
//   /npc skin <name> <player>       - re-copy an NPC's skin from an online player
//   /npc setaction <name> <command> - run a console command on click (%player% placeholder)
//   /npc clearaction <name>         - make the NPC purely decorative again
//   /npc lookat <name> on|off       - continuously face the nearest visible player
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
use crate::server::npc::supports_skin;

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
            let mut lines = vec![format!("NPCs ({}):", npcs.len())];
            for npc in &npcs {
                lines.push(format!(
                    "  {} @ {} ({:.1}, {:.1}, {:.1})",
                    npc.name, npc.world, npc.x, npc.y, npc.z
                ));
            }
            feedback(context, TextComponent::text(lines.join("\n"))).await;
            #[expect(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            Ok(npcs.len() as i32)
        })
    }
}

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
