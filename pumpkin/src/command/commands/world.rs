// EMBER - /world command: runtime world management
//
//   /world list                - list loaded worlds with player counts
//   /world load <name>         - load (or create) a world at runtime; a name
//                                ending in `_nether`/`_end` creates that
//                                dimension instead of an overworld (see
//                                `dimension_for_world_name` and the portal
//                                pairing lookup in `Server::get_paired_world`)
//   /world unload <name>       - evacuate players, save and unload a world
//   /world tp <name>           - teleport yourself to a world's spawn
//   /world clone <src> <dst> [save|readonly] - clone a world: `save` (default)
//                                copies it under a new name; `readonly` loads
//                                an in-memory instance that discards changes
//   /world prewarm <name>      - load a world's stored regions into memory
//   /world convert <name> <fmt> - migrate an UNLOADED world's storage format
//                                (anvil|linear|pump|easy)

use std::sync::Arc;

use pumpkin_config::chunk::ChunkConfig;
use pumpkin_config::ember_world::SMALL_MAP_MAX_BORDER;
use pumpkin_data::dimension::Dimension;
use pumpkin_util::PermissionLvl;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::permission::{Permission, PermissionDefault, PermissionRegistry};
use pumpkin_util::text::{TextComponent, color::NamedColor};

use crate::command::argument_builder::{ArgumentBuilder, argument, command, literal};
use crate::command::argument_types::core::integer::IntegerArgumentType;
use crate::command::argument_types::core::string::StringArgumentType;
use crate::command::context::command_context::CommandContext;
use crate::command::node::dispatcher::CommandDispatcher;
use crate::command::node::{CommandExecutor, CommandExecutorResult};
use crate::command::suggestion::provider::{SuggestionProvider, SuggestionProviderResult};
use crate::command::suggestion::suggestions::SuggestionsBuilder;
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

// EMBER start - dimension-by-suffix world naming convention (nether/end pairing)
/// A world named `<x>_nether`/`<x>_end` is created as that dimension instead
/// of an overworld, so it can pair with a world named `<x>` for portal
/// routing (see `Server::get_paired_world`). Anything else (including the
/// default world) is an overworld, unchanged from before this convention
/// existed.
fn dimension_for_world_name(name: &str) -> Dimension {
    if name.ends_with("_nether") {
        Dimension::THE_NETHER
    } else if name.ends_with("_end") {
        Dimension::THE_END
    } else {
        Dimension::OVERWORLD
    }
}
// EMBER end

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

            // create_world is infallible and builds a path from the name, so an
            // empty/invalid name (e.g. "/world load " with a trailing space)
            // would create a stray dimension tree at the worlds root. Reject it.
            if let Err(e) = crate::server::validate_world_name(&name) {
                feedback(context, err_text(e)).await;
                return Ok(0);
            }
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

            // EMBER: `_nether`/`_end`-suffixed names create that dimension
            // instead of an overworld, see `dimension_for_world_name`.
            let world = server
                .create_world(name.clone(), dimension_for_world_name(&name))
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

/// `/world clone` — save clone (persistent copy under a new name).
struct WorldCloneExecutor {
    readonly: bool,
}

impl CommandExecutor for WorldCloneExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let src = StringArgumentType::get(context, ARG_SRC)?.to_string();
            let dst = StringArgumentType::get(context, ARG_DST)?.to_string();
            let server = context.server().clone();

            // Both clone primitives live on Server so plugins share them.
            let result = if self.readonly {
                server.clone_world_readonly(&src, &dst).await
            } else {
                server.clone_world(&src, &dst).await
            };
            match result {
                Ok(world) => {
                    let kind = if self.readonly {
                        "read-only clone"
                    } else {
                        "clone"
                    };
                    feedback(
                        context,
                        TextComponent::text(format!(
                            "World '{src}' {kind}d to '{}' and loaded.",
                            world.get_world_name(),
                        ))
                        .color_named(NamedColor::Green),
                    )
                    .await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, err_text(format!("Cannot clone '{src}': {e}"))).await;
                    Ok(0)
                }
            }
        })
    }
}

struct WorldDeleteExecutor;

impl CommandExecutor for WorldDeleteExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            match context.server().delete_world(&name).await {
                Ok(()) => {
                    feedback(
                        context,
                        TextComponent::text(format!("World '{name}' deleted."))
                            .color_named(NamedColor::Green),
                    )
                    .await;
                    Ok(1)
                }
                Err(e) => {
                    feedback(context, err_text(format!("Cannot delete '{name}': {e}"))).await;
                    Ok(0)
                }
            }
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
            let cap = pumpkin_config::ember_world::MAX_PREWARM_REGIONS;
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
const ARG_BORDER: &str = "border";

