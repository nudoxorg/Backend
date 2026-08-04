//! Channel registry — capacities and overflow policies (GUI-PLAN Appendix C).
//!
//! ## Why this module exists
//!
//! Every bounded channel in the bridge kernel is declared here, with its
//! capacity and overflow policy, **before** the channel is constructed at the
//! call site. This makes capacity decisions auditable in one place and prevents
//! "magic number" capacities (`flume::bounded(128)`) from scattering throughout
//! stores and engine code with no explanation.
//!
//! ## Overflow policies
//!
//! | Policy             | Behaviour when full                                    |
//! |--------------------|--------------------------------------------------------|
//! | `Backpressure`     | Sender `.send_async().await` blocks until space opens. |
//! |                    | Used for protocol-paced streams (search pages, doc     |
//! |                    | sections) where the client yields between events.      |
//! | `CoalesceKeepLatest` | Sender keeps the newest value; old values are       |
//! |                    | discarded. Implemented **client-side** (before the     |
//! |                    | channel) so the capacity here is the honest maximum    |
//! |                    | simultaneous distinct keys in-flight. Used for progress|
//! |                    | events (idempotent state, not a log).                  |
//! | `DropOldest`       | Ring semantics: newest events overwrite oldest. Used   |
//! |                    | for log lines where the ring is the data structure.    |
//!
//! Note: `CoalesceKeepLatest` and `DropOldest` are both implemented client-
//! side (in the engine or producer), so the `flume::bounded(cap)` call at the
//! store still uses `Backpressure` semantics at the Rust level. The policy enum
//! here documents the *intended* semantic; the engine enforces it.
//!
//! ## Adding a new channel
//!
//! 1. Add a constant here with the appropriate `ChannelSpec`.
//! 2. Cite it at the channel-construction call site:
//!    `flume::bounded(SEARCH.capacity)`.
//! 3. If the policy is not `Backpressure`, document how the client enforces
//!    the policy in a comment on the constant.

/// The overflow policy for a bounded channel.
///
/// `#[non_exhaustive]` so adding a new policy variant (e.g. `DropNewest`) is
/// not a breaking change to downstream matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Overflow {
    /// The sender awaits capacity. Correct for protocol-paced, ordered streams.
    Backpressure,
    /// Stale values for a given key are discarded; the newest survives.
    /// Implemented client-side before the channel. Correct for progress events.
    CoalesceKeepLatest,
    /// Oldest elements are silently dropped when the channel is full.
    /// Implemented client-side or by a wrapper that evicts. Correct for logs.
    DropOldest,
}

/// The capacity and overflow policy for one channel.
#[derive(Clone, Copy, Debug)]
pub struct ChannelSpec {
    /// Maximum number of events that can be buffered.
    pub capacity: usize,
    /// What happens when the channel is full and the sender tries to send.
    pub overflow: Overflow,
}

// ── Channel registry (GUI-PLAN Appendix C) ────────────────────────────────────

/// `ClientHandle::search` → `SearchStore` event stream.
///
/// Pages are few and small; backpressure is correct here — the client yields
/// between pages and the UI is always ready.
pub const SEARCH: ChannelSpec = ChannelSpec {
    capacity: 256,
    overflow: Overflow::Backpressure,
};

/// `ClientHandle::open_symbol` → `SymbolStore` `DocEvent` stream.
///
/// The protocol is self-paced: the client emits a `Section`, optionally waits
/// for acknowledgment of highlight priority, then emits the next. Backpressure
/// is correct.
pub const OPEN_SYMBOL: ChannelSpec = ChannelSpec {
    capacity: 128,
    overflow: Overflow::Backpressure,
};

/// `ClientHandle::open_package` → `SymbolStore`/`RegistryStore` events.
///
/// Tree levels + chunk events; bounded by the depth of the module tree.
pub const OPEN_PACKAGE: ChannelSpec = ChannelSpec {
    capacity: 128,
    overflow: Overflow::Backpressure,
};

