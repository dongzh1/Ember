// EMBER - /world command: runtime world management
//
//   /world list                - list loaded worlds with player counts
//   /world load <name>         - load (or create) a world at runtime
//   /world unload <name>       - evacuate players, save and unload a world
//   /world tp <name>           - teleport yourself to a world's spawn
//   /world clone <src> <dst>   - SlimeWorld-style clone: copy a world's
//                                data under a new name and load it
//   /world prewarm <name>      - load a world's stored regions into memory
//   /world convert <name> <fmt> - migrate an UNLOADED world's storage format
//                                (anvil|linear|pump|easy|easy_shard)

use std::path::Path;
use std::sync::Arc;

use pumpkin_config::chunk::ChunkConfig;
use pumpkin_data::dimension::Dimension;
use pumpkin_util::PermissionLvl;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::permission::{Permission, PermissionDefault, PermissionRegistry};
use pumpkin_util::text::{TextComponent, color::NamedColor};

use crate::command::argument_builder::{ArgumentBuilder, argument, command, literal};
use crate::command::argument_types::core::string::StringArgumentType;
use crate::command::context::command_context::CommandContext;
use crate::command::node::dispatcher::CommandDispatcher;
use crate::command::node::{CommandExecutor, CommandExecutorResult};
use crate::server::Server;
use crate::world::World;

const DESCRIPTION: &str = "Manage worlds at runtime: list, load, unload, teleport, clone.";
const PERMISSION: &str = "ember:command.world";
const ARG_NAME: &str = "name";
const ARG_SRC: &str = "source";
const ARG_DST: &str = "destination";

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

async fn feedback(context: &CommandContext<'_>, msg: TextComponent) {
    context.source.send_feedback(msg, false).await;
}

fn err_text(msg: impl Into<String>) -> TextComponent {
    TextComponent::text(msg.into()).color_named(NamedColor::Red)
}

struct WorldListExecutor;

impl CommandExecutor for WorldListExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let worlds = context.server().worlds.load();
            let mut lines = vec![format!("Loaded worlds ({}):", worlds.len())];
            for w in worlds.iter() {
                lines.push(format!(
                    "  {} [{}] - {} player(s)",
                    w.get_world_name(),
                    w.dimension.minecraft_name,
                    w.players.load().len(),
                ));
            }
            feedback(context, TextComponent::text(lines.join("\n"))).await;
            #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
            Ok(worlds.len() as i32)
        })
    }
}

struct WorldLoadExecutor;

impl CommandExecutor for WorldLoadExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let server = context.server().clone();

            if find_world(&server, &name).is_some() {
                feedback(
                    context,
                    err_text(format!("World '{name}' is already loaded.")),
                )
                .await;
                return Ok(0);
            }
            if server.is_world_unloading(&name) {
                feedback(
                    context,
                    err_text(format!("World '{name}' is still unloading, retry shortly.")),
                )
                .await;
                return Ok(0);
            }

            let world = server
                .create_world(name.clone(), Dimension::OVERWORLD)
                .await;
            feedback(
                context,
                TextComponent::text(format!(
                    "World '{}' loaded ({}).",
                    world.get_world_name(),
                    world.dimension.minecraft_name,
                ))
                .color_named(NamedColor::Green),
            )
            .await;
            Ok(1)
        })
    }
}

struct WorldUnloadExecutor;

impl CommandExecutor for WorldUnloadExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let server = context.server().clone();

            let Some(world) = find_world(&server, &name) else {
                feedback(context, err_text(format!("World '{name}' is not loaded."))).await;
                return Ok(0);
            };
            let Some(fallback) = server.worlds.load().first().cloned() else {
                feedback(context, err_text("No fallback world available.")).await;
                return Ok(0);
            };

            match server.unload_world(&world, &fallback).await {
                Ok(()) => {
                    feedback(
                        context,
                        TextComponent::text(format!("World '{name}' saved and unloaded."))
                            .color_named(NamedColor::Green),
                    )
                    .await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, err_text(format!("Cannot unload '{name}': {e}"))).await;
                    Ok(0)
                }
            }
        })
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

struct WorldCloneExecutor;

