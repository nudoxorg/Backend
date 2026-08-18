//! Automatic compaction policy (09-vector §13.4; 09c §1.1 "no background
//! optimizers" patch).
//!
//! Edge has no background optimizer thread: forgetting `optimize()` means
//! tombstone bloat and latency death. So compaction is **automatic and
//! counter-driven**, never per-upsert and never on a UI thread:
//!
//! - ≥ 5 000 upserts since the last compact **and** the store has been
//!   write-idle ≥ 30 s, or
//! - the approximate deleted ratio reaches 20 %.
//!
//! [`CompactPolicy`] is a pure counter machine (unit-testable with injected
//! `Instant`s); [`spawn_compactor`] is the async idle scheduler that
//! consults it and calls `store.compact()`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::vector::core::JinaCodeV2;

use super::store::LocalShardStore;

/// Compact after this many upserts, once write-idle (09-vector §13.4).
pub const COMPACT_UPSERT_THRESHOLD: u64 = 5_000;

/// Required write-idle window before an upsert-triggered compact.
pub const COMPACT_IDLE: Duration = Duration::from_secs(30);

/// Compact when at least this fraction of the shard is tombstones.
pub const COMPACT_DELETED_RATIO: f64 = 0.2;

/// Telemetry counters deciding *when* to compact. Shared between the store
/// (which records writes) and the scheduler (which asks
/// [`Self::should_compact`]).
#[derive(Debug, Default)]
pub struct CompactPolicy {
    upserts_since_compact: u64,
    deletes_since_compact: u64,
    /// Live-point baseline as of the last compact; denominator input for
    /// the approximate deleted ratio.
    live_points: u64,
    last_write: Option<Instant>,
}

impl CompactPolicy {
    pub fn record_upserts(&mut self, count: u64, now: Instant) {
        self.upserts_since_compact += count;
        self.live_points += count;
        self.last_write = Some(now);
    }

    pub fn record_deletes(&mut self, count: u64, now: Instant) {
        self.deletes_since_compact += count;
        self.last_write = Some(now);
    }

    /// Approximate fraction of shard points that are tombstones:
    /// `deletes / (live + deletes)` against the post-compact baseline.
    pub fn deleted_ratio(&self) -> f64 {
        let total = self.live_points + self.deletes_since_compact;
        if total == 0 {
            return 0.0;
        }
        self.deletes_since_compact as f64 / total as f64
    }

    /// §13.4 trigger: bulk upserts once idle, or tombstone pressure
    /// regardless of idleness.
    pub fn should_compact(&self, now: Instant) -> bool {
        let idle = self
            .last_write
            .is_none_or(|last| now.duration_since(last) >= COMPACT_IDLE);
        let upsert_pressure = self.upserts_since_compact >= COMPACT_UPSERT_THRESHOLD && idle;
        upsert_pressure || self.deleted_ratio() >= COMPACT_DELETED_RATIO
    }

    /// Reset after a successful compact; `live_points` is the store's
    /// post-compact cardinality.
    pub fn note_compacted(&mut self, live_points: u64) {
        self.upserts_since_compact = 0;
        self.deletes_since_compact = 0;
        self.live_points = live_points;
    }
}

/// Spawn the idle compaction task for a store: every `check_interval` it
/// consults the shared [`CompactPolicy`] and, when triggered, runs
/// `store.compact()` (which resets the counters).
///
/// Stop it with `JoinHandle::abort` (e.g. right before
/// [`LocalShardStore::close`]). Aborting between optimize passes is safe —
/// Edge operations complete on the actor thread.
pub fn spawn_compactor(
    store: Arc<LocalShardStore>,
    check_interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(check_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            let due = {
                let policy = store.compact_policy();
                let policy = policy
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                policy.should_compact(Instant::now())
            };
            if !due {
                continue;
            }
            tracing::debug!("compact triggered by policy counters");
            if let Err(err) = VectorStore::<JinaCodeV2>::compact(&*store).await {
                tracing::warn!(%err, "background compact failed; will retry on a later tick");
            }
        }
    })
}

// Re-import under the trait name for the scheduler body above.
use crate::vector::core::VectorStore;

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Instant {
        Instant::now()
    }

    #[test]
    fn fresh_policy_does_not_compact() {
        let policy = CompactPolicy::default();
        assert!(!policy.should_compact(now()));
    }

    #[test]
    fn upsert_threshold_requires_idle() {
        let start = now();
        let mut policy = CompactPolicy::default();
        policy.record_upserts(COMPACT_UPSERT_THRESHOLD, start);

        // Not idle yet.
        assert!(!policy.should_compact(start + Duration::from_secs(5)));
        // Idle window elapsed.
        assert!(policy.should_compact(start + COMPACT_IDLE));
    }

    #[test]
    fn below_upsert_threshold_never_triggers_on_idle_alone() {
        let start = now();
        let mut policy = CompactPolicy::default();
        policy.record_upserts(COMPACT_UPSERT_THRESHOLD - 1, start);
        assert!(!policy.should_compact(start + COMPACT_IDLE * 10));
    }

    #[test]
    fn deleted_ratio_triggers_even_while_busy() {
        let start = now();
        let mut policy = CompactPolicy::default();
        policy.note_compacted(1_000);
        // 250 deletes over 1000 live → 250/1250 = 0.2 exactly.
        policy.record_deletes(250, start);
        assert!(policy.deleted_ratio() >= COMPACT_DELETED_RATIO);
        // Deleted-pressure path ignores the idle window.
        assert!(policy.should_compact(start));
    }

    #[test]
    fn deleted_ratio_below_threshold_does_not_trigger() {
        let start = now();
        let mut policy = CompactPolicy::default();
        policy.note_compacted(1_000);
        policy.record_deletes(100, start); // 100/1100 ≈ 0.09
        assert!(!policy.should_compact(start));
    }

    #[test]
    fn note_compacted_resets_counters() {
        let start = now();
        let mut policy = CompactPolicy::default();
        policy.record_upserts(COMPACT_UPSERT_THRESHOLD, start);
        policy.record_deletes(10_000, start);
        policy.note_compacted(42);
        assert_eq!(policy.deletes_since_compact, 0);
        assert!(!policy.should_compact(start + COMPACT_IDLE));
    }

    #[test]
    fn empty_store_ratio_is_zero() {
        let policy = CompactPolicy::default();
        assert_eq!(policy.deletes_since_compact, 0);
        assert_eq!(policy.live_points, 0);
    }
}
