// EMBER start: custom furniture (packet-only, view-distance broadcast)
//
// Furniture here is never a real world entity - similar in spirit to
// `server::npc::NpcManager`: a placed furniture instance is a persisted
// `FurnitureInstanceConfig` (`<world folder>/furniture_instances.toml`)
// plus a runtime-only `RuntimeFurniture` (fake entity ids + the set of
// players it's currently spawned for). Visibility is re-evaluated on an
// interval using the exact same chunk/view-distance rule real entities
// use, mirroring `NpcManager::tick`. Unlike NPCs (one global manager
// scanning every world by a `world: String` field), one `FurnitureManager`
// is owned per loaded `World` (constructed in `World::load`, dropped with
// it on unload) - the instance file lives inside the world's own folder so
// it travels with it if that folder is copied to another server, the same
// reasoning `World::portal_poi` already follows for its own per-world
// index. The other structural difference from NPCs is the rendering: an
// `item_display` (showing the held custom item's own model, `render_mode =
// "item"`) or a `block_display` (showing a chosen vanilla blockstate,
// `render_mode = "block"` - phase four's non-solid, no-core-edits sibling
// to the real blockstate-carrier custom blocks), plus a separate
// `interaction` hitbox for click-to-break, reusing the exact
// metadata-writing technique from `server::menu::MenuManager`.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use pumpkin_config::chunk::{ChunkConfig, EasyBackend, EasyWorldMode};
use pumpkin_config::{
    FurnitureConfig, FurnitureInstanceConfig, FurnitureInstanceListConfig, FurnitureListConfig,
    LoadConfiguration, RenderMode,
};
use pumpkin_data::Block;
use pumpkin_data::data_component::DataComponent;
use pumpkin_data::data_component_impl::ItemModelImpl;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::meta_data_type::MetaDataType;
use pumpkin_data::tracked_data::TrackedData;
use pumpkin_protocol::codec::item_stack_seralizer::ItemStackSerializer;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_protocol::java::client::play::{
    CRemoveEntities, CSetEntityMetadata, CSpawnEntity, Metadata,
};
use pumpkin_util::math::vector2::{Vector2, to_chunk_pos};
use pumpkin_util::math::vector3::Vector3;
use pumpkin_world::chunk::easy_mysql::world_key_for;
use tokio::sync::RwLock;
use tracing::error;
use uuid::Uuid;

use crate::entity::{Entity, EntityBase};
use crate::net::ClientPlatform;
use crate::server::custom_item::CustomItemManager;
use crate::world::World;
use crate::world::chunker::{get_view_distance, is_within_view_distance};

const CREATE_TABLE: &str = concat!(
    "CREATE TABLE IF NOT EXISTS ember_furniture_instances (",
    "world_key VARCHAR(512) NOT NULL,",
    "instance_id CHAR(36) NOT NULL,",
    "furniture_id VARCHAR(128) NOT NULL,",
    "x DOUBLE NOT NULL,",
    "y DOUBLE NOT NULL,",
    "z DOUBLE NOT NULL,",
    "yaw FLOAT NOT NULL,",
    "PRIMARY KEY (world_key, instance_id)",
    ")"
);

/// Mirrors this world's chunk storage backend (see module doc): `file`
/// keeps the instance list in a TOML file inside the world's own folder;
/// `mysql` (a world shared read-write/read-only across servers) stores the
/// same rows in that world's own database instead, so every server
/// actually sharing it sees the same placements.
enum Storage {
    File {
        world_root: PathBuf,
        instances: FurnitureInstanceListConfig,
    },
    /// This world's chunk backend is mysql, but `World::load` (sync) can't
    /// connect yet - `load_runtime` connects once it's able to `.await`.
    PendingMysql { url: String, world_key: String },
    Mysql {
        pool: sqlx::mysql::MySqlPool,
        world_key: String,
    },
}

/// Re-evaluate visibility every 10 ticks (0.5s) - same cadence as
/// `NpcManager`, for the same reason (imperceptible latency, near-zero cost).
const VISIBILITY_INTERVAL_TICKS: i32 = 10;

