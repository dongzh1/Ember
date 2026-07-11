use crate::block::registry::BlockRegistry;
use crate::command::commands::default_dispatcher;
use crate::command::commands::defaultgamemode::DefaultGamemode;
use crate::data::VanillaData;
use crate::data::player_server::ServerPlayerData;
use crate::entity::{EntityBase, NBTStorage};
use crate::item::registry::ItemRegistry;
use crate::net::authentication::fetch_mojang_public_keys;
use crate::net::{ClientPlatform, DisconnectReason, EncryptionError, GameProfile, PlayerConfig};
use crate::plugin::PluginManager;
use crate::plugin::player::player_login::PlayerLoginEvent;
use crate::plugin::server::server_broadcast::ServerBroadcastEvent;
use crate::server::tick_rate_manager::ServerTickRateManager;
use crate::world::WorldPortal;
use crate::world::custom_bossbar::CustomBossbars;
use crate::{
    command::node::dispatcher::CommandDispatcher, entity::player::Player, world::World,
    world::map::MapManager,
};
use arc_swap::ArcSwap;
use connection_cache::{CachedBranding, CachedStatus};
use key_store::KeyStore;
use pumpkin_config::{AdvancedConfiguration, BasicConfiguration};
use pumpkin_data::dimension::Dimension;
use pumpkin_data::entity::EntityType;
use pumpkin_util::permission::{PermissionManager, PermissionRegistry};
use pumpkin_util::text::color::NamedColor;
use pumpkin_world::dimension::into_level;
use pumpkin_world::world::WorldPortalExt;
use tracing::{debug, error, info, warn};

use crate::command::CommandSender;
use pumpkin_macros::send_cancellable;
use pumpkin_protocol::java::client::login::CEncryptionRequest;
use pumpkin_protocol::java::client::play::{CChangeDifficulty, CTabList};
use pumpkin_protocol::{ClientPacket, java::client::config::CPluginMessage};
use pumpkin_util::Difficulty;
use pumpkin_util::text::TextComponent;
use pumpkin_world::world_info::anvil::{
    AnvilLevelInfo, LEVEL_DAT_BACKUP_FILE_NAME, LEVEL_DAT_FILE_NAME,
};
use pumpkin_world::world_info::{LevelData, WorldInfoError, WorldInfoReader, WorldInfoWriter};
use rand::seq::{IndexedRandom, SliceRandom};
use rsa::RsaPublicKey;
use std::collections::HashSet;
use std::fs;
use std::net::IpAddr;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32};
use std::{future::Future, sync::atomic::Ordering, time::Duration};
// EMBER: tick soft-budget isolation (`tick_worlds`)
use futures::FutureExt;
use tokio::sync::{Mutex, OnceCell, RwLock};
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::task::TaskTracker;

mod connection_cache;
// EMBER start - built-in economy system
pub mod economy;
// EMBER end
mod key_store;
// EMBER start - packet-only NPC manager
pub mod npc;
// EMBER end
pub mod recipe;
pub mod scheduler;
pub mod seasonal_events;
pub mod tick_rate_manager;
pub mod ticker;

pub use recipe::RecipeManager;
// EMBER start - built-in economy system
pub use economy::EconomyManager;
// EMBER end
// EMBER start - packet-only NPC manager
pub use npc::NpcManager;
// EMBER end

use crate::command::args::entities::{
    EntityFilter, EntityFilterSort, EntitySelectorType, TargetSelector, ValueCondition,
};
use crate::data::advancement_data::AdvancementManager;
use crate::server::scheduler::TaskScheduler;

/// Represents a Minecraft server instance.
pub struct Server {
    pub basic_config: BasicConfiguration,
    pub advanced_config: AdvancedConfiguration,

    pub data: VanillaData,

    /// Plugin manager
    pub plugin_manager: Arc<PluginManager>,

    /// Permission manager for the server.
    pub permission_manager: Arc<RwLock<PermissionManager>>,
    /// Permission registry for the server.
    pub permission_registry: Arc<RwLock<PermissionRegistry>>,

    /// Handles cryptographic keys for secure communication.
    key_store: OnceCell<Arc<KeyStore>>,
    /// Bedrock OIDC provider keys, fetched on startup for 1.26.10+ token validation.
    pub bedrock_oidc_keys: OnceCell<(String, pumpkin_util::jwt::Jwks)>,
    /// Cached Bedrock server private key (process-lifetime). Generated on first Bedrock login and reused.
    pub bedrock_private_key: OnceCell<Arc<pumpkin_util::p384::ecdsa::SigningKey>>,
    /// Manages server status information.
    listing: Mutex<CachedStatus>,
    /// Saves server branding information.
    branding: CachedBranding,
    /// Saves and dispatches commands to appropriate handlers.
    pub command_dispatcher: RwLock<CommandDispatcher>,
    /// Block behaviour.
    pub block_registry: Arc<BlockRegistry>,
    /// Item behaviour.
    pub item_registry: Arc<ItemRegistry>,
    /// Manages multiple worlds within the server.
    pub worlds: ArcSwap<Vec<Arc<World>>>,
    // EMBER start - dynamic world unload
    /// Names of worlds whose background save+stop is still running after
    /// removal from the tick loop; reloading such a name must wait, or two
    /// `Level`s would write the same data concurrently.
    pub pending_world_unloads: std::sync::Mutex<std::collections::HashSet<String>>,
    // EMBER end
    // EMBER start - shared chunk-generation thread pool
    /// Rayon pool used for chunk generation, shared by every world (the
    /// startup dimensions and any created later via `create_world_with`) so
    /// dynamically created worlds don't each spin up their own pool/threads.
    pub gen_pool: Arc<rayon::ThreadPool>,
    /// Cross-world admission control for `gen_pool`: each world computes its
    /// own local in-flight cap independently, with no awareness of other
    /// worlds sharing the same pool. See `GenPoolBudget` doc comment.
    pub gen_budget: Arc<pumpkin_world::chunk_system::GenPoolBudget>,
    // EMBER end
    // EMBER start - built-in economy system
    /// Multi-currency, `MySQL`-backed economy system. Off (all operations
    /// return `EconomyError::Disabled`) unless `[economy] enabled = true`.
    pub economy_manager: Arc<economy::EconomyManager>,
    // EMBER end
    // EMBER start - packet-only NPC manager
    /// Packet-only NPCs (`data/npcs.json`): never real world entities, spawned
    /// per-viewer purely via packets. See `npc::NpcManager` doc comment.
    pub npc_manager: Arc<npc::NpcManager>,
    // EMBER end
    /// All the dimensions that exist on the server.
    pub dimensions: Vec<Dimension>,
    /// Assigns unique IDs to containers.
    container_id: AtomicU32,
    pub recipe_manager: Arc<recipe::RecipeManager>,
    /// Assigns unique IDs to maps.
    map_id: AtomicI32,
    /// Mojang's public keys, used for chat session signing
    /// Pulled from Mojang API on startup
    pub mojang_public_keys: ArcSwap<Vec<RsaPublicKey>>,
    /// The server's custom bossbars
    pub bossbars: Mutex<CustomBossbars>,
    /// Manages all maps on the server
    pub map_manager: MapManager,
    /// The default gamemode when a player joins the server (reset every restart)
    pub defaultgamemode: Mutex<DefaultGamemode>,
    /// Manages player data storage
    pub player_data_storage: ServerPlayerData,
    // Manages player advancement
    pub advancement_manager: Arc<AdvancementManager>,
    // Whether the server whitelist is on or off
    pub white_list: AtomicBool,
    /// Manages the server's tick rate, freezing, and sprinting
    pub tick_rate_manager: Arc<ServerTickRateManager>,
    /// Stores the duration of the last 100 ticks for performance analysis
    pub tick_times_nanos: Mutex<[i64; 100]>,
    /// Aggregated tick times for efficient rolling average calculation
    pub aggregated_tick_times_nanos: AtomicI64,
    /// Total number of ticks processed by the server
    pub tick_count: AtomicI32,
    /// Random unique Server ID used by Bedrock Edition
    pub server_guid: u64,
    /// Player idle timeout in minutes (0 = disabled)
    pub player_idle_timeout: AtomicI32,
    /// Manages scheduled tasks (e.g. from plugins)
    pub task_scheduler: Arc<TaskScheduler>,
    tasks: TaskTracker,

