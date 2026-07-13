// EMBER start: floating packet-only menu system
//
// A menu here is never a real world entity, same philosophy as
// `server::npc::NpcManager`: every visual piece (title, button icons,
// button labels, button hitboxes, and the invisible "vehicle" the player
// rides to freeze in place) is spawned purely via packets to one specific
// client. Nothing is added to `world.entities`, nothing is persisted beyond
// the `menu/menus.toml` button/layout configuration itself.
//
// Layout: every piece is placed once, at open time, relative to a fixed
// anchor (the player's eye position, `distance` blocks ahead along their
// *horizontal* facing only - pitch is ignored so looking up/down at open
// time doesn't tilt the menu). This mirrors the mechanic of the reference
// datapack this feature was modeled on (`floatmenu_demo.zip`): compute a
// camera-relative anchor once, then place every other piece with a fixed
// offset from it, rather than continuously re-tracking the camera.
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

use pumpkin_config::{LoadConfiguration, MenuButton, MenuListConfig};
use pumpkin_data::entity::EntityType;
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::meta_data_type::MetaDataType;
use pumpkin_data::tracked_data::{TrackedData, TrackedId};
use pumpkin_protocol::PositionFlag;
use pumpkin_protocol::codec::item_stack_seralizer::ItemStackSerializer;
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_protocol::java::client::play::{
    CPlayerPosition, CRemoveEntities, CSetEntityMetadata, CSetPassengers, CSpawnEntity, Metadata,
};
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::text::TextComponent;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::entity::player::Player;
use crate::entity::{Entity, EntityBase};
use crate::net::ClientPlatform;

