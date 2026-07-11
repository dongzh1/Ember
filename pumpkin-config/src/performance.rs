use serde::{Deserialize, Serialize};

// EMBER start - server-wide concurrency tuning
/// Server-wide performance/concurrency tuning.
///
/// Unlike per-world settings (`LevelConfig`), these govern process-wide
/// shared resources that exist once regardless of how many worlds are
/// loaded, so they don't belong on a per-world config.
#[derive(Deserialize, Serialize, Default)]
#[serde(default)]
pub struct PerformanceConfig {
    /// Caps how many chunk-generation jobs may be in flight at once across
    /// ALL worlds sharing the process-wide generation thread pool. `0`
    /// (default) means unlimited: each world computes its own limit
    /// independently, with no cross-world awareness, matching today's
    /// behaviour. Only raise this if multiple worlds generating chunks at
    /// once are starving each other's throughput.
    pub max_concurrent_world_gen_jobs: u32,
}
// EMBER end