const ITEM_DISPLAY_ITEM_OLD: pumpkin_data::tracked_data::TrackedId =
    pumpkin_data::tracked_data::TrackedId {
        v1_21: 23,
        v1_21_2: 23,
        v1_21_4: 23,
        v1_21_5: 23,
        v1_21_6: 23,
        v1_21_7: 23,
        v1_21_9: 23,
        v1_21_11: 23,
        v26_1: 255,
        v26_2: 255,
    };
const BILLBOARD_CENTER: i8 = 3;

/// What a placed instance actually renders as - resolved once in
/// `build_runtime` from the furniture type's `render_mode`, not
/// re-evaluated per spawn.
enum FurnitureVisual {
    Item {
        item: &'static Item,
        model: String,
    },
    Block {
        state_id: pumpkin_data::BlockStateId,
    },
}

struct RuntimeFurniture {
    instance_id: Uuid,
    /// The `item_display`/`block_display` visual.
    entity_id: i32,
    /// The `interaction` hitbox - what an attack/interact packet targets.
    hitbox_id: i32,
    fake_uuid: Uuid,
    hitbox_uuid: Uuid,
    chunk_pos: Vector2<i32>,
    position: Vector3<f64>,
    furniture_id: String,
    visual: FurnitureVisual,
    scale: f64,
    visible_to: HashSet<Uuid>,
}

pub struct FurnitureManager {
    storage: RwLock<Storage>,
    /// The configured furniture *types* - server-level (see module doc),
    /// reloaded independently per world since it's tiny and read-only after
    /// boot; not worth threading a shared handle through `World::load` for.
    types: RwLock<FurnitureListConfig>,
    runtime: RwLock<Vec<RuntimeFurniture>>,
}

impl FurnitureManager {
    /// Loads this world's furniture type list and (for file storage) its
    /// instance list synchronously - safe to call from `World::load` (not
    /// an `async fn`). `runtime` starts empty either way; the caller must
    /// follow up with `load_runtime` once it can `.await` (resolving each
    /// instance's visual needs the `CustomItemManager`, and mysql storage
    /// needs to actually connect - neither is possible synchronously here).
    #[must_use]
    pub fn new(world_root: &Path, chunk_config: &ChunkConfig) -> Self {
        let exec_dir = std::env::current_dir().expect("Failed to get current directory");
        let types = FurnitureListConfig::load(&exec_dir);

        if let ChunkConfig::Easy(cfg) = chunk_config
            && cfg.backend == EasyBackend::Mysql
        {
            let world_key = world_key_for(&cfg.mysql(EasyWorldMode::default()), world_root);
            return Self {
                storage: RwLock::new(Storage::PendingMysql {
                    url: cfg.url.clone(),
                    world_key,
                }),
                types: RwLock::new(types),
                runtime: RwLock::new(Vec::new()),
            };
        }

        let instances = FurnitureInstanceListConfig::load(world_root);
        Self {
            storage: RwLock::new(Storage::File {
                world_root: world_root.to_path_buf(),
                instances,
            }),
            types: RwLock::new(types),
            runtime: RwLock::new(Vec::new()),
        }
    }

    /// Builds runtime state for every persisted instance - called once
    /// after construction (needs the `CustomItemManager` to resolve each
    /// instance's visual, which isn't available yet inside `new()` during
    /// `Server::new`'s own construction order; mysql storage also connects
    /// here for the same reason - both need to `.await`).
    pub async fn load_runtime(&self, custom_items: &CustomItemManager) {
        let instances = self.load_instances().await;
        let types = self.types.read().await.clone();
        let mut runtime = self.runtime.write().await;
        for instance in &instances {
            if let Some(state) = Self::build_runtime(&types, custom_items, instance).await {
                runtime.push(state);
            }
        }
    }

