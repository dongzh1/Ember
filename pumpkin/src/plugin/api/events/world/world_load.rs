// EMBER - world lifecycle event: a world was loaded/created at runtime
use crate::world::World;
use pumpkin_macros::Event;
use std::sync::Arc;

/// Fired after a world is created or loaded at runtime (via
/// `Server::create_world_with`, `/world load`, `/world clone`, or a dungeon
/// instance start). Purely informational — the world is already live.
#[derive(Event, Clone)]
pub struct WorldLoad {
    /// The world that was loaded.
    pub world: Arc<World>,
}

impl WorldLoad {
    #[must_use]
    pub const fn new(world: Arc<World>) -> Self {
        Self { world }
    }
}