impl CommandExecutor for WorldCloneExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let src = StringArgumentType::get(context, ARG_SRC)?.to_string();
            let dst = StringArgumentType::get(context, ARG_DST)?.to_string();
            let server = context.server().clone();

            let Some(src_world) = find_world(&server, &src) else {
                feedback(context, err_text(format!("World '{src}' is not loaded."))).await;
                return Ok(0);
            };
            if find_world(&server, &dst).is_some() {
                feedback(
                    context,
                    err_text(format!("World '{dst}' is already loaded.")),
                )
                .await;
                return Ok(0);
            }
            if server.is_world_unloading(&dst) {
                feedback(
                    context,
                    err_text(format!("World '{dst}' is still unloading, retry shortly.")),
                )
                .await;
                return Ok(0);
            }

            let src_dir = src_world.level.level_folder.root_folder.clone();
            let dst_dir = server.basic_config.get_world_path().join(&dst);
            if dst_dir.exists() {
                feedback(
                    context,
                    err_text(format!("Folder '{}' already exists.", dst_dir.display())),
                )
                .await;
                return Ok(0);
            }

            // Copy any on-disk data (region files, level.dat, entities).
            if src_dir.exists() {
                let (src_copy, dst_copy) = (src_dir.clone(), dst_dir.clone());
                let copied =
                    tokio::task::spawn_blocking(move || copy_dir_recursive(&src_copy, &dst_copy))
                        .await;
                match copied {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        feedback(context, err_text(format!("File copy failed: {e}"))).await;
                        return Ok(0);
                    }
                    Err(e) => {
                        feedback(context, err_text(format!("File copy panicked: {e}"))).await;
                        return Ok(0);
                    }
                }
            }

            // easy_mysql keeps region data in the database — clone the rows
            // to the new key as well (in-database, no data transfer).
            // Resolve the SOURCE world's effective config (its sidecar may
            // select a different backend than the global one).
            let src_level_config = pumpkin_config::ember_world::resolve_level_config(
                &server.advanced_config.world,
                &src_dir,
            );
            if let ChunkConfig::EasyMysql(cfg) = &src_level_config.chunk {
                match pumpkin_world::chunk::easy_mysql::clone_world_data(cfg, &src_dir, &dst_dir)
                    .await
                {
                    Ok(regions) => {
                        feedback(
                            context,
                            TextComponent::text(format!("Cloned {regions} database regions.")),
                        )
                        .await;
                    }
                    Err(e) => {
                        feedback(context, err_text(format!("Database clone failed: {e}"))).await;
                        return Ok(0);
                    }
                }
            }

            let world = server
                .create_world(dst.clone(), src_world.dimension.clone())
                .await;
            feedback(
                context,
                TextComponent::text(format!(
                    "World '{src}' cloned to '{}' and loaded.",
                    world.get_world_name(),
                ))
                .color_named(NamedColor::Green),
            )
            .await;
            Ok(1)
        })
    }
}

struct WorldPrewarmExecutor;

impl CommandExecutor for WorldPrewarmExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let Some(world) = find_world(context.server(), &name) else {
                feedback(context, err_text(format!("World '{name}' is not loaded."))).await;
                return Ok(0);
            };

            // Manual prewarm is explicit operator intent: allow up to the
            // hard safety cap regardless of the sidecar's automatic policy.
            let cap = pumpkin_config::ember_world::MAX_RESIDENT_REGIONS;
            let level = world.level.clone();
            tokio::spawn(async move {
                level.prewarm_storage(cap).await;
            });
            feedback(
                context,
                TextComponent::text(format!(
                    "Prewarming world '{name}' in the background (up to {cap} regions)."
                ))
                .color_named(NamedColor::Green),
            )
            .await;
            Ok(1)
        })
    }
}

const ARG_FORMAT: &str = "format";

/// Converts every dimension tree of a world folder; returns
/// `(regions, chunks, entity chunks, skipped)` or the first error.
async fn convert_dimension_trees(
    resolved: &pumpkin_config::world::LevelConfig,
    dims: Vec<pumpkin_world::level::LevelFolder>,
    target: &ChunkConfig,
) -> Result<(usize, usize, usize, usize), String> {
    let (mut regions, mut chunks, mut entities, mut skipped) = (0usize, 0usize, 0usize, 0usize);
    for folder in dims {
        // Per-dimension source: the on-disk format that is NOT the target
        // (robust against reruns after a partial conversion). A DB-backed
        // source (easy_mysql) leaves no region files, so fall back to the
        // world's resolved config for it.
        let from = pumpkin_world::chunk::convert::detect_source_for_conversion(
            &folder.region_folder,
            target,
        )
        .or_else(|| {
            matches!(resolved.chunk, ChunkConfig::EasyMysql(_)).then(|| resolved.chunk.clone())
        });
        let Some(from) = from else {
            continue; // nothing stored (or already converted)
        };
        let stats = pumpkin_world::chunk::convert::convert_world(&folder, &from, target)
            .await
            .map_err(|e| format!("in {}: {e}", folder.dim_folder.display()))?;
        regions += stats.regions;
        chunks += stats.chunks;
        entities += stats.entity_chunks;
        skipped += stats.skipped;
    }
    Ok((regions, chunks, entities, skipped))
}

