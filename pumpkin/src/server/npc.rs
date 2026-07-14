// EMBER start: packet-only NPC manager
//
// An NPC here is never a real world entity: no `Entity`, no NBT, no save
// footprint. Each one is a `data::npc::NpcEntry` (persisted in `npc/npcs.json`)
// plus a runtime-only `RuntimeNpc` (fake UUID + reserved entity id + the set
// of players it is currently spawned for). Visibility is driven purely by
// packets, re-evaluated on an interval from `Server::tick_worlds` using the
// exact same chunk/view-distance rule real entities use
// (`world::chunker::is_within_view_distance`), so an NPC pops in/out at the
// same boundary a real entity would, without ever being in `world.entities`.
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use pumpkin_data::Block;
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::meta_data_type::MetaDataType;
use pumpkin_data::tracked_data::TrackedData;
use pumpkin_protocol::ResolvableProfile;
use pumpkin_protocol::codec::item_stack_seralizer::ItemStackSerializer;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_protocol::java::client::play::{
    Animation, CEntityAnimation, CEntityPositionSync, CHeadRot, CPlayerInfoUpdate, CRemoveEntities,
    CRemovePlayerInfo, CSetEntityMetadata, CSpawnEntity, CUpdateEntityRot, Metadata,
    Player as InfoPlayer, PlayerAction, PlayerInfoFlags,
};
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector2::{Vector2, to_chunk_pos};
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::text::TextComponent;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::data::npc::{NpcConfig, NpcEntry};
use crate::entity::player::Player;
use crate::entity::{Entity, EntityBase};
use crate::net::ClientPlatform;
use crate::server::Server;
use crate::world::chunker::{get_view_distance, is_within_view_distance};

/// Re-evaluate NPC visibility every 10 ticks (0.5s at 20 tps) rather than
/// every tick — an admin-placed standing NPC popping in/out half a second
/// late is imperceptible, and this keeps the per-tick cost near zero.
const VISIBILITY_INTERVAL_TICKS: i32 = 10;

// EMBER start - look-at-nearest-player
/// Re-evaluate `look_at_nearest_player` every 4 ticks (5/s) — noticeably
/// smoother than the visibility interval without re-scanning every tick.
const LOOK_INTERVAL_TICKS: i32 = 4;
// EMBER end

// EMBER start - NPC movement (moveto/wander)
/// Advance active movement every 2 ticks (10/s) — smooth enough for a
/// walking pace without moving every single tick.
const MOVEMENT_INTERVAL_TICKS: i32 = 2;
/// Blocks moved per `MOVEMENT_INTERVAL_TICKS` call (~3 blocks/s), a fixed
/// walking pace — not currently configurable per NPC.
const MOVE_STEP_BLOCKS: f64 = 0.3;
/// How long a wandering NPC waits at each stop before picking a new target.
const WANDER_PAUSE_MIN_TICKS: i32 = 40;
const WANDER_PAUSE_MAX_TICKS: i32 = 120;
// EMBER end

// EMBER start - NPC gravity
/// Blocks fallen per `MOVEMENT_INTERVAL_TICKS` call while airborne (~4
/// blocks/s) — a fixed rate, not real accelerating gravity, close enough to
/// vanilla's ~3.92 blocks/s terminal velocity for a "don't float" correction.
const FALL_STEP_BLOCKS: f64 = 0.4;
// EMBER end

// EMBER start - NPC escort (guide)
/// Follow mode: how close behind the escorted player to stay.
const ESCORT_FOLLOW_DISTANCE: f64 = 3.0;
/// Lead mode: pause and wait once the player falls this far behind.
const ESCORT_WAIT_DISTANCE: f64 = 6.0;
/// Either mode: teleport-catch-up once the player is this far away (a
/// different world's worth of distance away, or through a portal) rather
/// than walking the whole way — this system has no collision or pathfinding,
/// so a long walk back could clip through terrain anyway.
const ESCORT_CATCHUP_TELEPORT_DISTANCE: f64 = 24.0;
// EMBER end

/// All bits set: cape/jacket/sleeves/pants-legs/hat all rendered. A real
/// player's byte here mirrors their client-side settings; a fake NPC has no
/// such source, so it hardcodes "show everything".
const SKIN_LAYERS_ALL: u8 = 0x7F;

/// Sneaking bit in the base-entity shared-flags byte (see `Entity::set_flag`,
/// `Flag::Sneaking as u8 == 1`).
const SNEAKING_BIT: i8 = 0x02;

