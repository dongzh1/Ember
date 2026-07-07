// EMBER - world lifecycle event: a world is about to be unloaded
use crate::world::World;
use pumpkin_macros::{Event, cancellable};
use std::sync::Arc;

/// Fired before a world is unloaded at runtime.
///
/// Triggered by `Server::unload_world`, `/world unload`, or a dungeon
/// instance stop, BEFORE players are evacuated. Cancelling it aborts the
/// unload and leaves the world loaded.
#[cancellable]
#[derive(Event, Clone)]
pub struct WorldUnload {
    /// The world that is about to be unloaded.
    pub world: Arc<World>,

    /// The world players will be evacuated to.
    pub fallback: Arc<World>,
}

impl WorldUnload {
    #[must_use]
    pub const fn new(world: Arc<World>, fallback: Arc<World>) -> Self {
        Self {
            world,
            fallback,
            cancelled: false,
        }
    }
}
