//! The usage ledger: accumulate tool calls locally, flush them in batches, and
//! survive a crash without losing or double-counting.
//!
//! # Why this exists at all
//!
//! Every `tools/call` is a billable `tool_call`. A synchronous
//! `POST v1/usage/record` before answering would add a full internet round trip
//! — tens to hundreds of milliseconds — to every query against a *local*
//! documentation index, which is the one thing this product is supposed to be
//! fast at. So the request path does one thing: increment a counter and write
//! a ~300-byte file without `fsync`. Everything else happens on a timer.
//!
//! The shape is the one every metering and telemetry client converges on —
//! flush on a size trigger *or* a time trigger, whichever comes first, plus an
//! explicit flush on shutdown. OpenTelemetry's `BatchSpanProcessor` ships
//! `maxQueueSize` 2048, `scheduledDelayMillis` 5000, `maxExportBatchSize` 512
//! (<https://opentelemetry.io/docs/specs/otel/trace/sdk/>); Amberflo's metering
//! SDK ships `batch_size` 100 with a 0.5 s interval
//! (<https://github.com/amberflo/metering-python>). Our numbers are larger in
//! time and smaller in size ([`FLUSH_INTERVAL`], [`MAX_BATCH`]) because a
//! billing unit is not a span: nobody is waiting for it, and a chatty billing
//! endpoint is a cost centre.
//!
//! # Durability: what a crash actually loses
//!
//! OpenTelemetry's queue is memory-only and says so; Sentry writes each
//! envelope to disk before sending
//! (<https://develop.sentry.dev/sdk/foundations/transport/offline-caching/>).
//! We are closer to Sentry: [`UsageLedger::record_call`] persists on **every
//! call**, using [`super::cache::Durability::Fast`] — atomic `rename`, no
//! `fsync`. So a process crash, which is the realistic loss event for a desktop
//! app, loses nothing; a power cut can lose the last few calls. Sealing a batch
//! and shutting down both use [`super::cache::Durability::Durable`], because
//! those are the moments where a loss costs money in a specific direction.
//!
//! # The hard part: a timeout that may have succeeded
//!
//! `POST v1/usage/record` returns 204 or 429 and **no body**, and the contract
//! offers no idempotency key. That is not an implementation detail we can
//! engineer around; it is a property of the protocol we were handed. Every
//! metering vendor that solves this solves it the same way — by having the
//! client supply a key the *server* stores and deduplicates on: OpenMeter
//! deduplicates CloudEvents by `source` + `id` over a 32-day window
//! (<https://openmeter.io/docs/getting-started/event-ingestion>), Lago
//! deduplicates on `transaction_id`
//! (<https://getlago.com/docs/api-reference/events/batch>), Stripe caches the
//! full response under an idempotency key for at least 24 hours
//! (<https://docs.stripe.com/api/idempotent_requests>). Stripe's own guidance
//! for the ambiguous case is to *"retry such requests with the same idempotency
//! keys ... until they're able to receive a result"*
//! (<https://docs.stripe.com/error-low-level>) — advice that presupposes the
//! key.
//!
//! Without one, a client is choosing a bias, not eliminating an ambiguity:
//!
//! * **At-least-once** — retry the timeout. Risks charging the user for work
//!   they did not do.
//! * **At-most-once** — drop the batch. Risks not charging for work they did.
//!
//! **This client chooses at-most-once**, and it is a deliberate product call:
//! over-billing a developer for calls they never made is a support ticket and a
//! trust problem, and under-billing is a cost. OpenMeter argues the opposite
//! default — *"it's better to report usage twice and filter out duplicates
//! later than to underreport it"*
//! (<https://openmeter.io/blog/usage-deduplication>) — and they are right,
//! *given a server that can filter*. We have not got one. `docs/auth.md` § "Gaps in
//! the contract" asks for one, and the day it exists this policy flips.
//!
//! # Reconciliation makes the drop rare rather than routine
//!
//! Before dropping an ambiguous batch, the ledger asks `GET v1/usage` — the one
//! endpoint that is authoritative about how much the account has actually been
//! charged — and compares. [`reconcile`] is that comparison, as a pure
//! function, and it has three outcomes rather than two, because a third party
//! recording usage concurrently can make the answer genuinely unknowable.
//!
//! Its bias is stated and tested: concurrent activity on the account inflates
//! the observed total, which pushes an ambiguous batch toward
//! [`Reconciliation::Landed`], which means we *do not* re-send it, which means
//! we under-count. Every uncertainty in this module resolves in the user's
//! favour, and that is a design property, not a coincidence.
//!
//! # Drops are counted and visible, never silent
//!
//! Doctrine §8: a repair — and dropping a billable batch is one — is permitted
//! only when it is typed, counted, bounded and visible. [`DroppedTally`] is the
//! count, it is derived from the batches actually dropped rather than
//! accumulated beside them, it is persisted, and
//! [`UsageLedger::unreported_calls`] puts it where the status bar can show it.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use super::cache::{Durability, write_atomically};
use super::credential::KeyFingerprint;
use super::state::QuotaSnapshot;

