// EMBER start: custom furniture (packet-only, view-distance broadcast)
//
// Furniture here is never a real world entity - same philosophy as
// `server::npc::NpcManager`: a placed furniture instance is a persisted
// `FurnitureInstanceConfig` (`furniture/instances.toml`) plus a runtime-only
// `RuntimeFurniture` (fake entity ids + the set of players it's currently
// spawned for). Visibility is re-evaluated on an interval using the exact
// same chunk/view-distance rule real entities use, mirroring
// `NpcManager::tick` - the only structural difference from NPCs is that
// furniture renders as an `item_display` (showing the same model as the
// `CustomItemConfig` it was placed from) plus a separate `interaction`
// hitbox for click-to-break, reusing the exact metadata-writing technique
// from `server::menu::MenuManager`.
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use pumpkin_config::{
    FurnitureConfig, FurnitureInstanceConfig, FurnitureInstanceListConfig, FurnitureListConfig,
    LoadConfiguration,
};
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
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::entity::{Entity, EntityBase};
use crate::net::ClientPlatform;
use crate::server::Server;
use crate::server::custom_item::CustomItemManager;
use crate::world::chunker::{get_view_distance, is_within_view_distance};

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

struct RuntimeFurniture {
    instance_id: Uuid,
    /// The `item_display` visual.
    entity_id: i32,
    /// The `interaction` hitbox - what an attack/interact packet targets.
    hitbox_id: i32,
    fake_uuid: Uuid,
    hitbox_uuid: Uuid,
    world: String,
    chunk_pos: Vector2<i32>,
    position: Vector3<f64>,
    furniture_id: String,
    item: &'static Item,
    model: String,
    scale: f64,
    visible_to: HashSet<Uuid>,
}

pub struct FurnitureManager {
    exec_dir: std::path::PathBuf,
    types: RwLock<FurnitureListConfig>,
    instances: RwLock<FurnitureInstanceListConfig>,
    runtime: RwLock<Vec<RuntimeFurniture>>,
}

impl Default for FurnitureManager {
    fn default() -> Self {
        Self::new()
    }
}

impl FurnitureManager {
    #[must_use]
    pub fn new() -> Self {
        let exec_dir = std::env::current_dir().expect("Failed to get current directory");
        let types = FurnitureListConfig::load(&exec_dir);
        let instance_list = FurnitureInstanceListConfig::load(&exec_dir);
        Self {
            exec_dir,
            types: RwLock::new(types),
            instances: RwLock::new(instance_list),
            runtime: RwLock::new(Vec::new()),
        }
    }