    // world stuff which maybe should be put into a struct
    pub level_info: Arc<ArcSwap<LevelData>>,
    world_info_writer: Arc<dyn WorldInfoWriter>,
}

impl Server {
    #[expect(clippy::too_many_lines)]
    #[must_use]
    pub async fn new(
        basic_config: BasicConfiguration,
        advanced_config: AdvancedConfiguration,
        vanilla_data: VanillaData,
    ) -> Arc<Self> {
        let permission_registry = Arc::new(RwLock::new(PermissionRegistry::new()));
        // First register the default commands. After that, plugins can put in their own.
        let command_dispatcher =
            RwLock::new(default_dispatcher(&permission_registry, &basic_config).await);

        crate::command::set_broadcast_console_to_ops(
            advanced_config.commands.broadcast_console_to_ops,
        );

        let world_path = basic_config.get_world_path();

        let block_registry = super::block::registry::default_registry();

        let level_info = AnvilLevelInfo.read_world_info(&world_path);
        if let Err(error) = &level_info {
            match error {
                // If it doesn't exist, just make a new one
                WorldInfoError::InfoNotFound => (),
                WorldInfoError::UnsupportedDataVersion(_version)
                | WorldInfoError::UnsupportedLevelVersion(_version) => {
                    error!("Failed to load world info!");
                    error!("{error}");
                    panic!("Unsupported world version! See the logs for more info.");
                }
                e => {
                    panic!("World Error {e}");
                }
            }
        } else {
            let dat_path = world_path.join(LEVEL_DAT_FILE_NAME);
            if dat_path.exists() {
                let backup_path = world_path.join(LEVEL_DAT_BACKUP_FILE_NAME);
                fs::copy(dat_path, backup_path).unwrap();
            }
        }
        let level_info = level_info.unwrap_or_else(|err| {
            warn!("Failed to get level_info, using default instead: {err}");
            let default_data = LevelData::default(basic_config.seed);
            if let Err(err) = AnvilLevelInfo.write_world_info(&default_data, &world_path) {
                error!("Failed to save level.dat: {err}");
            }
            default_data
        });

        let seed = level_info.world_gen_settings.seed;
        let level_info = Arc::new(ArcSwap::new(Arc::new(level_info)));

        let listing = Mutex::new(CachedStatus::new(&basic_config));
        let defaultgamemode = Mutex::new(DefaultGamemode {
            gamemode: basic_config.default_gamemode,
        });
        let players_dir = world_path.join("players");
        let player_data_storage = ServerPlayerData::new(
            players_dir.join("data"),
            Duration::from_secs(advanced_config.player_data.save_player_cron_interval),
            advanced_config.player_data.save_player_data,
        );
        let advancement_manager = Arc::new(AdvancementManager::new(
            players_dir.clone(),
            advanced_config.advancement.save_advancements,
        ));
        let white_list = AtomicBool::new(basic_config.white_list);

        let tick_rate_manager = Arc::new(ServerTickRateManager::new(basic_config.tps));

        let mojang_keys_task = tokio::spawn({
            let auth_config = advanced_config.networking.authentication.clone();
            let allow_chat = basic_config.allow_chat_reports;
            async move {
                if allow_chat {
                    fetch_mojang_public_keys(&auth_config).unwrap_or_else(|e| {
                        error!("Failed to fetch Mojang keys: {e}");
                        Vec::new()
                    })
                } else {
                    Vec::new()
                }
            }
        });

        let dimensions = {
            let mut dimensions = vec![Dimension::OVERWORLD];
            if basic_config.allow_nether {
                dimensions.push(Dimension::THE_NETHER);
            }
            if basic_config.allow_end {
                dimensions.push(Dimension::THE_END);
            }
            dimensions
        };
        info!(
            "Enabled dimensions: {:?}",
            dimensions
                .iter()
                .map(|d| d.minecraft_name)
                .collect::<Vec<_>>()
        );

        // EMBER: moved up from after `Arc::new(server)` below so the pool can
        // be stored on the struct itself (see `gen_pool` field) instead of
        // only living in this function's closures.
        let gen_pool = Arc::new(
            rayon::ThreadPoolBuilder::new()
                .thread_name(|i| format!("Gen-Pool-{i}"))
                .build()
                .expect("Failed to build generation thread pool"),
        );
        // EMBER start - cross-world admission control for the shared gen_pool
        let gen_budget = Arc::new(pumpkin_world::chunk_system::GenPoolBudget::new(
            advanced_config.performance.max_concurrent_world_gen_jobs,
        ));
        // EMBER end
        // EMBER start - built-in economy system
        let economy_manager = Arc::new(economy::EconomyManager::new(&advanced_config.economy));
        // EMBER end
        // EMBER start - packet-only NPC manager
        let npc_manager = Arc::new(npc::NpcManager::new());
        // EMBER end

        let server = Self {
            basic_config,
            advanced_config,
            data: vanilla_data,
            plugin_manager: Arc::new(PluginManager::new()),
            permission_manager: Arc::new(RwLock::new(PermissionManager::new(
                permission_registry.clone(),
            ))),
            permission_registry,
            container_id: 0.into(),
            recipe_manager: Arc::new(recipe::RecipeManager::new()),
            map_id: level_info.load().map_id.into(),
            worlds: ArcSwap::from_pointee(vec![]),
            // EMBER start - dynamic world unload
            pending_world_unloads: std::sync::Mutex::new(std::collections::HashSet::new()),
            // EMBER end
            dimensions,
            command_dispatcher,
            block_registry: block_registry.clone(),
            item_registry: super::item::items::default_registry(),
            key_store: OnceCell::new(),
            bedrock_oidc_keys: OnceCell::new(),
            bedrock_private_key: OnceCell::new(),
            listing,
            branding: CachedBranding::new(),
            bossbars: Mutex::new(CustomBossbars::new()),
            map_manager: MapManager::new(),
            defaultgamemode,
            player_data_storage,
            advancement_manager,
            white_list,
            tick_rate_manager,
            tick_times_nanos: Mutex::new([0; 100]),
            aggregated_tick_times_nanos: AtomicI64::new(0),
            tick_count: AtomicI32::new(0),
            tasks: TaskTracker::new(),
            task_scheduler: Arc::new(TaskScheduler::new()),
            server_guid: rand::random(),
            player_idle_timeout: AtomicI32::new(0),
            mojang_public_keys: ArcSwap::from_pointee(Vec::new()),
            world_info_writer: Arc::new(AnvilLevelInfo),
            level_info,
            gen_pool: gen_pool.clone(),
            gen_budget: gen_budget.clone(), // EMBER
            economy_manager,                // EMBER
            npc_manager,                    // EMBER
        };
        let server = Arc::new(server);

        let server_clone = server.clone();
        tokio::spawn(async move {
            server_clone
                .key_store
                .get_or_init(|| async { Arc::new(KeyStore::new()) })
                .await;
        });

        let world_loader = |dim: Dimension| {
            let path = world_path.clone();
            let registry = block_registry.clone();
            let l_info = server.level_info.clone(); // Access from struct
            let weak = Arc::downgrade(&server);
            let config = Arc::new(server.advanced_config.world.clone());
            let pool = server.gen_pool.clone();
            let budget = server.gen_budget.clone(); // EMBER

            tokio::task::spawn_blocking(move || {
                info!(
                    "Loading {}",
                    TextComponent::text(dim.minecraft_name.to_string())
                        .color_named(NamedColor::DarkGreen)
                        .to_pretty_console()
                );
                let level = into_level(dim.clone(), &config, path, seed, Some(pool), Some(budget));
                let world = Arc::new(World::load(level.clone(), l_info, dim, registry, weak));
                let portal: Arc<dyn WorldPortalExt> = Arc::new(WorldPortal(world.clone()));
                level.world_portal.store(Arc::new(Some(portal)));
                world
            })
        };

        info!("Starting parallel world load...");
        let mut world_futures = Vec::new();
        for dim in &server.dimensions {
            world_futures.push(world_loader(dim.clone()));
        }

        let (worlds_results, keys) =
            tokio::join!(futures::future::join_all(world_futures), mojang_keys_task);

        let mut worlds_vec = Vec::new();
        for world_result in worlds_results {
            worlds_vec.push(world_result.expect("World loading panicked"));
        }

        server.worlds.store(Arc::new(worlds_vec));
        if let Ok(k) = keys {
            server.mojang_public_keys.store(Arc::new(k));
        }

        // EMBER start - sidecar residency prewarm + worldborder for startup worlds
        // `create_world_with` applies a sidecar's `border` to worlds it
        // creates at runtime; startup worlds (the default world's own
        // dimensions) never went through that path, so without this their
        // configured border was silently storage/generation-only — enforced
        // against players in none of them, however-configured.
        for world in server.worlds.load().iter() {
            let root = world.level.level_folder.root_folder.clone();
            if let Some(sidecar) = pumpkin_config::ember_world::EmberWorldConfig::load(&root) {
                let cap = sidecar.resident_region_cap();
                if cap > 0 {
                    let level = world.level.clone();
                    tokio::spawn(async move {
                        level.prewarm_storage(cap).await;
                    });
                }
                if let Some(border) = sidecar.border
                    && border > 0
                {
                    let spawn = world.level_info.load();
                    let (cx, cz) = (f64::from(spawn.spawn_x), f64::from(spawn.spawn_z));
                    let mut wb = world.worldborder.lock().await;
                    wb.set_center(world, cx, cz);
                    wb.set_diameter(world, f64::from(border), None);
                }
            }
        }
        // EMBER end

        info!("All worlds loaded successfully.");

        if server.basic_config.online_mode {
            let server_clone = server.clone();
            tokio::spawn(async move {
                server_clone
                    .bedrock_oidc_keys
                    .get_or_init(|| async {
                        tokio::task::block_in_place(|| {
                            pumpkin_util::jwt::fetch_oidc_jwks().unwrap_or_else(|e| {
                                error!("Failed to fetch Bedrock OIDC keys: {e}");
                                (String::new(), pumpkin_util::jwt::Jwks { keys: Vec::new() })
                            })
                        })
                    })
                    .await;
            });
        }
        server
    }

