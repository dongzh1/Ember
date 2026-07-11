// EMBER start: packet-only NPC manager
//
// An NPC here is never a real world entity: no `Entity`, no NBT, no save
// footprint. Each one is a `data::npc::NpcEntry` (persisted in `data/npcs.json`)
// plus a runtime-only `RuntimeNpc` (fake UUID + reserved entity id + the set
// of players it is currently spawned for). Visibility is driven purely by
// packets, re-evaluated on an interval from `Server::tick_worlds` using the
// exact same chunk/view-distance rule real entities use
// (`world::chunker::is_within_view_distance`), so an NPC pops in/out at the
// same boundary a real entity would, without ever being in `world.entities`.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use pumpkin_data::entity::EntityType;
use pumpkin_data::meta_data_type::MetaDataType;
use pumpkin_data::tracked_data::TrackedData;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_protocol::java::client::play::{
    CPlayerInfoUpdate, CRemoveEntities, CRemovePlayerInfo, CSetEntityMetadata, CSpawnEntity,
    Metadata, Player as InfoPlayer, PlayerAction, PlayerInfoFlags,
};
use pumpkin_util::math::vector2::{Vector2, to_chunk_pos};
use pumpkin_util::math::vector3::Vector3;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::data::npc::{NpcConfig, NpcEntry};
use crate::data::{LoadJSONConfiguration, SaveJSONConfiguration};
use crate::entity::player::Player;
use crate::entity::{Entity, EntityBase};
use crate::net::ClientPlatform;
use crate::server::Server;
use crate::world::chunker::{get_view_distance, is_within_view_distance};

/// Re-evaluate NPC visibility every 10 ticks (0.5s at 20 tps) rather than
/// every tick — an admin-placed standing NPC popping in/out half a second
/// late is imperceptible, and this keeps the per-tick cost near zero.
const VISIBILITY_INTERVAL_TICKS: i32 = 10;

/// All bits set: cape/jacket/sleeves/pants-legs/hat all rendered. A real
/// player's byte here mirrors their client-side settings; a fake NPC has no
/// such source, so it hardcodes "show everything".
const SKIN_LAYERS_ALL: u8 = 0x7F;

struct RuntimeNpc {
    fake_uuid: Uuid,
    entity_id: i32,
    chunk_pos: Vector2<i32>,
    visible_to: HashSet<Uuid>,
}

pub struct NpcManager {
    config: RwLock<NpcConfig>,
    runtime: RwLock<HashMap<String, RuntimeNpc>>,
}

impl Default for NpcManager {
    fn default() -> Self {
        Self::new()
    }
}

impl NpcManager {
    #[must_use]
    pub fn new() -> Self {
        let config = NpcConfig::load();
        let mut runtime = HashMap::with_capacity(config.npcs.len());
        for entry in &config.npcs {
            runtime.insert(entry.name.clone(), Self::spawn_runtime_state(entry));
        }
        Self {
            config: RwLock::new(config),
            runtime: RwLock::new(runtime),
        }
    }

    fn spawn_runtime_state(entry: &NpcEntry) -> RuntimeNpc {
        RuntimeNpc {
            fake_uuid: Uuid::new_v4(),
            entity_id: Entity::reserve_ids(1),
            chunk_pos: to_chunk_pos(&Vector2::new(
                entry.x.floor() as i32,
                entry.z.floor() as i32,
            )),
            visible_to: HashSet::new(),
        }
    }

    pub async fn list(&self) -> Vec<NpcEntry> {
        self.config.read().await.npcs.clone()
    }

    /// Creates a new NPC. Fails if the name (case-insensitive) is already taken.
    pub async fn create(&self, entry: NpcEntry) -> Result<(), String> {
        let mut config = self.config.write().await;
        if config.find(&entry.name).is_some() {
            return Err(format!("An NPC named '{}' already exists.", entry.name));
        }
        let mut runtime = self.runtime.write().await;
        runtime.insert(entry.name.clone(), Self::spawn_runtime_state(&entry));
        config.npcs.push(entry);
        config.save();
        Ok(())
    }

