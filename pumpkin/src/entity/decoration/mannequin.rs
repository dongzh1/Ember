// EMBER start: mannequin NPC entity
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use arc_swap::ArcSwap;
use pumpkin_data::meta_data_type::MetaDataType;
use pumpkin_data::tracked_data::TrackedData;
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::ResolvableProfile;
use pumpkin_protocol::java::client::play::Metadata;
use pumpkin_util::text::TextComponent;

use crate::entity::{
    Entity, EntityBase, EntityBaseFuture, NBTStorage, NbtFuture, living::LivingEntity,
};

/// A signed set of skin textures — the base64 `value` and its optional Mojang
/// `signature` — used to render a [`MannequinEntity`].
///
/// The value/signature are supplied directly (e.g. from mineskin or a custom
/// skin service); the server never resolves usernames against Mojang.
#[derive(Clone)]
pub struct SkinTextures {
    pub value: Box<str>,
    pub signature: Option<Box<str>>,
}

/// A `minecraft:mannequin` — a player-shaped display entity that renders any
/// skin without a connected player.
///
/// Added in MC 1.21.9 and carried through the 26.x line, it is the native basis
/// for skinned NPCs.
///
/// It behaves like a stationary [`LivingEntity`] (inheriting movement, effects
/// and death), and adds the mannequin-specific tracked data: the skin profile
/// (index 17), the below-name description (index 19) and the immovable flag
/// (index 18). Those are pushed to viewers on spawn and re-sent live on change.
pub struct MannequinEntity {
    living_entity: LivingEntity,
    /// Skin textures to render. `None` renders the default skin.
    skin: ArcSwap<Option<SkinTextures>>,
    /// Text shown on the below-name line. `None` keeps the default "NPC" label.
    description: ArcSwap<Option<TextComponent>>,
    /// Whether the mannequin resists being pushed.
    immovable: AtomicBool,
}

impl MannequinEntity {
    pub fn new(entity: Entity) -> Self {
        Self {
            living_entity: LivingEntity::new(entity),
            skin: ArcSwap::from_pointee(None),
            description: ArcSwap::from_pointee(None),
            immovable: AtomicBool::new(false),
        }
    }

    /// Builds the wire profile from the currently stored skin.
    fn profile(&self) -> ResolvableProfile {
        let guard = self.skin.load();
        (**guard)
            .as_ref()
            .map_or_else(ResolvableProfile::empty, |skin| {
                ResolvableProfile::from_textures(skin.value.clone(), skin.signature.clone())
            })
    }

    /// Sets the rendered skin and broadcasts it to viewers immediately.
    pub fn set_skin(&self, textures: Option<SkinTextures>) {
        self.skin.store(Arc::new(textures));
        self.get_entity().send_meta_data(&[Metadata::new(
            TrackedData::PROFILE,
            MetaDataType::RESOLVABLE_PROFILE,
            self.profile(),
        )]);
    }

    /// Sets the below-name description text and broadcasts it immediately.
    pub fn set_description(&self, description: Option<TextComponent>) {
        self.description.store(Arc::new(description.clone()));
        self.get_entity().send_meta_data(&[Metadata::new(
            TrackedData::DESCRIPTION,
            MetaDataType::OPTIONAL_TEXT_COMPONENT,
            description,
        )]);
    }

    /// Sets whether the mannequin resists being pushed and broadcasts it.
    pub fn set_immovable(&self, immovable: bool) {
        self.immovable.store(immovable, Ordering::Relaxed);
        self.get_entity().send_meta_data(&[Metadata::new(
            TrackedData::IMMOVABLE,
            MetaDataType::BOOLEAN,
            immovable,
        )]);
    }

    pub fn is_immovable(&self) -> bool {
        self.immovable.load(Ordering::Relaxed)
    }
}

impl NBTStorage for MannequinEntity {
    fn write_nbt<'a>(&'a self, nbt: &'a mut NbtCompound) -> NbtFuture<'a, ()> {
        Box::pin(async {
            self.living_entity.write_nbt(nbt).await;
            nbt.put_bool("Immovable", self.is_immovable());
            if let Some(skin) = &**self.skin.load() {
                nbt.put_string("EmberSkinValue", skin.value.to_string());
                if let Some(signature) = &skin.signature {
                    nbt.put_string("EmberSkinSignature", signature.to_string());
                }
            }
        })
    }

    fn read_nbt_non_mut<'a>(&'a self, nbt: &'a NbtCompound) -> NbtFuture<'a, ()> {
        Box::pin(async {
            self.living_entity.read_nbt_non_mut(nbt).await;
            if let Some(immovable) = nbt.get_bool("Immovable") {
                self.immovable.store(immovable, Ordering::Relaxed);
            }
            if let Some(value) = nbt.get_string("EmberSkinValue") {
                let signature = nbt.get_string("EmberSkinSignature").map(Box::from);
                self.skin.store(Arc::new(Some(SkinTextures {
                    value: Box::from(value),
                    signature,
                })));
            }
        })
    }
}

impl EntityBase for MannequinEntity {
    fn get_entity(&self) -> &Entity {
        &self.living_entity.entity
    }

    fn get_living_entity(&self) -> Option<&LivingEntity> {
        Some(&self.living_entity)
    }

    fn as_nbt_storage(&self) -> &dyn NBTStorage {
        self
    }

    /// Pushes the mannequin's skin, immovable flag and description to viewers on
    /// spawn. Replaces the default (baby-flag) tracker init, which mannequins do
    /// not use.
    fn init_data_tracker(&self) -> EntityBaseFuture<'_, ()> {
        Box::pin(async move {
            let entity = self.get_entity();
            entity.send_meta_data(&[Metadata::new(
                TrackedData::IMMOVABLE,
                MetaDataType::BOOLEAN,
                self.is_immovable(),
            )]);
            entity.send_meta_data(&[Metadata::new(
                TrackedData::PROFILE,
                MetaDataType::RESOLVABLE_PROFILE,
                self.profile(),
            )]);
            entity.send_meta_data(&[Metadata::new(
                TrackedData::DESCRIPTION,
                MetaDataType::OPTIONAL_TEXT_COMPONENT,
                (**self.description.load()).clone(),
            )]);
        })
    }

    fn get_gravity(&self) -> f64 {
        0.08
    }

    fn cast_any(&self) -> &dyn std::any::Any {
        self
    }
}
// EMBER end