/// How often the flusher wakes.
///
/// Thirty seconds rather than OpenTelemetry's five: a tool call is a billing
/// unit, not a trace span, and nothing downstream is waiting on it. At one
/// request every thirty seconds an all-day session costs the service ~1,000
/// writes, which is the same order as the tool calls themselves.
pub const FLUSH_INTERVAL: Duration = Duration::from_secs(30);

/// Flush early once this many calls are pending.
///
/// The size trigger is what keeps a burst — an agent running a long
/// investigation — from sitting unreported for the whole interval.
pub const FLUSH_THRESHOLD: u32 = 25;

/// The most calls one `POST v1/usage/record` may carry.
///
/// Caps the blast radius of a single ambiguous flush: at most this many calls
/// can be dropped by one timeout.
pub const MAX_BATCH: u32 = 100;

/// The on-disk file name.
pub const LEDGER_FILE: &str = "usage-ledger.json";

/// The on-disk format version.
const FORMAT_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Reconciliation
// ---------------------------------------------------------------------------

/// What `GET v1/usage` says about an ambiguous batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reconciliation {
    /// The service's total already includes the batch. Settle it; do not
    /// re-send.
    Landed,
    /// The service's total does not include the batch. Re-queue it; re-sending
    /// is safe.
    NotLanded,
    /// The observed total sits between the two expectations — something else
    /// recorded usage on this account while we were asking. Neither answer is
    /// supportable, so the batch is dropped and counted.
    Indeterminate,
}

/// Decide whether a batch of `batch` calls is reflected in `observed`.
///
/// * `anchor` — `tool_calls` from the last `GET v1/usage` we trusted.
/// * `acked_since` — calls the service has since 204'd, which are therefore in
///   its total but were not in `anchor`.
/// * `batch` — the size of the batch whose fate is unknown.
/// * `observed` — `tool_calls` from a `GET v1/usage` taken *after* the
///   ambiguous POST.
///
/// # Why `>=` and `<=` rather than `==`
///
/// Because this account is not necessarily ours alone to move: a second
/// machine, or the dashboard, can record between our anchor and our
/// observation. Exact equality would then be false in both directions and every
/// reconciliation would come back `Indeterminate`, which is the answer that
/// costs money. The inequalities absorb concurrent activity, at the price of a
/// stated bias — see the module docs.
pub fn reconcile(anchor: u64, acked_since: u64, batch: u32, observed: u64) -> Reconciliation {
    let without = anchor.saturating_add(acked_since);
    let with = without.saturating_add(u64::from(batch));

    if observed >= with {
        Reconciliation::Landed
    } else if observed <= without {
        Reconciliation::NotLanded
    } else {
        Reconciliation::Indeterminate
    }
}

// ---------------------------------------------------------------------------
// Persisted state
// ---------------------------------------------------------------------------

/// A batch that has been handed to the transport and not yet resolved.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InflightBatch {
    /// How many calls it carries.
    pub count: u32,
    /// When it was sealed, for diagnostics and for the "this looks stuck" case.
    pub sealed_at: SystemTime,
}

/// How much usage this client is known to have failed to report.
///
/// Persisted, surfaced, and never reset by anything except a successful
/// reconciliation. Doctrine §8's "counted and visible": the alternative is a
/// number that only exists in the difference between two systems nobody
/// compares.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DroppedTally {
    /// How many batches were dropped.
    pub batches: u64,
    /// How many calls those batches carried.
    pub calls: u64,
}

