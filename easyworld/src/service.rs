//! [`WorldService`] — the reusable world-management API that `EasyWorld` exposes
//! to other plugins.
//!
//! `EasyWorld` is a *prerequisite library* (`前置插件`): rather than reimplement
//! world lifecycle handling, dependent plugins fetch this service from the
//! plugin context and drive worlds through it.
//!
//! # Using the service from another plugin
//!
//! ```ignore
//! // Inside another plugin's `on_load`:
//! if let Some(worlds) = context.get_service::<WorldService>("easyworld").await {
//!     let world = worlds.create_or_load("lobby".to_string()).await;
//!     println!("loaded {}", world.get_world_name());
//! }
//! ```

use std::any::Any;
use std::sync::Arc;

use pumpkin::plugin::Payload;
use pumpkin::server::Server;
use pumpkin::world::World;
use pumpkin_data::dimension::Dimension;

/// Wraps the [`Server`] world primitives behind a small, stable surface so that
/// dependent plugins do not have to reach into server internals themselves.
pub struct WorldService {
    server: Arc<Server>,
}

impl WorldService {
    /// Creates a new service bound to the given [`Server`].
    #[must_use]
    pub const fn new(server: Arc<Server>) -> Self {
        Self { server }
    }

    /// Finds a currently loaded world by name.
    #[must_use]
    pub fn find(&self, name: &str) -> Option<Arc<World>> {
        self.server
            .worlds
            .load()
            .iter()
            .find(|w| w.get_world_name() == name)
            .cloned()
    }

    /// Returns `true` while a world with this name is still being unloaded.
    #[must_use]
    pub fn is_unloading(&self, name: &str) -> bool {
        self.server.is_world_unloading(name)
    }

    /// Loads an existing world, or creates a fresh overworld if none exists yet.
    ///
    /// Mirrors [`Server::create_world`], which loads-or-creates in one step.
    pub async fn create_or_load(&self, name: String) -> Arc<World> {
        self.server.create_world(name, Dimension::OVERWORLD).await
    }

    /// Evacuates players, saves, and unloads a world, using the first loaded
    /// world as the fallback destination.
    pub async fn unload(&self, name: &str) -> Result<(), String> {
        let Some(world) = self.find(name) else {
            return Err(format!("World '{name}' is not loaded."));
        };
        let Some(fallback) = self.server.worlds.load().first().cloned() else {
            return Err("No fallback world available.".to_string());
        };
        self.server.unload_world(&world, &fallback).await
    }

    /// Clones `src` into a new persistent world named `dst` and loads it.
    pub async fn clone_world(&self, src: &str, dst: &str) -> Result<Arc<World>, String> {
        self.server.clone_world(src, dst).await
    }

    /// Clones `src` into a read-only in-memory world named `dst` that discards
    /// changes on unload.
    pub async fn clone_world_readonly(&self, src: &str, dst: &str) -> Result<Arc<World>, String> {
        self.server.clone_world_readonly(src, dst).await
    }

    /// Permanently deletes a world's on-disk data.
    pub async fn delete(&self, name: &str) -> Result<(), String> {
        self.server.delete_world(name).await
    }

    /// Lists loaded worlds as `(name, player_count)` pairs.
    #[must_use]
    pub fn list_loaded(&self) -> Vec<(String, usize)> {
        self.server
            .worlds
            .load()
            .iter()
            .map(|w| (w.get_world_name().to_string(), w.players.load().len()))
            .collect()
    }

    /// Lists world folders found on disk (loaded or not).
    #[must_use]
    pub fn list_on_disk(&self) -> Vec<String> {
        self.server.list_world_folders()
    }
}

// Services must implement `Payload` so the plugin manager can store and later
// hand them back (via name-based downcasting) to dependent plugins. The
// `pumpkin_macros::Event` derive can't be reused here because it expands to
// `impl crate::plugin::Payload`, which only resolves inside the `pumpkin`
// crate itself — so we implement the trait by hand.
impl Payload for WorldService {
    fn get_name_static() -> &'static str {
        "easyworld::WorldService"
    }

    fn get_name(&self) -> &'static str {
        "easyworld::WorldService"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}