    /// Returns this world's persisted furniture instances, connecting to
    /// mysql (creating the table and loading its rows) first if this
    /// manager's storage is still pending.
    async fn load_instances(&self) -> Vec<FurnitureInstanceConfig> {
        if let Storage::File { instances, .. } = &*self.storage.read().await {
            return instances.instances.clone();
        }

        let (url, world_key) = match &*self.storage.read().await {
            Storage::PendingMysql { url, world_key } => (url.clone(), world_key.clone()),
            Storage::File { .. } | Storage::Mysql { .. } => return Vec::new(),
        };

        let pool = match crate::server::ember_db::connect_ember_database(&url).await {
            Ok(pool) => pool,
            Err(e) => {
                error!(
                    "Furniture manager: {e} - furniture in this world won't load or persist \
                     until this is fixed."
                );
                return Vec::new();
            }
        };
        if let Err(e) = sqlx::query(CREATE_TABLE).execute(&pool).await {
            error!("Furniture manager: failed to create table: {e}");
            return Vec::new();
        }

        let rows: Vec<(String, String, f64, f64, f64, f32)> = match sqlx::query_as(
            "SELECT instance_id, furniture_id, x, y, z, yaw FROM ember_furniture_instances \
             WHERE world_key = ?",
        )
        .bind(&world_key)
        .fetch_all(&pool)
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                error!("Furniture manager: failed to load instances: {e}");
                return Vec::new();
            }
        };

        let instances: Vec<FurnitureInstanceConfig> = rows
            .into_iter()
            .filter_map(|(id, furniture_id, x, y, z, yaw)| {
                Some(FurnitureInstanceConfig {
                    instance_id: Uuid::parse_str(&id).ok()?,
                    furniture_id,
                    x,
                    y,
                    z,
                    yaw,
                })
            })
            .collect();

        *self.storage.write().await = Storage::Mysql { pool, world_key };
        instances
    }

    async fn build_runtime(
        types: &FurnitureListConfig,
        custom_items: &CustomItemManager,
        instance: &FurnitureInstanceConfig,
    ) -> Option<RuntimeFurniture> {
        let furniture = types
            .furniture
            .iter()
            .find(|f| f.id.eq_ignore_ascii_case(&instance.furniture_id))?
            .clone();
        let visual = match furniture.render_mode {
            RenderMode::Item => {
                let (item, model) = custom_items
                    .resolve_visual(&furniture.custom_item_id)
                    .await?;
                FurnitureVisual::Item { item, model }
            }
            RenderMode::Block => {
                let block = Block::from_name(&furniture.block.to_lowercase())?;
                FurnitureVisual::Block {
                    state_id: block.default_state.id,
                }
            }
        };

        let base = Entity::reserve_ids(2);
        let position = Vector3::new(instance.x, instance.y, instance.z);
        Some(RuntimeFurniture {
            instance_id: instance.instance_id,
            entity_id: base,
            hitbox_id: base + 1,
            fake_uuid: Uuid::new_v4(),
            hitbox_uuid: Uuid::new_v4(),
            chunk_pos: to_chunk_pos(&Vector2::new(
                position.x.floor() as i32,
                position.z.floor() as i32,
            )),
            position,
            furniture_id: furniture.id,
            visual,
            scale: furniture.scale,
            visible_to: HashSet::new(),
        })
    }

    /// Looks up the furniture type a held custom item places, if any.
    pub async fn find_by_custom_item(&self, custom_item_id: &str) -> Option<FurnitureConfig> {
        self.types
            .read()
            .await
            .furniture
            .iter()
            .find(|f| f.custom_item_id.eq_ignore_ascii_case(custom_item_id))
            .cloned()
    }

    /// Looks up a furniture type by its own id (as opposed to
    /// `find_by_custom_item`, keyed by the item that places it) - used when
    /// breaking one, to resolve which item to hand back.
    pub async fn find_by_id(&self, furniture_id: &str) -> Option<FurnitureConfig> {
        self.types
            .read()
            .await
            .furniture
            .iter()
            .find(|f| f.id.eq_ignore_ascii_case(furniture_id))
            .cloned()
    }

    /// Places a new furniture instance, persists it, and spawns it for
    /// whoever's already in range (the next visibility tick handles that;
    /// this only needs to add the runtime entry).
    pub async fn place(
        &self,
        custom_items: &CustomItemManager,
        furniture: &FurnitureConfig,
        position: Vector3<f64>,
        yaw: f32,
    ) {
        let _ = yaw; // EMBER: item_display billboards to the camera, so a stored yaw isn't rendered - kept for a future non-billboard mode.
        let instance = FurnitureInstanceConfig {
            instance_id: Uuid::new_v4(),
            furniture_id: furniture.id.clone(),
            x: position.x,
            y: position.y,
            z: position.z,
            yaw,
        };

        let types = self.types.read().await.clone();
        let Some(state) = Self::build_runtime(&types, custom_items, &instance).await else {
            return;
        };

        match &mut *self.storage.write().await {
            Storage::File {
                world_root,
                instances,
            } => {
                instances.instances.push(instance);
                instances.save(world_root);
            }
            Storage::Mysql { pool, world_key } => {
                let result = sqlx::query(
                    "INSERT INTO ember_furniture_instances \
                     (world_key, instance_id, furniture_id, x, y, z, yaw) \
                     VALUES (?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(&*world_key)
                .bind(instance.instance_id.to_string())
                .bind(&instance.furniture_id)
                .bind(instance.x)
                .bind(instance.y)
                .bind(instance.z)
                .bind(instance.yaw)
                .execute(&*pool)
                .await;
                if let Err(e) = result {
                    error!("Furniture manager: failed to persist placement: {e}");
                }
            }
            Storage::PendingMysql { .. } => {}
        }

        self.runtime.write().await.push(state);
    }

    /// Removes the furniture instance whose hitbox is `entity_id`, if any -
    /// despawning it from current viewers and persisting the removal.
    /// Returns the furniture id (for a drop-the-item response) if one was
    /// removed.
    pub async fn break_at(&self, world: &World, entity_id: i32) -> Option<String> {
        let mut runtime = self.runtime.write().await;
        let index = runtime.iter().position(|f| f.hitbox_id == entity_id)?;
        let removed = runtime.remove(index);
        drop(runtime);

        match &mut *self.storage.write().await {
            Storage::File {
                world_root,
                instances,
            } => {
                instances
                    .instances
                    .retain(|i| i.instance_id != removed.instance_id);
                instances.save(world_root);
            }
            Storage::Mysql { pool, world_key } => {
                let result = sqlx::query(
                    "DELETE FROM ember_furniture_instances WHERE world_key = ? AND instance_id = ?",
                )
                .bind(&*world_key)
                .bind(removed.instance_id.to_string())
                .execute(&*pool)
                .await;
                if let Err(e) = result {
                    error!("Furniture manager: failed to persist removal: {e}");
                }
            }
            Storage::PendingMysql { .. } => {}
        }

        if !removed.visible_to.is_empty() {
            let ids = [VarInt(removed.entity_id), VarInt(removed.hitbox_id)];
            for player in world.players.load().iter() {
                if removed.visible_to.contains(&player.gameprofile.id)
                    && let ClientPlatform::Java(client) = player.client.as_ref()
                {
                    client.try_enqueue_packet(&CRemoveEntities::new(&ids));
                }
            }
        }

        Some(removed.furniture_id)
    }

    /// Re-evaluates visibility for every furniture instance against every
    /// connected player in this world, spawning/despawning per-viewer as
    /// they cross the view-distance boundary - identical rule to
    /// `NpcManager::tick`. Called once per game tick from
    /// `Server::tick_worlds`, once per loaded world.
    pub async fn tick(&self, world: &World) {
        let Some(server) = world.server.upgrade() else {
            return;
        };
        let tick_count = server.tick_count.load(Ordering::Relaxed);
        if tick_count % VISIBILITY_INTERVAL_TICKS != 0 {
            return;
        }

        let mut runtime = self.runtime.write().await;
        if runtime.is_empty() {
            return;
        }

        let players = world.players.load();
        for furniture in runtime.iter_mut() {
            let mut in_range = HashSet::with_capacity(furniture.visible_to.len());
            for player in players.iter() {
                let ClientPlatform::Java(client) = player.client.as_ref() else {
                    continue;
                };
                let uuid = player.gameprofile.id;
                let center = player.get_entity().chunk_pos.load();
                let view_distance = get_view_distance(player).get() as i32;
                if !is_within_view_distance(furniture.chunk_pos, center, view_distance) {
                    continue;
                }
                in_range.insert(uuid);
                if !furniture.visible_to.contains(&uuid) {
                    Self::send_spawn(client, furniture);
                }
            }
            for player in players.iter() {
                let uuid = player.gameprofile.id;
                if furniture.visible_to.contains(&uuid)
                    && !in_range.contains(&uuid)
                    && let ClientPlatform::Java(client) = player.client.as_ref()
                {
                    let ids = [VarInt(furniture.entity_id), VarInt(furniture.hitbox_id)];
                    client.try_enqueue_packet(&CRemoveEntities::new(&ids));
                }
            }
            furniture.visible_to = in_range;
        }
    }

    fn send_spawn(client: &crate::net::java::JavaClient, furniture: &RuntimeFurniture) {
        let visual_type = match furniture.visual {
            FurnitureVisual::Item { .. } => &EntityType::ITEM_DISPLAY,
            FurnitureVisual::Block { .. } => &EntityType::BLOCK_DISPLAY,
        };
        client.try_enqueue_packet(&CSpawnEntity::new(
            VarInt(furniture.entity_id),
            furniture.fake_uuid,
            VarInt(i32::from(visual_type.id)),
            furniture.position,
            0.0,
            0.0,
            0.0,
            VarInt(0),
            Vector3::new(0.0, 0.0, 0.0),
        ));
        Self::send_visual_metadata(client, furniture);

        // The clickable hitbox - a bare `interaction` entity, left at its
        // vanilla default size like the menu system's button hitboxes (see
        // `server::menu` doc comment for why: no reliable per-version index
        // exists for `interaction`'s own width/height in the generated
        // protocol data).
        client.try_enqueue_packet(&CSpawnEntity::new(
            VarInt(furniture.hitbox_id),
            furniture.hitbox_uuid,
            VarInt(i32::from(EntityType::INTERACTION.id)),
            furniture.position,
            0.0,
            0.0,
            0.0,
            VarInt(0),
            Vector3::new(0.0, 0.0, 0.0),
        ));
    }

    fn send_visual_metadata(client: &crate::net::java::JavaClient, furniture: &RuntimeFurniture) {
        let version = client.version.load();
        let mut buf = Vec::new();
        #[expect(
            clippy::cast_possible_truncation,
            reason = "scale is a small display multiplier"
        )]
        let scale = furniture.scale as f32;
        let mut ok = Metadata::new(
            TrackedData::SCALE,
            MetaDataType::VECTOR_3F,
            Vector3::new(scale, scale, scale),
        )
        .write(&mut buf, &version)
        .is_ok();
        ok &= Metadata::new(
            TrackedData::SCALE_ID,
            MetaDataType::VECTOR3,
            Vector3::new(scale, scale, scale),
        )
        .write(&mut buf, &version)
        .is_ok();

        match &furniture.visual {
            FurnitureVisual::Item { item, model } => {
                // Billboards to the camera - an item icon should stay
                // readable regardless of which way the player's facing.
                ok &= Metadata::new(TrackedData::BILLBOARD, MetaDataType::BYTE, BILLBOARD_CENTER)
                    .write(&mut buf, &version)
                    .is_ok();
                ok &= Metadata::new(
                    TrackedData::BILLBOARD_RENDER_CONSTRAINTS_ID,
                    MetaDataType::BYTE,
                    BILLBOARD_CENTER,
                )
                .write(&mut buf, &version)
                .is_ok();

                let mut item_stack = ItemStack::new(1, item);
                item_stack.patch.push((
                    DataComponent::ItemModel,
                    Some(Box::new(ItemModelImpl {
                        id: model.clone().into(),
                    })),
                ));
                let stack = ItemStackSerializer::from(item_stack);
                ok &= Metadata::new(ITEM_DISPLAY_ITEM_OLD, MetaDataType::ITEM_STACK, &stack)
                    .write(&mut buf, &version)
                    .is_ok();
                ok &= Metadata::new(TrackedData::ITEM_STACK_ID, MetaDataType::ITEM_STACK, &stack)
                    .write(&mut buf, &version)
                    .is_ok();
            }
            FurnitureVisual::Block { state_id } => {
                // No billboard - a block should hold a fixed orientation
                // like a real placed block, not spin to face the camera.
                // `Metadata::write` remaps the state id for whichever
                // protocol version the client is on automatically (the same
                // special-cased handling `MetaDataType::BLOCK_STATE` gets
                // for any block state value).
                let state = VarInt(i32::from(state_id.as_u16()));
                ok &= Metadata::new(TrackedData::BLOCK_STATE, MetaDataType::BLOCK_STATE, state)
                    .write(&mut buf, &version)
                    .is_ok();
                ok &= Metadata::new(
                    TrackedData::BLOCK_STATE_ID,
                    MetaDataType::BLOCK_STATE,
                    state,
                )
                .write(&mut buf, &version)
                .is_ok();
            }
        }

        if ok {
            buf.push(0xFF);
            client.try_enqueue_packet(&CSetEntityMetadata::new(
                VarInt(furniture.entity_id),
                buf.into(),
            ));
        }
    }
}

