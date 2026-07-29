use pumpkin_data::packet::serverbound::PLAY_RESOURCE_PACK;
use pumpkin_macros::java_packet;
use pumpkin_util::version::JavaMinecraftVersion;

use crate::java::server::config::ResourcePackResponseResult;
use crate::{
    ServerPacket, VarInt,
    ser::{NetworkReadExt, ReadingError},
};

// EMBER: play-state counterpart of `SConfigResourcePack` - same wire shape,
// but this crate previously had no struct/handler for it at all (only the
// configuration-state one existed), so any resource pack response a client
// sends while already in the Play state (e.g. re-confirming a pack after
// being moved between worlds) fell through to the generic "unhandled
// packet id" warning.
#[java_packet(PLAY_RESOURCE_PACK)]
pub struct SPlayResourcePack {
    /// The unique identifier of the resource pack this response refers to.
    pub uuid: uuid::Uuid,
    /// The status code of the operation, mapped to [`ResourcePackResponseResult`].
    pub result: VarInt,
}

impl<'a> ServerPacket<'a> for SPlayResourcePack {
    fn read(bytebuf: &mut &'a [u8], _version: &JavaMinecraftVersion) -> Result<Self, ReadingError> {
        Ok(Self {
            uuid: bytebuf.get_uuid()?,
            result: bytebuf.get_var_int()?,
        })
    }
}

impl SPlayResourcePack {
    #[must_use]
    pub const fn response_result(&self) -> ResourcePackResponseResult {
        match self.result.0 {
            0 => ResourcePackResponseResult::DownloadSuccess,
            1 => ResourcePackResponseResult::Declined,
            2 => ResourcePackResponseResult::DownloadFail,
            3 => ResourcePackResponseResult::Accepted,
            4 => ResourcePackResponseResult::Downloaded,
            5 => ResourcePackResponseResult::InvalidUrl,
            6 => ResourcePackResponseResult::ReloadFailed,
            7 => ResourcePackResponseResult::Discarded,
            x => ResourcePackResponseResult::Unknown(x),
        }
    }
}