    /// Spawns a task associated with this server. All tasks spawned with this method are awaited
    /// when the server stops. This means tasks should complete in a reasonable (no looping) amount of time.
    pub fn spawn_task<F>(&self, task: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        self.tasks.spawn(task)
    }

    pub fn get_world_from_dimension(&self, dimension: &Dimension) -> Arc<World> {
        self.worlds
            .load()
            .iter()
            .find(|w| w.dimension.minecraft_name == dimension.minecraft_name)
            .cloned()
            .unwrap_or_else(|| {
                self.worlds
                    .load()
                    .first()
                    .expect("Default world should exist")
                    .clone()
            })
    }

    // EMBER start - portal pairing by world-name convention (<x>/<x>_nether/<x>_end)
    /// Resolves a portal's destination world, preferring an explicitly paired
    /// world over `get_world_from_dimension`'s "first loaded world of that
    /// dimension" default.
    ///
    /// A world named `<x>_nether`/`<x>_end` pairs with the world named `<x>`
    /// (see `dimension_for_world_name` in the `/world load` command); this
    /// strips a `_nether`/`_end` suffix off `current`'s name (if present) to
    /// get `<x>`, builds the expected name for `target_dimension`, and looks
    /// for a *loaded* world matching both that name and that dimension. If
    /// none is loaded — no pairing was ever set up for this world, or it's
    /// unloaded right now — this falls back to `get_world_from_dimension`,
    /// same as before this pairing convention existed (in practice: the
    /// default world's own nether/end, since nothing else creates one
    /// without being asked to via the naming convention).
    pub fn get_paired_world(
        &self,
        current: &Arc<World>,
        target_dimension: &Dimension,
    ) -> Arc<World> {
        let current_name = current.get_world_name();
        let base_name = current_name
            .strip_suffix("_nether")
            .or_else(|| current_name.strip_suffix("_end"))
            .unwrap_or(current_name);
        let paired_name = if target_dimension.minecraft_name == Dimension::THE_NETHER.minecraft_name
        {
            format!("{base_name}_nether")
        } else if target_dimension.minecraft_name == Dimension::THE_END.minecraft_name {
            format!("{base_name}_end")
        } else {
            base_name.to_string()
        };
        self.worlds
            .load()
            .iter()
            .find(|w| {
                w.dimension.minecraft_name == target_dimension.minecraft_name
                    && w.get_world_name() == paired_name
            })
            .cloned()
            .unwrap_or_else(|| self.get_world_from_dimension(target_dimension))
    }
    // EMBER end

    pub async fn create_world(self: &Arc<Self>, name: String, dimension: Dimension) -> Arc<World> {
        // EMBER start - delegate to create_world_with (per-world config)
        self.create_world_with(name, dimension, None).await
        // EMBER end
    }

    // EMBER start - create_world_with: explicit per-world LevelConfig
    /// Like [`Self::create_world`], but an explicit [`LevelConfig`] replaces
    /// the global configuration for this world — used by ephemeral dungeon
    /// instances. After creation, a world with an `ember-world.toml`
    /// sidecar is prewarmed in the background per its residency policy.
    pub async fn create_world_with(
        self: &Arc<Self>,
        name: String,
        dimension: Dimension,
        level_config: Option<pumpkin_config::world::LevelConfig>,
    ) -> Arc<World> {
        // Border/residency come from the explicit config (dungeon instances
        // have no on-disk sidecar) or the world's sidecar file.
        let world_path = self.basic_config.get_world_path().join(&name);
        let runtime = level_config.as_ref().map_or_else(
            || {
                pumpkin_config::ember_world::EmberWorldConfig::load(&world_path)
                    .map(|s| (s.border, s.resident_region_cap()))
            },
            |lc| {
                let border = lc.ember.border;
                let small = border.is_some_and(|b| {
                    b > 0 && b <= pumpkin_config::ember_world::SMALL_MAP_MAX_BORDER
                });
                let cap = if small {
                    pumpkin_config::ember_world::SMALL_MAP_REGIONS
                } else {
                    0
                };
                Some((border, cap))
            },
        );
        {
            let worlds = self.worlds.load();
            if let Some(world) = worlds
                .iter()
                .find(|w| w.get_world_name() == name && w.dimension == dimension)
            {
                return world.clone();
            }
        }

        // A world of this name may still be flushing after an unload: it has
        // already left `worlds` (so the dedup above misses it) but its old
        // Level keeps writing the same folder/DB rows for up to seconds. Wait
        // for that flush to finish before opening a second Level on the same
        // path, or the two writers would corrupt each other's data. The unload
        // task clears the name once shutdown() completes, so this terminates.
        while self.is_world_unloading(&name) {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let server = self.clone();
        let name_clone = name.clone();
        let world = tokio::task::spawn_blocking(move || {
            let world_path = server.basic_config.get_world_path().join(name_clone);
            let registry = server.block_registry.clone();
            // EMBER: each world loads its own level.dat instead of reusing
            // the default world's `level_info` — otherwise every world
            // created here (dungeon instances, `/world load`, clones) would
            // inherit the default world's spawn point/seed/game rules
            // regardless of what its own level.dat (if any) actually says.
            let l_info = load_world_level_info(&world_path, server.basic_config.seed);
            let weak = Arc::downgrade(&server);
            let config =
                Arc::new(level_config.unwrap_or_else(|| server.advanced_config.world.clone()));
            let seed = l_info.load().world_gen_settings.seed;

            let level = pumpkin_world::dimension::into_level(
                dimension.clone(),
                &config,
                world_path,
                seed,
                Some(server.gen_pool.clone()),
                Some(server.gen_budget.clone()), // EMBER
            );
            let world: World = World::load(level.clone(), l_info, dimension, registry, weak);
            let world = Arc::new(world);
            let portal: Arc<dyn WorldPortalExt> = Arc::new(WorldPortal(world.clone()));
            level.world_portal.store(Arc::new(Some(portal)));
            server.worlds.rcu(|worlds| {
                let mut new_worlds = (**worlds).clone();
                new_worlds.push(world.clone());
                new_worlds
            });
            world
        })
        .await
        .expect("World creation panicked");

        // Size-based policy: enforce the max border and prewarm small maps.
        if let Some((border, cap)) = runtime {
            if let Some(border) = border
                && border > 0
            {
                let spawn = world.level_info.load();
                let (cx, cz) = (f64::from(spawn.spawn_x), f64::from(spawn.spawn_z));
                let mut wb = world.worldborder.lock().await;
                wb.set_center(&world, cx, cz);
                wb.set_diameter(&world, f64::from(border), None);
            }
            if cap > 0 {
                let level = world.level.clone();
                tokio::spawn(async move {
                    level.prewarm_storage(cap).await;
                });
            }
        }

        // Notify plugins that a world came online (informational).
        self.plugin_manager
            .fire(crate::plugin::api::events::world::world_load::WorldLoad::new(world.clone()))
            .await;

        world
    }
    // EMBER end

    // EMBER start - dynamic world management (unload/clone/clone_readonly/delete/list_world_folders)
    /// Unloads a world at runtime: evacuates its players to `fallback`,
    /// removes it from the tick loop, then saves and stops it.
    ///
    /// The default world (first in the list) cannot be unloaded.
    // EMBER start - tick soft-budget isolation: wait out a straggler tick
    // before tearing a world down.
    //
    // `tick_worlds`'s soft budget can leave a world's previous tick still
    // running in the background after that world stops being scheduled
    // (removed from `self.worlds`, or the whole server shutting down). That
    // straggler task holds its own `Arc<World>`/`Arc<Level>` clone and keeps
    // touching the world's Schedule/IO threads and chunk state; tearing the
    // `Level` down underneath it (`Level::shutdown` cancels those threads and
    // flushes storage) races with whatever the straggler is doing mid-tick.
    // Every caller of `World::shutdown` must wait for `ticking` to clear
    // first. Bounded with the same 3s timeout style `Level::shutdown` itself
    // already uses for joining its OS threads: proceed anyway on timeout
    // (never block a shutdown forever on a wedged world) but log it.
    async fn wait_for_tick_to_finish(world: &Arc<World>) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while world.ticking.load(Ordering::Acquire) {
            let notified = world.ticking_notify.notified();
            if !world.ticking.load(Ordering::Acquire) {
                break;
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                warn!(
                    "Timed out waiting for world at {} to finish its in-flight tick before shutdown",
                    world.level.level_folder.root_folder.display()
                );
                break;
            }
        }
    }
    // EMBER end