/// Converts every dimension tree of a world folder; returns
/// `(regions, chunks, entity chunks, skipped, regions_cropped)` or the
/// first error.
async fn convert_dimension_trees(
    resolved: &pumpkin_config::world::LevelConfig,
    dims: Vec<pumpkin_world::level::LevelFolder>,
    target: &ChunkConfig,
    border: Option<i32>,
) -> Result<(usize, usize, usize, usize, usize), String> {
    let (mut regions, mut chunks, mut entities, mut skipped, mut cropped) =
        (0usize, 0usize, 0usize, 0usize, 0usize);
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
            matches!(&resolved.chunk,
                ChunkConfig::Easy(c) if c.backend == pumpkin_config::chunk::EasyBackend::Mysql)
            .then(|| resolved.chunk.clone())
        });
        let Some(from) = from else {
            continue; // nothing stored (or already converted)
        };
        let stats = pumpkin_world::chunk::convert::convert_world(&folder, &from, target, border)
            .await
            .map_err(|e| format!("in {}: {e}", folder.dim_folder.display()))?;
        regions += stats.regions;
        chunks += stats.chunks;
        entities += stats.entity_chunks;
        skipped += stats.skipped;
        cropped += stats.regions_cropped;
    }
    Ok((regions, chunks, entities, skipped, cropped))
}

/// `border > 0` crops the conversion to a `border`-side-length square
/// centered on the origin (0, 0) — same units/center as `ember-world.toml`'s
/// `border` — and pins that same value into the resulting sidecar.
struct WorldConvertExecutor {
    cropped: bool,
}

/// Feedback suffix describing a crop, or empty when the conversion wasn't cropped.
fn crop_note(border: Option<i32>, cropped_regions: usize) -> String {
    border.map_or_else(String::new, |b| {
        format!(
            " Cropped to a {b}-block border ({cropped_regions} region(s) outside it skipped); \
             border pinned in ember-world.toml."
        )
    })
}

