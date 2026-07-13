use std::sync::Arc;

use pumpkin_macros::{Event, cancellable};

use crate::entity::player::Player;

/// An event that occurs when a player clicks a button in a floating
/// packet-only menu (`crate::server::menu::MenuManager`).
///
/// The menu is always closed before this fires - cancelling only prevents
/// the button's configured command from running, not the close itself.
/// Rewriting `command` changes what runs instead.
#[cancellable]
#[derive(Event, Clone)]
pub struct MenuClickEvent {
    /// The player who clicked the button.
    pub player: Arc<Player>,

    /// The name of the menu the button belonged to.
    pub menu_name: String,

    /// The command configured to run on click, if any. A blocking handler
    /// can rewrite this to change what runs, or clear it to suppress the
    /// command without cancelling the whole event.
    pub command: Option<String>,
}

impl MenuClickEvent {
    /// Creates a new instance of `MenuClickEvent`.
    #[must_use]
    pub const fn new(player: Arc<Player>, menu_name: String, command: Option<String>) -> Self {
        Self {
            player,
            menu_name,
            command,
            cancelled: false,
        }
    }
}