/// The anchor a reconciliation is measured against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Anchor {
    /// `tool_calls` at the last trusted `GET v1/usage`.
    tool_calls: u64,
    /// Calls 204'd since then.
    acked_since: u64,
}

/// Everything the ledger keeps across a restart.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct LedgerFile {
    version: u32,
    /// Which account these counts belong to.
    ///
    /// A ledger is only valid for the key that accrued it. Signing in as
    /// someone else must not bill them for the previous user's calls.
    fingerprint: Option<KeyFingerprint>,
    /// Calls recorded and not yet sealed into a batch.
    pending: u32,
    /// A batch handed to the transport whose fate is unknown.
    inflight: Option<InflightBatch>,
    /// Usage we know we failed to report.
    dropped: DroppedTally,
    /// The reconciliation anchor.
    anchor: Option<Anchor>,
}

impl Default for LedgerFile {
    fn default() -> Self {
        Self {
            version: FORMAT_VERSION,
            fingerprint: None,
            pending: 0,
            inflight: None,
            dropped: DroppedTally::default(),
            anchor: None,
        }
    }
}

// ---------------------------------------------------------------------------
// UsageLedger
// ---------------------------------------------------------------------------

/// A batch removed from `pending` and handed to the caller to send.
///
/// Carries no way to construct itself outside this module, so the only batch a
/// caller can send is one the ledger sealed and persisted first. That is the
/// invariant crash-safety rests on: there is no window in which calls have left
/// `pending`, are on the wire, and are recorded nowhere.
#[derive(Debug)]
pub struct SealedBatch {
    count: u32,
}

impl SealedBatch {
    /// How many calls this batch carries.
    pub fn count(&self) -> u32 {
        self.count
    }
}

/// The local accumulator for billable tool calls.
///
/// Cheap to clone into a background task: the state is behind one `Mutex` and
/// the ledger is normally held in an `Arc`.
pub struct UsageLedger {
    /// `None` when no state directory could be resolved. The ledger still
    /// counts in memory — a session with no writable home is a degraded session,
    /// not a free one — and says so through [`UsageLedger::is_persistent`].
    path: Option<PathBuf>,
    inner: Mutex<LedgerFile>,
}

impl UsageLedger {
    /// Open (or create) the ledger for `fingerprint` in `dir`.
    ///
    /// A ledger on disk belonging to a *different* key is discarded rather than
    /// adopted, and the calls it held are counted as dropped: they are real
    /// usage that will never be reported, and hiding that would be exactly the
    /// silent repair doctrine §8 forbids.
    pub fn open(dir: Option<&Path>, fingerprint: &KeyFingerprint) -> Self {
        let path = dir.map(|d| d.join(LEDGER_FILE));

        let mut file = path
            .as_ref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice::<LedgerFile>(&bytes).ok())
            .filter(|f| f.version == FORMAT_VERSION)
            .unwrap_or_default();

        if file.fingerprint.as_ref() != Some(fingerprint) {
            let orphaned_calls =
                u64::from(file.pending) + file.inflight.as_ref().map_or(0, |b| u64::from(b.count));
            let orphaned_batches = u64::from(file.pending > 0) + u64::from(file.inflight.is_some());
            let mut dropped = file.dropped;
            // Keep the previous key's drop tally: it is a fact about this
            // installation's reporting, and zeroing it on a key change would
            // make "how much did we fail to report?" answerable only until the
            // next sign-in.
            dropped.calls += orphaned_calls;
            dropped.batches += orphaned_batches;

            file = LedgerFile {
                version: FORMAT_VERSION,
                fingerprint: Some(fingerprint.clone()),
                pending: 0,
                inflight: None,
                dropped,
                anchor: None,
            };
        }