/// `ClientHandle::resolve_project` → `ProjectStore` events.
///
/// Bounded by the number of dependency pages in the resolved workspace.
pub const RESOLVE_PROJECT: ChannelSpec = ChannelSpec {
    capacity: 64,
    overflow: Overflow::Backpressure,
};

/// Long-lived `sync()` progress stream → `RegistryStore`.
///
/// Progress is idempotent state: only the latest value per generation matters.
/// The engine coalesces updates client-side (keep-latest per generation id)
/// before sending, so the channel capacity here bounds the number of distinct
/// generations that can be concurrently in-flight.
pub const SYNC_PROGRESS: ChannelSpec = ChannelSpec {
    capacity: 64,
    overflow: Overflow::CoalesceKeepLatest,
};

/// Long-lived `jobs()` stream → `JobStore`.
///
/// Lifecycle events (started, completed, failed) must not be dropped —
/// backpressure for those. Progress events within a job are coalesced
/// client-side (keep-latest per job id). The capacity here is the maximum
/// number of concurrent jobs × average events in-flight.
pub const JOBS: ChannelSpec = ChannelSpec {
    capacity: 128,
    overflow: Overflow::CoalesceKeepLatest, // progress; lifecycle uses backpressure client-side
};

/// Log lines → `LogStore` ring buffer.
///
/// Ring semantics end-to-end: if the UI falls behind a 5 000 line/s flood,
/// the oldest lines are overwritten. This is the designed behaviour (§20).
pub const LOG_LINES: ChannelSpec = ChannelSpec {
    capacity: 1024,
    overflow: Overflow::DropOldest,
};

/// GUI → client intent/command channel.
///
/// Uses `try_send` (non-blocking). If the channel is full, the engine emits
/// `JobEvent::CommandRejected`; the UI surfaces that as a transient notice.
/// The channel must **never** block the foreground thread (GUI-PLAN §2.2.2).
pub const COMMAND: ChannelSpec = ChannelSpec {
    capacity: 64,
    overflow: Overflow::Backpressure, // try_send at call site; overflow → CommandRejected
};

/// Graph layout worker → canvas element.
///
/// Only the freshest layout position set is useful; older iterations are
/// superseded immediately. Capacity 2 ensures the canvas can always pick up
/// the newest without stalling the layout thread.
pub const GRAPH_LAYOUT: ChannelSpec = ChannelSpec {
    capacity: 2,
    overflow: Overflow::CoalesceKeepLatest,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_capacities_positive() {
        let specs = [
            SEARCH,
            OPEN_SYMBOL,
            OPEN_PACKAGE,
            RESOLVE_PROJECT,
            SYNC_PROGRESS,
            JOBS,
            LOG_LINES,
            COMMAND,
            GRAPH_LAYOUT,
        ];
        for spec in specs {
            assert!(spec.capacity > 0, "capacity must be positive: {spec:?}");
        }
    }

    #[test]
    fn command_channel_capacity_matches_plan() {
        // GUI-PLAN Appendix C: command channel capacity = 64.
        assert_eq!(COMMAND.capacity, 64);
    }

    #[test]
    fn log_lines_use_drop_oldest() {
        assert_eq!(LOG_LINES.overflow, Overflow::DropOldest);
    }

    #[test]
    fn search_uses_backpressure() {
        assert_eq!(SEARCH.overflow, Overflow::Backpressure);
    }

    #[test]
    fn sync_progress_coalesces() {
        assert_eq!(SYNC_PROGRESS.overflow, Overflow::CoalesceKeepLatest);
    }

    #[test]
    fn graph_layout_capacity_is_two() {
        // GUI-PLAN Appendix C: graph layout = 2, keep-latest.
        assert_eq!(GRAPH_LAYOUT.capacity, 2);
        assert_eq!(GRAPH_LAYOUT.overflow, Overflow::CoalesceKeepLatest);
    }
}
