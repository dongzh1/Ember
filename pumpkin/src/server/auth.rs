// EMBER start - offline-mode login verification
//! A lightweight login wall for `online_mode = false` servers.
//!
//! Loosely modeled on plugins like `LimboAuth`. Password entry happens
//! through a real dialog form (two text inputs for register, one for
//! login) using `minecraft:dynamic/custom` - the real protocol's mechanism
//! for collecting dialog input values, see `DialogAction::DynamicCustom`'s
//! doc comment in `pumpkin-protocol`. Chat is no longer part of this flow
//! (it used to be, back when `DialogInput` had no `key` field and
//! `SCustomClickAction` only carried an opaque static payload - see
//! `EMBER.md`'s changelog for that earlier limitation). Accounts live in
//! `MySQL` (single source of truth, no in-process cache beyond the
//! in-flight `pending` sessions below), so multiple servers can share one
//! login database.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use argon2::Argon2;
use argon2::password_hash::{
    PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng,
};
use pumpkin_config::{LoadConfiguration, LoginConfig};
use pumpkin_nbt::compound::NbtCompound;
use pumpkin_protocol::java::client::dialog::{
    ActionButton, Dialog, DialogAction, DialogBody, DialogInput,
};
use pumpkin_util::GameMode;
use pumpkin_util::math::vector2::Vector2;
use pumpkin_util::math::vector3::Vector3;
use pumpkin_util::text::TextComponent;
use sqlx::Row;
use sqlx::mysql::MySqlPoolOptions;
use tokio::sync::RwLock;
use tracing::error;
use uuid::Uuid;

use crate::entity::player::Player;
use crate::server::Server;

/// Custom-click action ids for the register/login dialogs' submit buttons.
///
/// Handled natively in `net/java/mod.rs` right where `SCustomClickAction`
/// is decoded - never reaches the plugin event bus.
pub const REGISTER_SUBMIT_ACTION_ID: &str = "ember:auth/register_submit";
pub const LOGIN_SUBMIT_ACTION_ID: &str = "ember:auth/login_submit";

const CREATE_TABLE: &str = concat!(
    "CREATE TABLE IF NOT EXISTS ember_login_accounts (",
    "uuid CHAR(36) NOT NULL PRIMARY KEY,",
    "username VARCHAR(16) NOT NULL,",
    "password_hash VARCHAR(255) NOT NULL,",
    "last_ip VARCHAR(45) NOT NULL,",
    "last_login_at BIGINT NOT NULL",
    ")"
);

const SELECT_ACCOUNT: &str =
    "SELECT password_hash, last_ip, last_login_at FROM ember_login_accounts WHERE uuid = ?";

const INSERT_ACCOUNT: &str = concat!(
    "INSERT INTO ember_login_accounts (uuid, username, password_hash, last_ip, last_login_at) ",
    "VALUES (?, ?, ?, ?, ?)"
);

const TOUCH_SESSION: &str =
    "UPDATE ember_login_accounts SET last_ip = ?, last_login_at = ? WHERE uuid = ?";

const DELETE_ACCOUNT: &str = "DELETE FROM ember_login_accounts WHERE uuid = ?";

/// Name of the dedicated holding world pending players spawn into. Never
/// player-visible as a "real" world name (no admin manages it via
/// `/world`), just an internal implementation detail.
pub const LIMBO_WORLD_NAME: &str = "__ember_limbo__";

/// The `LevelConfig` for [`LIMBO_WORLD_NAME`]: an empty (`generate = void`)
/// small map, same mechanism `/world clone ... readonly` uses for its
/// ephemeral worlds - see `Server::clone_world_readonly`.
#[must_use]
pub fn limbo_level_config() -> pumpkin_config::world::LevelConfig {
    pumpkin_config::world::LevelConfig {
        chunk: pumpkin_config::chunk::ChunkConfig::default(),
        lighting: pumpkin_config::lighting::LightingEngineConfig::default(),
        autosave_ticks: 0,
        ember: pumpkin_config::ember_world::EmberRuntime {
            mode: pumpkin_config::chunk::EasyWorldMode::ReadWrite,
            source: None,
            generate: pumpkin_config::ember_world::GenerateMode::Void,
            border: Some(pumpkin_config::ember_world::SMALL_MAP_MAX_BORDER),
        },
    }
}