    /// Removes an NPC, despawning it from anyone currently viewing it.
    pub async fn remove(&self, server: &Arc<Server>, name: &str) -> Result<(), String> {
        let mut config = self.config.write().await;
        let Some(index) = config
            .npcs
            .iter()
            .position(|n| n.name.eq_ignore_ascii_case(name))
        else {
            return Err(format!("No NPC named '{name}' exists."));
        };
        let removed = config.npcs.remove(index);
        config.save();
        drop(config);

        let mut runtime = self.runtime.write().await;
        if let Some(state) = runtime.remove(&removed.name) {
            Self::despawn_from_viewers(
                server,
                &removed.world,
                state.entity_id,
                state.fake_uuid,
                &state.visible_to,
            );
        }
        Ok(())
    }

    /// Moves an existing NPC to a new world/position/rotation. Currently
    /// visible viewers are despawned immediately; the next visibility tick
    /// re-spawns the NPC at its new location for whoever is still in range.
    pub async fn move_to(
        &self,
        server: &Arc<Server>,
        name: &str,
        world: String,
        pos: Vector3<f64>,
        yaw: f32,
        pitch: f32,
    ) -> Result<(), String> {
        let mut config = self.config.write().await;
        let Some(entry) = config
            .npcs
            .iter_mut()
            .find(|n| n.name.eq_ignore_ascii_case(name))
        else {
            return Err(format!("No NPC named '{name}' exists."));
        };
        entry.world = world;
        entry.x = pos.x;
        entry.y = pos.y;
        entry.z = pos.z;
        entry.yaw = yaw;
        entry.pitch = pitch;
        let entry = entry.clone();
        config.save();
        drop(config);

        self.reset_runtime_and_despawn(server, &entry).await;
        Ok(())
    }

    /// Copies another (currently online) player's `textures` property onto
    /// an NPC. Consistent with `MannequinEntity`'s design: the server never
    /// resolves a skin against Mojang itself, only ever copies a live one.
    pub async fn set_skin(
        &self,
        server: &Arc<Server>,
        name: &str,
        source: &Player,
    ) -> Result<(), String> {
        let textures = source
            .gameprofile
            .properties
            .load()
            .iter()
            .find(|p| &*p.name == "textures")
            .cloned();

        let mut config = self.config.write().await;
        let Some(entry) = config
            .npcs
            .iter_mut()
            .find(|n| n.name.eq_ignore_ascii_case(name))
        else {
            return Err(format!("No NPC named '{name}' exists."));
        };
        entry.skin = textures;
        let entry = entry.clone();
        config.save();
        drop(config);

        self.reset_runtime_and_despawn(server, &entry).await;
        Ok(())
    }

    pub async fn set_action(&self, name: &str, command: Option<String>) -> Result<(), String> {
        let mut config = self.config.write().await;
        let Some(entry) = config
            .npcs
            .iter_mut()
            .find(|n| n.name.eq_ignore_ascii_case(name))
        else {
            return Err(format!("No NPC named '{name}' exists."));
        };
        entry.click_command = command;
        config.save();
        Ok(())
    }

    /// Given an entity id from an interact packet the world doesn't
    /// recognize, returns the configured click command (if any) when it
    /// belongs to one of our NPCs. `None` means "not one of ours" — the
    /// caller should fall through to its normal unknown-entity handling.
    pub async fn click_command(&self, entity_id: i32) -> Option<Option<String>> {
        let runtime = self.runtime.read().await;
        let name = runtime
            .iter()
            .find(|(_, state)| state.entity_id == entity_id)
            .map(|(name, _)| name.clone())?;
        drop(runtime);
        let config = self.config.read().await;
        Some(config.find(&name).and_then(|e| e.click_command.clone()))
    }

    /// Clears the runtime state (new fake uuid/entity id/chunk pos) for an
    /// edited NPC and despawns it from anyone who could currently see the
    /// stale version; the next visibility tick respawns it fresh.
    async fn reset_runtime_and_despawn(&self, server: &Arc<Server>, entry: &NpcEntry) {
        let mut runtime = self.runtime.write().await;
        let old = runtime.insert(entry.name.clone(), Self::spawn_runtime_state(entry));
        drop(runtime);
        if let Some(old) = old {
            Self::despawn_from_viewers(
                server,
                &entry.world,
                old.entity_id,
                old.fake_uuid,
                &old.visible_to,
            );
        }
    }