impl CommandExecutor for WorldConvertExecutor {
    fn execute<'a>(&'a self, context: &'a CommandContext) -> CommandExecutorResult<'a> {
        Box::pin(async move {
            let name = StringArgumentType::get(context, ARG_NAME)?.to_string();
            let format = StringArgumentType::get(context, ARG_FORMAT)?.to_string();
            let border = self
                .cropped
                .then(|| IntegerArgumentType::get(context, ARG_BORDER))
                .transpose()?;
            let server = context.server().clone();

            let Some(target) = pumpkin_world::chunk::convert::config_for_name(&format) else {
                feedback(
                    context,
                    err_text(format!(
                        "Unknown format '{format}'. Valid: anvil, linear, pump, easy."
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
            let (regions, chunks, entities, skipped, cropped) =
                match convert_dimension_trees(&resolved, dims, &target, border).await {
                    Ok(stats) => stats,
                    Err(e) => {
                        feedback(context, err_text(format!("Conversion failed {e}"))).await;
                        return Ok(0);
                    }
                };

            // Make the migrated format explicit on disk so later default
            // changes can never flip this world again; a crop border is
            // pinned the same way so the world stays bounded (and gets the
            // small-map prewarm treatment) after this conversion.
            let sidecar = pumpkin_config::ember_world::EmberWorldConfig {
                chunk: Some(target.clone()),
                border,
                ..Default::default()
            };
            if let Err(e) = pumpkin_config::ember_world::write_sidecar(&root, &sidecar) {
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
                     Old files renamed to *.bak; format pinned in ember-world.toml.{}",
                    crop_note(border, cropped)
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

// EMBER start - tab-completion for world names, formats and border sizes
/// Names of currently loaded worlds — the completion set for subcommands
/// that require a world already in memory (`unload`, `tp`, `prewarm`).
fn loaded_world_names(server: &Server) -> Vec<String> {
    server
        .worlds
        .load()
        .iter()
        .map(|w| w.get_world_name().to_string())
        .collect()
}

/// Names of on-disk worlds that are NOT currently loaded — the completion
/// set for subcommands that require the world to be absent from memory
/// (`load`, `delete`, `convert`).
fn unloaded_world_names(server: &Server) -> Vec<String> {
    let loaded = loaded_world_names(server);
    server
        .list_world_folders()
        .into_iter()
        .filter(|name| !loaded.contains(name))
        .collect()
}

/// Every world name the server knows about, loaded or not. Used for
/// `clone`'s `<source>`: a `readonly` clone can read an unloaded world
/// straight off disk, so both categories are valid completions there.
fn any_world_names(server: &Server) -> Vec<String> {
    let mut names = loaded_world_names(server);
    for name in server.list_world_folders() {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

struct LoadedWorldSuggestionProvider;

impl SuggestionProvider for LoadedWorldSuggestionProvider {
    fn suggest<'a>(
        &'a self,
        context: &'a CommandContext,
        builder: SuggestionsBuilder,
    ) -> SuggestionProviderResult<'a> {
        Box::pin(async move {
            builder
                .filter_and_suggest_iter(loaded_world_names(context.server()))
                .build()
        })
    }
}

struct UnloadedWorldSuggestionProvider;

impl SuggestionProvider for UnloadedWorldSuggestionProvider {
    fn suggest<'a>(
        &'a self,
        context: &'a CommandContext,
        builder: SuggestionsBuilder,
    ) -> SuggestionProviderResult<'a> {
        Box::pin(async move {
            builder
                .filter_and_suggest_iter(unloaded_world_names(context.server()))
                .build()
        })
    }
}

struct AnyWorldSuggestionProvider;

impl SuggestionProvider for AnyWorldSuggestionProvider {
    fn suggest<'a>(
        &'a self,
        context: &'a CommandContext,
        builder: SuggestionsBuilder,
    ) -> SuggestionProviderResult<'a> {
        Box::pin(async move {
            builder
                .filter_and_suggest_iter(any_world_names(context.server()))
                .build()
        })
    }
}

struct WorldFormatSuggestionProvider;

impl SuggestionProvider for WorldFormatSuggestionProvider {
    fn suggest<'a>(
        &'a self,
        _context: &'a CommandContext,
        builder: SuggestionsBuilder,
    ) -> SuggestionProviderResult<'a> {
        Box::pin(async move {
            builder
                .filter_and_suggest_iter(["anvil", "linear", "pump", "easy"])
                .build()
        })
    }
}

/// Suggests only the small-map residency ceiling as a sensible default
/// border; operators are free to type any other value instead.
struct BorderSuggestionProvider;

impl SuggestionProvider for BorderSuggestionProvider {
    fn suggest<'a>(
        &'a self,
        _context: &'a CommandContext,
        builder: SuggestionsBuilder,
    ) -> SuggestionProviderResult<'a> {
        Box::pin(async move { builder.suggest(SMALL_MAP_MAX_BORDER).build() })
    }
}
// EMBER end

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
            .then(
                literal("load").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord)
                        .suggests(UnloadedWorldSuggestionProvider)
                        .executes(WorldLoadExecutor),
                ),
            )
            .then(
                literal("unload").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord)
                        .suggests(LoadedWorldSuggestionProvider)
                        .executes(WorldUnloadExecutor),
                ),
            )
            .then(
                literal("tp").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord)
                        .suggests(LoadedWorldSuggestionProvider)
                        .executes(WorldTpExecutor),
                ),
            )
            .then(
                literal("prewarm").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord)
                        .suggests(LoadedWorldSuggestionProvider)
                        .executes(WorldPrewarmExecutor),
                ),
            )
            .then(
                literal("delete").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord)
                        .suggests(UnloadedWorldSuggestionProvider)
                        .executes(WorldDeleteExecutor),
                ),
            )
            .then(
                literal("convert").then(
                    argument(ARG_NAME, StringArgumentType::SingleWord)
                        .suggests(UnloadedWorldSuggestionProvider)
                        .then(
                            argument(ARG_FORMAT, StringArgumentType::SingleWord)
                                .suggests(WorldFormatSuggestionProvider)
                                // `/world convert <name> <format>` — convert everything stored.
                                .executes(WorldConvertExecutor { cropped: false })
                                // `/world convert <name> <format> <border>` — also crop to a
                                // border-side-length square centered on the origin.
                                .then(
                                    argument(ARG_BORDER, IntegerArgumentType::with_min(1))
                                        .suggests(BorderSuggestionProvider)
                                        .executes(WorldConvertExecutor { cropped: true }),
                                ),
                        ),
                ),
            )
            .then(
                literal("clone").then(
                    argument(ARG_SRC, StringArgumentType::SingleWord)
                        .suggests(AnyWorldSuggestionProvider)
                        .then(
                            argument(ARG_DST, StringArgumentType::SingleWord)
                                // `/world clone <src> <dst>` — persistent save clone.
                                .executes(WorldCloneExecutor { readonly: false })
                                // `/world clone <src> <dst> readonly` — read-only.
                                .then(
                                    literal("readonly")
                                        .executes(WorldCloneExecutor { readonly: true }),
                                )
                                .then(
                                    literal("save")
                                        .executes(WorldCloneExecutor { readonly: false }),
                                ),
                        ),
                ),
            ),
    );
}