#[derive(thiserror::Error, Debug)]
pub enum LoginError {
    #[error("login verification is not enabled")]
    Disabled,
    #[error("player has no pending login session")]
    NoPendingSession,
    #[error("login database error: {0}")]
    Database(String),
}

fn db_err(e: impl std::fmt::Display) -> LoginError {
    LoginError::Database(e.to_string())
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Resolves `world`'s own default spawn point (on top of the terrain at its
/// configured spawn X/Z), the same computation `World::spawn_java_player`
/// uses for a brand-new player. Duplicated rather than shared, matching
/// this same snippet's existing duplication between `spawn_java_player` and
/// `spawn_bedrock_player`.
async fn real_world_default_spawn(world: &Arc<crate::world::World>) -> (Vector3<f64>, f32, f32) {
    let info = world.level_info.load();
    let spawn_position = Vector2::new(info.spawn_x, info.spawn_z);
    let chunk_pos = Vector2::new(info.spawn_x >> 4, info.spawn_z >> 4);
    world.level.get_or_fetch_chunk(chunk_pos, |_| ()).await;
    let pos_y = world.get_top_block(spawn_position) + 1;
    let position = Vector3::new(
        f64::from(info.spawn_x) + 0.5,
        f64::from(pos_y),
        f64::from(info.spawn_z) + 0.5,
    );
    (position, info.spawn_yaw, info.spawn_pitch)
}

/// What a dialog submission from a pending player accomplished.
pub enum AuthOutcome {
    /// Registration's password was too short.
    PasswordTooShort { min_length: u32 },
    /// Registration's password and confirmation didn't match.
    ConfirmationMismatch,
    /// Password wrong; `attempts_left` is `0` when this was their last try
    /// (the caller should kick).
    WrongPassword { attempts_left: u32 },
    /// The submitted dialog payload didn't decode, or was missing the
    /// expected `password`/`confirm_password` key(s) - a malformed/modified
    /// client, not something a normal player action produces.
    MalformedSubmission,
    /// Registered or logged in. The caller should restore
    /// `previous_gamemode` and teleport them back to `real_world`.
    ///
    /// `spawn_override`, when `Some`, is where to actually put them -
    /// `real_world`'s own default spawn point, resolved against `real_world`
    /// itself (not the limbo world they were just standing in). Only set
    /// for a fresh **registration**: a brand-new player has no saved
    /// position, so `player.position()` at this point is wherever they
    /// happened to spawn *inside limbo* (a small void map) - reusing that
    /// as-is in `real_world` risks landing them underground/inside terrain,
    /// since limbo's coordinates have no relationship to `real_world`'s
    /// actual generated terrain. `None` for a returning **login**: their
    /// real saved position was already loaded onto the entity from their
    /// player-data file before they ever got redirected to limbo (see
    /// `Server::add_player`/`Player::read_nbt`), and nothing moved them
    /// while pending (the packet gateway blocked all movement), so
    /// `player.position()` is already correct as-is.
    Success {
        previous_gamemode: GameMode,
        real_world: Arc<crate::world::World>,
        spawn_override: Option<(Vector3<f64>, f32, f32)>,
    },
}

enum PendingMode {
    /// No account exists yet - both password and confirmation now arrive
    /// together in one dialog submission, so there's no partial state left
    /// to track between them.
    Register,
    /// An account exists; counts wrong attempts toward `max_login_attempts`.
    Login { attempts: u32 },
}

struct PendingAuth {
    username: String,
    previous_gamemode: GameMode,
    real_world: Arc<crate::world::World>,
    mode: PendingMode,
    /// Server tick this player's dialog was last sent (initial show, an
    /// error re-show, or a periodic `tick()` re-show) - see
    /// `LoginManager::tick`.
    last_shown_tick: i32,
}

pub struct LoginManager {
    enabled: bool,
    url: String,
    pool: Arc<tokio::sync::OnceCell<Arc<sqlx::MySqlPool>>>,
    session_seconds: i64,
    min_password_length: u32,
    max_login_attempts: u32,
    reprompt_ticks: i32,
    pending: RwLock<HashMap<Uuid, PendingAuth>>,
}

impl Default for LoginManager {
    fn default() -> Self {
        Self::new()
    }
}

impl LoginManager {
    /// Loads `auth/auth.toml` itself - see `LoginConfig`'s doc comment for
    /// why this isn't threaded in as a constructor argument.
    #[must_use]
    pub fn new() -> Self {
        let exec_dir = std::env::current_dir().expect("Failed to get current directory");
        let config = LoginConfig::load(&exec_dir);
        let manager = Self {
            enabled: config.enabled,
            url: config.url,
            pool: Arc::new(tokio::sync::OnceCell::new()),
            session_seconds: i64::try_from(config.session_hours.saturating_mul(3600))
                .unwrap_or(i64::MAX),
            min_password_length: config.min_password_length,
            max_login_attempts: config.max_login_attempts,
            reprompt_ticks: i32::try_from(config.reprompt_ticks).unwrap_or(i32::MAX),
            pending: RwLock::new(HashMap::new()),
        };

        if manager.enabled
            && let Ok(handle) = tokio::runtime::Handle::try_current()
        {
            // Eagerly connect (and create the table) in the background so a
            // bad URL/unreachable database fails loudly at startup instead
            // of on the first join.
            let pool_cell = manager.pool.clone();
            let url = manager.url.clone();
            handle.spawn(async move {
                if let Err(e) = pool_cell.get_or_try_init(|| Self::init_pool(&url)).await {
                    error!("Login MySQL eager init failed (check [auth] url): {e}");
                }
            });
        }

        manager
    }

    async fn init_pool(url: &str) -> Result<Arc<sqlx::MySqlPool>, LoginError> {
        let pool = MySqlPoolOptions::new()
            .max_connections(8)
            .connect(url)
            .await
            .map_err(db_err)?;
        sqlx::query(CREATE_TABLE)
            .execute(&pool)
            .await
            .map_err(db_err)?;
        Ok(Arc::new(pool))
    }

    async fn ensure_pool(&self) -> Result<Arc<sqlx::MySqlPool>, LoginError> {
        if !self.enabled {
            return Err(LoginError::Disabled);
        }
        self.pool
            .get_or_try_init(|| Self::init_pool(&self.url))
            .await
            .cloned()
    }

    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Whether this player needs to go through the login wall at all: the
    /// feature is on, the server is offline-mode, and they haven't already
    /// verified from this exact IP within `session_hours`.
    pub async fn needs_auth(&self, uuid: Uuid, ip: &str) -> Result<bool, LoginError> {
        if !self.enabled {
            return Ok(false);
        }
        let pool = self.ensure_pool().await?;
        let row = sqlx::query(SELECT_ACCOUNT)
            .bind(uuid.to_string())
            .fetch_optional(pool.as_ref())
            .await
            .map_err(db_err)?;
        let Some(row) = row else {
            // No account: definitely needs to go through registration.
            return Ok(true);
        };
        let last_ip: String = row.try_get("last_ip").map_err(db_err)?;
        let last_login_at: i64 = row.try_get("last_login_at").map_err(db_err)?;
        let session_valid =
            last_ip == ip && now_secs().saturating_sub(last_login_at) <= self.session_seconds;
        if session_valid {
            // Refresh the timestamp so a player who stays connected across
            // the session boundary doesn't get walled mid-session on some
            // future rejoin edge case; harmless no-op otherwise.
            let _ = sqlx::query(TOUCH_SESSION)
                .bind(ip)
                .bind(now_secs())
                .bind(uuid.to_string())
                .execute(pool.as_ref())
                .await;
        }
        Ok(!session_valid)
    }

    /// Starts (or restarts) a pending session, determining register-vs-login
    /// by whether an account already exists. Returns `true` for register.
    /// `current_tick` seeds `last_shown_tick` - the caller shows the actual
    /// first dialog moments later (once the player has finished spawning),
    /// but that's within the same join sequence, well inside the
    /// coarse-grained `reprompt_ticks` window either way.
    pub async fn begin(
        &self,
        uuid: Uuid,
        username: &str,
        previous_gamemode: GameMode,
        real_world: Arc<crate::world::World>,
        current_tick: i32,
    ) -> Result<bool, LoginError> {
        let pool = self.ensure_pool().await?;
        let exists = sqlx::query(SELECT_ACCOUNT)
            .bind(uuid.to_string())
            .fetch_optional(pool.as_ref())
            .await
            .map_err(db_err)?
            .is_some();

        let mode = if exists {
            PendingMode::Login { attempts: 0 }
        } else {
            PendingMode::Register
        };
        self.pending.write().await.insert(
            uuid,
            PendingAuth {
                username: username.to_string(),
                previous_gamemode,
                real_world,
                mode,
                last_shown_tick: current_tick,
            },
        );
        Ok(!exists)
    }

    pub async fn is_pending(&self, uuid: Uuid) -> bool {
        self.pending.read().await.contains_key(&uuid)
    }

    /// `true` if this pending player is registering (no account yet), `false`
    /// if logging in to an existing one. Only meaningful when `is_pending`.
    pub async fn is_registering(&self, uuid: Uuid) -> bool {
        matches!(
            self.pending.read().await.get(&uuid).map(|a| &a.mode),
            Some(PendingMode::Register)
        )
    }

    /// Processes one register/login dialog submission - the decoded NBT
    /// compound of the dialog's input values, keyed by each input's own
    /// `key` (see `DialogAction::DynamicCustom`).
    pub async fn handle_dialog_submit(
        &self,
        uuid: Uuid,
        ip: &str,
        values: &NbtCompound,
    ) -> Result<AuthOutcome, LoginError> {
        let pool = self.ensure_pool().await?;
        let mut pending = self.pending.write().await;
        let Some(auth) = pending.get_mut(&uuid) else {
            return Err(LoginError::NoPendingSession);
        };

        match &mut auth.mode {
            PendingMode::Register => {
                let (Some(password), Some(confirm)) = (
                    values.get_string("password"),
                    values.get_string("confirm_password"),
                ) else {
                    return Ok(AuthOutcome::MalformedSubmission);
                };
                if password.len() < self.min_password_length as usize {
                    return Ok(AuthOutcome::PasswordTooShort {
                        min_length: self.min_password_length,
                    });
                }
                if password != confirm {
                    return Ok(AuthOutcome::ConfirmationMismatch);
                }
                let previous_gamemode = auth.previous_gamemode;
                let real_world = auth.real_world.clone();
                let username = auth.username.clone();
                let hash = hash_password(password);
                drop(pending);
                sqlx::query(INSERT_ACCOUNT)
                    .bind(uuid.to_string())
                    .bind(username)
                    .bind(hash)
                    .bind(ip)
                    .bind(now_secs())
                    .execute(pool.as_ref())
                    .await
                    .map_err(db_err)?;
                self.pending.write().await.remove(&uuid);
                let spawn_override = Some(real_world_default_spawn(&real_world).await);
                Ok(AuthOutcome::Success {
                    previous_gamemode,
                    real_world,
                    spawn_override,
                })
            }
            PendingMode::Login { attempts } => {
                let Some(password) = values.get_string("password") else {
                    return Ok(AuthOutcome::MalformedSubmission);
                };
                let row = sqlx::query(SELECT_ACCOUNT)
                    .bind(uuid.to_string())
                    .fetch_optional(pool.as_ref())
                    .await
                    .map_err(db_err)?;
                let stored_hash: Option<String> = match row {
                    Some(row) => Some(row.try_get("password_hash").map_err(db_err)?),
                    None => None,
                };
                let matches = stored_hash.is_some_and(|h| verify_password(password, &h));
                if matches {
                    let previous_gamemode = auth.previous_gamemode;
                    let real_world = auth.real_world.clone();
                    drop(pending);
                    sqlx::query(TOUCH_SESSION)
                        .bind(ip)
                        .bind(now_secs())
                        .bind(uuid.to_string())
                        .execute(pool.as_ref())
                        .await
                        .map_err(db_err)?;
                    self.pending.write().await.remove(&uuid);
                    Ok(AuthOutcome::Success {
                        previous_gamemode,
                        real_world,
                        spawn_override: None,
                    })
                } else {
                    *attempts += 1;
                    let attempts_left = self.max_login_attempts.saturating_sub(*attempts);
                    Ok(AuthOutcome::WrongPassword { attempts_left })
                }
            }
        }
    }

    /// Drops a pending session without completing it (e.g. the player
    /// disconnected mid-login).
    pub async fn abandon(&self, uuid: Uuid) {
        self.pending.write().await.remove(&uuid);
    }

    /// Admin recovery: deletes an account so its next join starts fresh
    /// registration. Returns whether an account existed to delete.
    pub async fn reset(&self, uuid: Uuid) -> Result<bool, LoginError> {
        let pool = self.ensure_pool().await?;
        let result = sqlx::query(DELETE_ACCOUNT)
            .bind(uuid.to_string())
            .execute(pool.as_ref())
            .await
            .map_err(db_err)?;
        Ok(result.rows_affected() > 0)
    }

    /// Shows the register/login form dialog and stamps `last_shown_tick`.
    /// `error`, when `Some`, is appended as an extra message line - used to
    /// give feedback on a re-show after a failed submission (a dialog can't
    /// be edited in place; showing feedback means sending a new one).
    pub async fn show_prompt(
        &self,
        player: &Arc<Player>,
        registering: bool,
        error: Option<TextComponent>,
        current_tick: i32,
    ) {
        let name = &player.gameprofile.name;
        let mut body = Vec::new();
        let (title, greeting, inputs, action_id, button_text) = if registering {
            (
                format!("欢迎，{name}"),
                format!(
                    "{name} 您好，这是你第一次加入本服。请设置一个密码来保护你的账户，\
                     两次输入的密码一致后即可完成注册并开始游戏。"
                ),
                vec![
                    DialogInput::Text {
                        key: "password".to_string(),
                        label: TextComponent::text("密码"),
                        initial: String::new(),
                        max_length: Some(64),
                    },
                    DialogInput::Text {
                        key: "confirm_password".to_string(),
                        label: TextComponent::text("确认密码"),
                        initial: String::new(),
                        max_length: Some(64),
                    },
                ],
                REGISTER_SUBMIT_ACTION_ID,
                "完成注册",
            )
        } else {
            (
                format!("欢迎回来，{name}"),
                format!("{name} 您好，请输入密码登录你的账户。"),
                vec![DialogInput::Text {
                    key: "password".to_string(),
                    label: TextComponent::text("密码"),
                    initial: String::new(),
                    max_length: Some(64),
                }],
                LOGIN_SUBMIT_ACTION_ID,
                "登录",
            )
        };
        body.push(DialogBody::PlainMessage {
            contents: TextComponent::text(greeting),
        });
        if let Some(error) = error {
            body.push(DialogBody::PlainMessage { contents: error });
        }

        player
            .show_dialog(&Dialog {
                r#type: "minecraft:confirmation".to_string(),
                title: TextComponent::text(title),
                body,
                inputs,
                buttons: vec![ActionButton {
                    text: TextComponent::text(button_text),
                    tooltip: None,
                    width: None,
                    action: DialogAction::DynamicCustom {
                        id: action_id.to_string(),
                        additions: None,
                    },
                }],
                links: vec![],
                exit_action: None,
                after_action: None,
                can_close_with_escape: false,
                external_title: None,
            })
            .await;

        if let Some(auth) = self.pending.write().await.get_mut(&player.gameprofile.id) {
            auth.last_shown_tick = current_tick;
        }
    }

    /// Periodically re-shows the register/login dialog to every still-pending
    /// player whose dialog hasn't been (re-)sent in the last `reprompt_ticks`
    /// ticks. Called once per game tick from `Server::tick_worlds`.
    ///
    /// This is **not** reactive dismiss-detection - the server has no way to
    /// know whether a player closed the dialog via Escape or the window's
    /// own close button (see `EMBER.md`'s changelog entry for this feature
    /// for why - confirmed against the real protocol, not a gap specific to
    /// this codebase). The packet gateway already blocks every other action
    /// for a pending player regardless of whether a dialog is currently
    /// visible on their screen, so this is purely a "make sure they still
    /// have a way out" safety net, not a guarantee their submission is
    /// imminent.
    pub async fn tick(&self, server: &Arc<Server>) {
        if !self.enabled {
            return;
        }
        let current_tick = server.tick_count.load(Ordering::Relaxed);
        let due: Vec<(Uuid, bool)> = self
            .pending
            .read()
            .await
            .iter()
            .filter(|(_, auth)| {
                current_tick.saturating_sub(auth.last_shown_tick) >= self.reprompt_ticks
            })
            .map(|(uuid, auth)| (*uuid, matches!(auth.mode, PendingMode::Register)))
            .collect();
        for (uuid, registering) in due {
            if let Some(player) = server.get_player_by_uuid(uuid) {
                self.show_prompt(&player, registering, None, current_tick)
                    .await;
            }
        }
    }
}

fn hash_password(password: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("argon2 hashing should not fail for a valid salt")
        .to_string()
}

fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

/// Integration tests against a real `MySQL` instance, mirroring
/// `server::economy`'s own test module - not run by normal `cargo
/// test`/`nextest`, explicitly with:
/// `EMBER_AUTH_TEST_MYSQL_URL=mysql://user:pass@host:port/db cargo test -p pumpkin --lib server::auth::tests -- --ignored`
///
/// `begin()`/`handle_dialog_submit()`'s full state machine needs a real
/// `Arc<World>` (for the post-auth teleport-back target), which has no
/// lightweight test constructor in this codebase - those two are exercised
/// end-to-end via a live server + real client instead. What's covered here
/// is everything that doesn't need a `World`: password hashing and the
/// session/IP timing logic in `needs_auth`, both driven directly against
/// the `accounts` table.
#[cfg(test)]
mod tests {
    use super::*;

    fn test_url() -> String {
        std::env::var("EMBER_AUTH_TEST_MYSQL_URL")
            .unwrap_or_else(|_| "mysql://root:password@127.0.0.1:3306/ember_auth_test".to_string())
    }

    async fn fresh_manager() -> LoginManager {
        let url = test_url();
        let (base_url, db_name) = url
            .rsplit_once('/')
            .expect("test MySQL URL must end in /<database>");

        let admin_pool = MySqlPoolOptions::new()
            .max_connections(1)
            .connect(&format!("{base_url}/"))
            .await
            .expect("connect to MySQL server (no database selected) for test setup");
        sqlx::query(&format!("CREATE DATABASE IF NOT EXISTS {db_name}"))
            .execute(&admin_pool)
            .await
            .expect("create test database");
        admin_pool.close().await;

        let manager = LoginManager {
            enabled: true,
            url,
            pool: Arc::new(tokio::sync::OnceCell::new()),
            session_seconds: 24 * 3600,
            min_password_length: 4,
            max_login_attempts: 5,
            reprompt_ticks: 1200,
            pending: RwLock::new(HashMap::new()),
        };
        let pool = manager
            .ensure_pool()
            .await
            .expect("manager should connect to the test database");
        sqlx::query("DELETE FROM ember_login_accounts")
            .execute(pool.as_ref())
            .await
            .expect("clear previous test rows");
        manager
    }

    #[test]
    fn hash_and_verify_password_roundtrip() {
        let hash = hash_password("correct horse battery staple");
        assert!(verify_password("correct horse battery staple", &hash));
    }

    #[test]
    fn wrong_password_fails_verification() {
        let hash = hash_password("correct horse battery staple");
        assert!(!verify_password("wrong password", &hash));
    }

    #[tokio::test]
    #[ignore = "requires a local MySQL instance; see module docs for how to run"]
    async fn needs_auth_is_true_with_no_account() {
        let manager = fresh_manager().await;
        let uuid = Uuid::new_v4();
        assert!(
            manager
                .needs_auth(uuid, "127.0.0.1")
                .await
                .expect("needs_auth should succeed")
        );
    }

    #[tokio::test]
    #[ignore = "requires a local MySQL instance; see module docs for how to run"]
    async fn needs_auth_skips_recent_session_same_ip() {
        let manager = fresh_manager().await;
        let pool = manager.ensure_pool().await.unwrap();
        let uuid = Uuid::new_v4();
        sqlx::query(INSERT_ACCOUNT)
            .bind(uuid.to_string())
            .bind("Steve")
            .bind(hash_password("irrelevant"))
            .bind("127.0.0.1")
            .bind(now_secs())
            .execute(pool.as_ref())
            .await
            .expect("insert test account");

        assert!(
            !manager
                .needs_auth(uuid, "127.0.0.1")
                .await
                .expect("needs_auth should succeed"),
            "same IP within the session window should skip verification"
        );
        assert!(
            manager
                .needs_auth(uuid, "10.0.0.1")
                .await
                .expect("needs_auth should succeed"),
            "a different IP must still require verification"
        );
    }

    #[tokio::test]
    #[ignore = "requires a local MySQL instance; see module docs for how to run"]
    async fn needs_auth_is_true_after_session_expires() {
        let manager = fresh_manager().await;
        let pool = manager.ensure_pool().await.unwrap();
        let uuid = Uuid::new_v4();
        sqlx::query(INSERT_ACCOUNT)
            .bind(uuid.to_string())
            .bind("Steve")
            .bind(hash_password("irrelevant"))
            .bind("127.0.0.1")
            .bind(now_secs() - manager.session_seconds - 60)
            .execute(pool.as_ref())
            .await
            .expect("insert test account");

        assert!(
            manager
                .needs_auth(uuid, "127.0.0.1")
                .await
                .expect("needs_auth should succeed"),
            "an expired session must require verification again"
        );
    }

    #[tokio::test]
    #[ignore = "requires a local MySQL instance; see module docs for how to run"]
    async fn reset_deletes_account_and_reports_whether_one_existed() {
        let manager = fresh_manager().await;
        let pool = manager.ensure_pool().await.unwrap();
        let uuid = Uuid::new_v4();
        sqlx::query(INSERT_ACCOUNT)
            .bind(uuid.to_string())
            .bind("Steve")
            .bind(hash_password("irrelevant"))
            .bind("127.0.0.1")
            .bind(now_secs())
            .execute(pool.as_ref())
            .await
            .expect("insert test account");

        assert!(manager.reset(uuid).await.expect("reset should succeed"));
        assert!(
            !manager.reset(uuid).await.expect("reset should succeed"),
            "resetting an already-deleted account should report false"
        );
        assert!(
            manager
                .needs_auth(uuid, "127.0.0.1")
                .await
                .expect("needs_auth should succeed"),
            "the account should behave as unregistered again"
        );
    }
}
// EMBER end
