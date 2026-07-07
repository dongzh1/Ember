// EMBER - /dungeon command: shared-template ephemeral instance worlds
//
//   /dungeon prewarm <template>   - load the template into shared memory
//   /dungeon start <template>     - open a new instance world and tp there
//   /dungeon stop <instance>      - unload an instance (changes discarded)
//   /dungeon list                 - resident templates + running instances
//   /dungeon reload <template>    - drop the template cache (next start reloads)
//
// A template is a world folder in the `easy` format (or a world stored via
// `easy_mysql`). Instances share ONE decompressed in-memory copy of the
// template; per-instance edits live in RAM and are discarded on stop.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

use pumpkin_config::chunk::{ChunkConfig, EasyInstanceConfig, InstanceTemplateSource};
use pumpkin_config::ember_world::resolve_level_config;
use pumpkin_config::world::LevelConfig;
use pumpkin_data::dimension::Dimension;
use pumpkin_util::PermissionLvl;
use pumpkin_util::permission::{Permission, PermissionDefault, PermissionRegistry};
use pumpkin_util::text::{TextComponent, color::NamedColor};
use pumpkin_world::chunk::easy_instance;

use crate::command::argument_builder::{ArgumentBuilder, argument, command, literal};
use crate::command::argument_types::core::string::StringArgumentType;
use crate::command::context::command_context::CommandContext;
use crate::command::node::dispatcher::CommandDispatcher;
use crate::command::node::{CommandExecutor, CommandExecutorResult};
use crate::server::Server;

const DESCRIPTION: &str =
    "Run ephemeral dungeon instances from a shared world template (changes are discarded).";
const PERMISSION: &str = "ember:command.dungeon";
const ARG_NAME: &str = "name";

/// Names of instance worlds started by this command (only these may be
/// stopped through `/dungeon stop`).
static INSTANCES: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
/// Monotonic instance counter for unique world names.
static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);

async fn feedback(context: &CommandContext<'_>, msg: TextComponent) {
    context.source.send_feedback(msg, false).await;
}

fn err_text(msg: impl Into<String>) -> TextComponent {
    TextComponent::text(msg.into()).color_named(NamedColor::Red)
}

fn ok_text(msg: impl Into<String>) -> TextComponent {
    TextComponent::text(msg.into()).color_named(NamedColor::Green)
}

/// Resolves where a template world's region data lives, honoring the
/// template folder's own sidecar (an `easy_mysql` template stays in the
/// database; everything else reads `.easy` files from the folder).
fn template_source(server: &Server, template: &str) -> (InstanceTemplateSource, String) {
    let path = server.basic_config.get_world_path().join(template);
    let resolved = resolve_level_config(&server.advanced_config.world, &path);
    let path_str = path.to_string_lossy().replace('\\', "/");
    let source = match resolved.chunk {
        ChunkConfig::EasyMysql(config) => InstanceTemplateSource::Mysql {
            path: path_str.clone(),
            config,
        },
        _ => InstanceTemplateSource::File {
            path: path_str.clone(),
        },
    };
    (source, path_str)
}

fn dim_path() -> String {
    easy_instance::dimension_path(Dimension::OVERWORLD.minecraft_name)
}

struct DungeonPrewarmExecutor;

impl CommandExecutor for DungeonPrewarmExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let template = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let (source, _) = template_source(context.server(), &template);

            match easy_instance::prewarm_template(&template, &source, &dim_path()).await {
                Ok((regions, chunks)) => {
                    feedback(
                        context,
                        ok_text(format!(
                            "Template '{template}' resident: {regions} region(s), {chunks} chunk(s)."
                        )),
                    )
                    .await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(
                        context,
                        err_text(format!("Cannot load template '{template}': {e}")),
                    )
                    .await;
                    Ok(0)
                }
            }
        })
    }
}

struct DungeonStartExecutor;

impl CommandExecutor for DungeonStartExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let template = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let server = context.server().clone();

            // Warm (or verify) the template before creating the world so a
            // broken template fails here with a clear message instead of
            // filling the world with void.
            let (source, _) = template_source(&server, &template);
            if let Err(e) = easy_instance::prewarm_template(&template, &source, &dim_path()).await {
                feedback(
                    context,
                    err_text(format!("Cannot load template '{template}': {e}")),
                )
                .await;
                return Ok(0);
            }

            // Unique instance name.
            let name = loop {
                let n = NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed);
                let candidate = format!("{template}-i{n}");
                let taken = server
                    .worlds
                    .load()
                    .iter()
                    .any(|w| w.get_world_name() == candidate)
                    || server.is_world_unloading(&candidate);
                if !taken {
                    break candidate;
                }
            };

            let global = &server.advanced_config.world;
            let level_config = LevelConfig {
                chunk: ChunkConfig::EasyInstance(EasyInstanceConfig {
                    template: template.clone(),
                    source,
                }),
                lighting: global.lighting,
                // Instances never persist: disable the autosave rounds.
                autosave_ticks: 0,
            };

            let world = server
                .create_world_with(name.clone(), Dimension::OVERWORLD, Some(level_config))
                .await;
            if let Ok(mut instances) = INSTANCES.lock() {
                instances.insert(name.clone());
            }

            if let Some(player) = context.source.output.as_player() {
                let spawn = {
                    let info = world.level_info.load();
                    pumpkin_util::math::vector3::Vector3::new(
                        f64::from(info.spawn_x) + 0.5,
                        f64::from(info.spawn_y),
                        f64::from(info.spawn_z) + 0.5,
                    )
                };
                player.teleport_world(world, spawn, None, None).await;
            }

            feedback(
                context,
                ok_text(format!(
                    "Instance '{name}' of template '{template}' is running. \
                     Stop it with /dungeon stop {name} (changes are discarded)."
                )),
            )
            .await;
            Ok(1)
        })
    }
}

