// EMBER start: custom blocks (real blockstate carrier, phase 4 of the CraftEngine portation)
//! Custom blocks: a real vanilla block ("carrier") placed at its own
//! default state, wearing a resource pack skin - real collision, real
//! physics, unlike `server::furniture`'s non-solid display entities.
//!
//! "Which position is secretly which custom block id" is tracked entirely
//! by this manager's own position index (`<world folder>/
//! custom_block_instances.toml`), *not* a vanilla `BlockEntity` - the
//! carrier's own real block state is saved/loaded through the normal world
//! save format like any other block, but attaching a full `BlockEntity` for
//! the extra bookkeeping would need a new arm in
//! `block::entities::block_entity_from_nbt`'s closed match statement to
//! load back correctly (easy to add, but one more core-file edit than this
//! needs - a manager-owned index does the same job without it).
//!
//! One `CustomBlockManager` per loaded `World` (constructed in
//! `World::load`, dropped with it on unload) rather than one global
//! manager keyed by world name - the index lives inside the world's own
//! folder so it travels with it if that folder is copied to another
//! server, the same reasoning `World::portal_poi` already follows for its
//! own per-world index.
//!
//! Interception happens at the `BlockRegistry` dispatch level
//! (`on_use`/`World::break_block`), not inside any carrier block's own
//! file (e.g. `blocks/note.rs`) - every existing vanilla block's behavior
//! is completely unchanged for any position with no recorded custom block.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use pumpkin_config::{
    CustomBlockConfig, CustomBlockInstanceConfig, CustomBlockInstanceListConfig,
    CustomBlockListConfig, LoadConfiguration,
};
use pumpkin_util::math::position::BlockPos;
use tokio::sync::RwLock;

pub struct CustomBlockManager {
    world_root: PathBuf,
    /// The configured custom block *types* - server-level (see module
    /// doc), reloaded independently per world since it's tiny and
    /// read-only after boot; not worth threading a shared handle through
    /// `World::load` for.
    types: RwLock<CustomBlockListConfig>,
    instances: RwLock<CustomBlockInstanceListConfig>,
    /// `position -> custom block id`, rebuilt from `instances` at load -
    /// the hot lookup `on_use`/breaking hooks use.
    runtime: RwLock<HashMap<BlockPos, String>>,
}

impl CustomBlockManager {
    #[must_use]
    pub fn new(world_root: &Path) -> Self {
        let exec_dir = std::env::current_dir().expect("Failed to get current directory");
        let types = CustomBlockListConfig::load(&exec_dir);
        let instance_list = CustomBlockInstanceListConfig::load(world_root);
        let mut runtime = HashMap::with_capacity(instance_list.instances.len());
        for instance in &instance_list.instances {
            runtime.insert(block_pos(instance), instance.block_id.clone());
        }
        Self {
            world_root: world_root.to_path_buf(),
            types: RwLock::new(types),
            instances: RwLock::new(instance_list),
            runtime: RwLock::new(runtime),
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

    /// The custom block id recorded at `position`, if any - the lookup
    /// `BlockRegistry::on_use`/`World::break_block` consult before falling
    /// through to a carrier's own vanilla behavior.
    pub async fn get_at(&self, position: &BlockPos) -> Option<String> {
        self.runtime.read().await.get(position).cloned()
    }

    /// Records a new placement (the caller is responsible for actually
    /// setting the carrier's block state in the world).
    pub async fn place(&self, position: BlockPos, block_id: &str) {
        self.runtime
            .write()
            .await
            .insert(position, block_id.to_string());

        let mut instances = self.instances.write().await;
        instances.instances.push(CustomBlockInstanceConfig {
            block_id: block_id.to_string(),
            x: position.0.x,
            y: position.0.y,
            z: position.0.z,
        });
        instances.save(&self.world_root);
    }

    /// Removes the recorded placement at `position`, if any, returning its
    /// custom block id (for a drop-the-item response). The caller is
    /// responsible for actually clearing the carrier's block state.
    pub async fn remove(&self, position: &BlockPos) -> Option<String> {
        let removed = self.runtime.write().await.remove(position)?;

        let mut instances = self.instances.write().await;
        instances.instances.retain(|i| block_pos(i) != *position);
        instances.save(&self.world_root);

        Some(removed)
    }
}

const fn block_pos(instance: &CustomBlockInstanceConfig) -> BlockPos {
    BlockPos(pumpkin_util::math::vector3::Vector3::new(
        instance.x, instance.y, instance.z,
    ))
}
// EMBER end