// Manual-only: needs a real, reachable mysql server, so it's excluded from
// the normal `cargo test` run (`#[ignore]`) and never hardcodes a
// connection string - set `EMBER_TEST_MYSQL_URL` and run with `--ignored`
// to actually exercise it. Exercises the storage layer directly
// (`load_instances`, and `place`/`remove`'s own insert/delete queries)
// rather than through `place`/`remove` themselves, which also need a
// configured `CustomItemManager`/`FurnitureConfig` to resolve a visual -
// business logic already covered by boot-testing, not what this is
// checking (the mysql schema/round-trip itself).
#[cfg(test)]
mod mysql_tests {
    use super::*;

    fn test_url() -> Option<String> {
        std::env::var("EMBER_TEST_MYSQL_URL").ok()
    }

    fn mysql_chunk_config(url: &str) -> ChunkConfig {
        ChunkConfig::Easy(pumpkin_config::chunk::EasyConfig {
            backend: EasyBackend::Mysql,
            url: url.to_string(),
            key_prefix: "ember_mysql_test".to_string(),
            max_cached_regions: 1,
        })
    }

    #[tokio::test]
    #[ignore = "needs a real mysql server; set EMBER_TEST_MYSQL_URL and run with --ignored"]
    async fn insert_persists_and_reloads_across_managers() {
        let Some(url) = test_url() else {
            panic!("set EMBER_TEST_MYSQL_URL to a real mysql connection string to run this");
        };
        let chunk_config = mysql_chunk_config(&url);
        let world_root =
            std::path::PathBuf::from(format!("/ember-mysql-test-{}", std::process::id()));

        let manager = FurnitureManager::new(&world_root, &chunk_config);
        assert!(
            manager.load_instances().await.is_empty(),
            "fresh world_key should start empty"
        );

        let (pool, world_key) = match &*manager.storage.read().await {
            Storage::Mysql { pool, world_key } => (pool.clone(), world_key.clone()),
            Storage::File { .. } | Storage::PendingMysql { .. } => {
                panic!("expected `load_instances` to have connected by now")
            }
        };

        let instance_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO ember_furniture_instances \
             (world_key, instance_id, furniture_id, x, y, z, yaw) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&world_key)
        .bind(instance_id.to_string())
        .bind("test_chair")
        .bind(1.5f64)
        .bind(64.0f64)
        .bind(-2.5f64)
        .bind(90.0f32)
        .execute(&pool)
        .await
        .expect("insert should succeed");

