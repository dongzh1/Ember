// EMBER start - /tpa teleport request system
//! Pending `/tpa`/`/tpahere` teleport requests.
//!
//! One pending request per recipient — a new request from anyone replaces
//! an older, still-unanswered one — and requests expire after
//! `REQUEST_TIMEOUT_SECS` so a stale `/tpaaccept` can't fire a teleport
//! nobody still expects.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::RwLock;
use uuid::Uuid;

pub const REQUEST_TIMEOUT_SECS: u64 = 120;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TpaKind {
    /// `/tpa`: the requester wants to teleport to the recipient.
    To,
    /// `/tpahere`: the requester wants the recipient to teleport to them.
    Here,
}

struct PendingTpa {
    from: Uuid,
    from_name: String,
    kind: TpaKind,
    requested_at: Instant,
}

pub struct TpaManager {
    /// Keyed by the recipient — the player who must `/tpaaccept`/`/tpadeny`.
    pending: RwLock<HashMap<Uuid, PendingTpa>>,
}

impl TpaManager {
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: RwLock::new(HashMap::new()),
        }
    }

    pub async fn request(&self, from: Uuid, from_name: String, to: Uuid, kind: TpaKind) {
        self.pending.write().await.insert(
            to,
            PendingTpa {
                from,
                from_name,
                kind,
                requested_at: Instant::now(),
            },
        );
    }

    /// Removes and returns `recipient`'s pending request as
    /// `(from, from_name, kind)`, or `None` if there isn't one or it expired.
    pub async fn take(&self, recipient: Uuid) -> Option<(Uuid, String, TpaKind)> {
        let mut pending = self.pending.write().await;
        let entry = pending.remove(&recipient)?;
        if entry.requested_at.elapsed() > Duration::from_secs(REQUEST_TIMEOUT_SECS) {
            return None;
        }
        Some((entry.from, entry.from_name, entry.kind))
    }
}

impl Default for TpaManager {
    fn default() -> Self {
        Self::new()
    }
}
// EMBER end