struct WorldConvertExecutor;

impl CommandExecutor for WorldConvertExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let format = StringArgumentType::get(context, ARG_FORMAT)?.to_string();
            let server = context.server().clone();

            let Some(target) = pumpkin_world::chunk::convert::config_for_name(&format) else {
                feedback(
                    context,
                    err_text(format!(
                        "Unknown format '{format}'. Valid: anvil, linear, pump, easy, easy_shard."
                    )),
                )
                .await;
                return Ok(0);
            };
            if find_world(&server, &name).is_some() {
                feedback(
                    context,
                    err_text(format!(
                        "World '{name}' is loaded — unload it first (/world unload {name})."
                    )),
                )
                .await;
                return Ok(0);
            }
            if server.is_world_unloading(&name) {
                feedback(
                    context,
                    err_text(format!("World '{name}' is still unloading, retry shortly.")),
                )
                .await;
                return Ok(0);
            }
            let root = server.basic_config.get_world_path().join(&name);
            if !root.is_dir() {
                feedback(
                    context,
                    err_text(format!("World folder '{}' does not exist.", root.display())),
                )
                .await;
                return Ok(0);
            }

            let dims = pumpkin_world::chunk::convert::discover_dimension_folders(&root);
            if dims.is_empty() {
                feedback(
                    context,
                    err_text("No dimension data found in that world folder."),
                )
                .await;
                return Ok(0);
            }

            feedback(
                context,
                TextComponent::text(format!(
                    "Converting '{name}' to '{format}' ({} dimension tree(s))...",
                    dims.len()
                )),
            )
            .await;

            let global = &server.advanced_config.world;
            let resolved = pumpkin_config::ember_world::resolve_level_config(global, &root);
            let (regions, chunks, entities, skipped) =
                match convert_dimension_trees(&resolved, dims, &target).await {
                    Ok(stats) => stats,
                    Err(e) => {
                        feedback(context, err_text(format!("Conversion failed {e}"))).await;
                        return Ok(0);
                    }
                };

            // Make the migrated format explicit on disk so later default
            // changes can never flip this world again.
            if let Err(e) = pumpkin_config::ember_world::write_sidecar_chunk(&root, target.clone())
            {
                feedback(
                    context,
                    err_text(format!(
                        "Converted, but writing ember-world.toml failed: {e}"
                    )),
                )
                .await;
                return Ok(0);
            }

            feedback(
                context,
                TextComponent::text(format!(
                    "World '{name}' converted to '{format}': {regions} region(s), \
                     {chunks} chunk(s), {entities} entity chunk(s), {skipped} skipped. \
                     Old files renamed to *.bak; format pinned in ember-world.toml."
                ))
                .color_named(NamedColor::Green),
            )
            .await;
            Ok(1)
        })
    }
}

struct WorldTpExecutor;

impl CommandExecutor for WorldTpExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();

            let Some(player) = context.source.output.as_player() else {
                feedback(context, err_text("Only players can use /world tp.")).await;
                return Ok(0);
            };
            let Some(world) = find_world(context.server(), &name) else {
                feedback(context, err_text(format!("World '{name}' is not loaded."))).await;
                return Ok(0);
            };

            let spawn = spawn_of(&world);
            player.teleport_world(world, spawn, None, None).await;
            feedback(
                context,
                TextComponent::text(format!("Teleported to world '{name}'."))
                    .color_named(NamedColor::Green),
            )
            .await;
            Ok(1)
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
        command("world", DESCRIPTION)
            .requires(PERMISSION)
            .then(literal("list").executes(WorldListExecutor))
            .then(literal("load").then(
                argument(ARG_NAME, StringArgumentType::SingleWord).executes(WorldLoadExecutor),
            ))
            .then(literal("unload").then(
                argument(ARG_NAME, StringArgumentType::SingleWord).executes(WorldUnloadExecutor),
            ))
            .then(
                literal("tp").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord).executes(WorldTpExecutor),
                ),
            )
            .then(literal("prewarm").then(
                argument(ARG_NAME, StringArgumentType::SingleWord).executes(WorldPrewarmExecutor),
            ))
            .then(
                literal("convert").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord).then(
                        argument(ARG_FORMAT, StringArgumentType::SingleWord)
                            .executes(WorldConvertExecutor),
                    ),
                ),
            )
            .then(
                literal("clone").then(argument(ARG_SRC, StringArgumentType::SingleWord).then(
                    argument(ARG_DST, StringArgumentType::SingleWord).executes(WorldCloneExecutor),
                )),
            ),
    );
}
