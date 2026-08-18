//! Emitting a finished blob: writing its sections to the object store and
//! recording its manifest.
//!
//! "Emit" is the terminal step of a blob's life — the manifest and all pending
//! `cas/` sections are committed idempotently. Fan-out to the derived sinks is
//! deliberately **not** done here: this function's one caller
//! (`server::coordination::indexing::execute_emit_phase`) always follows it
//! immediately with `Outbox::record_stored`, which is the actual "this
//! generation is ready" signal (lifecycle transition + facets + Vector/Graph
//! intents; see its doc comment for why Text isn't re-signaled there either).
//! This function used to *also* fan out to every sink itself
//! (`outbox.append(package, generation, &kinds)`, one `OutboxEntry` per
//! `SinkKind`) — entirely redundant with the fan-out `record_stored` performs
//! a moment later on the very same generation, and the biggest single
//! contributor to one parse leaving nine-plus outbox intents instead of one
//! (see `tests/pipeline_end_to_end.rs::package_is_parsed_once_and_fanned_out`).

use std::collections::{HashMap, HashSet};

use futures::stream::{self, StreamExt as _};
use heart::content::ContentHash;

use super::{BlobManifest, creation::PendingSection};
use crate::{
    cas::Store,
    error::{BlobError, StoreError},
};

/// The outcome of emitting a blob: how many sections were newly written vs
/// deduped as already-present.
#[derive(Debug, Clone, Copy)]
pub struct Emitted {
    /// Sections that were freshly written to `cas/`.
    pub written: usize,

    /// Sections that were already present (idempotent no-op).
    pub deduped: usize,
}

/// Upper bound on `HEAD`-then-`PUT` requests [`emit`] (and
/// [`crate::server::save::blobs::Server::verify_blobs`]'s companion read
/// fan-out) run against the object store at once.
///
/// # Why bounded, and why this number
///
/// `Store::put_section` is two sequential requests (`HEAD`, then `PUT` if
/// absent) per section, and `Store` is generic over `object_store` backends
/// that include `s3://`/`gs://`/`az://` as well as local disk (see
/// `server/mod.rs:803-824`). Against the real corpus, packages are median 52
/// files but p90 535 and max 4,325 (`tests/emit_concurrency.rs`'s own module
/// doc has the measurement) — running every section serially costs one
/// round-trip pair per file, which is fine on local disk (microseconds) but
/// is 10-25s of pure network latency at a realistic 20-50ms/request p90
/// package over a remote backend. Fanning out *all* of them at once instead
/// trades that latency bug for a resource-exhaustion one: a 4,325-file
/// package would open 4,325 simultaneous connections, which a local
/// filesystem's fd limit and a remote backend's connection pool (S3 SDKs
/// commonly default their own pools well under four figures) will not
/// tolerate.
///
/// 16 matches `HYDRATE_CONCURRENCY` in `server/coordination/search.rs`, the
/// other bounded-fan-out cap already proven in this crate for per-item
/// catalog round trips (also `object_store`/DB-backed, also
/// N-up-to-thousands). Reusing the same number keeps "how much do we let one
/// request fan out" a single, auditable policy instead of a guess re-tuned
/// per call site.
pub(crate) const SECTION_EMIT_CONCURRENCY: usize = 16;

