// EMBER start: custom blocks (real blockstate carrier, phase 4 of the CraftEngine portation)
//! Custom blocks: a real vanilla block ("carrier") placed at its own
//! default state, wearing a resource pack skin - real collision, real
//! physics, unlike `server::furniture`'s non-solid display entities.
//!
//! "Which position is secretly which custom block id" is tracked entirely
//! by this manager's own position index, *not* a vanilla `BlockEntity` -
//! the carrier's own real block state is saved/loaded through the normal
//! world save format like any other block, but attaching a full
//! `BlockEntity` for the extra bookkeeping would need a new arm in
//! `block::entities::block_entity_from_nbt`'s closed match statement to
//! load back correctly (easy to add, but one more core-file edit than this
//! needs - a manager-owned index does the same job without it).
//!
//! One `CustomBlockManager` per loaded `World` (constructed in
//! `World::load`, dropped with it on unload) rather than one global
//! manager keyed by world name. The index's storage mirrors this world's
//! own chunk storage backend: `file` (the common case) keeps a TOML file
//! inside the world's own folder, the same reasoning `World::portal_poi`
//! already follows for its own per-world index - it travels with the
//! folder if copied to another server. `mysql` (a world shared read-write/
//! read-only across multiple servers) stores the same rows in that same
//! world's `MySQL` database instead, so every server actually sharing that
//! world sees the same placements - a local file wouldn't be visible to
//! them.
//!
//! Interception happens at the `BlockRegistry` dispatch level
//! (`on_use`/`World::break_block`), not inside any carrier block's own
//! file (e.g. `blocks/note.rs`) - every existing vanilla block's behavior
//! is completely unchanged for any position with no recorded custom block.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use pumpkin_config::chunk::{ChunkConfig, EasyBackend, EasyWorldMode};
use pumpkin_config::{
    CustomBlockConfig, CustomBlockInstanceConfig, CustomBlockInstanceListConfig,
    CustomBlockListConfig, LoadConfiguration,
};
use pumpkin_util::math::position::BlockPos;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_world::chunk::easy_mysql::world_key_for;
use tokio::sync::RwLock;
use tracing::error;

const CREATE_TABLE: &str = concat!(
    "CREATE TABLE IF NOT EXISTS ember_custom_block_instances (",
    "world_key VARCHAR(512) NOT NULL,",
    "x INT NOT NULL,",
    "y INT NOT NULL,",
    "z INT NOT NULL,",
    "block_id VARCHAR(128) NOT NULL,",
    "PRIMARY KEY (world_key, x, y, z)",
    ")"
);

enum Storage {
    File {
        world_root: PathBuf,
        instances: CustomBlockInstanceListConfig,
    },
    /// This world's chunk backend is mysql, but `World::load` (sync) can't
    /// connect yet - a caller must `.await` `connect_mysql` once it's able
    /// to. Placements made before that finishes only land in the in-memory
    /// `runtime` index, not persisted - unreachable in practice, since
    /// world loading always completes before the server accepts players.
    PendingMysql { url: String, world_key: String },
    Mysql {
        pool: sqlx::mysql::MySqlPool,
        world_key: String,
    },
}

pub struct CustomBlockManager {
    storage: RwLock<Storage>,
    /// The configured custom block *types* - server-level (see module
    /// doc), reloaded independently per world since it's tiny and
    /// read-only after boot; not worth threading a shared handle through
    /// `World::load` for.
    types: RwLock<CustomBlockListConfig>,
    /// `position -> custom block id`, rebuilt from storage at load - the
    /// hot lookup `on_use`/breaking hooks use, identical shape regardless
    /// of which storage backend feeds it.
    runtime: RwLock<HashMap<BlockPos, String>>,
}

impl CustomBlockManager {
    #[must_use]
    pub fn new(world_root: &Path, chunk_config: &ChunkConfig) -> Self {
        let exec_dir = std::env::current_dir().expect("Failed to get current directory");
        let types = CustomBlockListConfig::load(&exec_dir);

        if let ChunkConfig::Easy(cfg) = chunk_config
            && cfg.backend == EasyBackend::Mysql
        {
            let world_key = world_key_for(&cfg.mysql(EasyWorldMode::default()), world_root);
            return Self {
                storage: RwLock::new(Storage::PendingMysql {
                    url: cfg.url.clone(),
                    world_key,
                }),
                types: RwLock::new(types),
                runtime: RwLock::new(HashMap::new()),
            };
        }

        let instances = CustomBlockInstanceListConfig::load(world_root);
        let mut runtime = HashMap::with_capacity(instances.instances.len());
        for instance in &instances.instances {
            runtime.insert(block_pos(instance), instance.block_id.clone());
        }
        Self {
            storage: RwLock::new(Storage::File {
                world_root: world_root.to_path_buf(),
                instances,
            }),
            types: RwLock::new(types),
            runtime: RwLock::new(runtime),
        }
    }

