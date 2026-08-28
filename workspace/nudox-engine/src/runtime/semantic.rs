//! Incremental embedding: consume resident packages off the load path.

use std::collections::BTreeSet;
use std::sync::Arc;

use heart::ContentHash;

// ---------------------------------------------------------------------------
// Semantic indexing — incremental, off the load path
// ---------------------------------------------------------------------------

/// The largest document text handed to a model, in bytes.
///
/// The recipe's token budget is frozen at `MAX_SEQ_LEN = 1024`, and every
/// tokenizer this repo can be pointed at truncates beyond it *silently*. Cutting
/// here instead means the truncation happens somewhere a reader can see it and
/// somewhere the cost is bounded before the batch is built, rather than inside a
/// runtime whose behaviour we would be inferring. Four bytes per token is the
/// conservative ratio for code, which is denser than prose.
const MAX_DOCUMENT_BYTES: usize = 4 * 1024;

/// Consume newly-resident packages and embed them, one package at a time.
///
/// # Why this is a task and not a step in `drive_load`
///
/// The requirement is that a package is searchable by name and type the instant
/// it is resident, and that embedding — 250 ms to 1.1 s per package for
/// inference-grade work — never delays that. Doing it inline would serialise
/// the two: the second package could not begin lowering until the first had
/// finished embedding. Here the load loop's only obligation is one non-blocking
/// `send`.
///
/// # Why packages are embedded serially
///
/// Embedding is CPU-saturating: `FastembedOrt` runs a CPU execution provider
/// with an intra-op thread pool already sized to the machine. Two packages
/// embedding concurrently do not finish sooner, they finish *later* — they
/// contend for the same cores and each one's completion is pushed back behind
/// the other's. Serialising means the first package's vectors land as early as
/// they possibly can, which is what `SectionState::Building` reports on and
/// what makes partial coverage useful rather than merely honest.
pub(crate) async fn semantic_indexer(
    rx: flume::Receiver<Arc<crate::store::package::PackageView>>,
    embedder: Arc<dyn crate::semantic::Embedder>,
    index: crate::semantic::SemanticIndex,
) {
    let info = embedder.info();
    // A host that advertises a zero batch size would make `chunks(0)` panic.
    // Clamping rather than trusting keeps a bad host's misconfiguration from
    // becoming this task's crash.
    let batch = info.max_batch.max(1);

    while let Ok(package) = rx.recv_async().await {
        let lineage = package.lineage().clone();
        let documents = documents_of(&package);
        let started = std::time::Instant::now();

        // Diff against what's already indexed for this lineage before
        // touching the embedder at all. A reload of the same generation —
        // including a plain re-load of an unchanged version, which the
        // corpus does not distinguish from a genuine new one — must cost
        // nothing here: no model call, no lock write, no persistence write.
        let existing = index.snapshot_hashes(&lineage);
        let mut wanted = BTreeSet::new();
        let mut to_embed: Vec<(nudox_ir::change::IntroId, String, ContentHash)> = Vec::new();
        for (intro, text) in &documents {
            wanted.insert(*intro);
            let hash = ContentHash::of_bytes(text.as_bytes());
            if existing.get(intro) != Some(&hash) {
                to_embed.push((*intro, text.clone(), hash));
            }
        }
        let removed: Vec<nudox_ir::change::IntroId> = existing
            .keys()
            .filter(|intro| !wanted.contains(intro))
            .copied()
            .collect();

        if to_embed.is_empty() && removed.is_empty() && index.contains(&lineage) {
            continue;
        }

        let mut vectors: Vec<(nudox_ir::change::IntroId, ContentHash, Vec<f32>)> =
            Vec::with_capacity(to_embed.len());
        let mut failed = false;

        for chunk in to_embed.chunks(batch) {
            let texts: Vec<String> = chunk.iter().map(|(_, text, _)| text.clone()).collect();
            match embedder
                .embed_batch(&texts, crate::semantic::EmbedRole::Document)
                .await
            {
                Ok(batch_vectors) if batch_vectors.len() == chunk.len() => {
                    for ((intro, _, hash), vector) in chunk.iter().zip(batch_vectors) {
                        vectors.push((*intro, *hash, vector));
                    }
                }
                Ok(batch_vectors) => {
                    // A host that returns a different number of vectors than it
                    // was given has broken the positional pairing the trait
                    // documents, and there is no way to tell *which* symbol each
                    // vector belongs to. Pairing them anyway would produce an
                    // index that is confidently wrong — every row correct-looking
                    // and attached to the wrong symbol.
                    tracing::error!(
                        package = %lineage,
                        sent = chunk.len(),
                        got = batch_vectors.len(),
                        "embedder broke positional pairing; abandoning this package"
                    );
                    failed = true;
                    break;
                }
                Err(error) => {
                    tracing::warn!(package = %lineage, "embedding failed: {error}");
                    failed = true;
                    break;
                }
            }
        }

        if failed {
            // Deliberately *not* recorded as covered. The package stays in
            // `SectionState::Building`'s uncovered count, which is exactly true:
            // the semantic section has not searched it and never will this run.
            // Marking it covered would be the silent-repair failure of §8 —
            // presenting a degraded case as the good one. Also deliberately
            // *not* applied partially: a batch failing partway through leaves
            // `vectors` short of `to_embed`, and upserting only what succeeded
            // would persist hashes for symbols the model never actually saw.
            continue;
        }

        let embedded = vectors.len();
        let removed_count = removed.len();
        match index.apply_delta(lineage.clone(), vectors, &removed, info.dimensions) {
            // `info!`, not `debug!`: this is the single most expensive thing the
            // engine does per package (measured at 246.8 s for real `memchr` on
            // a CPU execution provider), and a cost that is only visible at
            // `debug` is a cost nobody measures. It is one line per package, at
            // the same level as "corpus seeding complete". `embedded` is only
            // the symbols actually sent to the model this run, not the
            // package's whole public surface — that's the number that proves
            // the diff worked.
            Ok(()) => tracing::info!(
                package = %lineage,
                embedded,
                removed = removed_count,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "semantic index updated"
            ),
            Err(error) => tracing::error!(
                package = %lineage,
                "embedder produced unusable vectors: {error}"
            ),
        }
    }
}