        let ledger = Self {
            path,
            inner: Mutex::new(file),
        };
        // Persist immediately so a fingerprint change is durable even if the
        // process never records another call.
        ledger.persist_locked(&ledger.lock(), Durability::Durable);
        ledger
    }

    /// An in-memory-only ledger, for tests and for a process with no writable
    /// state directory.
    pub fn in_memory(fingerprint: &KeyFingerprint) -> Self {
        Self {
            path: None,
            inner: Mutex::new(LedgerFile {
                fingerprint: Some(fingerprint.clone()),
                ..LedgerFile::default()
            }),
        }
    }

    /// Whether counts survive a restart.
    ///
    /// Surfaced rather than assumed: a session that cannot persist its ledger
    /// is one where a crash loses billing, and the operator should be able to
    /// find that out from the app rather than from an invoice.
    pub fn is_persistent(&self) -> bool {
        self.path.is_some()
    }

    /// Count one billable tool call.
    ///
    /// The only thing on the request path. Increments and writes; no `fsync`,
    /// no network, no allocation beyond the serialised file.
    pub fn record_call(&self) {
        let mut guard = self.lock();
        guard.pending = guard.pending.saturating_add(1);
        self.persist_locked(&guard, Durability::Fast);
    }

    /// Whether a flush is due, given how long since the last one.
    ///
    /// `near_limit` collapses the size trigger to one call: within
    /// [`super::state::NEAR_LIMIT_MARGIN`] of the quota, a batch of 25 would
    /// blur the boundary by 25 calls, and the user would keep working for a
    /// while after they had actually run out. Adaptive batching is what makes
    /// "you are over your limit" arrive within one call instead of one batch.
    pub fn flush_is_due(&self, since_last_flush: Duration, near_limit: bool) -> bool {
        let pending = self.pending();
        if pending == 0 {
            return false;
        }
        if near_limit {
            return true;
        }
        pending >= FLUSH_THRESHOLD || since_last_flush >= FLUSH_INTERVAL
    }

    /// Calls counted but not yet handed to the transport.
    pub fn pending(&self) -> u32 {
        self.lock().pending
    }

    /// A batch left in flight by a previous run.
    ///
    /// A process that finds one crashed between sealing and settling, and must
    /// resolve it before sealing anything new — otherwise the two batches
    /// become indistinguishable to [`reconcile`].
    pub fn orphaned_batch(&self) -> Option<InflightBatch> {
        self.lock().inflight.clone()
    }

    /// Usage this client knows it failed to report.
    pub fn dropped(&self) -> DroppedTally {
        self.lock().dropped
    }

    /// Everything not yet acknowledged by the service: pending, in flight, and
    /// dropped.
    ///
    /// One number for the status bar, derived from the three fields rather than
    /// tracked beside them.
    pub fn unreported_calls(&self) -> u64 {
        let guard = self.lock();
        u64::from(guard.pending)
            + guard.inflight.as_ref().map_or(0, |b| u64::from(b.count))
            + guard.dropped.calls
    }

    /// Move up to [`MAX_BATCH`] pending calls into flight and persist before
    /// returning.
    ///
    /// Returns `None` when there is nothing to send, or when a previous batch
    /// is still unresolved — two in-flight batches would make the reconciliation
    /// arithmetic unsound.
    pub fn seal(&self, now: SystemTime) -> Option<SealedBatch> {
        let mut guard = self.lock();
        if guard.inflight.is_some() || guard.pending == 0 {
            return None;
        }
        let count = guard.pending.min(MAX_BATCH);
        guard.pending -= count;
        guard.inflight = Some(InflightBatch {
            count,
            sealed_at: now,
        });
        // Durable: this is the write that makes the crash window closed. If
        // the process dies after this point, the next run finds the batch and
        // reconciles it; if it died before, the calls are still `pending`.
        self.persist_locked(&guard, Durability::Durable);
        Some(SealedBatch { count })
    }

    /// The service accepted the batch (204, or 429 which also consumes it).
    ///
    /// `acked_since` grows so the next reconciliation knows these calls are
    /// already inside the service's total.
    pub fn settle_accepted(&self, batch: SealedBatch) {
        let mut guard = self.lock();
        guard.inflight = None;
        if let Some(anchor) = guard.anchor.as_mut() {
            anchor.acked_since = anchor.acked_since.saturating_add(u64::from(batch.count));
        }
        self.persist_locked(&guard, Durability::Durable);
    }

    /// The request provably never reached the service. Put the calls back.
    ///
    /// Only ever called for [`super::state::ProbeFailure::NotDelivered`]; the
    /// type system does not enforce that, but there is exactly one call site
    /// and it branches on `is_safely_retryable`.
    pub fn requeue(&self, batch: SealedBatch) {
        let mut guard = self.lock();
        guard.inflight = None;
        guard.pending = guard.pending.saturating_add(batch.count);
        self.persist_locked(&guard, Durability::Durable);
    }

    /// The batch's fate is unknowable. Drop it, and count the drop.
    pub fn drop_ambiguous(&self, batch: SealedBatch) {
        let mut guard = self.lock();
        guard.inflight = None;
        guard.dropped.batches += 1;
        guard.dropped.calls += u64::from(batch.count);
        self.persist_locked(&guard, Durability::Durable);
        tracing::warn!(
            count = batch.count,
            total_dropped = guard.dropped.calls,
            "a usage batch could not be confirmed and was dropped rather than re-sent; \
             the account has been under-charged by this much"
        );
    }

    /// Resolve a batch left in flight by a crash, given a fresh `GET v1/usage`.
    ///
    /// The batch is reconstructed from the persisted record rather than passed
    /// in, because the only thing that knows it existed is the file.
    pub fn resolve_orphan(&self, observed_tool_calls: u64) -> Option<Reconciliation> {
        let (count, anchor) = {
            let guard = self.lock();
            (guard.inflight.as_ref()?.count, guard.anchor)
        };

        // With no anchor there is nothing to measure against: we have never
        // seen a `GET v1/usage` for this account, so any observed total is
        // consistent with both outcomes. Drop, and count it.
        let Some(anchor) = anchor else {
            self.drop_ambiguous(SealedBatch { count });
            return Some(Reconciliation::Indeterminate);
        };

        let verdict = reconcile(anchor.tool_calls, anchor.acked_since, count, observed_tool_calls);
        match verdict {
            Reconciliation::Landed => self.settle_accepted(SealedBatch { count }),
            Reconciliation::NotLanded => self.requeue(SealedBatch { count }),
            Reconciliation::Indeterminate => self.drop_ambiguous(SealedBatch { count }),
        }
        Some(verdict)
    }

    /// Adopt a fresh `GET v1/usage` as the reconciliation anchor.
    ///
    /// Resets `acked_since`, because the snapshot already contains everything
    /// acknowledged before it was taken.
    pub fn adopt_anchor(&self, snapshot: &QuotaSnapshot) {
        let mut guard = self.lock();
        guard.anchor = Some(Anchor {
            tool_calls: snapshot.tool_calls,
            acked_since: 0,
        });
        self.persist_locked(&guard, Durability::Fast);
    }

    /// Flush every counter to disk durably. Called on shutdown.
    pub fn checkpoint(&self) {
        let guard = self.lock();
        self.persist_locked(&guard, Durability::Durable);
    }

    /// Forget everything, on sign-out.
    ///
    /// Pending calls belong to the account being signed out of and can no
    /// longer be reported, so they are counted as dropped rather than silently
    /// discarded — the same rule as an orphaned ledger in [`Self::open`].
    pub fn reset_for_sign_out(&self) {
        let mut guard = self.lock();
        let losing = u64::from(guard.pending)
            + guard.inflight.as_ref().map_or(0, |b| u64::from(b.count));
        if losing > 0 {
            guard.dropped.batches += 1;
            guard.dropped.calls += losing;
        }
        guard.pending = 0;
        guard.inflight = None;
        guard.anchor = None;
        guard.fingerprint = None;
        self.persist_locked(&guard, Durability::Durable);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LedgerFile> {
        // A panic elsewhere cannot have left these integers torn; propagating
        // the poison would turn one unrelated failure into a permanently
        // unusable ledger, which for a billing counter is the worse outcome.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn persist_locked(&self, file: &LedgerFile, durability: Durability) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let Ok(bytes) = serde_json::to_vec(file) else {
            // A struct of integers and one `Option<String>` cannot fail to
            // serialise. Degrading rather than panicking because §L7.6 bans
            // `unwrap` outside tests and a billing counter must not take the
            // process down.
            tracing::error!("usage ledger could not be serialised; counts are memory-only");
            return;
        };
        if let Err(e) = write_atomically(path, &bytes, durability) {
            tracing::error!(
                error = %e,
                path = %path.display(),
                "usage ledger could not be written; a crash will now lose unreported calls"
            );
        }
    }
}