    pub async fn unload_world(
        self: &Arc<Self>,
        world: &Arc<World>,
        fallback: &Arc<World>,
    ) -> Result<(), String> {
        if world.uuid == fallback.uuid {
            return Err("fallback world must differ from the world being unloaded".to_string());
        }
        {
            let worlds = self.worlds.load();
            let Some(first) = worlds.first() else {
                return Err("no worlds loaded".to_string());
            };
            if first.uuid == world.uuid {
                return Err("cannot unload the default world".to_string());
            }
            if !worlds.iter().any(|w| w.uuid == world.uuid) {
                return Err("world is not loaded".to_string());
            }
            if !worlds.iter().any(|w| w.uuid == fallback.uuid) {
                return Err("fallback world is not loaded".to_string());
            }
        }

        // Let plugins veto the unload before anything is disturbed.
        let event = self
            .plugin_manager
            .fire(
                crate::plugin::api::events::world::world_unload::WorldUnload::new(
                    world.clone(),
                    fallback.clone(),
                ),
            )
            .await;
        if event.cancelled {
            return Err("world unload cancelled by a plugin".to_string());
        }

        // Evacuate players to the fallback world's spawn.
        let spawn = {
            let info = fallback.level_info.load();
            pumpkin_util::math::vector3::Vector3::new(
                f64::from(info.spawn_x) + 0.5,
                f64::from(info.spawn_y),
                f64::from(info.spawn_z) + 0.5,
            )
        };
        let players: Vec<_> = world.players.load().iter().cloned().collect();
        for player in players {
            player
                .teleport_world(fallback.clone(), spawn, None, None)
                .await;
        }
        // A plugin may cancel PlayerChangeWorldEvent; never pull a live
        // world out from under a player.
        if !world.players.load().is_empty() {
            return Err("players could not be moved out of the world".to_string());
        }

        // Leave the tick loop first so no new work starts...
        self.worlds.rcu(|worlds| {
            worlds
                .iter()
                .filter(|w| w.uuid != world.uuid)
                .cloned()
                .collect::<Vec<_>>()
        });

        // ...then save and stop it in the background: a world flush can
        // take seconds and must never stall the tick loop. The name stays
        // in `pending_world_unloads` until the flush finishes so the same
        // world cannot be reloaded while its old Level is still writing.
        let name = world.get_world_name().to_string();
        if let Ok(mut pending) = self.pending_world_unloads.lock() {
            pending.insert(name.clone());
        }
        let world = world.clone();
        let server = self.clone();
        tokio::spawn(async move {
            Self::wait_for_tick_to_finish(&world).await; // EMBER
            world.shutdown().await;
            // Break the Level -> World back-reference so the World can drop.
            world.level.world_portal.store(Arc::new(None));
            if let Ok(mut pending) = server.pending_world_unloads.lock() {
                pending.remove(&name);
            }
            info!("Unloaded world '{name}'");
        });
        Ok(())
    }

    /// Whether a world of this name is still flushing after an unload.
    pub fn is_world_unloading(&self, name: &str) -> bool {
        self.pending_world_unloads
            .lock()
            .is_ok_and(|pending| pending.contains(name))
    }

