use std::io::Read;

use pumpkin_data::packet::serverbound::PLAY_CUSTOM_CLICK_ACTION;
use pumpkin_macros::java_packet;
use pumpkin_util::resource_location::ResourceLocation;
use pumpkin_util::version::JavaMinecraftVersion;

use crate::{ReadingError, ServerPacket, ser::NetworkReadExt};

// EMBER: was `#[derive(Deserialize)]`, which is broken for any `Box<[u8]>`/
// `Vec<T>` field with this crate's hand-rolled packet `Deserializer` - its
// `deserialize_seq` has no length tracking and never terminates (see the
// `SeqAccess` impl in `ser/deserializer.rs`), so a derived `Box<[u8]>` reads
// one "sequence element" at a time forever until it runs off the end of the
// buffer, which is exactly "incomplete: failed to fill whole buffer". Every
// other packet with a `Box<[u8]>` field (`SCookieResponse`,
// `SLoginPluginResponse`, `SPluginMessage`, ...) already has a manual `read`
// for this reason; this one just hadn't been exercised by a real client
// before (dialog submissions were the first thing that ever sent this
// packet with a real client attached). The `Option<Box<[u8]>>` shape here
// matches `SLoginPluginResponse.data` exactly - same bool-then-remaining-
// bytes encoding, not a length-prefixed byte array like `SCookieResponse`
// (this field's content is self-delimiting NBT, and it's the last field in
// the packet, so there's nothing to delimit against).
const MAX_PAYLOAD_SIZE: usize = 1_048_576;

#[java_packet(PLAY_CUSTOM_CLICK_ACTION)]
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