    /// Connects to mysql and loads this world's placed custom blocks, if
    /// this manager's storage is still pending (a no-op otherwise - file
    /// storage already loaded everything synchronously in `new`). Called
    /// once, right after the world it belongs to finishes loading.
    pub async fn connect_mysql(&self) {
        let (url, world_key) = match &*self.storage.read().await {
            Storage::PendingMysql { url, world_key } => (url.clone(), world_key.clone()),
            Storage::File { .. } | Storage::Mysql { .. } => return,
        };

        let pool = match sqlx::mysql::MySqlPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
        {
            Ok(pool) => pool,
            Err(e) => {
                error!(
                    "Custom block manager: failed to connect to mysql ({url}): {e} - custom \
                     blocks in this world won't load or persist until this is fixed."
                );
                return;
            }
        };
        if let Err(e) = sqlx::query(CREATE_TABLE).execute(&pool).await {
            error!("Custom block manager: failed to create table: {e}");
            return;
        }

        let rows: Vec<(i32, i32, i32, String)> = match sqlx::query_as(
            "SELECT x, y, z, block_id FROM ember_custom_block_instances WHERE world_key = ?",
        )
        .bind(&world_key)
        .fetch_all(&pool)
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                error!("Custom block manager: failed to load instances: {e}");
                return;
            }
        };

        let mut runtime = self.runtime.write().await;
        for (x, y, z, block_id) in rows {
            runtime.insert(BlockPos(Vector3::new(x, y, z)), block_id);
        }
        drop(runtime);

        *self.storage.write().await = Storage::Mysql { pool, world_key };
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

        match &mut *self.storage.write().await {
            Storage::File {
                world_root,
                instances,
            } => {
                instances.instances.push(CustomBlockInstanceConfig {
                    block_id: block_id.to_string(),
                    x: position.0.x,
                    y: position.0.y,
                    z: position.0.z,
                });
                instances.save(world_root);
            }
            Storage::Mysql { pool, world_key } => {
                let result = sqlx::query(
                    "INSERT INTO ember_custom_block_instances (world_key, x, y, z, block_id) \
                     VALUES (?, ?, ?, ?, ?) ON DUPLICATE KEY UPDATE block_id = VALUES(block_id)",
                )
                .bind(&*world_key)
                .bind(position.0.x)
                .bind(position.0.y)
                .bind(position.0.z)
                .bind(block_id)
                .execute(&*pool)
                .await;
                if let Err(e) = result {
                    error!("Custom block manager: failed to persist placement: {e}");
                }
            }
            Storage::PendingMysql { .. } => {}
        }
    }

    /// Removes the recorded placement at `position`, if any, returning its
    /// custom block id (for a drop-the-item response). The caller is
    /// responsible for actually clearing the carrier's block state.
    pub async fn remove(&self, position: &BlockPos) -> Option<String> {
        let removed = self.runtime.write().await.remove(position)?;

        match &mut *self.storage.write().await {
            Storage::File {
                world_root,
                instances,
            } => {
                instances.instances.retain(|i| block_pos(i) != *position);
                instances.save(world_root);
            }
            Storage::Mysql { pool, world_key } => {
                let result = sqlx::query(
                    "DELETE FROM ember_custom_block_instances \
                     WHERE world_key = ? AND x = ? AND y = ? AND z = ?",
                )
                .bind(&*world_key)
                .bind(position.0.x)
                .bind(position.0.y)
                .bind(position.0.z)
                .execute(&*pool)
                .await;
                if let Err(e) = result {
                    error!("Custom block manager: failed to persist removal: {e}");
                }
            }
            Storage::PendingMysql { .. } => {}
        }

        Some(removed)
    }
}

const fn block_pos(instance: &CustomBlockInstanceConfig) -> BlockPos {
    BlockPos(Vector3::new(instance.x, instance.y, instance.z))
}

// Manual-only: needs a real, reachable mysql server, so it's excluded from
// the normal `cargo test` run (`#[ignore]`) and never hardcodes a
// connection string - set `EMBER_TEST_MYSQL_URL` and run with
// `--ignored` to actually exercise it.
#[cfg(test)]
mod mysql_tests {
    use super::*;

    fn test_url() -> Option<String> {
        std::env::var("EMBER_TEST_MYSQL_URL").ok()
    }

    fn mysql_chunk_config(url: &str) -> ChunkConfig {
        ChunkConfig::Easy(pumpkin_config::chunk::EasyConfig {
            backend: EasyBackend::Mysql,
            url: url.to_string(),
            key_prefix: "ember_mysql_test".to_string(),
            max_cached_regions: 1,
        })
    }

    #[tokio::test]
    #[ignore = "needs a real mysql server; set EMBER_TEST_MYSQL_URL and run with --ignored"]
    async fn place_persists_and_reloads_across_managers() {
        let Some(url) = test_url() else {
            panic!("set EMBER_TEST_MYSQL_URL to a real mysql connection string to run this");
        };
        let chunk_config = mysql_chunk_config(&url);
        // A unique-ish folder per run so repeated manual runs don't collide
        // on the same world_key.
        let world_root =
            std::path::PathBuf::from(format!("/ember-mysql-test-{}", std::process::id()));

        let manager = CustomBlockManager::new(&world_root, &chunk_config);
        manager.connect_mysql().await;

        let pos = BlockPos(Vector3::new(12, 34, -56));
        assert!(
            manager.get_at(&pos).await.is_none(),
            "fresh world_key should start empty"
        );

        manager.place(pos, "test_custom_block").await;
        assert_eq!(
            manager.get_at(&pos).await.as_deref(),
            Some("test_custom_block"),
            "placement should be visible in-memory immediately"
        );

        // A second manager instance, pointed at the same world_key, proves
        // the placement actually round-tripped through mysql rather than
        // just staying in the first manager's own memory.
        let reloaded = CustomBlockManager::new(&world_root, &chunk_config);
        reloaded.connect_mysql().await;
        assert_eq!(
            reloaded.get_at(&pos).await.as_deref(),
            Some("test_custom_block"),
            "a second manager sharing the same mysql world_key should see the same placement"
        );

        let removed = manager.remove(&pos).await;
        assert_eq!(removed.as_deref(), Some("test_custom_block"));

        let reloaded_after_remove = CustomBlockManager::new(&world_root, &chunk_config);
        reloaded_after_remove.connect_mysql().await;
        assert!(
            reloaded_after_remove.get_at(&pos).await.is_none(),
            "removal should also round-trip through mysql"
        );
    }
}
// EMBER end
