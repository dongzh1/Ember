use pumpkin_macros::Event;

/// An event that fires at the end of every server tick.
///
// EMBER start - tick soft-budget isolation
/// A world whose tick overruns its budget (see `Server::tick_worlds`) is
/// skipped rather than waited for, so this does NOT guarantee every loaded
/// world finished its tick logic when it fires - only that every world
/// within budget did. `duration_nanos` reflects that same budget, not the
/// straggler's actual runtime.
// EMBER end
#[derive(Event, Clone)]
pub struct ServerTickEndEvent {
    /// 0-indexed number of the tick that just finished.
    pub tick: i32,

    /// Duration (in nanoseconds) of the tick that just finished.
    pub duration_nanos: i64,
}

impl ServerTickEndEvent {
    /// Creates a new `ServerTickEndEvent`.
    #[must_use]
    pub const fn new(tick: i32, duration_nanos: i64) -> Self {
        Self {
            tick,
            duration_nanos,
        }
    }
}
