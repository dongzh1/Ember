// EMBER start: custom blocks (real blockstate carrier, phase 4 of the CraftEngine portation)
//! Custom blocks: a real vanilla block ("carrier") placed at its own
//! default state, wearing a resource pack skin - real collision, real
//! physics, unlike `server::furniture`'s non-solid display entities.
//!
//! "Which position is secretly which custom block id" is tracked in
//! `ChunkData::ember_custom_blocks` - a field Ember owns exclusively,
//! parallel to (never reusing) vanilla's own `pending_block_entities`. It
//! rides along with the owning chunk's own save/load cycle, so it
//! automatically follows whichever backend that chunk's world already uses
//! (file or mysql) with no separate storage/config of its own. See
//! `World::get_ember_custom_block`/`set_ember_custom_block`/
//! `remove_ember_custom_block` for the actual reads/writes - this manager
//! only holds the server-level *type* definitions (`CustomBlockConfig`),
//! not any per-world placement data.
//!
//! Interception happens at the `BlockRegistry` dispatch level
//! (`on_use`/`World::break_block`), not inside any carrier block's own
//! file (e.g. `blocks/note.rs`) - every existing vanilla block's behavior
//! is completely unchanged for any position with no recorded custom block.
use pumpkin_config::{CustomBlockConfig, CustomBlockListConfig, LoadConfiguration};
use tokio::sync::RwLock;

pub struct CustomBlockManager {
    /// The configured custom block *types* - server-level, reloaded
    /// independently per world since it's tiny and read-only after boot;
    /// not worth threading a shared handle through `World::load` for.
    types: RwLock<CustomBlockListConfig>,
}

impl CustomBlockManager {
    #[must_use]
    pub fn new() -> Self {
        let exec_dir = std::env::current_dir().expect("Failed to get current directory");
        Self {
            types: RwLock::new(CustomBlockListConfig::load(&exec_dir)),
        }
    }

    /// Looks up the custom block type a held custom item places, if any.
    pub async fn find_by_custom_item(&self, custom_item_id: &str) -> Option<CustomBlockConfig> {
        self.types
            .read()
            .await
            .blocks
            .iter()
            .find(|b| b.custom_item_id.eq_ignore_ascii_case(custom_item_id))
            .cloned()
    }

    /// Looks up a custom block type by its own id (as opposed to
    /// `find_by_custom_item`, keyed by the item that places it) - used when
    /// breaking one, to resolve which item to hand back.
    pub async fn find_by_id(&self, block_id: &str) -> Option<CustomBlockConfig> {
        self.types
            .read()
            .await
            .blocks
            .iter()
            .find(|b| b.id.eq_ignore_ascii_case(block_id))
            .cloned()
    }
}

impl Default for CustomBlockManager {
    fn default() -> Self {
        Self::new()
    }
}
// EMBER end