// EMBER start - packet-only NPCs generalized to any entity type
/// Resolves an [`NpcEntry`]'s stored resource name to a real [`EntityType`].
///
/// Falls back to `PLAYER` for a name that no longer resolves (a hand-edited
/// `npcs.json`, or a name a future data-gen no longer has). Case-insensitive:
/// `EntityType::from_name` itself is not.
#[must_use]
pub fn resolve_entity_type(entry: &NpcEntry) -> &'static EntityType {
    EntityType::from_name(&entry.entity_type.to_lowercase()).unwrap_or(&EntityType::PLAYER)
}

/// Whether an NPC needs `PLAYER`-specific handling — a Mojang skin resolved
/// through the tab list rather than plain metadata.
fn is_player_kind(entity_type: &EntityType) -> bool {
    entity_type == &EntityType::PLAYER
}

/// Whether an NPC's entity type has any concept of a settable skin at all.
///
/// Only `player` (tab-list texture) and `mannequin` (`PROFILE` metadata) do.
/// Shared by `NpcManager::set_skin` and the `/npc create ... as <type>
/// <player>` command validation so the two can never drift apart.
#[must_use]
pub fn supports_skin(entity_type: &EntityType) -> bool {
    is_player_kind(entity_type) || entity_type == &EntityType::MANNEQUIN
}

/// Builds the `PROFILE` metadata value from an NPC's stored skin, the same
/// conversion `MannequinEntity::profile` uses for the real entity.
fn profile_from_skin(skin: Option<&pumpkin_protocol::Property>) -> ResolvableProfile {
    skin.map_or_else(ResolvableProfile::empty, |prop| {
        ResolvableProfile::from_textures(prop.value.clone(), prop.signature.clone())
    })
}

// EMBER start - look-at-nearest-player
/// Yaw/pitch (in degrees) to face `to` from `from`, the same formula
/// `Entity::look_at` uses (no eye-height offset, matching that precedent).
fn yaw_pitch_towards(from: Vector3<f64>, to: Vector3<f64>) -> (f32, f32) {
    let delta = to.sub(&from);
    let root = delta.x.hypot(delta.z);
    let pitch = pumpkin_util::math::wrap_degrees((-delta.y.atan2(root) as f32).to_degrees());
    let yaw = pumpkin_util::math::wrap_degrees((delta.z.atan2(delta.x) as f32).to_degrees() - 90.0);
    (yaw, pitch)
}

/// Degrees to the packet's 1/256-of-a-turn byte encoding, the same
/// conversion `net/java/play.rs`'s rotation handling uses.
fn angle_to_byte(degrees: f32) -> u8 {
    (degrees * 256.0 / 360.0).rem_euclid(256.0) as u8
}
// EMBER end

/// Sends the despawn packets for one NPC to one client. `is_player` gates
/// `CRemovePlayerInfo` — every other entity type never entered the tab list
/// in the first place, so removing it there would be a meaningless no-op at
/// best and is simply skipped.
fn send_despawn_packets(
    client: &crate::net::java::JavaClient,
    entity_id: i32,
    fake_uuid: Uuid,
    is_player: bool,
) {
    client.try_enqueue_packet(&CRemoveEntities::new(&[VarInt(entity_id)]));
    if is_player {
        client.try_enqueue_packet(&CRemovePlayerInfo::new(&[fake_uuid]));
    }
}
// EMBER end

struct RuntimeNpc {
    fake_uuid: Uuid,
    entity_id: i32,
    chunk_pos: Vector2<i32>,
    visible_to: HashSet<Uuid>,
    /// Last head yaw byte sent to viewers, so `look_at_nearest_player`
    /// doesn't spam an identical `CHeadRot` every interval.
    last_sent_head_yaw: Option<u8>,
    // EMBER start - NPC movement (moveto/wander)
    /// Live position, distinct from `NpcEntry.x/y/z` (that stays the "home"
    /// point) while walking. Reset to the entry's stored position whenever
    /// the runtime state is (re)built, so a restart mid-walk just resumes
    /// from the last-saved home point rather than the exact mid-stride spot.
    position: Vector3<f64>,
    yaw: f32,
    /// Active walk target, if any. `None` means stationary (whether or not
    /// wander is enabled — see `wander`'s own pause bookkeeping).
    goal: Option<MoveGoal>,
    /// Wander behavior state — present iff `NpcEntry.wander_radius` is
    /// `Some`, independent of whether a `goal` is currently in progress.
    wander: Option<WanderState>,
    // EMBER end
    // EMBER start - NPC escort (guide)
    /// Active escort, if any. Runtime-only, like `goal`: tied to a currently
    /// online player, so there's nothing sensible to resume across a
    /// restart. Takes priority over `goal`/`wander` while present; wander
    /// resumes on its own once escort ends (its config was never touched).
    escort: Option<EscortState>,
    // EMBER end
}

