//! Adversarial contract tests for `nudox-store`.
//!
//! These are deliberately written *against* the crate rather than alongside it.
//! Where the in-module unit tests check that each piece does what its author
//! intended, these tests re-derive the expected answers from `nudox-ir`
//! directly and compare — so a shared misunderstanding between the index
//! builder and its own unit test cannot hide here.
//!
//! They also pin the properties the layers above depend on but cannot check
//! for themselves: the `IrSource` stream's ordering guarantees (GUI-LOCAL-PLAN
//! §L3.2), and the `Send + Sync + 'static` shape that lets `nudox-graph` build
//! `'static` vertices and the engine move the corpus across runtimes (§L4.1).

use std::collections::{HashMap, HashSet};

use futures::StreamExt as _;
use nudox_ir::change::{IntroId, PackageLineageId, StableRef};
use nudox_ir::kind::Kind;
use nudox_ir::view::IrView;
use nudox_ir::vocab::Confidence;
use nudox_store::prelude::*;
use nudox_store::source::fixtures::{FixtureSource, build_rich_view, rich_lineage};

// ── Type-level guarantees ────────────────────────────────────────────────────

/// `Corpus` must be shareable across the engine's runtimes without ceremony.
///
/// This is a compile-time assertion: if `Corpus` ever gains a non-`Send` field
/// the whole engine design (LR-9) stops working, and it should fail *here*
/// with a clear message rather than at some distant call site.
#[test]
fn corpus_is_send_sync_static() {
    fn assert_shape<T: Send + Sync + 'static>() {}
    assert_shape::<Corpus>();
    assert_shape::<PackageView>();
    assert_shape::<EntryRef>();
}

// ── Stream protocol (§L3.2) ──────────────────────────────────────────────────

/// Every package must announce itself before it is delivered, and a package
/// that fails must never subsequently be reported ready.
///
/// The GUI relies on `Discovered` to paint a placeholder row; if `Ready` could
/// arrive first the row would appear already-filled and then re-animate.
#[tokio::test]
async fn load_stream_orders_discovered_before_ready_per_package() {
    let source = FixtureSource::both();
    let mut stream = source.load(LoadRequest::default());

    let mut discovered: Vec<PackageLineageId> = Vec::new();
    let mut ready: Vec<PackageLineageId> = Vec::new();
    let mut failed: HashSet<PackageLineageId> = HashSet::new();

    while let Some(event) = stream.next().await {
        match event.expect("fixture source is infallible at the stream level") {
            LoadEvent::Discovered { lineage, .. } => discovered.push(lineage),
            LoadEvent::Progress { lineage, .. } => {
                assert!(
                    discovered.contains(&lineage),
                    "Progress for {lineage} arrived before its Discovered"
                );
            }
            LoadEvent::Ready { package } => {
                let lineage = package.lineage().clone();
                assert!(
                    discovered.contains(&lineage),
                    "Ready for {lineage} arrived before its Discovered"
                );
                assert!(
                    !failed.contains(&lineage),
                    "{lineage} was reported Ready after it had already Failed"
                );
                ready.push(lineage);
            }
            LoadEvent::Failed { lineage, .. } => {
                failed.insert(lineage);
            }
            _ => {}
        }
    }

    assert!(!ready.is_empty(), "fixture source produced no packages");
    assert_eq!(
        discovered.len(),
        ready.len() + failed.len(),
        "every discovered package must end in exactly one terminal event"
    );
}

// ── Index correctness, re-derived independently ──────────────────────────────

/// Recompute the usages postings straight from the occurrence facts and compare.
///
/// `PackageIndexes` filters occurrences to `Confidence >= Index` (the
/// "graph-worthy floor" ported from `registry::graph::reverse_index`). This
/// test rebuilds that mapping from `IrView::all_occurrences` with no knowledge
/// of how the index does it, so an off-by-one in the confidence comparison —
/// `>` instead of `>=`, say — cannot pass both implementations.
#[test]
fn usages_postings_match_independently_computed_truth() {
    let view = build_rich_view();
    let package = rich_lineage();
    let pv = PackageView::build(view, Provenance::TrustedLocal);

    let mut expected: HashMap<StableRef, Vec<IntroId>> = HashMap::new();
    for (owner, occ) in pv.view().all_occurrences() {
        if occ.confidence >= Confidence::Index {
            expected.entry(occ.target.clone()).or_default().push(owner);
        }
    }
    for owners in expected.values_mut() {
        owners.sort();
        owners.dedup();
    }

    assert!(
        !expected.is_empty(),
        "fixture must contain graph-worthy occurrences or this test proves nothing"
    );

    for (target, owners) in &expected {
        assert_eq!(
            pv.indexes().usages_of(target),
            owners.as_slice(),
            "usages_of({target}) disagreed with the independently computed postings"
        );
    }

    // And the converse: nothing below the floor may have leaked in.
    for (_owner, occ) in pv.view().all_occurrences() {
        if occ.confidence < Confidence::Index && !expected.contains_key(&occ.target) {
            assert!(
                pv.indexes().usages_of(&occ.target).is_empty(),
                "occurrence below the Index confidence floor leaked into usages for {}",
                occ.target
            );
        }
    }

    let _ = package;
}