struct DungeonStopExecutor;

impl CommandExecutor for DungeonStopExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let server = context.server().clone();

            let known = INSTANCES.lock().is_ok_and(|i| i.contains(&name));
            if !known {
                feedback(
                    context,
                    err_text(format!(
                        "'{name}' is not a running dungeon instance (see /dungeon list)."
                    )),
                )
                .await;
                return Ok(0);
            }

            let Some(world) = server
                .worlds
                .load()
                .iter()
                .find(|w| w.get_world_name() == name)
                .cloned()
            else {
                // Already gone; forget it.
                if let Ok(mut instances) = INSTANCES.lock() {
                    instances.remove(&name);
                }
                feedback(
                    context,
                    err_text(format!("Instance '{name}' is not loaded.")),
                )
                .await;
                return Ok(0);
            };
            let Some(fallback) = server.worlds.load().first().cloned() else {
                feedback(context, err_text("No fallback world available.")).await;
                return Ok(0);
            };

            match server.unload_world(&world, &fallback).await {
                Ok(()) => {
                    if let Ok(mut instances) = INSTANCES.lock() {
                        instances.remove(&name);
                    }
                    // Remove any leftover instance folder once the background
                    // unload finishes (instances persist nothing on disk).
                    let folder = server.basic_config.get_world_path().join(&name);
                    let server_bg = server.clone();
                    let name_bg = name.clone();
                    tokio::spawn(async move {
                        for _ in 0..240u32 {
                            if !server_bg.is_world_unloading(&name_bg) {
                                break;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        }
                        let _ = tokio::fs::remove_dir_all(&folder).await;
                    });
                    feedback(
                        context,
                        ok_text(format!("Instance '{name}' unloaded, changes discarded.")),
                    )
                    .await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, err_text(format!("Cannot stop '{name}': {e}"))).await;
                    Ok(0)
                }
            }
        })
    }
}

struct DungeonListExecutor;

impl CommandExecutor for DungeonListExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let templates = easy_instance::list_templates().await;
            let mut lines = vec![format!("Resident templates ({}):", templates.len())];
            for (id, regions, chunks, handles) in &templates {
                lines.push(format!(
                    "  {id} - {regions} region(s), {chunks} chunk(s), {handles} live handle(s)"
                ));
            }
            let instances: Vec<String> = INSTANCES
                .lock()
                .map(|i| {
                    let mut v: Vec<String> = i.iter().cloned().collect();
                    v.sort();
                    v
                })
                .unwrap_or_default();
            lines.push(format!("Running instances ({}):", instances.len()));
            let worlds = context.server().worlds.load();
            for name in instances {
                let players = worlds
                    .iter()
                    .find(|w| w.get_world_name() == name)
                    .map_or(0, |w| w.players.load().len());
                lines.push(format!("  {name} - {players} player(s)"));
            }
            feedback(context, TextComponent::text(lines.join("\n"))).await;
            Ok(1)
        })
    }
}

struct DungeonReloadExecutor;

impl CommandExecutor for DungeonReloadExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let template = StringArgumentType::get(context, ARG_NAME)?.to_string();
            if easy_instance::reload_template(&template).await {
                feedback(
                    context,
                    ok_text(format!(
                        "Template '{template}' dropped; the next /dungeon start reloads it. \
                         Running instances keep their current copy."
                    )),
                )
                .await;
                Ok(1)
            } else {
                feedback(
                    context,
                    err_text(format!("Template '{template}' is not resident.")),
                )
                .await;
                Ok(0)
            }
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
        command("dungeon", DESCRIPTION)
            .requires(PERMISSION)
            .then(literal("prewarm").then(
                argument(ARG_NAME, StringArgumentType::SingleWord).executes(DungeonPrewarmExecutor),
            ))
            .then(literal("start").then(
                argument(ARG_NAME, StringArgumentType::SingleWord).executes(DungeonStartExecutor),
            ))
            .then(literal("stop").then(
                argument(ARG_NAME, StringArgumentType::SingleWord).executes(DungeonStopExecutor),
            ))
            .then(literal("list").executes(DungeonListExecutor))
            .then(literal("reload").then(
                argument(ARG_NAME, StringArgumentType::SingleWord).executes(DungeonReloadExecutor),
            )),
    );
}