// EMBER start - NPC movement (moveto/wander)
struct MoveGoal {
    target: Vector3<f64>,
    /// Whether reaching the target should re-arm wandering (vs. a one-shot
    /// `/npc moveto` that just stops).
    is_wander_leg: bool,
}

struct WanderState {
    center: Vector3<f64>,
    radius: f64,
    /// Don't pick a new target until `Server::tick_count` reaches this.
    pause_until_tick: i32,
}
// EMBER end

// EMBER start - NPC escort (guide)
struct EscortState {
    target: Uuid,
    /// `None` = follow mode (indefinite). `Some` = lead mode: walk to this
    /// point, pausing if the player falls behind, ending on arrival.
    destination: Option<Vector3<f64>>,
    /// Lead mode only: true while paused, waiting for the player to catch up.
    waiting: bool,
}
// EMBER end

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
            last_sent_head_yaw: None,
            position: Vector3::new(entry.x, entry.y, entry.z),
            yaw: entry.yaw,
            goal: None,
            wander: entry.wander_radius.map(|radius| WanderState {
                center: Vector3::new(entry.x, entry.y, entry.z),
                radius,
                pause_until_tick: 0,
            }),
            escort: None,
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
                is_player_kind(resolve_entity_type(&removed)),
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
        if !supports_skin(resolve_entity_type(entry)) {
            return Err(format!(
                "NPC '{name}' is a '{}' — that entity type doesn't support skins.",
                entry.entity_type
            ));
        }
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

    // EMBER start - basic NPC actions (look-at, sneak, swing)
    /// Toggles continuously facing the nearest visible viewer. Picked up by
    /// the next `tick()` on its own interval — no respawn needed, unlike
    /// `sneaking`, since nothing about the initial spawn packet changes.
    pub async fn set_look_at_nearest_player(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<(), String> {
        let mut config = self.config.write().await;
        let Some(entry) = config
            .npcs
            .iter_mut()
            .find(|n| n.name.eq_ignore_ascii_case(name))
        else {
            return Err(format!("No NPC named '{name}' exists."));
        };
        entry.look_at_nearest_player = enabled;
        config.save();
        Ok(())
    }

    // EMBER start - NPC gravity
    /// Toggles falling when airborne. Picked up by the next `tick()`, like
    /// `look_at_nearest_player` — no respawn needed, since nothing about the
    /// spawn packet itself changes.
    pub async fn set_gravity(&self, name: &str, enabled: bool) -> Result<(), String> {
        let mut config = self.config.write().await;
        let Some(entry) = config
            .npcs
            .iter_mut()
            .find(|n| n.name.eq_ignore_ascii_case(name))
        else {
            return Err(format!("No NPC named '{name}' exists."));
        };
        entry.gravity = enabled;
        config.save();
        Ok(())
    }
    // EMBER end

    /// Sets the crouch pose. Forces a respawn (like `set_skin`) since the
    /// pose is part of the metadata sent once at spawn time, not re-sent
    /// per-tick.
    pub async fn set_sneaking(
        &self,
        server: &Arc<Server>,
        name: &str,
        sneaking: bool,
    ) -> Result<(), String> {
        let mut config = self.config.write().await;
        let Some(entry) = config
            .npcs
            .iter_mut()
            .find(|n| n.name.eq_ignore_ascii_case(name))
        else {
            return Err(format!("No NPC named '{name}' exists."));
        };
        entry.sneaking = sneaking;
        let entry = entry.clone();
        config.save();
        drop(config);

        self.reset_runtime_and_despawn(server, &entry).await;
        Ok(())
    }

    /// Plays the swing-main-arm animation for current viewers. One-shot —
    /// nothing persisted, unlike the other actions here.
    pub async fn swing_arm(&self, server: &Arc<Server>, name: &str) -> Result<(), String> {
        let config = self.config.read().await;
        let Some(entry) = config.find(name) else {
            return Err(format!("No NPC named '{name}' exists."));
        };
        let (world_name, canonical_name) = (entry.world.clone(), entry.name.clone());
        drop(config);

        let runtime = self.runtime.read().await;
        let Some(npc) = runtime.get(&canonical_name) else {
            return Ok(());
        };
        let Some(world) = server
            .worlds
            .load()
            .iter()
            .find(|w| w.get_world_name() == world_name)
            .cloned()
        else {
            return Ok(());
        };
        for player in world.players.load().iter() {
            if npc.visible_to.contains(&player.gameprofile.id)
                && let ClientPlatform::Java(client) = player.client.as_ref()
            {
                client.try_enqueue_packet(&CEntityAnimation::new(
                    VarInt(npc.entity_id),
                    Animation::SwingMainArm,
                ));
            }
        }
        Ok(())
    }
    // EMBER end

    // EMBER start - NPC movement (moveto/wander)
    /// One-shot: walks to `target` at the fixed pace, overriding any wander
    /// leg in progress (wandering resumes on its own once this arrives).
    /// Runtime-only — a restart loses an in-progress `walk_to`, same as
    /// every other one-shot action here.
    pub async fn walk_to(&self, name: &str, target: Vector3<f64>) -> Result<(), String> {
        let config = self.config.read().await;
        let Some(entry) = config.find(name) else {
            return Err(format!("No NPC named '{name}' exists."));
        };
        let canonical_name = entry.name.clone();
        drop(config);

        let mut runtime = self.runtime.write().await;
        if let Some(npc) = runtime.get_mut(&canonical_name) {
            npc.goal = Some(MoveGoal {
                target,
                is_wander_leg: false,
            });
        }
        Ok(())
    }

    /// Enables/disables random wandering within `radius` blocks of the NPC's
    /// home point (`x`/`y`/`z`). Forces a respawn (like `set_sneaking`) so
    /// wandering state resets cleanly from the persisted config.
    pub async fn set_wander_radius(
        &self,
        server: &Arc<Server>,
        name: &str,
        radius: Option<f64>,
    ) -> Result<(), String> {
        let mut config = self.config.write().await;
        let Some(entry) = config
            .npcs
            .iter_mut()
            .find(|n| n.name.eq_ignore_ascii_case(name))
        else {
            return Err(format!("No NPC named '{name}' exists."));
        };
        entry.wander_radius = radius;
        let entry = entry.clone();
        config.save();
        drop(config);

        self.reset_runtime_and_despawn(server, &entry).await;
        Ok(())
    }
    // EMBER end

    // EMBER start - NPC escort (guide)
    /// Starts escorting `target`: follows indefinitely if `destination` is
    /// `None`, otherwise leads them there (pausing if they fall behind,
    /// ending automatically on arrival). Overrides any `moveto`/wander leg
    /// in progress — wander itself (if configured) resumes once escort ends.
    pub async fn escort(
        &self,
        name: &str,
        target: Uuid,
        destination: Option<Vector3<f64>>,
    ) -> Result<(), String> {
        let config = self.config.read().await;
        let Some(entry) = config.find(name) else {
            return Err(format!("No NPC named '{name}' exists."));
        };
        let canonical_name = entry.name.clone();
        drop(config);

        let mut runtime = self.runtime.write().await;
        if let Some(npc) = runtime.get_mut(&canonical_name) {
            npc.goal = None;
            npc.escort = Some(EscortState {
                target,
                destination,
                waiting: false,
            });
        }
        Ok(())
    }

    /// Stops escorting. A no-op (not an error) if the NPC wasn't escorting
    /// anyone.
    pub async fn stop_escort(&self, name: &str) -> Result<(), String> {
        let config = self.config.read().await;
        let Some(entry) = config.find(name) else {
            return Err(format!("No NPC named '{name}' exists."));
        };
        let canonical_name = entry.name.clone();
        drop(config);

        let mut runtime = self.runtime.write().await;
        if let Some(npc) = runtime.get_mut(&canonical_name) {
            npc.escort = None;
        }
        Ok(())
    }
    // EMBER end

    // EMBER start - per-player visibility control
    /// Hides an NPC from a specific player, regardless of distance,
    /// persisted across restarts.
    pub async fn hide_from(
        &self,
        server: &Arc<Server>,
        name: &str,
        player: Uuid,
    ) -> Result<(), String> {
        let mut config = self.config.write().await;
        let Some(entry) = config
            .npcs
            .iter_mut()
            .find(|n| n.name.eq_ignore_ascii_case(name))
        else {
            return Err(format!("No NPC named '{name}' exists."));
        };
        entry.hidden_from.insert(player);
        let entry = entry.clone();
        config.save();
        drop(config);

        self.reset_runtime_and_despawn(server, &entry).await;
        Ok(())
    }

    /// Undoes `hide_from` — the player is visible again once back in range.
    pub async fn show_to(
        &self,
        server: &Arc<Server>,
        name: &str,
        player: Uuid,
    ) -> Result<(), String> {
        let mut config = self.config.write().await;
        let Some(entry) = config
            .npcs
            .iter_mut()
            .find(|n| n.name.eq_ignore_ascii_case(name))
        else {
            return Err(format!("No NPC named '{name}' exists."));
        };
        entry.hidden_from.remove(&player);
        let entry = entry.clone();
        config.save();
        drop(config);

        self.reset_runtime_and_despawn(server, &entry).await;
        Ok(())
    }

    /// Overrides the view distance viewers need this NPC to appear within.
    /// `None` reverts to each viewer's own client view distance.
    pub async fn set_visible_distance(
        &self,
        server: &Arc<Server>,
        name: &str,
        blocks: Option<f64>,
    ) -> Result<(), String> {
        let mut config = self.config.write().await;
        let Some(entry) = config
            .npcs
            .iter_mut()
            .find(|n| n.name.eq_ignore_ascii_case(name))
        else {
            return Err(format!("No NPC named '{name}' exists."));
        };
        entry.visible_distance = blocks;
        let entry = entry.clone();
        config.save();
        drop(config);

        self.reset_runtime_and_despawn(server, &entry).await;
        Ok(())
    }
    // EMBER end

    /// Given an entity id from an interact packet the world doesn't
    /// recognize, returns the NPC's name and configured click command (if
    /// any) when it belongs to one of our NPCs. `None` means "not one of
    /// ours" — the caller should fall through to its normal unknown-entity
    /// handling.
    pub async fn click_command(&self, entity_id: i32) -> Option<(String, Option<String>)> {
        let runtime = self.runtime.read().await;
        let name = runtime
            .iter()
            .find(|(_, state)| state.entity_id == entity_id)
            .map(|(name, _)| name.clone())?;
        drop(runtime);
        let config = self.config.read().await;
        let command = config.find(&name).and_then(|e| e.click_command.clone());
        Some((name, command))
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
                is_player_kind(resolve_entity_type(entry)),
            );
        }
    }

    fn despawn_from_viewers(
        server: &Arc<Server>,
        world_name: &str,
        entity_id: i32,
        fake_uuid: Uuid,
        viewers: &HashSet<Uuid>,
        is_player: bool,
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
                send_despawn_packets(client, entity_id, fake_uuid, is_player);
            }
        }
    }

    /// Re-evaluates visibility for every NPC against every connected player,
    /// spawning/despawning per-viewer as they cross the view-distance
    /// boundary, and (on its own faster interval) turns `look_at_nearest_player`
    /// NPCs to face their nearest viewer. Called once per game tick from
    /// `Server::tick_worlds`; the interval checks keep the real work at a
    /// fraction of that rate.
    pub async fn tick(&self, server: &Arc<Server>) {
        let tick_count = server.tick_count.load(Ordering::Relaxed);
        let do_visibility = tick_count % VISIBILITY_INTERVAL_TICKS == 0;
        let do_look = tick_count % LOOK_INTERVAL_TICKS == 0;
        let do_movement = tick_count % MOVEMENT_INTERVAL_TICKS == 0;
        if !do_visibility && !do_look && !do_movement {
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
                let is_player = is_player_kind(resolve_entity_type(entry));

                if do_visibility {
                    let mut in_range = HashSet::with_capacity(npc.visible_to.len());
                    for player in players.iter() {
                        let ClientPlatform::Java(client) = player.client.as_ref() else {
                            continue;
                        };
                        let uuid = player.gameprofile.id;
                        if entry.hidden_from.contains(&uuid) {
                            continue;
                        }
                        let center = player.get_entity().chunk_pos.load();
                        // EMBER: a per-NPC override replaces the viewer's own
                        // client view distance entirely (not a min/max with
                        // it) — an operator who sets this wants that exact
                        // distance, not whichever of the two is smaller.
                        let view_distance = entry.visible_distance.map_or_else(
                            || get_view_distance(player).get() as i32,
                            |blocks| (blocks / 16.0).ceil() as i32,
                        );
                        if !is_within_view_distance(npc.chunk_pos, center, view_distance) {
                            continue;
                        }
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
                            send_despawn_packets(client, npc.entity_id, npc.fake_uuid, is_player);
                        }
                    }
                    npc.visible_to = in_range;
                }

                if do_look && entry.look_at_nearest_player {
                    Self::update_look_at(npc, &players);
                }

                if do_movement {
                    if npc.escort.is_some() {
                        Self::update_escort(npc, &players);
                    } else if npc.goal.is_some() || npc.wander.is_some() {
                        Self::move_npc(npc, tick_count, &players);
                    } else if entry.gravity {
                        Self::apply_gravity(npc, world, &players);
                    }
                }
            }
        }
    }

    // EMBER start - NPC movement (moveto/wander)
    /// Advances one movement step: starts a new wander leg if idle and past
    /// its pause, then steps toward the active goal, re-arming the wander
    /// pause once a wander leg lands.
    fn move_npc(npc: &mut RuntimeNpc, tick_count: i32, players: &[Arc<Player>]) {
        if npc.goal.is_none()
            && let Some(wander) = &npc.wander
            && tick_count >= wander.pause_until_tick
        {
            let center = wander.center;
            let radius = wander.radius;
            let target = Vector3::new(
                center.x + rand::random_range(-radius..radius),
                center.y,
                center.z + rand::random_range(-radius..radius),
            );
            npc.goal = Some(MoveGoal {
                target,
                is_wander_leg: true,
            });
        }

        let Some(goal) = &npc.goal else {
            return;
        };
        let target = goal.target;
        let is_wander_leg = goal.is_wander_leg;

        if Self::step_towards(npc, target, players) {
            npc.goal = None;
            if is_wander_leg && let Some(wander) = &mut npc.wander {
                wander.pause_until_tick =
                    tick_count + rand::random_range(WANDER_PAUSE_MIN_TICKS..WANDER_PAUSE_MAX_TICKS);
            }
        }
    }

    /// Moves `npc.position` `MOVE_STEP_BLOCKS` toward `target` (snapping to
    /// it once within one step), updating yaw/`chunk_pos` and broadcasting
    /// the new position to current viewers via `CEntityPositionSync`. Shared
    /// by `move_npc` (moveto/wander) and `update_escort`. Returns `true` once
    /// the step lands exactly on `target`.
    fn step_towards(npc: &mut RuntimeNpc, target: Vector3<f64>, players: &[Arc<Player>]) -> bool {
        let old_position = npc.position;
        let delta = target.sub(&old_position);
        let distance = delta.length();
        let arrived = distance <= MOVE_STEP_BLOCKS;

        if arrived {
            npc.position = target;
        } else {
            let (yaw, _pitch) = yaw_pitch_towards(old_position, target);
            npc.yaw = yaw;
            npc.position = Vector3::new(
                old_position.x + delta.x / distance * MOVE_STEP_BLOCKS,
                old_position.y + delta.y / distance * MOVE_STEP_BLOCKS,
                old_position.z + delta.z / distance * MOVE_STEP_BLOCKS,
            );
        }

        npc.chunk_pos = to_chunk_pos(&Vector2::new(
            npc.position.x.floor() as i32,
            npc.position.z.floor() as i32,
        ));

        let velocity = npc.position.sub(&old_position);
        let head_yaw = angle_to_byte(npc.yaw);
        for player in players {
            if npc.visible_to.contains(&player.gameprofile.id)
                && let ClientPlatform::Java(client) = player.client.as_ref()
            {
                client.try_enqueue_packet(&CEntityPositionSync::new(
                    VarInt(npc.entity_id),
                    npc.position,
                    velocity,
                    npc.yaw,
                    0.0,
                    true,
                ));
                client.try_enqueue_packet(&CHeadRot::new(VarInt(npc.entity_id), head_yaw));
            }
        }
        arrived
    }
    // EMBER end

    // EMBER start - NPC escort (guide)
    /// Advances one escort step: follows or leads `escort.target`,
    /// teleport-catching-up if they've fallen far behind, pausing in lead
    /// mode if they've fallen moderately behind, and clearing escort
    /// entirely once the target is no longer in this world (offline, or
    /// somewhere else) or (lead mode) the destination is reached.
    fn update_escort(npc: &mut RuntimeNpc, players: &[Arc<Player>]) {
        let Some(escort) = &npc.escort else {
            return;
        };
        let target_uuid = escort.target;
        let destination = escort.destination;

        let Some(target_player) = players.iter().find(|p| p.gameprofile.id == target_uuid) else {
            npc.escort = None;
            return;
        };
        let player_pos = target_player.get_entity().pos.load();
        let dist_to_player = player_pos.sub(&npc.position).length();

        if dist_to_player > ESCORT_CATCHUP_TELEPORT_DISTANCE {
            npc.position = player_pos;
            npc.chunk_pos = to_chunk_pos(&Vector2::new(
                npc.position.x.floor() as i32,
                npc.position.z.floor() as i32,
            ));
            let head_yaw = angle_to_byte(npc.yaw);
            for player in players {
                if npc.visible_to.contains(&player.gameprofile.id)
                    && let ClientPlatform::Java(client) = player.client.as_ref()
                {
                    client.try_enqueue_packet(&CEntityPositionSync::new(
                        VarInt(npc.entity_id),
                        npc.position,
                        Vector3::new(0.0, 0.0, 0.0),
                        npc.yaw,
                        0.0,
                        true,
                    ));
                    client.try_enqueue_packet(&CHeadRot::new(VarInt(npc.entity_id), head_yaw));
                }
            }
            return;
        }

        let move_target = if let Some(dest) = destination {
            if dist_to_player > ESCORT_WAIT_DISTANCE {
                if let Some(state) = &mut npc.escort {
                    state.waiting = true;
                }
                return;
            }
            if let Some(state) = &mut npc.escort {
                state.waiting = false;
            }
            dest
        } else {
            if dist_to_player <= ESCORT_FOLLOW_DISTANCE {
                return;
            }
            let away = npc.position.sub(&player_pos);
            let away_len = away.length();
            let (ux, uz) = if away_len > 0.0001 {
                (away.x / away_len, away.z / away_len)
            } else {
                (0.0, 1.0)
            };
            Vector3::new(
                player_pos.x + ux * ESCORT_FOLLOW_DISTANCE,
                player_pos.y,
                player_pos.z + uz * ESCORT_FOLLOW_DISTANCE,
            )
        };

        if Self::step_towards(npc, move_target, players) && destination.is_some() {
            npc.escort = None;
        }
    }
    // EMBER end

    // EMBER start - look-at-nearest-player
    /// Turns `npc` to face its nearest currently-visible viewer, broadcasting
    /// `CHeadRot` to every viewer — one shared head orientation, not a
    /// separate one per viewer (a head-yaw byte can't express "face whoever's
    /// looking at you" individually per client anyway).
    fn update_look_at(npc: &mut RuntimeNpc, players: &[Arc<Player>]) {
        if npc.visible_to.is_empty() {
            return;
        }
        let npc_pos = npc.position;
        let Some(nearest) = players
            .iter()
            .filter(|p| npc.visible_to.contains(&p.gameprofile.id))
            .min_by(|a, b| {
                let da = a.get_entity().pos.load().sub(&npc_pos).length_squared();
                let db = b.get_entity().pos.load().sub(&npc_pos).length_squared();
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
        else {
            return;
        };

        let (yaw, _pitch) = yaw_pitch_towards(npc_pos, nearest.get_entity().pos.load());
        let head_yaw = angle_to_byte(yaw);
        if npc.last_sent_head_yaw == Some(head_yaw) {
            return;
        }
        npc.last_sent_head_yaw = Some(head_yaw);
        // EMBER: keep the body yaw in sync with the head, or a stationary
        // look-at only ever swivels the head while the body stays frozen at
        // its last movement/spawn orientation - the same gap `step_towards`/
        // `update_escort` don't have, since they already turn the whole body.
        npc.yaw = yaw;

        for player in players {
            if npc.visible_to.contains(&player.gameprofile.id)
                && let ClientPlatform::Java(client) = player.client.as_ref()
            {
                client.try_enqueue_packet(&CHeadRot::new(VarInt(npc.entity_id), head_yaw));
                client.try_enqueue_packet(&CUpdateEntityRot::new(
                    VarInt(npc.entity_id),
                    head_yaw,
                    0,
                    true,
                ));
            }
        }
    }
    // EMBER end

    // EMBER start - NPC gravity
    /// Simple non-accelerating fall: steps `npc` down by `FALL_STEP_BLOCKS`
    /// while the block at its feet is non-solid, snapping onto the surface
    /// instead of tunneling through it once solid ground is reached. Not
    /// real physics (no acceleration, no terminal-velocity curve) - just
    /// enough that a misplaced or terrain-orphaned NPC doesn't float. Only
    /// runs while idle (no active goal/wander/escort); those already own the
    /// NPC's `y` via their own straight-line movement toward a target.
    fn apply_gravity(
        npc: &mut RuntimeNpc,
        world: &Arc<crate::world::World>,
        players: &[Arc<Player>],
    ) {
        let feet = BlockPos::new(
            npc.position.x.floor() as i32,
            (npc.position.y - 0.01).floor() as i32,
            npc.position.z.floor() as i32,
        );
        if world.get_block_state(&feet).is_solid() {
            return;
        }

        let old_position = npc.position;
        let mut new_y = npc.position.y - FALL_STEP_BLOCKS;
        let mut on_ground = false;
        let below = BlockPos::new(feet.0.x, (new_y - 0.01).floor() as i32, feet.0.z);
        if world.get_block_state(&below).is_solid() {
            new_y = f64::from(below.0.y) + 1.0;
            on_ground = true;
        }
        npc.position = Vector3::new(npc.position.x, new_y, npc.position.z);

        let velocity = npc.position.sub(&old_position);
        for player in players {
            if npc.visible_to.contains(&player.gameprofile.id)
                && let ClientPlatform::Java(client) = player.client.as_ref()
            {
                client.try_enqueue_packet(&CEntityPositionSync::new(
                    VarInt(npc.entity_id),
                    npc.position,
                    velocity,
                    npc.yaw,
                    0.0,
                    on_ground,
                ));
            }
        }
    }
    // EMBER end

    fn send_spawn(client: &crate::net::java::JavaClient, npc: &RuntimeNpc, entry: &NpcEntry) {
        let entity_type = resolve_entity_type(entry);
        let is_player = is_player_kind(entity_type);

        // PLAYER needs tab-list registration before it spawns, or the
        // client has no profile (and thus no skin) to render it with.
        if is_player {
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
        }

        // EMBER: `falling_block`'s appearance is carried in the spawn
        // packet's own `data` field (a block-state id) rather than
        // metadata — every other entity type just leaves it at 0.
        let data = if entity_type == &EntityType::FALLING_BLOCK {
            i32::from(
                entry
                    .block
                    .as_deref()
                    .and_then(|name| Block::from_name(&name.to_lowercase()))
                    .unwrap_or(&Block::SAND)
                    .default_state
                    .id
                    .as_u16(),
            )
        } else {
            0
        };

        client.try_enqueue_packet(&CSpawnEntity::new(
            VarInt(npc.entity_id),
            npc.fake_uuid,
            VarInt(i32::from(entity_type.id)),
            npc.position,
            entry.pitch,
            npc.yaw,
            npc.yaw,
            VarInt(data),
            Vector3::new(0.0, 0.0, 0.0),
        ));

        Self::send_spawn_metadata(client, npc, entry, entity_type, is_player);
    }

    /// The metadata half of [`Self::send_spawn`] — split out purely to keep
    /// each function under clippy's line-count lint; both run unconditionally
    /// as one logical spawn.
    fn send_spawn_metadata(
        client: &crate::net::java::JavaClient,
        npc: &RuntimeNpc,
        entry: &NpcEntry,
        entity_type: &EntityType,
        is_player: bool,
    ) {
        let version = client.version.load();
        let mut buf = Vec::new();

        // EMBER: the sneaking bit lives on every entity's base shared-flags
        // metadata (see `Entity::set_flag`), so it applies regardless of kind.
        let mut ok = Metadata::new(
            TrackedData::SHARED_FLAGS_ID,
            MetaDataType::BYTE,
            if entry.sneaking { SNEAKING_BIT } else { 0 },
        )
        .write(&mut buf, &version)
        .is_ok();

        if is_player {
            ok &= Metadata::new(
                TrackedData::PLAYER_MODE_CUSTOMISATION,
                MetaDataType::BYTE,
                SKIN_LAYERS_ALL,
            )
            .write(&mut buf, &version)
            .is_ok();
            if ok {
                buf.push(0xFF);
                client.try_enqueue_packet(&CSetEntityMetadata::new(
                    VarInt(npc.entity_id),
                    buf.into(),
                ));
            }
            return;
        }

        // Every non-player kind needs its name sent explicitly: a player
        // gets a nametag "for free" from its tab-list username, nothing
        // else does.
        ok &= Metadata::with_fallback_type(
            TrackedData::CUSTOM_NAME,
            MetaDataType::OPTIONAL_TEXT_COMPONENT,
            MetaDataType::OPTIONAL_COMPONENT, // EMBER: v26.1+ renamed this slot, see Metadata::fallback_type
            Some(TextComponent::text(entry.name.clone())),
        )
        .write(&mut buf, &version)
        .is_ok();
        ok &= Metadata::new(
            TrackedData::CUSTOM_NAME_VISIBLE,
            MetaDataType::BOOLEAN,
            true,
        )
        .write(&mut buf, &version)
        .is_ok();

        if entity_type == &EntityType::MANNEQUIN {
            ok &= Metadata::new(
                TrackedData::PROFILE,
                MetaDataType::RESOLVABLE_PROFILE,
                profile_from_skin(entry.skin.as_ref()),
            )
            .write(&mut buf, &version)
            .is_ok();
            ok &= Metadata::new(TrackedData::IMMOVABLE, MetaDataType::BOOLEAN, true)
                .write(&mut buf, &version)
                .is_ok();
        } else if entity_type == &EntityType::ITEM {
            let item = entry
                .item
                .as_deref()
                .and_then(|name| Item::from_registry_key(&name.to_lowercase()))
                .unwrap_or(&Item::STONE);
            ok &= Metadata::new(
                TrackedData::ITEM,
                MetaDataType::ITEM_STACK,
                &ItemStackSerializer::from(ItemStack::new(1, item)),
            )
            .write(&mut buf, &version)
            .is_ok();
        }

        if ok {
            buf.push(0xFF);
            client.try_enqueue_packet(&CSetEntityMetadata::new(VarInt(npc.entity_id), buf.into()));
        }
    }
}
// EMBER end
