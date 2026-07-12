use std::sync::Arc;

use pumpkin_macros::{Event, cancellable};
use pumpkin_protocol::java::server::play::ActionType;

use crate::entity::player::Player;

/// An event that occurs when a player clicks a packet-only NPC.
///
/// Packet-only NPCs (`crate::server::npc::NpcManager`) are never a real
/// world entity, so this is the only way plugins can observe or react to a
/// click on one. Cancelling prevents the NPC's configured click command (if
/// any) from running; rewriting `command` changes what runs instead.
#[cancellable]
#[derive(Event, Clone)]
pub struct NpcClickEvent {
    /// The player who clicked the NPC.
    pub player: Arc<Player>,

    /// The name of the NPC that was clicked.
    pub npc_name: String,

    /// Whether this was a left-click (attack) or right-click (interact).
    pub action: ActionType,

    /// The command configured to run on click (`/npc setaction`), if any.
    /// A blocking handler can rewrite this to change what runs, or clear it
    /// to suppress the command without cancelling the whole event.
    pub command: Option<String>,
}

impl NpcClickEvent {
    /// Creates a new instance of `NpcClickEvent`.
    #[must_use]
    pub const fn new(
        player: Arc<Player>,
        npc_name: String,
        action: ActionType,
        command: Option<String>,
    ) -> Self {
        Self {
            player,
            npc_name,
            action,
            command,
            cancelled: false,
        }
    }
}