/// Old-protocol (pre-26) index for `item_display`'s own item field. Not in
/// the generated `TrackedData` table under this name - the generated
/// `ITEM_STACK` constant (index 8-9) is a different entity's "item" field
/// that happens to share the same mapped name after codegen. Vanilla's
/// `ItemDisplayEntity` extends `Display` (whose own fields run 0-22), so its
/// first added field sits at 23 - exactly where `TextDisplayEntity::TEXT`
/// (a sibling `Display` subclass) sits too. `TrackedData::ITEM_STACK_ID`
/// (26.x) independently resolves to 23 as well, corroborating this.
const ITEM_DISPLAY_ITEM_OLD: TrackedId = TrackedId {
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

/// `Display.BillboardConstraints.CENTER` - always face the camera,
/// regardless of the entity's own rotation. Needed so button icons/labels
/// stay readable as the player looks around while frozen (mounting only
/// blocks movement, not the camera).
const BILLBOARD_CENTER: i8 = 3;

/// Text display style flags: bit0 `has_shadow`, bit1 `is_see_through`
/// (renders through blocks - a HUD shouldn't disappear behind terrain that
/// happens to clip the anchor point), bit2 `use_default_background`.
const TEXT_STYLE_FLAGS: i8 = 0b0000_0111;

/// Vertical gap between a button's icon and its label, in blocks.
const LABEL_DROP: f64 = 0.35;

struct ActiveButton {
    hitbox_id: i32,
    command: String,
}

struct ActiveMenu {
    menu_name: String,
    vehicle_id: i32,
    /// Every entity id spawned for this menu (vehicle + title + each
    /// button's icon/label/hitbox) - despawned together on close.
    entity_ids: Vec<i32>,
    buttons: Vec<ActiveButton>,
    /// Where the player was standing when the menu opened - restored
    /// verbatim on close (they never actually move while mounted).
    origin_pos: Vector3<f64>,
}

pub struct MenuManager {
    config: RwLock<MenuListConfig>,
    active: RwLock<HashMap<Uuid, ActiveMenu>>,
}

impl Default for MenuManager {
    fn default() -> Self {
        Self::new()
    }
}

impl MenuManager {
    #[must_use]
    pub fn new() -> Self {
        let exec_dir = std::env::current_dir().expect("Failed to get current directory");
        Self {
            config: RwLock::new(MenuListConfig::load(&exec_dir)),
            active: RwLock::new(HashMap::new()),
        }
    }

    pub async fn list_names(&self) -> Vec<String> {
        self.config
            .read()
            .await
            .menus
            .iter()
            .map(|m| m.name.clone())
            .collect()
    }

    /// Opens `name` (or the first configured menu if `None`) for `player`,
    /// closing whatever menu they already had open first. Toggle semantics:
    /// re-requesting the menu that's already open just closes it.
    pub async fn open(&self, player: &Arc<Player>, name: Option<&str>) -> Result<(), String> {
        let config = self.config.read().await;
        if config.menus.is_empty() {
            return Err("No menus are configured (menu/menus.toml).".to_string());
        }
        let menu = match name {
            Some(name) => config
                .menus
                .iter()
                .find(|m| m.name.eq_ignore_ascii_case(name))
                .cloned()
                .ok_or_else(|| format!("No menu named '{name}' exists."))?,
            None => config.menus[0].clone(),
        };
        drop(config);

        let uuid = player.gameprofile.id;
        let already_open_same = self
            .active
            .read()
            .await
            .get(&uuid)
            .is_some_and(|m| m.menu_name.eq_ignore_ascii_case(&menu.name));
        self.close(player).await;
        if already_open_same {
            return Ok(());
        }

        let ClientPlatform::Java(client) = player.client.as_ref() else {
            return Err("Floating menus aren't supported on Bedrock clients yet.".to_string());
        };

        let entity = player.get_entity();
        let origin_pos = entity.pos.load();
        let eye_pos = player.eye_position();
        let yaw = entity.yaw.load();
        let forward = Vector3::from_yaw_pitch(yaw, 0.0);
        let right = Vector3::new(-forward.z, 0.0, forward.x);
        let anchor = eye_pos.add(&forward.multiply(menu.distance, menu.distance, menu.distance));

        let base = Entity::reserve_ids(2 + 3 * menu.buttons.len() as i32);
        let vehicle_id = base;
        let title_id = base + 1;
        let mut entity_ids = vec![vehicle_id, title_id];
        let mut buttons = Vec::with_capacity(menu.buttons.len());

        // The vehicle: an invisible, zero-hitbox `item_display` (no item set
        // - a `Display` entity with no content renders nothing) spawned at
        // the player's own current position. Its whole purpose is to be
        // mounted, freezing the player exactly where they already are -
        // NOT at the anchor, which sits out in front of them.
        Self::spawn_bare(client, vehicle_id, &EntityType::ITEM_DISPLAY, origin_pos);

        let title_pos = anchor.add(&Vector3::new(0.0, menu.title_height, 0.0));
        Self::spawn_bare(client, title_id, &EntityType::TEXT_DISPLAY, title_pos);
        Self::send_text_metadata(client, title_id, &menu.title, 1.0);

        for (i, button) in menu.buttons.iter().enumerate() {
            #[expect(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
            let i = i as i32;
            let icon_id = base + 2 + 3 * i;
            let label_id = base + 3 + 3 * i;
            let hitbox_id = base + 4 + 3 * i;
            entity_ids.push(icon_id);
            entity_ids.push(label_id);
            entity_ids.push(hitbox_id);

            let icon_pos = Self::button_pos(anchor, right, forward, button);
            let label_pos = icon_pos.add(&Vector3::new(0.0, -LABEL_DROP, 0.0));
            #[expect(clippy::cast_possible_truncation)]
            let scale = button.scale as f32;

            let item = Item::from_registry_key(&button.item.to_lowercase()).unwrap_or(&Item::STONE);
            Self::spawn_bare(client, icon_id, &EntityType::ITEM_DISPLAY, icon_pos);
            Self::send_item_metadata(client, icon_id, item, scale);

            Self::spawn_bare(client, label_id, &EntityType::TEXT_DISPLAY, label_pos);
            Self::send_text_metadata(client, label_id, &button.label, scale);

            // The clickable hitbox: a bare `interaction` entity, deliberately
            // left at its vanilla default size (~1x1) rather than a custom
            // width/height - the generated protocol data has no reliable
            // per-version index for `interaction`'s own width/height (only
            // the unrelated `Display` family's), and a ~1x1 box is already
            // a reasonable button target.
            Self::spawn_bare(client, hitbox_id, &EntityType::INTERACTION, icon_pos);

            buttons.push(ActiveButton {
                hitbox_id,
                command: button.command.clone(),
            });
        }

        client.try_enqueue_packet(&CSetPassengers::new(
            VarInt(vehicle_id),
            &[VarInt(player.entity_id())],
        ));

        self.active.write().await.insert(
            uuid,
            ActiveMenu {
                menu_name: menu.name,
                vehicle_id,
                entity_ids,
                buttons,
                origin_pos,
            },
        );
        Ok(())
    }

    /// Closes `player`'s currently open menu, if any: unmounts them (with
    /// the same teleport-id gating `Entity::remove_passenger` uses, so a
    /// stale movement packet from the old mounted frame can't be misapplied)
    /// and despawns every entity the menu spawned. Returns `false` if
    /// nothing was open.
    pub async fn close(&self, player: &Arc<Player>) -> bool {
        let Some(active) = self.active.write().await.remove(&player.gameprofile.id) else {
            return false;
        };

        let ClientPlatform::Java(client) = player.client.as_ref() else {
            return true;
        };

        let teleport_id = player.teleport_id_count.fetch_add(1, Relaxed) + 1;
        *player.awaiting_teleport.lock().await = Some((teleport_id.into(), active.origin_pos));
        client
            .enqueue_packet(&CSetPassengers::new(VarInt(active.vehicle_id), &[]))
            .await;
        player.get_entity().set_pos(active.origin_pos);
        client
            .enqueue_packet(&CPlayerPosition::new(
                teleport_id.into(),
                active.origin_pos,
                Vector3::new(0.0, 0.0, 0.0),
                0.0,
                0.0,
                vec![
                    PositionFlag::DeltaX,
                    PositionFlag::DeltaY,
                    PositionFlag::DeltaZ,
                    PositionFlag::YRot,
                    PositionFlag::XRot,
                ],
            ))
            .await;

        let ids: Vec<VarInt> = active.entity_ids.iter().map(|&id| VarInt(id)).collect();
        client.try_enqueue_packet(&CRemoveEntities::new(&ids));
        true
    }

    /// Whether `entity_id` is one of `player`'s own currently open menu's
    /// button hitboxes; if so, the menu name and that button's command.
    /// Scoped to the clicking player's own menu (not a global lookup) so a
    /// forged/guessed entity id from another player can't trigger it.
    pub async fn click_command(
        &self,
        player_uuid: Uuid,
        entity_id: i32,
    ) -> Option<(String, String)> {
        let active = self.active.read().await;
        let menu = active.get(&player_uuid)?;
        let button = menu.buttons.iter().find(|b| b.hitbox_id == entity_id)?;
        Some((menu.menu_name.clone(), button.command.clone()))
    }

    fn button_pos(
        anchor: Vector3<f64>,
        right: Vector3<f64>,
        forward: Vector3<f64>,
        button: &MenuButton,
    ) -> Vector3<f64> {
        anchor
            .add(&right.multiply(
                button.offset_right,
                button.offset_right,
                button.offset_right,
            ))
            .add(&Vector3::new(0.0, button.offset_up, 0.0))
            .add(&forward.multiply(
                button.offset_forward,
                button.offset_forward,
                button.offset_forward,
            ))
    }

    fn spawn_bare(
        client: &crate::net::java::JavaClient,
        entity_id: i32,
        entity_type: &'static EntityType,
        pos: Vector3<f64>,
    ) {
        client.try_enqueue_packet(&CSpawnEntity::new(
            VarInt(entity_id),
            Uuid::new_v4(),
            VarInt(i32::from(entity_type.id)),
            pos,
            0.0,
            0.0,
            0.0,
            VarInt(0),
            Vector3::new(0.0, 0.0, 0.0),
        ));
    }

    fn send_text_metadata(
        client: &crate::net::java::JavaClient,
        entity_id: i32,
        text: &str,
        scale: f32,
    ) {
        let version = client.version.load();
        let mut buf = Vec::new();
        let component = TextComponent::text(text.to_string());
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
        ok &= Metadata::new(
            TrackedData::TEXT,
            MetaDataType::TEXT_COMPONENT,
            component.clone(),
        )
        .write(&mut buf, &version)
        .is_ok();
        ok &= Metadata::new(TrackedData::TEXT_ID, MetaDataType::COMPONENT, component)
            .write(&mut buf, &version)
            .is_ok();
        ok &= Metadata::new(
            TrackedData::TEXT_DISPLAY_FLAGS,
            MetaDataType::BYTE,
            TEXT_STYLE_FLAGS,
        )
        .write(&mut buf, &version)
        .is_ok();
        ok &= Metadata::new(
            TrackedData::STYLE_FLAGS_ID,
            MetaDataType::BYTE,
            TEXT_STYLE_FLAGS,
        )
        .write(&mut buf, &version)
        .is_ok();

        if ok {
            buf.push(0xFF);
            client.try_enqueue_packet(&CSetEntityMetadata::new(VarInt(entity_id), buf.into()));
        }
    }

    fn send_item_metadata(
        client: &crate::net::java::JavaClient,
        entity_id: i32,
        item: &'static Item,
        scale: f32,
    ) {
        let version = client.version.load();
        let mut buf = Vec::new();
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

        let stack = ItemStackSerializer::from(ItemStack::new(1, item));
        ok &= Metadata::new(ITEM_DISPLAY_ITEM_OLD, MetaDataType::ITEM_STACK, &stack)
            .write(&mut buf, &version)
            .is_ok();
        ok &= Metadata::new(TrackedData::ITEM_STACK_ID, MetaDataType::ITEM_STACK, &stack)
            .write(&mut buf, &version)
            .is_ok();

        if ok {
            buf.push(0xFF);
            client.try_enqueue_packet(&CSetEntityMetadata::new(VarInt(entity_id), buf.into()));
        }
    }
}
// EMBER end
