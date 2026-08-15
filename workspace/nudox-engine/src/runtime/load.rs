//! The load driver: durable load-failure record and the one insert path.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use nudox_ir::change::PackageLineageId;

use crate::{
    runtime::EngineInner,
    store::source::{IrSource, LoadEvent, LoadRequest},
    wire::SharedStr,
    PackageLoadEvent,
};

// ---------------------------------------------------------------------------
// LoadFailures
// ---------------------------------------------------------------------------

/// Durable record of package loads that were attempted and failed, keyed by
/// lineage.
///
/// # Why this exists
///
/// `EngineHandle::packages()` broadcasts `PackageLoadEvent::LoadFailed` once,
/// live, to whoever happens to be subscribed at that moment (§the type's own
/// docs: "capacity 64 ... a slow subscriber may lag"). An MCP tool call is
/// not a subscriber — it typically arrives well after seeding has finished —
/// so without a durable copy, `EngineError::PackageNotLoaded` could never
/// tell "this lineage was attempted and broke" apart from "this lineage was
/// never named at all", and both surfaced as the identical bare "not
/// loaded". This is the durable half; the broadcast stays the live half for
/// a GUI toast.
#[derive(Clone, Default)]
pub(crate) struct LoadFailures(Arc<std::sync::RwLock<HashMap<PackageLineageId, SharedStr>>>);

impl LoadFailures {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record that loading `lineage` failed with `reason`.
    ///
    /// A later record for the same lineage overwrites the earlier one — a
    /// reload attempt's outcome is the one worth reporting, not its history.
    fn record(&self, lineage: PackageLineageId, reason: SharedStr) {
        let mut guard = self
            .0
            .write()
            .expect("load-failure lock is never held across a panic");
        guard.insert(lineage, reason);
    }

    /// The recorded failure reason for `lineage`, or `None` if no load for
    /// it was ever recorded as failed (either it succeeded, or it was never
    /// attempted — this type cannot tell those two apart, which is exactly
    /// why `EngineError::PackageNotLoaded::attempted` is itself an
    /// `Option`).
    pub(crate) fn get(&self, lineage: &PackageLineageId) -> Option<SharedStr> {
        let guard = self
            .0
            .read()
            .expect("load-failure lock is never held across a panic");
        guard.get(lineage).cloned()
    }
}


// ---------------------------------------------------------------------------
// drive_load — the one place a produced package enters the corpus
// ---------------------------------------------------------------------------