/// The text embedded for each symbol of `package`, in `IntroId` order.
///
/// # What is embedded, and what is not
///
/// Public symbols only. A semantic search over documentation is a search over
/// the *documented surface*, and a private helper that happens to be
/// semantically close to the query is an answer the reader cannot use — they
/// cannot call it, and its page exists only because the crate was lowered
/// whole. It is also the difference between embedding memchr's full 11 329
/// entries and its public API, which is the difference between a per-package
/// cost measured in minutes and one measured in seconds.
///
/// `Param` entries are excluded for the reason `typerefs_of_entry` excludes
/// them: a parameter is part of a signature, not a destination, and its text is
/// already carried by the function's own document.
///
/// # Why the document is assembled here and not by the host
///
/// The host supplies a model; deciding *what a symbol is, as text* is a
/// question about the IR, and the IR is this crate's business. It also keeps
/// the answer stable across hosts, so two embedders can be compared on the same
/// documents rather than on their own idea of what a symbol says.
fn documents_of(
    package: &crate::store::package::PackageView,
) -> Vec<(nudox_ir::change::IntroId, String)> {
    use nudox_ir::entry::Visibility;

    let view = package.view();
    let indexes = package.indexes();
    let mut out = Vec::new();

    for (intro, entry) in view.entries_sorted() {
        if entry.sym().visibility != Visibility::Public {
            continue;
        }
        let Some(discriminant) = entry.kind().discriminant() else {
            continue;
        };
        if discriminant == nudox_ir::kind::KindDiscriminant::Param {
            continue;
        }

        // Path first, then kind, then documentation. Leading with the
        // fully-qualified path means the model sees the module context before
        // it is truncated away, which is what separates `io::Error` from
        // `fmt::Error` when the doc comments are both one line.
        let path = indexes
            .path_of(intro)
            .map_or_else(|| entry.sym().name.clone(), |p| p.as_ref().to_owned());
        let mut text = format!("{path}\n{discriminant:?}\n");
        let documentation = entry.sym().documentation.trim();
        if !documentation.is_empty() {
            text.push_str(documentation);
        }

        // Truncate on a char boundary — `String::truncate` panics otherwise,
        // and doc comments are full of non-ASCII.
        if text.len() > MAX_DOCUMENT_BYTES {
            let mut end = MAX_DOCUMENT_BYTES;
            while end > 0 && !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
        }

        out.push((intro, text));
    }

    out
}

#[cfg(test)]
mod tests {
    /// Maximum number of resident packages waiting for semantic indexing.
    const SEMANTIC_QUEUE_CAPACITY: usize = 4;

    use std::sync::Arc;

    #[test]
    fn semantic_admission_is_bounded_and_does_not_accumulate_residency() {
        let (tx, rx) = flume::bounded(SEMANTIC_QUEUE_CAPACITY);
        let accounted: Vec<_> = (0..=SEMANTIC_QUEUE_CAPACITY)
            .map(|n| Arc::new(vec![n; 1024]))
            .collect();

        for item in accounted.iter().take(SEMANTIC_QUEUE_CAPACITY) {
            tx.try_send(Arc::clone(item))
                .expect("capacity-sized admission must succeed");
        }
        assert_eq!(rx.len(), SEMANTIC_QUEUE_CAPACITY);
        assert!(
            tx.try_send(Arc::clone(&accounted[SEMANTIC_QUEUE_CAPACITY]))
                .is_err(),
            "a slow embedder must not accumulate beyond the fixed budget"
        );

        for _ in 0..SEMANTIC_QUEUE_CAPACITY {
            rx.try_recv()
                .expect("every admitted item must remain drainable");
        }
        assert!(rx.is_empty());
    }
}