    /// SlimeWorld-style clone: copies a loaded world's on-disk data (and its
    /// `easy_mysql` database rows, if any) under a new name, then loads it.
    ///
    /// This is the reusable primitive behind `/world clone` and the plugin
    /// API. Business policy (permissions, quotas) belongs to the caller.
    ///
    /// # Errors
    /// Fails when the source is not loaded, the destination already exists
    /// or is unloading, or copying fails.
    pub async fn clone_world(
        self: &Arc<Self>,
        src_name: &str,
        dst_name: &str,
    ) -> Result<Arc<World>, String> {
        // Never build a destination path from an unchecked name (see delete_world).
        validate_world_name(dst_name)?;
        let src_world = self
            .worlds
            .load()
            .iter()
            .find(|w| w.get_world_name() == src_name)
            .cloned()
            .ok_or_else(|| format!("world '{src_name}' is not loaded"))?;
        if self
            .worlds
            .load()
            .iter()
            .any(|w| w.get_world_name() == dst_name)
        {
            return Err(format!("world '{dst_name}' is already loaded"));
        }
        if self.is_world_unloading(dst_name) {
            return Err(format!("world '{dst_name}' is still unloading"));
        }

        let src_dir = src_world.level.level_folder.root_folder.clone();
        let dst_dir = self.basic_config.get_world_path().join(dst_name);
        if dst_dir.exists() {
            return Err(format!("folder '{}' already exists", dst_dir.display()));
        }

        // Copy on-disk data and clone DB rows. Both steps can create/populate
        // dst_dir, so on ANY failure we best-effort remove dst_dir before
        // returning — a failed clone must never leave a half-built world behind.
        let cloned = async {
            // Copy any on-disk data (region files, level.dat, entities).
            if src_dir.exists() {
                let (src_copy, dst_copy) = (src_dir.clone(), dst_dir.clone());
                tokio::task::spawn_blocking(move || copy_dir_recursive(&src_copy, &dst_copy))
                    .await
                    .map_err(|e| format!("file copy panicked: {e}"))?
                    .map_err(|e| format!("file copy failed: {e}"))?;
            }

            // easy_mysql keeps region data in the database — clone those rows to
            // the new key too (in-database, no data transfer). Resolve the
            // SOURCE world's effective config; its sidecar may pick a different
            // backend than the global one.
            let src_config = pumpkin_config::ember_world::resolve_level_config(
                &self.advanced_config.world,
                &src_dir,
            );
            if let pumpkin_config::chunk::ChunkConfig::Easy(cfg) = &src_config.chunk
                && cfg.backend == pumpkin_config::chunk::EasyBackend::Mysql
            {
                let mysql = cfg.mysql(src_config.ember.mode);
                pumpkin_world::chunk::easy_mysql::clone_world_data(&mysql, &src_dir, &dst_dir)
                    .await
                    .map_err(|e| format!("database clone failed: {e}"))?;
            }
            Ok::<(), String>(())
        }
        .await;

        if cloned.is_err() {
            let cleanup = dst_dir.clone();
            let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&cleanup)).await;
        }
        cloned?;

        Ok(self
            .create_world(dst_name.to_string(), src_world.dimension.clone())
            .await)
    }

    /// Read-only clone: loads `dst_name` as an in-memory instance that reads
    /// `src_name`'s stored data. Edits stay in RAM and are discarded on
    /// unload; nothing is copied, so many instances share the source's
    /// memory. This is the reusable primitive behind read-only clones and
    /// dungeon instances.
    ///
    /// # Errors
    /// Fails when the destination is already loaded/unloading.
    pub async fn clone_world_readonly(
        self: &Arc<Self>,
        src_name: &str,
        dst_name: &str,
    ) -> Result<Arc<World>, String> {
        if self
            .worlds
            .load()
            .iter()
            .any(|w| w.get_world_name() == dst_name)
        {
            return Err(format!("world '{dst_name}' is already loaded"));
        }
        if self.is_world_unloading(dst_name) {
            return Err(format!("world '{dst_name}' is still unloading"));
        }

        let global = &self.advanced_config.world;
        let src_root = self.basic_config.get_world_path().join(src_name);
        let src = pumpkin_config::ember_world::resolve_level_config(global, &src_root);
        let level_config = pumpkin_config::world::LevelConfig {
            chunk: src.chunk,
            lighting: global.lighting,
            autosave_ticks: 0,
            ember: pumpkin_config::ember_world::EmberRuntime {
                mode: pumpkin_config::chunk::EasyWorldMode::ReadOnly,
                source: Some(src_name.to_string()),
                generate: pumpkin_config::ember_world::GenerateMode::Void,
                border: Some(pumpkin_config::ember_world::SMALL_MAP_MAX_BORDER),
            },
        };
        Ok(self
            .create_world_with(
                dst_name.to_string(),
                Dimension::OVERWORLD,
                Some(level_config),
            )
            .await)
    }

    /// Permanently deletes a world's data (folder on disk, plus its rows in
    /// the database for a `mysql`-backed world). The world must not be
    /// loaded, unloading, or the default world.
    ///
    /// # Errors
    /// Fails when the world is loaded/unloading/default, or deletion fails.
    pub async fn delete_world(self: &Arc<Self>, name: &str) -> Result<(), String> {
        // Reject names that escape the worlds directory BEFORE any path is
        // built: "", ".", "..", or names with separators would otherwise let
        // remove_dir_all wipe the worlds container or the server root. This is
        // the authoritative guard — the plugin API reaches this primitive
        // directly, bypassing the command parser.
        validate_world_name(name)?;
        if self
            .worlds
            .load()
            .iter()
            .any(|w| w.get_world_name() == name)
        {
            return Err(format!("world '{name}' is loaded — unload it first"));
        }
        if self.is_world_unloading(name) {
            return Err(format!("world '{name}' is still unloading"));
        }
        if let Some(first) = self.worlds.load().first()
            && first.get_world_name() == name
        {
            return Err("cannot delete the default world".to_string());
        }

        let root = self.basic_config.get_world_path().join(name);

        // Delete database rows for a mysql-backed world before the folder,
        // so the sidecar (which selects the backend) is still readable.
        let config =
            pumpkin_config::ember_world::resolve_level_config(&self.advanced_config.world, &root);
        if let pumpkin_config::chunk::ChunkConfig::Easy(cfg) = &config.chunk
            && cfg.backend == pumpkin_config::chunk::EasyBackend::Mysql
        {
            let mysql = cfg.mysql(config.ember.mode);
            pumpkin_world::chunk::easy_mysql::delete_world_data(&mysql, &root)
                .await
                .map_err(|e| format!("database delete failed: {e}"))?;
        }

        if root.exists() {
            let root_copy = root.clone();
            tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&root_copy))
                .await
                .map_err(|e| format!("delete panicked: {e}"))?
                .map_err(|e| format!("delete failed: {e}"))?;
        }
        info!("Deleted world '{name}'");
        Ok(())
    }

    /// Lists world folders present on disk (each subfolder of the worlds
    /// directory that holds a `level.dat` or a `dimensions/` tree). Used by
    /// tooling to show worlds that exist but are not loaded.
    #[must_use]
    pub fn list_world_folders(&self) -> Vec<String> {
        let base = self.basic_config.get_world_path();
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir(&base) else {
            return out;
        };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let path = entry.path();
            let looks_like_world =
                path.join("level.dat").exists() || path.join("dimensions").is_dir();
            if looks_like_world && let Some(name) = entry.file_name().to_str() {
                out.push(name.to_string());
            }
        }
        out.sort();
        out
    }
    // EMBER end

    /// Adds a new player to the server.
    ///
    /// This function takes an `Arc<Client>` representing the connected client and performs the following actions:
    ///
    /// 1. Generates a new entity ID for the player.
    /// 2. Determines the player's gamemode (defaulting to Survival if not specified in configuration).
    /// 3. **(TODO: Select default from config)** Selects the world for the player (currently uses the first world).
    /// 4. Creates a new `Player` instance using the provided information.
    /// 5. Adds the player to the chosen world.
    /// 6. **(TODO: Config if we want increase online)** Optionally updates server listing information based on the player's configuration.
    ///
    /// # Arguments
    ///
    /// * `client`: An `Arc<Client>` representing the connected client.
    ///
    /// # Returns
    ///
    /// A tuple containing:
    ///
    /// - `Arc<Player>`: A reference to the newly created player object.
    /// - `Arc<World>`: A reference to the world the player was added to.
    ///
    /// # Note
    ///
    /// You still have to spawn the `Player` in a `World` to let them join and make them visible.
    pub async fn add_player(
        &self,
        client: Arc<ClientPlatform>,
        profile: GameProfile,
        config: Option<PlayerConfig>,
    ) -> Option<(Arc<Player>, Arc<World>)> {
        let gamemode = self.defaultgamemode.lock().await.gamemode;

        let (world, nbt) =
            if let Ok(Some(data)) = self.player_data_storage.load_data(&profile.id).await {
                if let Some(dimension_key) = data.get_string("Dimension") {
                    if let Some(dimension) = Dimension::from_name(dimension_key) {
                        let world = self.get_world_from_dimension(dimension);
                        (world, Some(data))
                    } else {
                        warn!("Invalid dimension key in player data: {dimension_key}");
                        let default_world = self
                            .worlds
                            .load()
                            .first()
                            .expect("Default world should exist")
                            .clone();
                        (default_world, Some(data))
                    }
                } else {
                    // Player data exists but doesn't have a "Dimension" key.
                    let default_world = self
                        .worlds
                        .load()
                        .first()
                        .expect("Default world should exist")
                        .clone();
                    (default_world, Some(data))
                }
            } else {
                // No player data found or an error occurred, default to the Overworld.
                let default_world = self
                    .worlds
                    .load()
                    .first()
                    .expect("Default world should exist")
                    .clone();
                (default_world, None)
            };

        let mut player = Player::new(
            client,
            profile,
            config.clone().unwrap_or_default(),
            world.clone(),
            gamemode,
        )
        .await;

        if let Some(mut nbt_data) = nbt {
            player.read_nbt(&mut nbt_data).await;
        }

        // Wrap in Arc after data is loaded
        let player = Arc::new(player);
        {
            let mut advancements = player.advancements.lock().await;
            if let Err(e) = advancements.load().await {
                warn!("Error loading player {}: {e}", player.gameprofile.id);
            }
            advancements.player = Arc::downgrade(&player);
        };

        send_cancellable! {{
            self;
            PlayerLoginEvent::new(player.clone(), TextComponent::text("You have been kicked from the server"));
            'after: {
                player.screen_handler_sync_handler.store_player(player.clone()).await;
                if world
                    .add_player(&player)
                    .is_ok() {
                    let mut user_cache = self.data.user_cache.write().await;
                    user_cache.upsert(player.gameprofile.id, player.gameprofile.name.clone());

                    // TODO: Config if we want increase online
                    if let Some(config) = config {
                        // TODO: Config so we can also just ignore this hehe
                        if config.server_listing {
                            self.listing.lock().await.add_player(&player);
                        }
                    }

                    Some((player, world.clone()))
                } else {
                    None
                }
            }

            'cancelled: {
                player.kick(DisconnectReason::Kicked, event.kick_message).await;
                None
            }
        }}
    }

    pub async fn remove_player(&self, player: &Player) {
        player
            .increment_stat(
                pumpkin_data::statistic::StatisticCategory::Custom,
                pumpkin_data::statistic::CustomStatistic::LeaveGame as i32,
                1,
            )
            .await;
        // TODO: Config if we want decrease online
        self.listing.lock().await.remove_player(player);
    }

    pub async fn shutdown(&self) {
        self.tasks.close();
        debug!("Awaiting tasks for server");
        self.tasks.wait().await;
        debug!("Done awaiting tasks for server");

        info!("Starting worlds");
        for world in self.worlds.load().iter() {
            Self::wait_for_tick_to_finish(world).await; // EMBER
            world.shutdown().await;
        }
        let level_data = self.level_info.load();
        // then lets save the world info

        if let Err(err) = self
            .world_info_writer
            .write_world_info(&level_data, &self.basic_config.get_world_path())
        {
            error!("Failed to save level.dat: {err}");
        }
        info!("Completed worlds");
    }

    /// Broadcasts a packet to all players in all worlds.
    ///
    /// This function sends the specified packet to every connected player in every world managed by the server.
    ///
    /// # Arguments
    ///
    /// * `packet`: A reference to the packet to be broadcast. The packet must implement the `ClientPacket` trait.
    pub fn broadcast_packet_all<P: ClientPacket>(&self, packet: &P) {
        for world in self.worlds.load().iter() {
            world.broadcast_packet_all(packet);
        }
    }

    pub async fn broadcast_tab_list_header_footer(
        &self,
        header: &TextComponent,
        footer: &TextComponent,
    ) {
        let packet = CTabList::new(header, footer);
        for world in self.worlds.load().iter() {
            for player in world.players.load().iter() {
                *player.tab_list_header.lock().await = header.clone();
                *player.tab_list_footer.lock().await = footer.clone();
                player.client.enqueue_packet(&packet).await;
            }
        }
    }

    pub async fn broadcast_message(
        &self,
        message: &TextComponent,
        sender_name: &TextComponent,
        chat_type: u8,
        target_name: Option<&TextComponent>,
    ) {
        send_cancellable! {{
            self;
            ServerBroadcastEvent::new(message.clone(), sender_name.clone());

            'after: {
                for world in self.worlds.load().iter() {
                    world
                        .broadcast_message(&event.message, &event.sender, chat_type, target_name)
                        .await;
                }
            }
        }}
    }

    /// Gets the current difficulty of the server.
    pub fn get_difficulty(&self) -> Difficulty {
        self.level_info.load().difficulty
    }

    /// Sets the difficulty of the server.
    ///
    /// This function updates the difficulty level of the server and broadcasts the change to all players.
    /// It also iterates through all worlds to ensure the difficulty is applied consistently.
    /// If `force_update` is `Some(true)`, the difficulty will be set regardless of the current state.
    /// If `force_update` is `Some(false)` or `None`, the difficulty will only be updated if it is not locked.
    ///
    /// # Arguments
    ///
    /// * `difficulty`: The new difficulty level to set. This should be one of the variants of the `Difficulty` enum.
    /// * `force_update`: An optional boolean that, if set to `Some(true)`, forces the difficulty to be updated even if it is currently locked.
    ///
    /// # Note
    ///
    /// This function does not handle the actual mob spawn options update, which is a TODO item for future implementation.
    pub fn set_difficulty(&self, difficulty: Difficulty, force_update: bool) {
        let current_info = self.level_info.load();
        if current_info.difficulty_locked && !force_update {
            return;
        }

        let new_difficulty = if self.basic_config.hardcore {
            Difficulty::Hard
        } else {
            difficulty
        };

        let mut new_info = (**current_info).clone();

        new_info.difficulty = new_difficulty;
        let locked = new_info.difficulty_locked;
        self.level_info.store(Arc::new(new_info));

        for world in self.worlds.load().iter() {
            world.set_difficulty(difficulty);
        }

        self.broadcast_packet_all(&CChangeDifficulty::new(difficulty as u8, locked));
    }

    /// Searches for a player by their username across all worlds.
    ///
    /// This function iterates through each world managed by the server and attempts to find a player with the specified username.
    /// If a player is found in any world, it returns an `Arc<Player>` reference to that player. Otherwise, it returns `None`.
    ///
    /// # Arguments
    ///
    /// * `name`: The username of the player to search for.
    ///
    /// # Returns
    ///
    /// An `Option<Arc<Player>>` containing the player if found, or `None` if not found.
    pub fn get_player_by_name(&self, name: &str) -> Option<Arc<Player>> {
        for world in self.worlds.load().iter() {
            if let Some(player) = world.get_player_by_name(name) {
                return Some(player);
            }
        }
        None
    }

    pub async fn get_players_by_ip(&self, ip: IpAddr) -> Vec<Arc<Player>> {
        let mut players = Vec::<Arc<Player>>::new();

        for world in self.worlds.load().iter() {
            for player in world.players.load().iter() {
                if player.client.address().await.ip() == ip {
                    players.push(player.clone());
                }
            }
        }

        players
    }

    /// Returns all players from all worlds.
    pub fn get_all_players(&self) -> Vec<Arc<Player>> {
        let mut players = Vec::<Arc<Player>>::new();

        for world in self.worlds.load().iter() {
            players.extend(world.players.load().iter().cloned());
        }

        players
    }

    pub fn for_each_player<F>(&self, mut f: F)
    where
        F: FnMut(&Arc<Player>),
    {
        let worlds = self.worlds.load();

        for world in worlds.iter() {
            let players = world.players.load();
            for player in players.iter() {
                f(player);
            }
        }
    }

    /// Returns a random player from any of the worlds, or `None` if all worlds are empty.
    pub fn get_random_player(&self) -> Option<Arc<Player>> {
        let players = self.get_all_players();
        players.choose(&mut rand::rng()).map(Arc::<_>::clone)
    }

    /// Searches for a player by their UUID across all worlds.
    ///
    /// This function iterates through each world managed by the server and attempts to find a player with the specified UUID.
    /// If a player is found in any world, it returns an `Arc<Player>` reference to that player. Otherwise, it returns `None`.
    ///
    /// # Arguments
    ///
    /// * `id`: The UUID of the player to search for.
    ///
    /// # Returns
    ///
    /// An `Option<Arc<Player>>` containing the player if found, or `None` if not found.
    pub fn get_player_by_uuid(&self, id: uuid::Uuid) -> Option<Arc<Player>> {
        for world in self.worlds.load().iter() {
            if let Some(player) = world.get_player_by_uuid(id) {
                return Some(player);
            }
        }
        None
    }

    /// Counts the total number of players across all worlds.
    ///
    /// This function iterates through each world and sums up the number of players currently connected to that world.
    ///
    /// # Returns
    ///
    /// The total number of players connected to the server.
    pub fn get_player_count(&self) -> usize {
        let mut count = 0;
        for world in self.worlds.load().iter() {
            count += world.players.load().len();
        }
        count
    }

    /// Similar to [`Server::get_player_count`] >= n, but may be more efficient since it stops its iteration through all worlds as soon as n players were found.
    pub fn has_n_players(&self, n: usize) -> bool {
        let mut count = 0;
        for world in self.worlds.load().iter() {
            count += world.players.load().len();
            if count >= n {
                return true;
            }
        }
        false
    }

    /// Generates a new container id.
    pub fn new_container_id(&self) -> u32 {
        self.container_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Generates a new map id.
    pub fn next_map_id(&self) -> i32 {
        let id = self.map_id.fetch_add(1, Ordering::SeqCst);
        self.level_info.rcu(|level_info| {
            let mut new_level_info = (**level_info).clone();
            new_level_info.map_id = self.map_id.load(Ordering::SeqCst);
            new_level_info
        });
        id
    }

    pub fn get_branding(&self) -> CPluginMessage<'_> {
        self.branding.get_branding()
    }

    pub const fn get_status(&self) -> &Mutex<CachedStatus> {
        &self.listing
    }

    pub async fn encryption_request<'a>(
        &'a self,
        verification_token: &'a [u8; 4],
        should_authenticate: bool,
    ) -> CEncryptionRequest<'a> {
        self.key_store
            .get_or_init(|| async { Arc::new(KeyStore::new()) })
            .await
            .encryption_request("", verification_token, should_authenticate)
    }

    pub async fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>, EncryptionError> {
        self.key_store
            .get_or_init(|| async { Arc::new(KeyStore::new()) })
            .await
            .decrypt(data)
    }

    pub async fn digest_secret(&self, secret: &[u8]) -> String {
        self.key_store
            .get_or_init(|| async { Arc::new(KeyStore::new()) })
            .await
            .get_digest(secret)
    }

    /// Main server tick method. This now handles both player/network ticking (which always runs)
    /// and world/game logic ticking (which is affected by freeze state).
    pub async fn tick(self: &Arc<Self>) {
        if self.tick_rate_manager.runs_normally() || self.tick_rate_manager.is_sprinting() {
            self.tick_worlds().await;
            // Always run player and network ticking, even when game is frozen
        } else {
            self.tick_players_and_network().await;
        }
    }

    /// Ticks essential server functions that must run even when the game is frozen.
    /// This includes player ticking (network, keep-alives) and flushing world updates to clients.
    pub async fn tick_players_and_network(self: &Arc<Self>) {
        let worlds = self.worlds.load();

        for world in worlds.iter() {
            world.flush_block_updates().await;
            world.flush_synced_block_events().await;
        }

        let mut set = JoinSet::new();
        for world in worlds.iter() {
            let players = world.players.load();
            for player in players.iter() {
                let player_clone = player.clone();
                let server_clone = self.clone();
                set.spawn(async move {
                    player_clone.tick(&server_clone).await;
                });
            }
        }
        // EMBER start - a panicking player tick must not propagate: join_all()
        // re-panics on the first failed task, which would kill the fire-and-
        // forget Ticker task and silently freeze every world's ticking forever.
        while let Some(res) = set.join_next().await {
            if let Err(e) = res {
                error!("Player tick task failed: {e}");
            }
        }
        // EMBER end
    }
    /// Ticks the game logic for all worlds. This is the part that is affected by `/tick freeze`.
    // EMBER start - tick soft-budget isolation
    //
    // Goal: a world whose tick overruns its budget (stuck, or just doing a
    // lot of work) must not hold back the other worlds' tick progress or the
    // server's overall tick pacing.
    //
    // `ticking` on each `World` (an `Arc<AtomicBool>`) guards against
    // spawning a second overlapping tick for a world whose previous tick is
    // still running in the background - `World::tick` assumes it is never
    // called concurrently with itself (unsynchronized `flush_*` queues,
    // `chunk_loading.lock().unwrap()`, etc.), so this guard is required for
    // correctness, not just bookkeeping.
    //
    // Released via a local `TickingGuard` drop-guard rather than "the line
    // after `.await` completes": `World::tick` has real panic paths (e.g.
    // that same `chunk_loading.lock().unwrap()` poisoning on any earlier
    // panic), and a plain post-await release would be skipped by an
    // unwinding panic, stranding the world's `ticking` flag at `true`
    // forever with no indication why. Drop runs during unwinding (this
    // workspace does not set `panic = "abort"`), so the guard is reliable
    // either way. `catch_unwind` around the tick call additionally turns the
    // panic into a logged error - without it, the panic would still be
    // contained by the guard, but silently.
    //
    // Plain `tokio::spawn` (NOT `JoinSet`) is required for the tasks
    // themselves: dropping a `JoinSet` aborts every task still inside it,
    // which would kill exactly the straggler task this mechanism is meant to
    // let keep running. Dropping a bare `JoinHandle` - which is what happens
    // below when `timeout` wins the race and the `join_all` future is
    // discarded - only detaches it: the task keeps running to completion on
    // its own, and the next `tick_worlds` cycle will skip that world (via
    // `ticking`) until it finishes.
    //
    // Trade-off callers must know about: `ServerTickEndEvent`/
    // `tick_duration_nanos`/`tick_count` no longer guarantee "every world
    // finished this tick" - only "every world within budget finished".
    pub async fn tick_worlds(self: &Arc<Self>) {
        struct TickingGuard(Arc<World>);
        impl Drop for TickingGuard {
            fn drop(&mut self) {
                self.0.ticking.store(false, Ordering::Release);
                self.0.ticking_notify.notify_waiters();
            }
        }

        self.task_scheduler.tick(self).await;
        self.npc_manager.tick(self).await; // EMBER - packet-only NPC visibility

        let mut handles = Vec::new();
        for world in self.worlds.load().iter() {
            if world.ticking.swap(true, Ordering::AcqRel) {
                warn!(
                    "World at {} is still ticking from a previous cycle - skipping it this tick",
                    world.level.level_folder.root_folder.display()
                );
                continue;
            }
            let world = world.clone();
            let server = self.clone();
            handles.push(tokio::spawn(async move {
                let _guard = TickingGuard(world.clone());
                if let Err(e) = AssertUnwindSafe(world.tick(server)).catch_unwind().await {
                    error!("World tick task panicked: {e:?}");
                }
            }));
        }

        let budget = Duration::from_nanos(self.tick_rate_manager.nanoseconds_per_tick() as u64);
        if tokio::time::timeout(budget, futures::future::join_all(handles))
            .await
            .is_err()
        {
            debug!(
                "tick_worlds: one or more worlds are still ticking past this tick's budget; \
                 continuing without waiting for them"
            );
        }
        // EMBER end

        // Global tasks
        if let Err(e) = self.player_data_storage.tick(self).await {
            error!("Error ticking player data: {e}");
        }
    }

    /// Updates the tick time statistics with the duration of the last tick.
    pub async fn update_tick_times(&self, tick_duration_nanos: i64) {
        let tick_count = self.tick_count.fetch_add(1, Ordering::Relaxed);
        let index = (tick_count % 100) as usize;

        let mut tick_times = self.tick_times_nanos.lock().await;
        let old_time = tick_times[index];
        tick_times[index] = tick_duration_nanos;
        drop(tick_times);

        self.aggregated_tick_times_nanos
            .fetch_add(tick_duration_nanos - old_time, Ordering::Relaxed);
    }

    /// Gets the rolling average tick time over the last 100 ticks, in nanoseconds.
    pub fn get_average_tick_time_nanos(&self) -> i64 {
        let tick_count = self.tick_count.load(Ordering::Relaxed);
        let sample_size = (tick_count as usize).min(100);
        if sample_size == 0 {
            return 0;
        }
        self.aggregated_tick_times_nanos.load(Ordering::Relaxed) / sample_size as i64
    }

    /// Returns the average Milliseconds Per Tick (MSPT).
    pub fn get_mspt(&self) -> f64 {
        let avg_nanos = self.get_average_tick_time_nanos();
        // Convert nanoseconds to decimal milliseconds
        avg_nanos as f64 / 1_000_000.0
    }

    /// Returns the Ticks Per Second (TPS).
    pub fn get_tps(&self) -> f64 {
        let mspt = self.get_mspt();
        if mspt <= 0.0 {
            return 0.0;
        }
        1000.0 / mspt
    }

    /// Returns a copy of the last 100 tick times.
    pub async fn get_tick_times_nanos_copy(&self) -> [i64; 100] {
        *self.tick_times_nanos.lock().await
    }

    #[allow(clippy::too_many_lines, clippy::option_if_let_else)]
    pub fn select_players(
        &self,
        target_selector: &TargetSelector,
        source: Option<&CommandSender>,
    ) -> Vec<Arc<Player>> {
        let mut players = match &target_selector.selector_type {
            EntitySelectorType::Source => source
                .and_then(CommandSender::as_player)
                .map_or_else(Vec::new, |player| vec![player]),
            EntitySelectorType::NearestPlayer
            | EntitySelectorType::NearestEntity
            | EntitySelectorType::RandomPlayer
            | EntitySelectorType::AllPlayers
            | EntitySelectorType::AllEntities => self.get_all_players(),
            EntitySelectorType::NamedPlayer(name) => self
                .get_player_by_name(name)
                .map_or_else(Vec::new, |player| vec![player]),
            EntitySelectorType::Uuid(uuid) => self
                .get_player_by_uuid(*uuid)
                .map_or_else(Vec::new, |player| vec![player]),
        };

        let player_type = EntityType::from_name("player").expect("entity type player must exist");
        let type_included = target_selector
            .conditions
            .iter()
            .filter_map(|f| {
                if let EntityFilter::Type(ValueCondition::Equals(entity_type)) = f {
                    Some(*entity_type)
                } else {
                    None
                }
            })
            .collect::<HashSet<_>>();
        let type_excluded = target_selector
            .conditions
            .iter()
            .filter_map(|f| {
                if let EntityFilter::Type(ValueCondition::NotEquals(entity_type)) = f {
                    Some(*entity_type)
                } else {
                    None
                }
            })
            .collect::<HashSet<_>>();

        players.retain(|_| {
            (type_excluded.is_empty() || !type_excluded.contains(player_type))
                && (type_included.is_empty() || type_included.contains(player_type))
        });

        let limit = target_selector.get_limit();
        if limit == 0 {
            return Vec::new();
        }

        match target_selector
            .get_sort()
            .unwrap_or(EntityFilterSort::Arbitrary)
        {
            EntityFilterSort::Arbitrary => players.into_iter().take(limit).collect(),
            EntityFilterSort::Random => {
                players.shuffle(&mut rand::rng());
                players.into_iter().take(limit).collect()
            }
            EntityFilterSort::Nearest | EntityFilterSort::Furthest => {
                let center = source.and_then(CommandSender::position).unwrap_or_default();
                let nearest_first = target_selector
                    .get_sort()
                    .is_none_or(|sort| sort == EntityFilterSort::Nearest);
                players.sort_by(|a, b| {
                    let a_distance = a.get_entity().pos.load().squared_distance_to_vec(&center);
                    let b_distance = b.get_entity().pos.load().squared_distance_to_vec(&center);
                    if nearest_first {
                        a_distance
                            .partial_cmp(&b_distance)
                            .unwrap_or(core::cmp::Ordering::Equal)
                    } else {
                        b_distance
                            .partial_cmp(&a_distance)
                            .unwrap_or(core::cmp::Ordering::Equal)
                    }
                });
                players.into_iter().take(limit).collect()
            }
        }
    }

    #[allow(clippy::too_many_lines, clippy::option_if_let_else)]
    pub fn select_entities(
        &self,
        target_selector: &TargetSelector,
        source: Option<&CommandSender>,
    ) -> Vec<Arc<dyn EntityBase>> {
        let all_entities_and_players = || {
            let mut entities = Vec::new();
            for world in self.worlds.load().iter() {
                entities.extend(world.entities.load().iter().cloned());
                entities.extend(
                    world
                        .players
                        .load()
                        .iter()
                        .cloned()
                        .map(|player| player as Arc<dyn EntityBase>),
                );
            }
            entities
        };
        let all_players_as_entities = || {
            self.get_all_players()
                .into_iter()
                .map(|player| player as Arc<dyn EntityBase>)
                .collect::<Vec<_>>()
        };

        let mut entities = match &target_selector.selector_type {
            EntitySelectorType::Source => source
                .and_then(CommandSender::as_player)
                .map_or_else(Vec::new, |player| vec![player as Arc<dyn EntityBase>]),
            EntitySelectorType::NearestPlayer
            | EntitySelectorType::RandomPlayer
            | EntitySelectorType::AllPlayers => all_players_as_entities(),
            EntitySelectorType::NearestEntity | EntitySelectorType::AllEntities => {
                all_entities_and_players()
            }
            EntitySelectorType::NamedPlayer(name) => self
                .get_player_by_name(name)
                .map_or_else(Vec::new, |player| vec![player as Arc<dyn EntityBase>]),
            EntitySelectorType::Uuid(uuid) => self
                .get_player_by_uuid(*uuid)
                .map_or_else(Vec::new, |player| vec![player as Arc<dyn EntityBase>]),
        };

        let type_included = target_selector
            .conditions
            .iter()
            .filter_map(|f| {
                if let EntityFilter::Type(ValueCondition::Equals(entity_type)) = f {
                    Some(*entity_type)
                } else {
                    None
                }
            })
            .collect::<HashSet<_>>();
        let type_excluded = target_selector
            .conditions
            .iter()
            .filter_map(|f| {
                if let EntityFilter::Type(ValueCondition::NotEquals(entity_type)) = f {
                    Some(*entity_type)
                } else {
                    None
                }
            })
            .collect::<HashSet<_>>();
        entities.retain(|entity| {
            // Filter by entity type
            (type_excluded.is_empty() || !type_excluded.contains(&entity.get_entity().entity_type))
                && (type_included.is_empty()
                    || type_included.contains(&entity.get_entity().entity_type))
        });

        let limit = target_selector.get_limit();
        if limit == 0 {
            return vec![];
        }

        match target_selector
            .get_sort()
            .unwrap_or(EntityFilterSort::Arbitrary)
        {
            EntityFilterSort::Arbitrary => entities.into_iter().take(limit).collect(),
            EntityFilterSort::Random => {
                entities.shuffle(&mut rand::rng());
                entities.into_iter().take(limit).collect()
            }
            EntityFilterSort::Nearest | EntityFilterSort::Furthest => {
                let center = source.and_then(CommandSender::position).unwrap_or_default();
                let nearest_first = target_selector
                    .get_sort()
                    .is_none_or(|sort| sort == EntityFilterSort::Nearest);
                entities.sort_by(|a, b| {
                    let a_distance = a.get_entity().pos.load().squared_distance_to_vec(&center);
                    let b_distance = b.get_entity().pos.load().squared_distance_to_vec(&center);
                    if nearest_first {
                        a_distance
                            .partial_cmp(&b_distance)
                            .unwrap_or(core::cmp::Ordering::Equal)
                    } else {
                        b_distance
                            .partial_cmp(&a_distance)
                            .unwrap_or(core::cmp::Ordering::Equal)
                    }
                });
                entities.into_iter().take(limit).collect()
            }
        }
    }
}

