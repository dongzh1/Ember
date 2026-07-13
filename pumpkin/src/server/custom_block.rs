// EMBER start: custom blocks (real blockstate carrier, phase 4 of the CraftEngine portation)
//! Custom blocks: a real vanilla block ("carrier") placed at its own
//! default state, wearing a resource pack skin - real collision, real
//! physics, unlike `server::furniture`'s non-solid display entities.
//!
//! "Which position is secretly which custom block id" is tracked entirely
//! by this manager's own position index (`blocks/instances.toml`), *not*
//! a vanilla `BlockEntity` - the carrier's own real block state is saved/
//! loaded through the normal world save format like any other block, but
//! attaching a full `BlockEntity` for the extra bookkeeping would need a
//! new arm in `block::entities::block_entity_from_nbt`'s closed match
//! statement to load back correctly (easy to add, but one more core-file
//! edit than this needs - a manager-owned index does the same job without
//! it).
//!
//! Interception happens at the `BlockRegistry` dispatch level
//! (`on_use`/`World::break_block`), not inside any carrier block's own
//! file (e.g. `blocks/note.rs`) - every existing vanilla block's behavior
//! is completely unchanged for any position with no recorded custom block.
use std::collections::HashMap;
use std::path::PathBuf;

use pumpkin_config::{
    CustomBlockConfig, CustomBlockInstanceConfig, CustomBlockInstanceListConfig,
    CustomBlockListConfig, LoadConfiguration,
};
use pumpkin_util::math::position::BlockPos;
use tokio::sync::RwLock;

pub struct CustomBlockManager {
    exec_dir: PathBuf,
    types: RwLock<CustomBlockListConfig>,
    instances: RwLock<CustomBlockInstanceListConfig>,
    /// `(world name, position) -> custom block id`, rebuilt from
    /// `instances` at startup - the hot lookup `on_use`/breaking hooks use.
    runtime: RwLock<HashMap<(String, BlockPos), String>>,
}

impl Default for CustomBlockManager {
    fn default() -> Self {
        Self::new()
    }
}

impl CustomBlockManager {
    #[must_use]
    pub fn new() -> Self {
        let exec_dir = std::env::current_dir().expect("Failed to get current directory");
        let types = CustomBlockListConfig::load(&exec_dir);
        let instance_list = CustomBlockInstanceListConfig::load(&exec_dir);
        let mut runtime = HashMap::with_capacity(instance_list.instances.len());
        for instance in &instance_list.instances {
            runtime.insert(
                (instance.world.clone(), block_pos(instance)),
                instance.block_id.clone(),
            );
        }
        Self {
            exec_dir,
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

    /// The custom block id recorded at `position` in `world`, if any - the
    /// lookup `BlockRegistry::on_use`/`World::break_block` consult before
    /// falling through to a carrier's own vanilla behavior.
    pub async fn get_at(&self, world: &str, position: &BlockPos) -> Option<String> {
        self.runtime
            .read()
            .await
            .get(&(world.to_string(), *position))
            .cloned()
    }

    /// Records a new placement (the caller is responsible for actually
    /// setting the carrier's block state in the world).
    pub async fn place(&self, world: &str, position: BlockPos, block_id: &str) {
        self.runtime
            .write()
            .await
            .insert((world.to_string(), position), block_id.to_string());

        let mut instances = self.instances.write().await;
        instances.instances.push(CustomBlockInstanceConfig {
            block_id: block_id.to_string(),
            world: world.to_string(),
            x: position.0.x,
            y: position.0.y,
            z: position.0.z,
        });
        instances.save(&self.exec_dir);
    }

    /// Removes the recorded placement at `position`, if any, returning its
    /// custom block id (for a drop-the-item response). The caller is
    /// responsible for actually clearing the carrier's block state.
    pub async fn remove(&self, world: &str, position: &BlockPos) -> Option<String> {
        let removed = self
            .runtime
            .write()
            .await
            .remove(&(world.to_string(), *position))?;

        let mut instances = self.instances.write().await;
        instances
            .instances
            .retain(|i| !(i.world == world && block_pos(i) == *position));
        instances.save(&self.exec_dir);

        Some(removed)
    }
}

const fn block_pos(instance: &CustomBlockInstanceConfig) -> BlockPos {
    BlockPos(pumpkin_util::math::vector3::Vector3::new(
        instance.x, instance.y, instance.z,
    ))
}
// EMBER end
