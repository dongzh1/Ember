// EMBER start - built-in shop/bank/market/lottery system
//! One-shot chat input capture.
//!
//! The bank's "custom amount" buttons and the market's "search" both need a
//! moment of free-text input, and this is the only place in Ember that
//! needs one - there's no existing form/prompt UI to reuse, so this is new,
//! minimal, shared infrastructure (mirroring `PixelShop`'s own single
//! `addChatCatch` mechanism reused by both features).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{RwLock, oneshot};
use uuid::Uuid;

const CAPTURE_TIMEOUT: Duration = Duration::from_mins(1);
const CANCEL_WORD: &str = "取消";

struct PendingCapture {
    sender: oneshot::Sender<Option<String>>,
}

/// Registers and resolves one-shot chat captures.
///
/// A player can have at most one pending capture; starting a new one
/// silently replaces theirs (the old receiver just resolves to `None`, as
/// if it had been cancelled).
#[derive(Default)]
pub struct ChatCaptureManager {
    pending: RwLock<HashMap<Uuid, PendingCapture>>,
}

impl ChatCaptureManager {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts capturing `player`'s next chat message. Resolves to `Some(text)`
    /// on a real message, or `None` if they type the cancel word
    /// (`取消`) or don't respond within 60 seconds.
    pub async fn capture(self: &Arc<Self>, player: Uuid) -> oneshot::Receiver<Option<String>> {
        let (tx, rx) = oneshot::channel();
        let previous = self
            .pending
            .write()
            .await
            .insert(player, PendingCapture { sender: tx });
        if let Some(old) = previous {
            let _ = old.sender.send(None);
        }

        let this = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(CAPTURE_TIMEOUT).await;
            let expired = this.pending.write().await.remove(&player);
            if let Some(capture) = expired {
                let _ = capture.sender.send(None);
            }
        });

        rx
    }

    /// Called from the chat-message handler before any normal chat
    /// processing. Returns `true` if `message` was consumed as a capture
    /// (the caller must not broadcast it as regular chat in that case).
    pub async fn try_consume(&self, player: Uuid, message: &str) -> bool {
        let Some(capture) = self.pending.write().await.remove(&player) else {
            return false;
        };
        let result = if message.trim() == CANCEL_WORD {
            None
        } else {
            Some(message.to_string())
        };
        let _ = capture.sender.send(result);
        true
    }
}
// EMBER end