// EMBER start - per-world level.dat load (create_world_with, startup border)
/// Loads (or creates a fresh default for) the `level.dat` at `world_path`,
/// independently of any other world's `level_info`. `Server::new` loads this
/// once, for the default world's own dimensions to share (they're one save);
/// every *other* world — dungeon instances, `/world load`, clones — needs its
/// own, or they'd all inherit the default world's spawn point/seed/game rules.
///
/// Unlike `Server::new`, a read failure here never panics the whole server
/// over one world: a missing file gets a fresh default (written to disk, same
/// as at startup); any other error (corrupt/unsupported file) logs and falls
/// back to an in-memory default for this session only, leaving the file on
/// disk untouched rather than risk overwriting something possibly recoverable.
fn load_world_level_info(
    world_path: &std::path::Path,
    seed: pumpkin_util::world_seed::Seed,
) -> Arc<ArcSwap<LevelData>> {
    let info = match AnvilLevelInfo.read_world_info(world_path) {
        Ok(info) => info,
        Err(WorldInfoError::InfoNotFound) => {
            let default_data = LevelData::default(seed);
            if let Err(err) = AnvilLevelInfo.write_world_info(&default_data, world_path) {
                error!(
                    "Failed to save new level.dat at {}: {err}",
                    world_path.display()
                );
            }
            default_data
        }
        Err(err) => {
            error!(
                "Failed to load level.dat at {}: {err}. Using in-memory defaults for this \
                 session without touching the file on disk.",
                world_path.display()
            );
            LevelData::default(seed)
        }
    };
    Arc::new(ArcSwap::new(Arc::new(info)))
}
// EMBER end

