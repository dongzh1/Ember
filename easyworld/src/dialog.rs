//! In-game dialog UI for `/ew menu` plus the handler that reacts to button
//! clicks.
//!
//! The menu is a server-driven `minecraft:multi_action` dialog. Each button
//! carries a `DialogAction::Custom` whose `id` encodes the operation, e.g.
//! `ew:unload:lobby`. When the player clicks it, the server fires a
//! [`CustomClickActionEvent`], which [`WorldClickHandler`] parses and routes
//! back into the [`WorldService`].

use std::collections::HashSet;
use std::sync::Arc;

use pumpkin::plugin::api::events::player::custom_click_action::CustomClickActionEvent;
use pumpkin::plugin::{BoxFuture, EventHandler};
use pumpkin::server::Server;
use pumpkin_protocol::java::client::dialog::{ActionButton, Dialog, DialogAction, DialogBody};
use pumpkin_util::text::{TextComponent, color::NamedColor};

use crate::service::WorldService;

/// Builds a fresh menu dialog reflecting the current set of worlds.
#[must_use]
pub fn build_menu_dialog(service: &WorldService) -> Dialog {
    let loaded = service.list_loaded();
    let on_disk = service.list_on_disk();
    let loaded_names: HashSet<&str> = loaded.iter().map(|(name, _)| name.as_str()).collect();

    let mut buttons: Vec<ActionButton> = Vec::new();

    // Loaded worlds get unload + delete actions.
    for (name, players) in &loaded {
        buttons.push(custom_button(
            format!("Unload {name} ({players})"),
            NamedColor::Gold,
            format!("ew:unload:{name}"),
        ));
        buttons.push(custom_button(
            format!("Delete {name}"),
            NamedColor::Red,
            format!("ew:delete:{name}"),
        ));
    }

    // On-disk worlds that aren't loaded get a load action.
    for name in on_disk
        .iter()
        .filter(|name| !loaded_names.contains(name.as_str()))
    {
        buttons.push(custom_button(
            format!("Load {name}"),
            NamedColor::Green,
            format!("ew:load:{name}"),
        ));
    }

    buttons.push(custom_button(
        "Create new world".to_string(),
        NamedColor::Aqua,
        "ew:create".to_string(),
    ));

    Dialog {
        r#type: "minecraft:multi_action".to_string(),
        title: TextComponent::text("EasyWorld"),
        body: vec![DialogBody::PlainMessage {
            contents: TextComponent::text(format!(
                "{} loaded, {} on disk",
                loaded.len(),
                on_disk.len()
            )),
        }],
        inputs: Vec::new(),
        buttons,
        links: Vec::new(),
        exit_action: None,
        after_action: None,
        can_close_with_escape: true,
        external_title: None,
    }
}

/// Small helper to build a colored button carrying a custom action `id`.
fn custom_button(label: String, color: NamedColor, id: String) -> ActionButton {
    ActionButton {
        text: TextComponent::text(label).color_named(color),
        tooltip: None,
        width: None,
        action: DialogAction::Custom { id, payload: None },
    }
}

/// A decoded menu button action.
enum MenuAction<'a> {
    Load(&'a str),
    Unload(&'a str),
    Delete(&'a str),
    Create,
}

impl<'a> MenuAction<'a> {
    /// Decodes a `DialogAction::Custom` id string into a menu action.
    fn parse(id: &'a str) -> Option<Self> {
        if let Some(name) = id.strip_prefix("ew:load:") {
            return Some(Self::Load(name));
        }
        if let Some(name) = id.strip_prefix("ew:unload:") {
            return Some(Self::Unload(name));
        }
        if let Some(name) = id.strip_prefix("ew:delete:") {
            return Some(Self::Delete(name));
        }
        if id == "ew:create" {
            return Some(Self::Create);
        }
        None
    }
}

/// Finds an unused `newworld<N>` name for the "Create new world" button, which
/// carries no text input to name the world.
fn next_free_name(service: &WorldService) -> String {
    let mut taken: Vec<String> = service.list_on_disk();
    for (name, _) in service.list_loaded() {
        taken.push(name);
    }
    (0..10_000)
        .map(|i| format!("newworld{i}"))
        .find(|candidate| !taken.contains(candidate))
        .unwrap_or_else(|| "newworld".to_string())
}

/// Handles [`CustomClickActionEvent`]s produced by the `EasyWorld` menu.
pub struct WorldClickHandler {
    service: Arc<WorldService>,
}

impl WorldClickHandler {
    /// Creates a handler that drives the shared [`WorldService`].
    #[must_use]
    pub const fn new(service: Arc<WorldService>) -> Self {
        Self { service }
    }
}

impl EventHandler<CustomClickActionEvent> for WorldClickHandler {
    fn handle<'a>(
        &'a self,
        _server: &'a Arc<Server>,
        event: &'a CustomClickActionEvent,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let Some(action) = MenuAction::parse(&event.id) else {
                return;
            };

            let message = match action {
                MenuAction::Load(name) => {
                    let world = self.service.create_or_load(name.to_string()).await;
                    format!("Loaded world '{}'.", world.get_world_name())
                }
                MenuAction::Unload(name) => match self.service.unload(name).await {
                    Ok(()) => format!("Unloaded world '{name}'."),
                    Err(e) => format!("Cannot unload '{name}': {e}"),
                },
                MenuAction::Delete(name) => match self.service.delete(name).await {
                    Ok(()) => format!("Deleted world '{name}'."),
                    Err(e) => format!("Cannot delete '{name}': {e}"),
                },
                MenuAction::Create => {
                    let name = next_free_name(&self.service);
                    let world = self.service.create_or_load(name).await;
                    format!("Created world '{}'.", world.get_world_name())
                }
            };

            event
                .player
                .send_system_message(&TextComponent::text(message))
                .await;

            // Re-render the menu so the player immediately sees the new state.
            let dialog = build_menu_dialog(&self.service);
            event.player.show_dialog(&dialog).await;
        })
    }
}
