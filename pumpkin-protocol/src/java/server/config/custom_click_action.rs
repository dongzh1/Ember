use std::io::Read;

use pumpkin_data::packet::serverbound::CONFIG_CUSTOM_CLICK_ACTION;
use pumpkin_macros::java_packet;
use pumpkin_util::resource_location::ResourceLocation;
use pumpkin_util::version::JavaMinecraftVersion;

use crate::{ReadingError, ServerPacket, ser::NetworkReadExt};

// EMBER: see the `play` variant of this same packet for why this needed a
// manual `read` instead of `#[derive(Deserialize)]`.
const MAX_PAYLOAD_SIZE: usize = 1_048_576;

#[java_packet(CONFIG_CUSTOM_CLICK_ACTION)]
pub struct SCustomClickAction {
    pub action_id: ResourceLocation,
    pub payload: Option<Box<[u8]>>,
}

impl ServerPacket for SCustomClickAction {
    fn read(mut read: impl Read, _version: &JavaMinecraftVersion) -> Result<Self, ReadingError> {
        Ok(Self {
            action_id: read.get_str()?.to_string(),
            payload: read.get_option(|v| v.read_remaining_to_boxed_slice(MAX_PAYLOAD_SIZE))?,
        })
    }
}