// EMBER start - world name validation (guards path-building primitives)
/// Rejects a world name that could escape the worlds directory once joined
/// into a filesystem path. A valid name is exactly one normal path component:
/// no empty string, no `.`/`..`, no `/`/`\`/NUL separators, no absolute path
/// or drive prefix, and not made up entirely of trailing `.`/` ` (Windows'
/// legacy, non-`\\?\`-prefixed path handling silently strips those from the
/// final component, so a name like `"..."` or `"   "` would otherwise resolve
/// to the parent directory itself). Guards the destructive primitives
/// (`delete_world`, `clone_world`) so neither a command with a trailing space
/// nor a plugin passing a raw name can `remove_dir_all` the worlds container
/// or the server root; also reused by the `/world load` command so an empty
/// name can't create a stray dimension tree at the container root.
pub(crate) fn validate_world_name(name: &str) -> Result<(), String> {
    use std::path::Component;
    let mut components = std::path::Path::new(name).components();
    let single_normal = matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    );
    let all_trailing_junk = name.trim_end_matches(['.', ' ']).is_empty();
    if single_normal
        && !all_trailing_junk
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
    {
        Ok(())
    } else {
        Err(format!(
            "invalid world name '{name}': must be a single folder name (no '/', '\\', '.', '..', or empty)"
        ))
    }
}
// EMBER end

// EMBER start - world clone helper (shared by Server::clone_world)
/// Recursively copies a directory tree (a world folder: region files,
/// level.dat, entities, ...).
fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
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
// EMBER end