    /// Builds runtime state for every persisted instance - called once
    /// after construction (needs the `CustomItemManager` to resolve each
    /// instance's visual, which isn't available yet inside `new()` during
    /// `Server::new`'s own construction order).
    pub async fn load_runtime(&self, custom_items: &CustomItemManager) {
        let types = self.types.read().await.clone();
        let instances = self.instances.read().await.instances.clone();
        let mut runtime = self.runtime.write().await;
        for instance in instances {
            if let Some(state) = Self::build_runtime(&types, custom_items, &instance).await {
                runtime.push(state);
            }
        }
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
        let (item, model) = custom_items
            .resolve_visual(&furniture.custom_item_id)
            .await?;

        let base = Entity::reserve_ids(2);
        let position = Vector3::new(instance.x, instance.y, instance.z);
        Some(RuntimeFurniture {
            instance_id: instance.instance_id,
            entity_id: base,
            hitbox_id: base + 1,
            fake_uuid: Uuid::new_v4(),
            hitbox_uuid: Uuid::new_v4(),
            world: instance.world.clone(),
            chunk_pos: to_chunk_pos(&Vector2::new(
                position.x.floor() as i32,
                position.z.floor() as i32,
            )),
            position,
            furniture_id: furniture.id,
            item,
            model,
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
        world: &str,
        position: Vector3<f64>,
        yaw: f32,
    ) {
        let _ = yaw; // EMBER: item_display billboards to the camera, so a stored yaw isn't rendered - kept for a future non-billboard mode.
        let instance = FurnitureInstanceConfig {
            instance_id: Uuid::new_v4(),
            furniture_id: furniture.id.clone(),
            world: world.to_string(),
            x: position.x,
            y: position.y,
            z: position.z,
            yaw,
        };

        let types = self.types.read().await.clone();
        let Some(state) = Self::build_runtime(&types, custom_items, &instance).await else {
            return;
        };

        let mut instances = self.instances.write().await;
        instances.instances.push(instance);
        instances.save(&self.exec_dir);
        drop(instances);

        self.runtime.write().await.push(state);
    }

    /// Removes the furniture instance whose hitbox is `entity_id`, if any -
    /// despawning it from current viewers and persisting the removal.
    /// Returns the furniture id (for a drop-the-item response) if one was
    /// removed.
    pub async fn break_at(&self, server: &Arc<Server>, entity_id: i32) -> Option<String> {
        let mut runtime = self.runtime.write().await;
        let index = runtime.iter().position(|f| f.hitbox_id == entity_id)?;
        let removed = runtime.remove(index);
        drop(runtime);

        let mut instances = self.instances.write().await;
        instances
            .instances
            .retain(|i| i.instance_id != removed.instance_id);
        instances.save(&self.exec_dir);
        drop(instances);

        if !removed.visible_to.is_empty()
            && let Some(world) = server
                .worlds
                .load()
                .iter()
                .find(|w| w.get_world_name() == removed.world)
                .cloned()
        {
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
    /// connected player, spawning/despawning per-viewer as they cross the
    /// view-distance boundary - identical rule to `NpcManager::tick`.
    /// Called once per game tick from `Server::tick_worlds`.
    pub async fn tick(&self, server: &Arc<Server>) {
        let tick_count = server.tick_count.load(Ordering::Relaxed);
        if tick_count % VISIBILITY_INTERVAL_TICKS != 0 {
            return;
        }

        let mut runtime = self.runtime.write().await;
        if runtime.is_empty() {
            return;
        }

        for world in server.worlds.load().iter() {
            let players = world.players.load();
            for furniture in runtime
                .iter_mut()
                .filter(|f| f.world == world.get_world_name())
            {
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
    }

    fn send_spawn(client: &crate::net::java::JavaClient, furniture: &RuntimeFurniture) {
        client.try_enqueue_packet(&CSpawnEntity::new(
            VarInt(furniture.entity_id),
            furniture.fake_uuid,
            VarInt(i32::from(EntityType::ITEM_DISPLAY.id)),
            furniture.position,
            0.0,
            0.0,
            0.0,
            VarInt(0),
            Vector3::new(0.0, 0.0, 0.0),
        ));
        Self::send_item_metadata(client, furniture);

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

    fn send_item_metadata(client: &crate::net::java::JavaClient, furniture: &RuntimeFurniture) {
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

        let mut item_stack = ItemStack::new(1, furniture.item);
        item_stack.patch.push((
            DataComponent::ItemModel,
            Some(Box::new(ItemModelImpl {
                id: furniture.model.clone().into(),
            })),
        ));
        let stack = ItemStackSerializer::from(item_stack);
        ok &= Metadata::new(ITEM_DISPLAY_ITEM_OLD, MetaDataType::ITEM_STACK, &stack)
            .write(&mut buf, &version)
            .is_ok();
        ok &= Metadata::new(TrackedData::ITEM_STACK_ID, MetaDataType::ITEM_STACK, &stack)
            .write(&mut buf, &version)
            .is_ok();

        if ok {
            buf.push(0xFF);
            client.try_enqueue_packet(&CSetEntityMetadata::new(
                VarInt(furniture.entity_id),
                buf.into(),
            ));
        }
    }
}
// EMBER end