        // A second manager instance, pointed at the same world_key, proves
        // the row actually round-trips through mysql (correct table/column
        // mapping) rather than this just being the same in-memory pool.
        let reloaded = FurnitureManager::new(&world_root, &chunk_config);
        let instances = reloaded.load_instances().await;
        let found = instances
            .iter()
            .find(|i| i.instance_id == instance_id)
            .expect("the row inserted above should load back");
        assert_eq!(found.furniture_id, "test_chair");
        assert!((found.x - 1.5).abs() < f64::EPSILON);
        assert!((found.y - 64.0).abs() < f64::EPSILON);
        assert!((found.z - (-2.5)).abs() < f64::EPSILON);
        assert!((found.yaw - 90.0).abs() < f32::EPSILON);

        sqlx::query(
            "DELETE FROM ember_furniture_instances WHERE world_key = ? AND instance_id = ?",
        )
        .bind(&world_key)
        .bind(instance_id.to_string())
        .execute(&pool)
        .await
        .expect("delete should succeed");

        let after_delete = FurnitureManager::new(&world_root, &chunk_config);
        assert!(
            after_delete
                .load_instances()
                .await
                .iter()
                .all(|i| i.instance_id != instance_id),
            "removal should also round-trip through mysql"
        );
    }
}
// EMBER end