/// Emit a finished blob.
///
/// Writes every [`PendingSection`] to the content-addressed store (idempotent —
/// a section whose hash already exists is a no-op) and records the manifest.
/// Does **not** fan out to the derived sinks — see the module doc for why;
/// the caller fans out itself immediately after, via `Outbox::record_stored`.
///
/// Consumes the manifest + sections: this is the end of the blob's assembly
/// life, after which it exists only as durable content-addressed state.
///
/// The compiled-lookup index (SV-6) is written separately by the iroh
/// SyncService apply-hook AFTER a merged change-set is verified and applied —
/// not here. See [`crate::compiled::ObjectCompiledStore::record`].
#[tracing::instrument(skip_all, fields(package = %manifest.package, sections = sections.len()))]
pub async fn emit(
    store: &Store,
    manifest: BlobManifest,
    sections: Vec<PendingSection>,
) -> Result<Emitted, BlobError> {
    // 1. Every section lands in `cas/` idempotently, concurrently, bounded by
    //    `SECTION_EMIT_CONCURRENCY`.
    //
    //    Two files in the *same* package can legitimately share a content
    //    hash (two byte-identical source files, or a file duplicated across
    //    a re-export). If each `PendingSection` fired its own concurrent
    //    `HEAD`-then-`PUT`, two tasks could both `HEAD` the same not-yet-
    //    written key before either `PUT` lands, and both would then report
    //    "I wrote it" — double-counting one physical write as two, which is
    //    exactly the miscount rule 3 of this change forbids. Rather than
    //    accept that race and paper over it with a lock, this collapses the
    //    batch to one `put_section` call per *unique* hash before fanning
    //    out, so at most one task ever touches a given `cas/{hash}` key
    //    within this call — the race cannot happen by construction, not by
    //    luck.
    //
    //    Accounting then mirrors exactly what the old serial `for` loop would
    //    have produced: within a group of sections sharing a hash, the single
    //    `put_section` call is the one that would have physically written the
    //    object had they been done one at a time (true) or found it already
    //    present (false, e.g. a re-emit of previously-stored content). Every
    //    *other* section in that group is — like the 2nd..Nth iteration of
    //    the old loop over the same hash — necessarily already present the
    //    instant the first is resolved, so it is `deduped`. This keeps the
    //    invariant `written + deduped == sections.len()` exact regardless of
    //    concurrency, and every `PendingSection` is still classified exactly
    //    once.
    //
    //    (A *different* `emit` call racing this one over genuinely shared
    //    content across packages is unaffected by any of this — it was never
    //    serialized by the old loop either, and remains safe because
    //    `put_section` is idempotent: whichever caller's `PUT` lands, the
    //    bytes at that key are identical either way.)
    //
    //    The fan-out below hands each concurrent task an *owned* `Store`
    //    clone (cheap — `Store::clone` only bumps the inner `Arc<dyn
    //    ObjectStore>` refcount, see `cas.rs`) and an owned `PendingSection`,
    //    rather than borrowing `store`/`sections` across the `.await`
    //    points inside `buffer_unordered`. This isn't just tidiness: `emit`
    //    is called many frames deep inside the generic, `tokio::spawn`-ed
    //    indexing queue worker, and a borrowed-reference future nested this
    //    far down a generic call chain has been observed to trip rustc's
    //    higher-ranked `Send`-generality checker into a false-positive
    //    "implementation of Send is not general enough" — reported against
    //    unrelated call sites several layers up, not against this function.
    //    Owning the data sidesteps that lifetime generalization entirely:
    //    there is nothing left to generalize over.
    let mut occurrences: HashMap<ContentHash, usize> = HashMap::new();
    for section in &sections {
        *occurrences.entry(section.hash).or_insert(0) += 1;
    }
    let mut seen = HashSet::with_capacity(occurrences.len());
    let unique_sections: Vec<PendingSection> = sections
        .into_iter()
        .filter(|section| seen.insert(section.hash))
        .collect();

    let mut emitted = Emitted {
        written: 0,
        deduped: 0,
    };
    let mut writes = stream::iter(unique_sections)
        .map(|section| put_one(store.clone(), section))
        .buffer_unordered(SECTION_EMIT_CONCURRENCY);

    while let Some(result) = writes.next().await {
        let (hash, written) = result.map_err(BlobError::from)?;
        // Present because `occurrences` was populated from the same
        // `sections` this hash was drawn from.
        let group_size = occurrences[&hash];
        if written {
            emitted.written += 1;
            emitted.deduped += group_size - 1;
        } else {
            emitted.deduped += group_size;
        }
    }

    // 2. The manifest becomes its own `cas/` object and the package pointer is
    //    repointed at it. The caller already computes the snapshot hash it
    //    needs (`ContentHash::of_bytes(&manifest.identity_bytes())`) before
    //    calling this, so the generation handle `put_manifest` returns is not
    //    needed here.
    store
        .put_manifest(&manifest)
        .await
        .map_err(BlobError::from)?;

    tracing::info!(
        written = emitted.written,
        deduped = emitted.deduped,
        "blob emitted"
    );
    Ok(emitted)
}

/// One section's idempotent `HEAD`-then-`PUT`, over an owned [`Store`] handle
/// and an owned [`PendingSection`] — see the long comment at `emit`'s fan-out
/// call site for why owned rather than borrowed. A plain named `async fn`
/// (rather than an `async move` closure inlined at the call site) so its
/// future is a single, ordinary, `'static` type: nothing here needs
/// generalizing over a caller-supplied lifetime.
async fn put_one(store: Store, section: PendingSection) -> Result<(ContentHash, bool), StoreError> {
    let written = store.put_section(&section).await?;
    Ok((section.hash, written))
}