    fn despawn_from_viewers(
        server: &Arc<Server>,
        world_name: &str,
        entity_id: i32,
        fake_uuid: Uuid,
        viewers: &HashSet<Uuid>,
    ) {
        if viewers.is_empty() {
            return;
        }
        let Some(world) = server
            .worlds
            .load()
            .iter()
            .find(|w| w.get_world_name() == world_name)
            .cloned()
        else {
            return;
        };
        for player in world.players.load().iter() {
            if !viewers.contains(&player.gameprofile.id) {
                continue;
            }
            if let ClientPlatform::Java(client) = player.client.as_ref() {
                client.try_enqueue_packet(&CRemoveEntities::new(&[VarInt(entity_id)]));
                client.try_enqueue_packet(&CRemovePlayerInfo::new(&[fake_uuid]));
            }
        }
    }

    /// Re-evaluates visibility for every NPC against every connected player,
    /// spawning/despawning per-viewer as they cross the view-distance
    /// boundary. Called once per game tick from `Server::tick_worlds`; the
    /// interval check keeps the real work at a fraction of that rate.
    pub async fn tick(&self, server: &Arc<Server>) {
        if server.tick_count.load(Ordering::Relaxed) % VISIBILITY_INTERVAL_TICKS != 0 {
            return;
        }

        let config = self.config.read().await;
        if config.npcs.is_empty() {
            return;
        }
        let mut by_world: HashMap<&str, Vec<&NpcEntry>> = HashMap::new();
        for entry in &config.npcs {
            by_world
                .entry(entry.world.as_str())
                .or_default()
                .push(entry);
        }

        let mut runtime = self.runtime.write().await;
        for world in server.worlds.load().iter() {
            let Some(entries) = by_world.get(world.get_world_name()) else {
                continue;
            };
            let players = world.players.load();

            for entry in entries {
                let Some(npc) = runtime.get_mut(&entry.name) else {
                    continue;
                };

                let mut in_range = HashSet::with_capacity(npc.visible_to.len());
                for player in players.iter() {
                    let ClientPlatform::Java(client) = player.client.as_ref() else {
                        continue;
                    };
                    let center = player.get_entity().chunk_pos.load();
                    let view_distance = get_view_distance(player).get() as i32;
                    if !is_within_view_distance(npc.chunk_pos, center, view_distance) {
                        continue;
                    }
                    let uuid = player.gameprofile.id;
                    in_range.insert(uuid);
                    if !npc.visible_to.contains(&uuid) {
                        Self::send_spawn(client, npc, entry);
                    }
                }
                for player in players.iter() {
                    let uuid = player.gameprofile.id;
                    if npc.visible_to.contains(&uuid)
                        && !in_range.contains(&uuid)
                        && let ClientPlatform::Java(client) = player.client.as_ref()
                    {
                        client.try_enqueue_packet(&CRemoveEntities::new(&[VarInt(npc.entity_id)]));
                        client.try_enqueue_packet(&CRemovePlayerInfo::new(&[npc.fake_uuid]));
                    }
                }
                npc.visible_to = in_range;
            }
        }
    }

    fn send_spawn(client: &crate::net::java::JavaClient, npc: &RuntimeNpc, entry: &NpcEntry) {
        let properties: Vec<_> = entry.skin.clone().into_iter().collect();
        client.try_enqueue_packet(&CPlayerInfoUpdate::new(
            (PlayerInfoFlags::ADD_PLAYER | PlayerInfoFlags::UPDATE_LISTED).bits(),
            &[InfoPlayer {
                uuid: npc.fake_uuid,
                actions: &[
                    PlayerAction::AddPlayer {
                        name: &entry.name,
                        properties: &properties,
                    },
                    PlayerAction::UpdateListed(false),
                ],
            }],
        ));

        client.try_enqueue_packet(&CSpawnEntity::new(
            VarInt(npc.entity_id),
            npc.fake_uuid,
            VarInt(i32::from(EntityType::PLAYER.id)),
            Vector3::new(entry.x, entry.y, entry.z),
            entry.pitch,
            entry.yaw,
            entry.yaw,
            VarInt(0),
            Vector3::new(0.0, 0.0, 0.0),
        ));

        let mut buf = Vec::new();
        if Metadata::new(
            TrackedData::PLAYER_MODE_CUSTOMISATION,
            MetaDataType::BYTE,
            SKIN_LAYERS_ALL,
        )
        .write(&mut buf, &client.version.load())
        .is_ok()
        {
            buf.push(0xFF);
            client.try_enqueue_packet(&CSetEntityMetadata::new(VarInt(npc.entity_id), buf.into()));
        }
    }
}
// EMBER end