/// Drive an [`IrSource`] to completion, recording every package it produces and
/// broadcasting the outcome.
///
/// # Why this is a function and not the body of `Engine::start`
///
/// It used to be the body of `Engine::start`, and that was the *actual*
/// blocker behind `EngineCapability::ProjectResolution`, whose `blocked_on`
/// reads: "today packages can only be supplied to `Engine::start_with_producer`,
/// so a resolved list could not be acted on". Nothing about the loop needed to
/// be start-time; it was simply written inline, so the only way to run it was
/// to start a new engine.
///
/// Extracting it — rather than writing a second insertion path for on-demand
/// packages — is what guarantees a package indexed from a PURL at minute ten is
/// indistinguishable from one named at minute zero: the same
/// `Discovered`-to-`Ready` version pairing, the same
/// [`VersionRegistry::record`] arbitration over which generation becomes
/// resident, the same durable [`LoadFailures`] copy, and the same `pkg_tx`
/// broadcast that `EngineHandle::packages()` fuses into its snapshot. A second
/// path would have had to re-derive all four, and the first one it got wrong
/// would be a lineage that behaves differently depending on when it arrived.
///
/// Returns the events it broadcast, in order, so a caller loading a known set
/// of packages can inspect the outcome without subscribing to a channel it
/// shares with everyone else.
pub(crate) async fn drive_load(
    source: impl IrSource,
    inner: &EngineInner,
) -> Vec<PackageLoadEvent> {
    use futures::StreamExt as _;

    // Version strings awaiting their `Ready`, keyed by lineage.
    //
    // `LoadEvent::Ready` carries only an `Arc<PackageView>`, and
    // `PackageView` has no version field — a `PackageLineageId` is
    // version-free by design and nothing downstream of it ever needed
    // the number. The only place the version appears in the load
    // protocol is `LoadEvent::Discovered { hint: PackageHint { version } }`.
    //
    // Recovering it here is sound because the `IrSource` contract
    // requires `Discovered` before `Ready` for every package, and both
    // in-tree sources are strictly sequential per package
    // (`ProducerSource::load` drives descriptors with `then`, which
    // awaits each in turn). A FIFO per lineage therefore pairs each
    // `Ready` with its own `Discovered` even when several generations
    // of the same lineage are being loaded.
    //
    // A hypothetical source that interleaved *two generations of the
    // same lineage* could mispair them; a source that interleaves
    // different lineages cannot, because the queues are per-lineage.
    // The durable fix is a `version` field on `PackageView` (or on
    // `LoadEvent::Ready`), which is a `nudox-store` change and so out
    // of scope here.
    let mut pending: HashMap<PackageLineageId, VecDeque<Option<String>>> = HashMap::new();
    let mut outcomes = Vec::new();

    let mut stream = source.load(LoadRequest::default());
    while let Some(event) = stream.next().await {
        match event {
            Ok(LoadEvent::Discovered { lineage, hint }) => {
                pending.entry(lineage).or_default().push_back(hint.version);
            }
            Ok(LoadEvent::Ready { package }) => {
                // Count symbols before the package is handed on.
                let symbol_count = package.view().table().len() as u64;
                let lineage = package.lineage().clone();
                let name = lineage.name.as_str().to_owned();
                let ecosystem = lineage.ecosystem.as_str().to_owned();
                let version = pending
                    .get_mut(&lineage)
                    .and_then(std::collections::VecDeque::pop_front)
                    .flatten();

                // Record the generation first. The registry decides
                // whether this generation becomes the resident one —
                // it is the single place that rule lives, so the
                // corpus cannot drift from the version list.
                //
                // This also fixes a latent ordering bug: previously
                // every `Ready` was inserted unconditionally, so with
                // several generations of one package the corpus ended
                // up holding whichever finished producing last rather
                // than the newest.
                if let Some(resident) =
                    inner
                        .versions
                        .record(&lineage, version.clone(), Arc::clone(&package))
                {
                    inner.corpus.insert(Arc::clone(&resident)).await;
                    // Hand the *resident* generation — not `package` — to the
                    // embedder. They differ whenever an older generation
                    // arrives after a newer one, and indexing the arriving one
                    // would embed symbols that no search can resolve, because
                    // every lookup goes through the corpus.
                    //
                    // `send` (not `send_async`) on an unbounded channel never
                    // blocks, which is what keeps this line off the critical
                    // path. `Err` means the indexer has shut down; the package
                    // is still fully loaded and searchable by name and type,
                    // and `SectionState::Building` already reports it as
                    // uncovered, so there is nothing to recover here.
                    if let Some(tx) = &inner.semantic_tx {
                        let _ = tx.send(resident);
                    }
                }

                let event = PackageLoadEvent::Loaded {
                    name: name.into(),
                    ecosystem: ecosystem.into(),
                    version,
                    symbol_count,
                    root: package_root_key(&package),
                };
                // Ignore `Err`: no subscribers yet is fine.
                let _ = inner.pkg_tx.send(event.clone());
                outcomes.push(event);
            }
            Ok(LoadEvent::Failed { lineage, error }) => {
                // Consume this package's queued version so the FIFO
                // stays aligned for the lineage's later generations.
                // `Discovered` is emitted for the failing package too.
                if let Some(q) = pending.get_mut(&lineage) {
                    q.pop_front();
                }
                tracing::warn!(
                    package = %lineage,
                    "package load failed: {error}",
                );
                let reason: SharedStr = error.to_string().into();
                // Durable copy — see `LoadFailures` docs for why the
                // broadcast below is not enough on its own.
                inner.load_failures.record(lineage.clone(), reason.clone());
                let event = PackageLoadEvent::LoadFailed {
                    name: lineage.name.as_str().to_owned().into(),
                    ecosystem: lineage.ecosystem.as_str().to_owned().into(),
                    error: reason,
                };
                let _ = inner.pkg_tx.send(event.clone());
                outcomes.push(event);
            }
            Ok(_) => {} // Progress — informational only
            Err(e) => {
                tracing::error!("stream-level load error: {e}");
                break;
            }
        }
    }
    outcomes
}


// ---------------------------------------------------------------------------
// Package root
// ---------------------------------------------------------------------------

/// The `SymbolKey` of a package's root entry — its crate / top-level module.
///
/// The root is the one entry with no parent. A well-formed package has exactly
/// one; this takes the first in declaration order so the answer is
/// deterministic even if a producer ever emits a second unparented entry (the
/// `IrView` iteration order is the insertion order of the sealed table, not a
/// hash order).
///
/// Returns `None` for a package with no unparented entry, which would be
/// malformed rather than merely empty.
pub(crate) fn package_root_key(pkg: &crate::store::package::PackageView) -> Option<crate::SymbolKey> {
    let view = pkg.view();
    view.entries()
        .map(|(intro, _)| intro)
        .find(|intro| view.parent_of(*intro).is_none())
        .map(|intro| crate::SymbolKey::new(pkg.lineage().clone(), intro))
}