impl std::fmt::Debug for UsageLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let guard = self.lock();
        f.debug_struct("UsageLedger")
            .field("persistent", &self.path.is_some())
            .field("pending", &guard.pending)
            .field("inflight", &guard.inflight)
            .field("dropped", &guard.dropped)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::account::credential::ApiKey;

    fn fp(body: &str) -> KeyFingerprint {
        ApiKey::parse(&format!("ndx_{body}"))
            .expect("test key parses")
            .fingerprint()
    }

    fn fp_a() -> KeyFingerprint {
        fp("2f8c41a9b60d47e3a5710c9fbe2d836a4517")
    }

    fn fp_b() -> KeyFingerprint {
        fp("0000000000000000000000000000000000000000")
    }

    fn t0() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "nudox-usage-ledger-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn snapshot(tool_calls: u64) -> QuotaSnapshot {
        QuotaSnapshot {
            tier: "free".to_owned(),
            period_start: "2026-08-01T00:00:00Z".to_owned(),
            api_requests: 0,
            tool_calls,
            used: tool_calls,
            limit: 1000,
            remaining: 1000_u64.saturating_sub(tool_calls),
            over_limit: false,
            observed_at: t0(),
        }
    }

    // -----------------------------------------------------------------------
    // reconcile — the pure core
    // -----------------------------------------------------------------------

    #[test]
    fn reconciliation_reads_the_services_total_as_the_authority() {
        // anchor 700, nothing acked since, a batch of 25 in doubt.
        assert_eq!(reconcile(700, 0, 25, 725), Reconciliation::Landed);
        assert_eq!(reconcile(700, 0, 25, 700), Reconciliation::NotLanded);
        assert_eq!(reconcile(700, 0, 25, 712), Reconciliation::Indeterminate);
    }

    #[test]
    fn acknowledged_calls_move_both_expectations() {
        // 700 at the anchor, 40 confirmed since, 25 in doubt. The service
        // should read 740 if the batch was lost and 765 if it landed.
        assert_eq!(reconcile(700, 40, 25, 765), Reconciliation::Landed);
        assert_eq!(reconcile(700, 40, 25, 740), Reconciliation::NotLanded);
        assert_eq!(reconcile(700, 40, 25, 741), Reconciliation::Indeterminate);
        assert_eq!(
            reconcile(700, 40, 25, 700),
            Reconciliation::NotLanded,
            "an observation below the anchor still means the batch is not in it"
        );
    }

    #[test]
    fn concurrent_activity_biases_toward_not_re_sending() {
        // A second machine recorded 500 calls while we were asking. The batch's
        // real fate is unknown; we resolve it as Landed, which means we do not
        // re-send, which means the user is under-charged rather than
        // over-charged. The bias is the design.
        assert_eq!(reconcile(700, 0, 25, 1200), Reconciliation::Landed);
    }

    #[test]
    fn a_zero_sized_batch_is_always_already_reflected() {
        assert_eq!(reconcile(700, 0, 0, 700), Reconciliation::Landed);
    }

    // -----------------------------------------------------------------------
    // Sealing and settling
    // -----------------------------------------------------------------------

    #[test]
    fn recording_accumulates_and_sealing_moves_calls_into_flight() {
        let ledger = UsageLedger::in_memory(&fp_a());
        for _ in 0..7 {
            ledger.record_call();
        }
        assert_eq!(ledger.pending(), 7);
        assert_eq!(ledger.unreported_calls(), 7);

        let batch = ledger.seal(t0()).expect("there is something to seal");
        assert_eq!(batch.count(), 7);
        assert_eq!(ledger.pending(), 0, "sealed calls leave pending");
        assert_eq!(
            ledger.unreported_calls(),
            7,
            "but they are still unreported until the service says otherwise"
        );

        ledger.settle_accepted(batch);
        assert_eq!(ledger.unreported_calls(), 0);
        assert_eq!(ledger.dropped(), DroppedTally::default());
    }

    #[test]
    fn a_batch_is_capped_and_the_remainder_stays_pending() {
        let ledger = UsageLedger::in_memory(&fp_a());
        for _ in 0..(MAX_BATCH + 13) {
            ledger.record_call();
        }
        let batch = ledger.seal(t0()).expect("seal");
        assert_eq!(batch.count(), MAX_BATCH);
        assert_eq!(ledger.pending(), 13);
    }

    #[test]
    fn only_one_batch_may_be_in_flight_at_a_time() {
        // Two concurrent batches would make the reconciliation arithmetic
        // unsound: `reconcile` can only reason about one unknown at a time.
        let ledger = UsageLedger::in_memory(&fp_a());
        for _ in 0..10 {
            ledger.record_call();
        }
        let first = ledger.seal(t0()).expect("first seal");
        ledger.record_call();
        assert!(
            ledger.seal(t0()).is_none(),
            "a second seal while one is in flight must be refused"
        );
        ledger.settle_accepted(first);
        assert!(ledger.seal(t0()).is_some(), "and permitted once resolved");
    }

    #[test]
    fn a_not_delivered_failure_returns_the_calls_to_the_queue() {
        let ledger = UsageLedger::in_memory(&fp_a());
        for _ in 0..5 {
            ledger.record_call();
        }
        let batch = ledger.seal(t0()).expect("seal");
        ledger.requeue(batch);
        assert_eq!(
            ledger.pending(),
            5,
            "a request that never left must cost nothing"
        );
        assert_eq!(ledger.dropped(), DroppedTally::default());
    }

    #[test]
    fn an_ambiguous_failure_drops_the_batch_and_counts_it() {
        // Doctrine §8: the repair must be counted, and the count derived from
        // the thing repaired. Asserting the drop happened is not the same as
        // asserting it was recorded — this asserts the record.
        let ledger = UsageLedger::in_memory(&fp_a());
        for _ in 0..9 {
            ledger.record_call();
        }
        let batch = ledger.seal(t0()).expect("seal");
        ledger.drop_ambiguous(batch);

        assert_eq!(ledger.pending(), 0);
        assert_eq!(
            ledger.dropped(),
            DroppedTally {
                batches: 1,
                calls: 9
            }
        );
        assert_eq!(
            ledger.unreported_calls(),
            9,
            "a dropped batch must stay visible; it is money that will never be billed"
        );
    }

    // -----------------------------------------------------------------------
    // Crash recovery
    // -----------------------------------------------------------------------

    #[test]
    fn a_crash_before_sealing_loses_nothing() {
        let scratch = Scratch::new("crash-pending");
        {
            let ledger = UsageLedger::open(Some(scratch.path()), &fp_a());
            for _ in 0..11 {
                ledger.record_call();
            }
            // No checkpoint, no seal: simulate the process simply ending.
        }
        let reopened = UsageLedger::open(Some(scratch.path()), &fp_a());
        assert_eq!(
            reopened.pending(),
            11,
            "record_call persists on every call, so a process crash costs nothing"
        );
        assert_eq!(reopened.dropped(), DroppedTally::default());
    }

    #[test]
    fn a_crash_mid_flight_leaves_an_orphan_that_reconciliation_resolves() {
        let scratch = Scratch::new("crash-inflight");
        {
            let ledger = UsageLedger::open(Some(scratch.path()), &fp_a());
            ledger.adopt_anchor(&snapshot(700));
            for _ in 0..25 {
                ledger.record_call();
            }
            let _sealed = ledger.seal(t0()).expect("seal");
            // Process dies here — the POST may or may not have been processed.
        }

        let reopened = UsageLedger::open(Some(scratch.path()), &fp_a());
        let orphan = reopened.orphaned_batch().expect("the batch survived the crash");
        assert_eq!(orphan.count, 25);

        // The service's total already includes them.
        assert_eq!(
            reopened.resolve_orphan(725),
            Some(Reconciliation::Landed),
            "the authoritative total is what decides"
        );
        assert_eq!(reopened.pending(), 0);
        assert_eq!(reopened.dropped(), DroppedTally::default());
        assert!(reopened.orphaned_batch().is_none());
    }

    #[test]
    fn an_orphan_the_service_never_saw_is_re_queued_not_dropped() {
        let scratch = Scratch::new("crash-notlanded");
        {
            let ledger = UsageLedger::open(Some(scratch.path()), &fp_a());
            ledger.adopt_anchor(&snapshot(700));
            for _ in 0..25 {
                ledger.record_call();
            }
            let _ = ledger.seal(t0()).expect("seal");
        }
        let reopened = UsageLedger::open(Some(scratch.path()), &fp_a());
        assert_eq!(reopened.resolve_orphan(700), Some(Reconciliation::NotLanded));
        assert_eq!(
            reopened.pending(),
            25,
            "calls the service demonstrably never received must be re-sent, not lost"
        );
        assert_eq!(reopened.dropped(), DroppedTally::default());
    }

    #[test]
    fn an_orphan_with_no_anchor_is_dropped_rather_than_guessed_at() {
        // Never having seen a `GET /usage` means every observed total is
        // consistent with both outcomes. Guessing "landed" would under-count
        // silently; guessing "not landed" would risk double-billing.
        let scratch = Scratch::new("crash-noanchor");
        {
            let ledger = UsageLedger::open(Some(scratch.path()), &fp_a());
            for _ in 0..4 {
                ledger.record_call();
            }
            let _ = ledger.seal(t0()).expect("seal");
        }
        let reopened = UsageLedger::open(Some(scratch.path()), &fp_a());
        assert_eq!(
            reopened.resolve_orphan(999),
            Some(Reconciliation::Indeterminate)
        );
        assert_eq!(
            reopened.dropped(),
            DroppedTally {
                batches: 1,
                calls: 4
            }
        );
    }

    #[test]
    fn a_ledger_belonging_to_another_key_is_never_billed_to_the_new_one() {
        let scratch = Scratch::new("fingerprint-change");
        {
            let ledger = UsageLedger::open(Some(scratch.path()), &fp_a());
            for _ in 0..6 {
                ledger.record_call();
            }
        }
        let other = UsageLedger::open(Some(scratch.path()), &fp_b());
        assert_eq!(
            other.pending(),
            0,
            "the previous account's calls must not follow the new key"
        );
        assert_eq!(
            other.dropped(),
            DroppedTally {
                batches: 1,
                calls: 6
            },
            "and they must be counted as unreportable, not silently forgotten"
        );
    }

    #[test]
    fn signing_out_counts_what_it_discards() {
        let ledger = UsageLedger::in_memory(&fp_a());
        for _ in 0..3 {
            ledger.record_call();
        }
        ledger.reset_for_sign_out();
        assert_eq!(ledger.pending(), 0);
        assert_eq!(
            ledger.dropped(),
            DroppedTally {
                batches: 1,
                calls: 3
            }
        );
    }

    // -----------------------------------------------------------------------
    // Flush triggers
    // -----------------------------------------------------------------------

    #[test]
    fn flushing_is_triggered_by_size_or_time_and_never_by_nothing() {
        let ledger = UsageLedger::in_memory(&fp_a());
        assert!(
            !ledger.flush_is_due(FLUSH_INTERVAL * 10, false),
            "an empty ledger must never wake the network, however long it has been"
        );

        ledger.record_call();
        assert!(!ledger.flush_is_due(Duration::ZERO, false));
        assert!(
            ledger.flush_is_due(FLUSH_INTERVAL, false),
            "the time trigger must fire for a single call"
        );

        for _ in 1..FLUSH_THRESHOLD {
            ledger.record_call();
        }
        assert_eq!(ledger.pending(), FLUSH_THRESHOLD);
        assert!(
            ledger.flush_is_due(Duration::ZERO, false),
            "the size trigger must fire without waiting"
        );
    }

    #[test]
    fn near_the_limit_every_call_is_flushed_on_its_own() {
        // Otherwise the over-limit boundary is blurred by a whole batch and the
        // user keeps working for a while after they have run out — which reads
        // as the limit being wrong rather than as batching.
        let ledger = UsageLedger::in_memory(&fp_a());
        ledger.record_call();
        assert!(!ledger.flush_is_due(Duration::ZERO, false));
        assert!(ledger.flush_is_due(Duration::ZERO, true));
    }

    #[test]
    fn an_in_memory_ledger_says_it_is_not_persistent() {
        assert!(!UsageLedger::in_memory(&fp_a()).is_persistent());
        let scratch = Scratch::new("persistent");
        assert!(UsageLedger::open(Some(scratch.path()), &fp_a()).is_persistent());
        assert!(!UsageLedger::open(None, &fp_a()).is_persistent());
    }

    #[test]
    fn the_ledger_debug_carries_no_credential() {
        let ledger = UsageLedger::in_memory(&fp_a());
        ledger.record_call();
        let rendered = format!("{ledger:?}");
        assert!(rendered.contains("pending"));
        assert!(
            !rendered.contains("ndx_"),
            "a ledger names a key by fingerprint and must never render one"
        );
    }
}