/// Every entry in the view must be reachable through the corpus by `StableRef`.
///
/// `StableRef` is the one key (LR-1) — it is the MCP argument, the tab key and
/// the vertex id. If any entry were unreachable by it, those surfaces would
/// have symbols they can display but cannot link to.
#[tokio::test]
async fn every_entry_is_reachable_by_stable_ref() {
    let view = build_rich_view();
    let lineage = view.package().clone();
    let expected: Vec<IntroId> = view.entries().map(|(intro, _)| intro).collect();

    let corpus = Corpus::new();
    corpus
        .insert(std::sync::Arc::new(PackageView::build(
            view,
            Provenance::TrustedLocal,
        )))
        .await;

    assert!(!expected.is_empty());
    for intro in expected {
        let key = StableRef::new(lineage.clone(), intro);
        let entry_ref = corpus
            .entry(&key)
            .await
            .unwrap_or_else(|| panic!("{key} was not reachable through the corpus"));
        assert!(
            entry_ref.get().is_some(),
            "{key} resolved to an EntryRef whose entry is missing"
        );
    }
}

/// Precomputed paths must exist for every entry, and be unique per entry.
///
/// Search rows and breadcrumbs render these directly (§1.1.4: no string work in
/// `render`), so a missing path means a blank row and a duplicated path means
/// two different symbols that look identical to the user.
#[test]
fn precomputed_paths_are_total_and_unique() {
    let pv = PackageView::build(build_rich_view(), Provenance::TrustedLocal);

    let mut seen: HashMap<String, IntroId> = HashMap::new();
    for (intro, _entry) in pv.view().entries() {
        let path = pv
            .indexes()
            .path_of(intro)
            .unwrap_or_else(|| panic!("no precomputed path for {intro:?}"));
        if let Some(previous) = seen.insert(path.to_string(), intro) {
            panic!("path {path:?} is shared by {previous:?} and {intro:?}");
        }
    }
}

// ── Concurrency ──────────────────────────────────────────────────────────────

/// Concurrent readers must not deadlock or starve, including while a write
/// lands between them.
///
/// The engine reads the corpus from the query `LocalSet` while the loader is
/// still inserting packages; if `Corpus`'s locking were re-entrant-unsafe this
/// is where it would hang rather than at some unlucky moment in the GUI.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_reads_during_insert_do_not_deadlock() {
    let corpus = Corpus::new();
    let view = build_rich_view();
    let lineage = view.package().clone();
    corpus
        .insert(std::sync::Arc::new(PackageView::build(
            view,
            Provenance::TrustedLocal,
        )))
        .await;

    let readers = (0..8).map(|_| {
        let corpus = corpus.clone();
        let lineage = lineage.clone();
        tokio::spawn(async move {
            for _ in 0..200 {
                assert!(corpus.package(&lineage).await.is_some());
                let _ = corpus.packages().await;
            }
        })
    });

    // A late insert racing the readers — the common shape during workspace load.
    let writer = {
        let corpus = corpus.clone();
        tokio::spawn(async move {
            let pv = PackageView::build(
                nudox_store::source::fixtures::build_perf_view(),
                Provenance::TrustedLocal,
            );
            corpus.insert(std::sync::Arc::new(pv)).await;
        })
    };

    for reader in readers {
        reader.await.expect("reader task panicked or deadlocked");
    }
    writer.await.expect("writer task panicked or deadlocked");
    assert_eq!(corpus.len().await, 2);
}

// ── Scale ────────────────────────────────────────────────────────────────────

/// The 10 000-symbol fixture must index within a budget the GUI can absorb.
///
/// This is not a micro-benchmark; it is a guard against an accidental
/// quadratic. Index construction happens off the GPUI thread, but a workspace
/// with 250 dependencies multiplies whatever this costs by 250.
#[test]
fn perf_corpus_indexes_without_quadratic_blowup() {
    let view = nudox_store::source::fixtures::build_perf_view();
    let entry_count = view.table().len();
    assert!(
        entry_count >= 10_000,
        "perf fixture shrank to {entry_count}; the guard is meaningless"
    );

    let started = std::time::Instant::now();
    let pv = PackageView::build(view, Provenance::TrustedLocal);
    let elapsed = started.elapsed();

    // Generous: a linear pass over 10k entries is milliseconds. This only
    // catches an algorithmic regression, not a constant-factor one.
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "indexing {entry_count} entries took {elapsed:?}"
    );
    assert_eq!(pv.view().table().len(), entry_count);
}

// ── Kind coverage, checked against nudox-ir's own enumeration ────────────────

/// The rich fixture must exercise every `Kind` the IR can represent.
///
/// The in-module test asserts against a hand-written list of 13 variants, which
/// silently stops being exhaustive the day a 14th is added. This one walks the
/// entries and asserts the count matches `KindDiscriminant`'s own round-trip
/// range, so adding a kind to `nudox-ir` fails here until the fixture covers it.
#[test]
fn rich_fixture_covers_every_kind_the_ir_defines() {
    let view: IrView = build_rich_view();

    let mut seen = HashSet::new();
    for (_intro, entry) in view.entries() {
        if let Some(kind) = entry.kind().as_owned_kind() {
            seen.insert(discriminant_index(kind));
        }
    }

    // `KindDiscriminant` is a frozen repr(u16) enumeration starting at 1.
    let defined: HashSet<u16> = (1..=u16::MAX)
        .take_while(|n| nudox_ir::kind::KindDiscriminant::from_u16(*n).is_some())
        .collect();

    assert!(!defined.is_empty(), "could not enumerate KindDiscriminant");
    let missing: Vec<u16> = defined.difference(&seen).copied().collect();
    assert!(
        missing.is_empty(),
        "rich fixture is missing kinds {missing:?} \
         (nudox-ir defines {} kinds, fixture covers {})",
        defined.len(),
        seen.len()
    );
}

fn discriminant_index(kind: &Kind) -> u16 {
    kind.discriminant().as_u16()
}
