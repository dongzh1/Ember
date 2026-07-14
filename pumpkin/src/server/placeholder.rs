// EMBER start: PAPI-style placeholder system
//! A `%player_health%`-style variable/placeholder registry.
//!
//! In the spirit of Bukkit/Paper's `PlaceholderAPI` - referenced for the
//! syntax and general idea (register a name, resolve it against a player at
//! display time), not its implementation (Java/Bukkit-specific, and built
//! to let separate plugin jars each register their own "expansion").
//!
//! Ember simplifies this: every Ember system lives in the same process, so
//! there's no "which plugin jar owns this expansion" dispatch problem to
//! solve. `%player_health%` is just one flat key in one registry - the
//! `player_*`/`server_*` naming convention is kept for familiarity, but the
//! lookup itself doesn't split on the first underscore the way PAPI's
//! per-expansion dispatch does.
//!
//! This is a shared, server-level utility - `server::hud::HudManager` is
//! its first consumer, but any future system wanting `%...%` expansion
//! (chat formatting, scoreboard, NPC names, ...) can call
//! [`PlaceholderManager::register`] with its own resolvers instead of
//! reimplementing token replacement.
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tokio::sync::RwLock;

use crate::entity::player::Player;

/// The result of resolving a single placeholder for a player.
pub type PlaceholderResult<'a> = Pin<Box<dyn Future<Output = String> + Send + 'a>>;

/// A registered resolver: given a player, produce the current value.
///
/// Mirrors the existing "registered async callback" shape already used by
/// `command::node::Requirement` (`Arc<dyn Fn(&CommandSource) ->
/// RequirementResult<'_> + Send + Sync>`), for consistency with the rest of
/// the codebase's own idiom for this kind of thing.
///
/// Built-in resolvers below are plain `fn` items, not closures - a closure
/// literal annotated `|player: &Player| -> PlaceholderResult<'_> { .. }`
/// doesn't actually satisfy `for<'a> Fn(&'a Player) -> PlaceholderResult<'a>`
/// (a well-known rustc inference gap for this exact shape); a named `fn`'s
/// signature is fully explicit and coerces to the trait object cleanly.
type Resolver = Arc<dyn Fn(&Player) -> PlaceholderResult<'_> + Send + Sync>;

pub struct PlaceholderManager {
    resolvers: RwLock<HashMap<String, Resolver>>,
}

impl Default for PlaceholderManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaceholderManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            resolvers: RwLock::new(Self::builtins()),
        }
    }

    /// Registers a placeholder resolver under `name` (without the `%`
    /// delimiters, e.g. `"player_health"`). Overwrites any existing
    /// resolver with the same name - last registration wins, matching the
    /// simple "flat table" design (see module doc).
    pub async fn register(
        &self,
        name: impl Into<String>,
        resolver: impl Fn(&Player) -> PlaceholderResult<'_> + Send + Sync + 'static,
    ) {
        self.resolvers
            .write()
            .await
            .insert(name.into(), Arc::new(resolver));
    }

    /// Expands every `%name%` token in `template` against `player`, using
    /// whatever resolvers are currently registered. A token with no
    /// matching resolver is left exactly as-is (not blanked out, not an
    /// error) - a typo in a config file shows up as a literal `%typo%` in
    /// the output instead of silently vanishing, which is easier to spot.
    pub async fn resolve(&self, template: &str, player: &Player) -> String {
        let resolvers = self.resolvers.read().await;
        let mut result = String::with_capacity(template.len());
        let mut rest = template;
        while let Some(start) = rest.find('%') {
            let Some(end_rel) = rest[start + 1..].find('%') else {
                // No closing `%` - stop treating the rest as tokens.
                result.push_str(rest);
                rest = "";
                break;
            };
            let name = &rest[start + 1..start + 1 + end_rel];
            result.push_str(&rest[..start]);
            if let Some(resolver) = resolvers.get(name) {
                result.push_str(&resolver(player).await);
            } else {
                result.push('%');
                result.push_str(name);
                result.push('%');
            }
            rest = &rest[start + 1 + end_rel + 1..];
        }
        result.push_str(rest);
        result
    }

    /// Built purely as a plain local map (no lock involved) so `new()`
    /// doesn't need to be async just to seed the built-in placeholders.
    fn builtins() -> HashMap<String, Resolver> {
        let mut resolvers: HashMap<String, Resolver> = HashMap::new();
        resolvers.insert("player_name".to_string(), Arc::new(player_name));
        resolvers.insert("player_health".to_string(), Arc::new(player_health));
        resolvers.insert("player_max_health".to_string(), Arc::new(player_max_health));
        resolvers.insert("player_food".to_string(), Arc::new(player_food));
        resolvers.insert("player_x".to_string(), Arc::new(player_x));
        resolvers.insert("player_y".to_string(), Arc::new(player_y));
        resolvers.insert("player_z".to_string(), Arc::new(player_z));
        resolvers.insert("player_world".to_string(), Arc::new(player_world));
        resolvers.insert("player_gamemode".to_string(), Arc::new(player_gamemode));
        resolvers.insert("player_ping".to_string(), Arc::new(player_ping));
        resolvers.insert("server_online".to_string(), Arc::new(server_online));
        resolvers.insert("server_tps".to_string(), Arc::new(server_tps));
        resolvers.insert("server_mspt".to_string(), Arc::new(server_mspt));
        resolvers
    }
}

fn player_name(player: &Player) -> PlaceholderResult<'_> {
    Box::pin(async move { player.gameprofile.name.clone() })
}

fn player_health(player: &Player) -> PlaceholderResult<'_> {
    Box::pin(async move { format!("{:.0}", player.living_entity.health.load()) })
}

fn player_max_health(player: &Player) -> PlaceholderResult<'_> {
    Box::pin(async move { format!("{:.0}", player.living_entity.get_max_health()) })
}

fn player_food(player: &Player) -> PlaceholderResult<'_> {
    Box::pin(async move { player.hunger_manager.level.load().to_string() })
}

fn player_x(player: &Player) -> PlaceholderResult<'_> {
    Box::pin(async move { format!("{:.0}", player.position().x) })
}

fn player_y(player: &Player) -> PlaceholderResult<'_> {
    Box::pin(async move { format!("{:.0}", player.position().y) })
}

fn player_z(player: &Player) -> PlaceholderResult<'_> {
    Box::pin(async move { format!("{:.0}", player.position().z) })
}

fn player_world(player: &Player) -> PlaceholderResult<'_> {
    Box::pin(async move { player.world().get_world_name().to_string() })
}

fn player_gamemode(player: &Player) -> PlaceholderResult<'_> {
    Box::pin(async move { player.gamemode.load().name().to_string() })
}

fn player_ping(player: &Player) -> PlaceholderResult<'_> {
    Box::pin(async move { player.ping.load(Ordering::Relaxed).to_string() })
}

fn server_online(player: &Player) -> PlaceholderResult<'_> {
    Box::pin(async move {
        let Some(server) = player.world().server.upgrade() else {
            return String::new();
        };
        server.get_player_count().to_string()
    })
}

fn server_tps(player: &Player) -> PlaceholderResult<'_> {
    Box::pin(async move {
        let Some(server) = player.world().server.upgrade() else {
            return String::new();
        };
        format!("{:.1}", server.get_tps())
    })
}

fn server_mspt(player: &Player) -> PlaceholderResult<'_> {
    Box::pin(async move {
        let Some(server) = player.world().server.upgrade() else {
            return String::new();
        };
        format!("{:.1}", server.get_mspt())
    })
}
// EMBER end
