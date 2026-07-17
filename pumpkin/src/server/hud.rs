// EMBER start: HUD system (boss-bar display, references BetterHud)
//! A per-player, persistently-displayed boss bar HUD.
//!
//! In the spirit of the Paper/Spigot plugin `BetterHud` - referenced for
//! the general idea (a constantly-refreshed on-screen readout of player/
//! server info, rendered through the boss bar rather than actionbar/
//! scoreboard) and not its implementation (Java/Bukkit-specific).
//!
//! Architecturally the same shape as `server::npc::NpcManager`: one global
//! manager, ticked from `Server::tick_worlds`, iterating every online
//! player directly (`Server::get_all_players`) rather than per-world - a
//! player's HUD on/off preference isn't information about which world
//! they're standing in, so unlike `server::furniture`/`server::custom_block`
//! this doesn't need to be owned per-`World`.
//!
//! Content comes from `server::placeholder::PlaceholderManager` - this
//! module only decides *when* to refresh and *where* to render (the boss
//! bar), not how any individual `%...%` token gets its value.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use pumpkin_config::{
    HudConfig, HudPlayerStateConfig, HudPlayerStateListConfig, LoadConfiguration,
};
use pumpkin_util::text::TextComponent;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::entity::player::Player;
use crate::server::Server;
use crate::world::bossbar::Bossbar;

pub struct HudManager {
    exec_dir: PathBuf,
    config: RwLock<HudConfig>,
    player_state: RwLock<HudPlayerStateListConfig>,
    /// Which boss bar (by its own uuid) is currently shown to a given
    /// player, if any - `tick` updates it in place; `set_enabled` creates
    /// or removes it immediately rather than waiting for the next refresh.
    active: RwLock<HashMap<Uuid, Uuid>>,
}

impl Default for HudManager {
    fn default() -> Self {
        Self::new()
    }
}

impl HudManager {
    #[must_use]
    pub fn new() -> Self {
        let exec_dir = std::env::current_dir().expect("Failed to get current directory");
        let config = HudConfig::load(&exec_dir);
        let player_state = HudPlayerStateListConfig::load(&exec_dir);
        Self {
            exec_dir,
            config: RwLock::new(config),
            player_state: RwLock::new(player_state),
            active: RwLock::new(HashMap::new()),
        }
    }

    /// Re-reads `hud/hud.toml` from disk (`/hud reload`). Per-player on/off
    /// preferences aren't affected - those live in a separate file.
    pub async fn reload(&self) {
        *self.config.write().await = HudConfig::load(&self.exec_dir);
    }

    /// Drops a disconnected player's `active` bossbar-uuid record.
    ///
    /// `active` outlives any single connection (it's server-lifetime state
    /// on this manager, not per-`Player`), so without this a rejoining
    /// player's very next HUD refresh would see a stale entry, believe
    /// their (brand new, empty) client already has that bossbar, and send
    /// an *update* instead of the *add* the client actually needs -
    /// `ClientboundBossEventPacket`'s update-name/update-health operations
    /// crash a real client with a `NullPointerException` when the bossbar
    /// they name isn't in its local map (confirmed against a real client's
    /// log). No packet needs to go out here - the connection is already
    /// gone, there's nothing left to tell it to remove.
    pub async fn player_disconnected(&self, uuid: Uuid) {
        self.active.write().await.remove(&uuid);
    }

    async fn is_enabled_for(&self, uuid: Uuid, config: &HudConfig) -> bool {
        self.player_state
            .read()
            .await
            .players
            .iter()
            .find(|p| p.uuid == uuid)
            .map_or(config.enabled_by_default, |p| p.enabled)
    }

    /// Same as `is_enabled_for`, but reads its own current config - for
    /// callers (like `/hud toggle`) that just want to know the current
    /// state without also needing a `HudConfig` on hand.
    pub async fn is_enabled_for_command(&self, uuid: Uuid) -> bool {
        let config = self.config.read().await.clone();
        self.is_enabled_for(uuid, &config).await
    }

    /// Sets a player's HUD preference, persists it, and immediately shows
    /// or hides their boss bar (doesn't wait for the next `tick`). Returns
    /// `false` if `enabled` was requested but the feature's own master
    /// switch (`hud.toml`'s `enabled`) is currently off — the preference is
    /// still saved (so it takes effect the moment an admin flips the master
    /// switch back on), it just doesn't show a boss bar yet, matching
    /// `tick`'s own gate (which would otherwise show one once here and then
    /// never refresh it again).
    pub async fn set_enabled(&self, player: &Player, enabled: bool) -> bool {
        let uuid = player.gameprofile.id;

        let mut state = self.player_state.write().await;
        if let Some(entry) = state.players.iter_mut().find(|p| p.uuid == uuid) {
            entry.enabled = enabled;
        } else {
            state.players.push(HudPlayerStateConfig { uuid, enabled });
        }
        state.save(&self.exec_dir);
        drop(state);

        if !enabled {
            let removed = self.active.write().await.remove(&uuid);
            if let Some(bossbar_uuid) = removed {
                player.remove_bossbar(bossbar_uuid).await;
            }
            return true;
        }

        let config = self.config.read().await.clone();
        if !config.enabled {
            return false;
        }
        self.show_or_update(player, &config).await;
        true
    }

    /// Re-evaluates every online player's HUD content on the configured
    /// interval. Called once per game tick from `Server::tick_worlds`.
    pub async fn tick(&self, server: &Arc<Server>) {
        let config = self.config.read().await.clone();
        if !config.enabled {
            return;
        }
        let refresh_ticks = i64::from(config.refresh_ticks.max(1));
        if server.tick_count.load(Ordering::Relaxed) as i64 % refresh_ticks != 0 {
            return;
        }

        for player in server.get_all_players() {
            if self.is_enabled_for(player.gameprofile.id, &config).await {
                self.show_or_update(&player, &config).await;
            }
        }
    }

    async fn show_or_update(&self, player: &Player, config: &HudConfig) {
        let text = player
            .world()
            .server
            .upgrade()
            .expect("player's world has no server")
            .placeholder_manager
            .resolve(&config.title, player)
            .await;
        let mut title = TextComponent::text(text);
        if !config.font.is_empty() {
            title = title.font(config.font.clone());
        }

        let max_health = player.living_entity.get_max_health();
        let health_pct = if max_health > 0.0 {
            (player.living_entity.health.load() / max_health).clamp(0.0, 1.0)
        } else {
            0.0
        };

        let uuid = player.gameprofile.id;
        let existing = *self.active.read().await.get(&uuid).unwrap_or(&Uuid::nil());
        if existing.is_nil() {
            let bossbar = Bossbar {
                health: health_pct,
                ..Bossbar::new(title)
            };
            let bossbar_uuid = bossbar.uuid;
            player.send_bossbar(&bossbar).await;
            self.active.write().await.insert(uuid, bossbar_uuid);
        } else {
            player.update_bossbar_title(&existing, title).await;
            player.update_bossbar_health(&existing, health_pct).await;
        }
    }
}
// EMBER end
