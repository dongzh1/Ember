use std::io::{Cursor, Write};

use pumpkin_data::{
    block_state_remap::remap_block_state_for_version, item_id_remap::remap_item_id_for_version,
    meta_data_type::MetaDataType, packet::clientbound::PLAY_SET_ENTITY_DATA,
    tracked_data::TrackedId,
};
use pumpkin_macros::java_packet;
use pumpkin_util::version::JavaMinecraftVersion;
use serde::Serialize;

use crate::{
    ClientPacket, VarInt,
    ser::{NetworkWriteExt, WritingError, serializer},
};

/// Updates the "Data Tracker" values for an entity.
///
/// Entity Metadata (or `DataWatchers`) controls persistent visual states that
/// don't require a full packet to update, such as whether an entity is on fire,
/// crouching, glowing, or the custom name displayed above its head.
#[java_packet(PLAY_SET_ENTITY_DATA)]
pub struct CSetEntityMetadata {
    /// The Entity ID of the entity whose metadata is being updated.
    pub entity_id: VarInt,
    /// A serialized collection of metadata entries.
    /// Ends with a terminal byte (0xFF).
    pub metadata: Box<[u8]>,
}

impl CSetEntityMetadata {
    #[must_use]
    pub const fn new(entity_id: VarInt, metadata: Box<[u8]>) -> Self {
        Self {
            entity_id,
            metadata,
        }
    }
}

impl ClientPacket for CSetEntityMetadata {
    fn write_packet_data(
        &self,
        write: impl Write,
        _version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        let mut write = write;

        // 1. Entity ID
        write.write_var_int(&self.entity_id)?;

        write
            .write_all(&self.metadata)
            .map_err(WritingError::IoError)
    }
}

pub struct Metadata<T> {
    pub index: TrackedId,
    pub r#type: MetaDataType,
    // EMBER start - versioned type rename support
    /// Tried when `r#type` doesn't exist (id < 0) in the recipient's
    /// protocol version. Some metadata *types themselves* were renamed
    /// across versions rather than just moved to a new id - e.g. the
    /// "optional chat component" slot was `OPTIONAL_TEXT_COMPONENT` (id 6)
    /// through v1.21.11, then became a differently-named `OPTIONAL_COMPONENT`
    /// (also id 6) from v26.1 onward. A single fixed `MetaDataType` constant
    /// can't resolve for both eras, so callers that need to span the rename
    /// pass the other era's constant here via [`Metadata::with_fallback_type`].
    pub fallback_type: Option<MetaDataType>,
    // EMBER end
    pub value: T,
}

impl<T> Metadata<T> {
    pub const fn new(index: TrackedId, r#type: MetaDataType, value: T) -> Self {
        Self {
            index,
            r#type,
            fallback_type: None, // EMBER
            value,
        }
    }

    // EMBER start - versioned type rename support
    pub const fn with_fallback_type(
        index: TrackedId,
        r#type: MetaDataType,
        fallback_type: MetaDataType,
        value: T,
    ) -> Self {
        Self {
            index,
            r#type,
            fallback_type: Some(fallback_type),
            value,
        }
    }
    // EMBER end

    pub fn write<W: std::io::Write>(
        &self,
        mut writer: W,
        version: &pumpkin_util::version::JavaMinecraftVersion,
    ) -> Result<(), WritingError>
    where
        T: Serialize,
    {
        let resolved_index = self.index.get(version);

        if resolved_index == 255 {
            return Ok(());
        }

        let mut remapped_type_id = self.r#type.id(*version);
        if remapped_type_id < 0 {
            // EMBER: the primary type doesn't exist in this protocol version -
            // try the other era's name for the same slot before giving up.
            if let Some(fallback_type) = self.fallback_type {
                remapped_type_id = fallback_type.id(*version);
            }
        }
        if remapped_type_id < 0 {
            // Metadata type does not exist in this protocol version.
            return Ok(());
        }

        writer.write_u8(resolved_index)?;
        writer.write_var_int(&VarInt(remapped_type_id))?;

        if self.r#type == MetaDataType::BLOCK_STATE {
            let mut serialized_value = Vec::new();
            {
                let mut serializer = serializer::Serializer::new(&mut serialized_value);
                self.value
                    .serialize(&mut serializer)
                    .map_err(|e| WritingError::Serde(e.to_string()))?;
            };

            let mut cursor = Cursor::new(serialized_value);
            let decoded_state = VarInt::decode(&mut cursor).map_err(|e| {
                WritingError::Message(format!("Failed to decode block state metadata: {e}"))
            })?;
            let remapped_state = u16::try_from(decoded_state.0).map_or(decoded_state, |state_id| {
                VarInt(i32::from(remap_block_state_for_version(state_id, *version)))
            });
            writer.write_var_int(&remapped_state)?;
            return Ok(());
        }

        if self.r#type == MetaDataType::ITEM_STACK {
            let mut serialized_value = Vec::new();
            {
                let mut serializer = serializer::Serializer::new(&mut serialized_value);
                self.value
                    .serialize(&mut serializer)
                    .map_err(|e| WritingError::Serde(e.to_string()))?;
            };

            let mut cursor = Cursor::new(serialized_value);
            let item_count = VarInt::decode(&mut cursor).map_err(|e| {
                WritingError::Message(format!("Failed to decodeitem stack count: {e}"))
            })?;

            if item_count.0 <= 0 {
                writer.write_var_int(&item_count)?;
            } else {
                let item_id = VarInt::decode(&mut cursor)
                    .map_err(|e| WritingError::Message(format!("Failed to decode item id: {e}")))?;
                let remapped_id = u16::try_from(item_id.0)
                    .map_or(0, |id| remap_item_id_for_version(id, *version));
                writer.write_var_int(&item_count)?;
                writer.write_var_int(&VarInt(i32::from(remapped_id)))?;
                let remainder_start = cursor.position() as usize;
                let inner = cursor.into_inner();
                writer.write_all(&inner[remainder_start..]).map_err(|e| {
                    WritingError::Message(format!("Failed to write item stack remainder: {e}"))
                })?;
            }
            return Ok(());
        }

        let mut serializer = serializer::Serializer::new(&mut writer);
        self.value
            .serialize(&mut serializer)
            .map_err(|e| WritingError::Serde(e.to_string()))?;

        Ok(())
    }
}
