//! Cross-process determinism of the search ranking.
//!
//! # The bug this file exists to make impossible
//!
//! Search results used to differ **between process launches**: the same corpus
//! and the same query produced a different ranking every run, because std
//! `HashMap`'s per-process `RandomState` seed reached the UI through three
//! independent paths —
//!
//! 1. `Corpus::packages()` collecting a `HashMap`'s values,
//! 2. `PackageIndexes::build` walking `IrView::entries()` (a `HashMap`) into a
//!    `NameIndex` that appended in arrival order and never sorted,
//! 3. `collect_name_hits`' comparator ending at `display_name` — which every
//!    case-folded exact match shares, so a whole result set could be one flat
//!    tie that a *stable* sort resolved by retaining input order, i.e. by (1)
//!    and (2).
//!
//! # Why the old guard could not have caught it
//!
//! `search_adversarial::repeated_identical_queries_produce_stable_results` runs
//! two queries in one process against one prebuilt index. Both share a single
//! `RandomState` seed and a single already-built `NameIndex`, so it is green
//! under every one of the three hazards above. It guards against a
//! *comparator* that is nondeterministic within a run — a real but different
//! invariant. It is kept, and this file adds the invariant it cannot state.
//!
//! # How a single-process test can prove a cross-process property
//!
//! Two ways, both used here.
//!
//! * **Pinned expectation.** [`ranking_over_total_ties_is_pinned_to_symbol_identity`]
//!   writes down the full expected row sequence as a literal. That literal was
//!   computed in a *different process* than the one that will check it, which
//!   is exactly what "the ranking does not depend on the process" asserts. Any
//!   hash seed reaching the output makes some process disagree with the
//!   literal.
//! * **Adversarial construction order.** [`corpora_built_in_different_orders_rank_identically`]
//!   builds the same logical corpus twice — packages registered in opposite
//!   order, entries inserted into each declaration table in opposite order —
//!   and requires byte-identical output. Construction order is the only thing
//!   a process seed can perturb, so a ranking invariant to it is invariant to
//!   the seed.
//!
//! # Adversarial coverage (doctrine §4)
//!
//! Empty query, single-element corpus, duplicate/colliding leaf names, unicode
//! names, and a set of entries that tie on *every* scoring input (identical
//! score, identical leaf name, identical kind, identical visibility) so that
//! only the final identity term of the comparator can separate them.

use std::sync::Arc;
use std::time::Duration;

use futures::stream::BoxStream;

use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, IntroId, PackageLineageId, PackageName},
    entry::{Entry, Node, Symbol, Visibility},
    index::RawRef,
    kind::Kind,
    kinds::{Function, Module},
    view::IrView,
};
use nudox_store::{
    package::{PackageView, Provenance},
    source::{IrSource, LoadEvent, LoadRequest, PackageHint, SourceDescriptor, SourceError},
};

use nudox_engine::{
    Engine, EngineConfig, SearchQuery,
    search::{SECTION_NAME, SECTION_TYPE},
    wire::{Gen, HitRow, SearchEvent, SearchSectionId},
};

// ---------------------------------------------------------------------------
// Corpus construction
// ---------------------------------------------------------------------------

/// One declaration in a synthetic package: raw `IntroId` byte, leaf name,
/// parent byte (`None` for the package root), kind and visibility.
#[derive(Clone, Copy)]
struct Decl {
    id: u8,
    name: &'static str,
    parent: Option<u8>,
    kind: DeclKind,
    visibility: Visibility,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeclKind {
    Module,
    Function,
}

fn lineage(name: &str) -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("test"), PackageName::new(name))
}

fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

fn entry_of(decl: &Decl) -> Entry {
    let sym = Symbol {
        name: decl.name.to_owned(),
        visibility: decl.visibility,
        documentation: String::new(),
        source: std::path::PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };
    let kind = match decl.kind {
        DeclKind::Module => Kind::Module(Module),
        DeclKind::Function => Kind::Function(Function::builder().build()),
    };
    Entry::new(sym, Node::build(None::<RawRef>, []), kind)
}

/// Build a `PackageView` from `decls`, inserting them in the given order.
///
/// `insertion` is the whole point of this helper: the declaration table is a
/// `HashMap`, so the order rows are inserted in is the one lever a test has on
/// its internal layout — and therefore on every projection that used to walk it
/// unordered. Parents must exist before children only for the *moniker* walk,
/// which reads the finished table, so any permutation is legal here.
fn build_package(name: &str, decls: &[Decl], insertion: Insertion) -> Arc<PackageView> {
    let mut ordered: Vec<&Decl> = decls.iter().collect();
    if insertion == Insertion::Reversed {
        ordered.reverse();
    }

    let mut table = PristineIntroTable::new();
    for decl in ordered {
        table.insert_live(intro(decl.id), entry_of(decl), decl.parent.map(intro));
    }
    let view = IrView::with_package(lineage(name), table);
    Arc::new(PackageView::build(view, Provenance::TrustedLocal))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Insertion {
    Forward,
    Reversed,
}

// ---------------------------------------------------------------------------
// The adversarial corpus
// ---------------------------------------------------------------------------
//
// Two packages, `aaa-pkg` and `zzz-pkg`, each declaring several functions
// named `dup` under distinct parent modules.
//
// Every `dup` row is a *total tie* on the entire scoring input set: identical
// case-folded key, identical query, identical kind (`Function`), identical
// visibility (`Public`). They also share a leaf `display_name` at sort time,
// because qualification happens after the sort. Nothing but the symbol's own
// identity can order them.
//
// The `IntroId` bytes are chosen so the expected order matches *no* other
// plausible ordering — not the module names, not the declaration order, not
// the order the decls appear in this array. `m2 < m4 < m1 < m3` is only
// derivable from the ids.

const AAA_DECLS: &[Decl] = &[
    Decl { id: 0xF0, name: "aaa-pkg", parent: None, kind: DeclKind::Module, visibility: Visibility::Public },
    Decl { id: 0xC1, name: "m1", parent: Some(0xF0), kind: DeclKind::Module, visibility: Visibility::Public },
    Decl { id: 0xC2, name: "m2", parent: Some(0xF0), kind: DeclKind::Module, visibility: Visibility::Public },
    Decl { id: 0xC3, name: "m3", parent: Some(0xF0), kind: DeclKind::Module, visibility: Visibility::Public },
    Decl { id: 0xC4, name: "m4", parent: Some(0xF0), kind: DeclKind::Module, visibility: Visibility::Public },
    // The tie set. Ids deliberately unsorted with respect to their module names.
    Decl { id: 0x40, name: "dup", parent: Some(0xC1), kind: DeclKind::Function, visibility: Visibility::Public },
    Decl { id: 0x10, name: "dup", parent: Some(0xC2), kind: DeclKind::Function, visibility: Visibility::Public },
    Decl { id: 0x70, name: "dup", parent: Some(0xC3), kind: DeclKind::Function, visibility: Visibility::Public },
    Decl { id: 0x20, name: "dup", parent: Some(0xC4), kind: DeclKind::Function, visibility: Visibility::Public },
    // Unicode: two entries whose names differ only by case under Unicode
    // folding, so they share one `NameIndex` bucket and tie exactly like `dup`.
    Decl { id: 0x33, name: "Ünïcödé", parent: Some(0xC1), kind: DeclKind::Function, visibility: Visibility::Public },
    Decl { id: 0x11, name: "ünïcödé", parent: Some(0xC2), kind: DeclKind::Function, visibility: Visibility::Public },
    // A uniquely-named entry, to prove the collision machinery is selective.
    Decl { id: 0x55, name: "solo", parent: Some(0xF0), kind: DeclKind::Function, visibility: Visibility::Public },
];

const ZZZ_DECLS: &[Decl] = &[
    Decl { id: 0xF1, name: "zzz-pkg", parent: None, kind: DeclKind::Module, visibility: Visibility::Public },
    Decl { id: 0xD1, name: "n1", parent: Some(0xF1), kind: DeclKind::Module, visibility: Visibility::Public },
    Decl { id: 0xD2, name: "n2", parent: Some(0xF1), kind: DeclKind::Module, visibility: Visibility::Public },
    Decl { id: 0xD3, name: "n3", parent: Some(0xF1), kind: DeclKind::Module, visibility: Visibility::Public },
    Decl { id: 0x90, name: "dup", parent: Some(0xD1), kind: DeclKind::Function, visibility: Visibility::Public },
    Decl { id: 0x50, name: "dup", parent: Some(0xD2), kind: DeclKind::Function, visibility: Visibility::Public },
    Decl { id: 0xB0, name: "dup", parent: Some(0xD3), kind: DeclKind::Function, visibility: Visibility::Public },
];

/// The ranking a query for `dup` must produce, in every process, forever.
///
/// Derivation, entirely from the data: all seven rows score 1.0 (exact,
/// `Public`, `Function`), all seven share the leaf `dup`, so the order is the
/// comparator's identity term — package lineage (`aaa-pkg` before `zzz-pkg`),
/// then `IntroId` ascending. Within `aaa-pkg`: `0x10 (m2) < 0x20 (m4) <
/// 0x40 (m1) < 0x70 (m3)`; within `zzz-pkg`: `0x50 (n2) < 0x90 (n1) <
/// 0xB0 (n3)`.
const EXPECTED_DUP_RANKING: &[&str] = &[
    "aaa-pkg.m2.dup",
    "aaa-pkg.m4.dup",
    "aaa-pkg.m1.dup",
    "aaa-pkg.m3.dup",
    "zzz-pkg.n2.dup",
    "zzz-pkg.n1.dup",
    "zzz-pkg.n3.dup",
];

// ---------------------------------------------------------------------------
// StaticSource — replays a fixed list of packages in a caller-chosen order
// ---------------------------------------------------------------------------

struct StaticSource {
    items: Vec<Arc<PackageView>>,
}

impl IrSource for StaticSource {
    fn describe(&self) -> SourceDescriptor {
        SourceDescriptor {
            label: "static-determinism".to_owned(),
            package_count_hint: Some(self.items.len() as u32),
        }
    }

    fn load(&self, _request: LoadRequest) -> BoxStream<'static, Result<LoadEvent, SourceError>> {
        use futures::StreamExt as _;
        let events: Vec<Result<LoadEvent, SourceError>> = self
            .items
            .iter()
            .flat_map(|pkg| {
                let lid = pkg.lineage().clone();
                [
                    Ok(LoadEvent::Discovered {
                        lineage: lid.clone(),
                        hint: PackageHint {
                            display_name: lid.name.as_str().to_owned(),
                            ecosystem: lid.ecosystem.as_str().to_owned(),
                            version: None,
                        },
                    }),
                    Ok(LoadEvent::Ready {
                        package: Arc::clone(pkg),
                    }),
                ]
            })
            .collect();
        futures::stream::iter(events).boxed()
    }
}

/// Build an engine over the two-package adversarial corpus.
///
/// `packages_reversed` flips the order the source announces packages in (which
/// is the order they are inserted into the `Corpus` map), and `insertion` flips
/// the order declarations enter each package's table. Together they are every
/// construction-order degree of freedom the corpus has.
fn engine_over_adversarial_corpus(
    packages_reversed: bool,
    insertion: Insertion,
) -> nudox_engine::EngineHandle {
    let mut items = vec![
        build_package("aaa-pkg", AAA_DECLS, insertion),
        build_package("zzz-pkg", ZZZ_DECLS, insertion),
    ];
    if packages_reversed {
        items.reverse();
    }
    Engine::start(EngineConfig::default(), StaticSource { items })
}

// ---------------------------------------------------------------------------
// Query plumbing
// ---------------------------------------------------------------------------

async fn wait_for_packages(engine: &nudox_engine::EngineHandle, n: usize) {
    let rx = engine.packages();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut seen = 0usize;
    while seen < n {
        match tokio::time::timeout_at(deadline, rx.recv_async()).await {
            Ok(Ok(nudox_engine::PackageLoadEvent::Loaded { .. })) => seen += 1,
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => break,
            Err(_) => panic!("corpus of {n} package(s) never fully seeded within 10 s"),
        }
    }
}

async fn drain(rx: flume::Receiver<SearchEvent>) -> Vec<SearchEvent> {
    let mut events = Vec::new();
    while let Ok(ev) = rx.recv_async().await {
        let terminal = matches!(ev, SearchEvent::Done { .. } | SearchEvent::Failed { .. });
        events.push(ev);
        if terminal {
            break;
        }
    }
    events
}

fn section_rows(events: &[SearchEvent], want: SearchSectionId) -> Vec<HitRow> {
    events
        .iter()
        .filter_map(|e| match e {
            SearchEvent::Section { section, rows, .. } if *section == want => {
                Some(rows.iter().cloned().collect::<Vec<_>>())
            }
            _ => None,
        })
        .flatten()
        .collect()
}

/// Run one query and return the `SECTION_NAME` rows in ranked order.
async fn name_rows(
    engine: &nudox_engine::EngineHandle,
    text: &str,
    generation: Gen,
) -> Vec<HitRow> {
    let q = SearchQuery {
        text: text.to_owned(),
        kinds: Vec::new(),
            packages: Vec::new(),
        limit: 0,
    };
    let (_h, rx) = engine.search(q, generation);
    section_rows(&drain(rx).await, SECTION_NAME)
}

/// The comparable projection of a ranked row: everything a client renders,
/// plus the identity it navigates to. Asserting on this rather than on
/// `display_name` alone means a fix that made names stable while leaving the
/// underlying rows shuffled would still fail.
fn fingerprint(rows: &[HitRow]) -> Vec<(String, String, String)> {
    rows.iter()
        .map(|r| {
            (
                r.display_name.to_string(),
                r.key.intro.to_hex(),
                format!("{:.6}", r.score),
            )
        })
        .collect()
}

fn measure_dir(case: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "nudox-engine-search-determinism-{case}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create measurement directory");
    dir
}

// ---------------------------------------------------------------------------
// 1. Pinned expectation — the cross-process assertion
// ---------------------------------------------------------------------------

/// Seven entries that tie on *every* scoring input must rank in one fixed
/// order, and that order is written down here as a literal.
///
/// This is the assertion that a per-process hash seed cannot survive. The
/// expected sequence was derived from the corpus data alone (see
/// [`EXPECTED_DUP_RANKING`]) and is checked by a different process on every
/// run; under the old code the order came from `HashMap` iteration, which
/// agrees with a fixed permutation of seven elements in roughly one run in
/// 5 040.
#[tokio::test]
async fn ranking_over_total_ties_is_pinned_to_symbol_identity() {
    let dir = measure_dir("pinned");
    // The measured region is the corpus build: `PackageView::build` runs
    // `PackageIndexes::build`, which is the O(n log n) walk this change moved
    // from `entries()` to `entries_sorted()`. It is the part of this test whose
    // cost the fix could plausibly move, so it is the part worth reporting.
    let (engine, _cost) = nudox_test_support::measured(
        "engine.search_determinism.adversarial_corpus_index_build",
        &dir,
        || engine_over_adversarial_corpus(false, Insertion::Forward),
    );
    wait_for_packages(&engine, 2).await;

    let rows = name_rows(&engine, "dup", Gen(1)).await;
    let names: Vec<String> = rows.iter().map(|r| r.display_name.to_string()).collect();

    assert_eq!(
        names, EXPECTED_DUP_RANKING,
        "the ranking of seven rows that tie on every scoring input is not the one \
         determined by their symbol identities — some ordering upstream of the \
         comparator (corpus map iteration, name-index bucket order) is reaching \
         the result"
    );

    // Content, not shape (doctrine §4): each row must resolve to the intro its
    // qualified path names, so a rename of the assertion above cannot be
    // satisfied by rows that merely happen to sort into that sequence.
    let expected_intros = [0x10u8, 0x20, 0x40, 0x70, 0x50, 0x90, 0xB0];
    let actual_intros: Vec<String> = rows.iter().map(|r| r.key.intro.to_hex()).collect();
    let want_intros: Vec<String> = expected_intros.iter().map(|b| intro(*b).to_hex()).collect();
    assert_eq!(
        actual_intros, want_intros,
        "ranked rows do not carry the intros their display names claim"
    );

    // Every tie really is a tie: if these scores differed, the ordering above
    // would be explained by the score and this test would not be exercising
    // the identity term at all.
    let scores: Vec<f32> = rows.iter().map(|r| r.score).collect();
    assert!(
        scores.iter().all(|s| (*s - scores[0]).abs() < f32::EPSILON),
        "the `dup` rows were supposed to tie on score; got {scores:?} — this test \
         no longer exercises the tiebreak it claims to"
    );
}

// ---------------------------------------------------------------------------
// 2. Adversarial construction order
// ---------------------------------------------------------------------------

/// The same logical corpus, assembled four different ways, must rank
/// identically.
///
/// Construction order is the only lever a process seed has: it decides the
/// `HashMap` layout of the declaration table and of the corpus package map.
/// A ranking invariant under every permutation of it is invariant under the
/// seed, which is the cross-process property.
#[tokio::test]
async fn corpora_built_in_different_orders_rank_identically() {
    let dir = measure_dir("orders");

    let mut fingerprints: Vec<(String, Vec<(String, String, String)>)> = Vec::new();
    let mut generation = 100u64;

    for (label, packages_reversed, insertion) in [
        ("forward/forward", false, Insertion::Forward),
        ("forward/reversed", false, Insertion::Reversed),
        ("reversed/forward", true, Insertion::Forward),
        ("reversed/reversed", true, Insertion::Reversed),
    ] {
        // Measure each corpus build separately: if a construction order were
        // ever to cost measurably more than another, the four `cost` lines
        // would say so rather than averaging it away.
        let (engine, _cost) = nudox_test_support::measured(
            &format!("engine.search_determinism.corpus_build[{label}]"),
            &dir,
            || engine_over_adversarial_corpus(packages_reversed, insertion),
        );
        wait_for_packages(&engine, 2).await;

        // Both sections: `collect_type_hits` never sorted at all, so a query
        // that hits the kind facet is a second, independent path to the same
        // bug.
        let names = name_rows(&engine, "dup", Gen(generation)).await;
        generation += 1;

        let q = SearchQuery {
            text: "fn".to_owned(),
            kinds: Vec::new(),
            packages: Vec::new(),
            limit: 0,
        };
        let (_h, rx) = engine.search(q, Gen(generation));
        generation += 1;
        let types = section_rows(&drain(rx).await, SECTION_TYPE);

        let mut fp = fingerprint(&names);
        fp.extend(fingerprint(&types));
        fingerprints.push((label.to_owned(), fp));
    }

    let ((_, baseline), rest) = fingerprints
        .split_first()
        .expect("four corpora were constructed");

    for (label, fp) in rest {
        assert_eq!(
            fp, baseline,
            "corpus built as {label} ranks differently from the baseline \
             corpus — the ranking depends on construction order, which is \
             exactly the channel a per-process hash seed travels through"
        );
    }

    assert_eq!(
        baseline.len(),
        EXPECTED_DUP_RANKING.len() + type_section_expected_len(),
        "the compared fingerprint is not the size the corpus implies; a shrunken \
         result set would make the equality above vacuous"
    );
}

/// Every `Function` in the two-package corpus, which is what a `fn` kind-facet
/// query returns: four `dup` + two unicode + one `solo` in `aaa-pkg`, three
/// `dup` in `zzz-pkg`.
fn type_section_expected_len() -> usize {
    let count = |decls: &[Decl]| {
        decls
            .iter()
            .filter(|d| d.kind == DeclKind::Function)
            .count()
    };
    count(AAA_DECLS) + count(ZZZ_DECLS)
}

// ---------------------------------------------------------------------------
// 3. Ranking quality: public API above a same-named private symbol
// ---------------------------------------------------------------------------

/// A `Public` function must outrank a `Private` module of exactly the same
/// name.
///
/// Both are case-folded exact matches, so text relevance cannot separate them;
/// only the visibility factor can. This is the shape real crates produce —
/// `memchr` declares a private `mod memchr` once per architecture backend, all
/// of them exact-matching a search for `memchr`.
#[tokio::test]
async fn public_api_outranks_same_named_private_module() {
    const DECLS: &[Decl] = &[
        Decl { id: 0x01, name: "vis-pkg", parent: None, kind: DeclKind::Module, visibility: Visibility::Public },
        Decl { id: 0x02, name: "arch", parent: Some(0x01), kind: DeclKind::Module, visibility: Visibility::Public },
        // The private implementation-detail module. Its id sorts *first*, so
        // under identity-only tiebreaking it would lead — only the visibility
        // weight can demote it.
        Decl { id: 0x03, name: "target", parent: Some(0x02), kind: DeclKind::Module, visibility: Visibility::Private },
        // The public API the reader is actually looking for.
        Decl { id: 0x04, name: "target", parent: Some(0x01), kind: DeclKind::Function, visibility: Visibility::Public },
    ];

    let pkg = build_package("vis-pkg", DECLS, Insertion::Forward);
    let engine = Engine::start(EngineConfig::default(), StaticSource { items: vec![pkg] });
    wait_for_packages(&engine, 1).await;

    let rows = name_rows(&engine, "target", Gen(200)).await;
    assert_eq!(rows.len(), 2, "both `target` entries must be returned: {rows:?}");

    let public_intro = intro(0x04).to_hex();
    let private_intro = intro(0x03).to_hex();

    assert_eq!(
        rows[0].key.intro.to_hex(),
        public_intro,
        "the public function must rank first; got {:?} then {:?}",
        rows[0].display_name,
        rows[1].display_name
    );
    assert_eq!(rows[1].key.intro.to_hex(), private_intro);
    assert!(
        rows[0].score > rows[1].score,
        "the public entry must *score* above the private one, not merely sort \
         above it by identity; got {} vs {}",
        rows[0].score,
        rows[1].score
    );
    assert!(
        (rows[0].score - 1.0).abs() < f32::EPSILON,
        "an exact match on a public, top-level symbol is the 1.0 reference \
         point of the scoring model; got {}",
        rows[0].score
    );
}

// ---------------------------------------------------------------------------
// 4. Adversarial inputs
// ---------------------------------------------------------------------------

/// An empty query matches nothing — it must not fall through to "every symbol
/// has the empty string as a prefix".
#[tokio::test]
async fn empty_query_produces_no_name_rows() {
    let engine = engine_over_adversarial_corpus(false, Insertion::Forward);
    wait_for_packages(&engine, 2).await;

    for text in ["", "   ", "\t\n"] {
        let rows = name_rows(&engine, text, Gen(300)).await;
        assert!(
            rows.is_empty(),
            "query {text:?} must match nothing; got {:?}",
            rows.iter().map(|r| r.display_name.to_string()).collect::<Vec<_>>()
        );
    }
}

/// A one-package, one-entry corpus returns exactly that entry, unqualified.
///
/// The degenerate end of the collision machinery: with a single candidate
/// there is nothing to disambiguate against, so the short form must survive.
#[tokio::test]
async fn single_entry_corpus_returns_exactly_that_entry() {
    const DECLS: &[Decl] = &[Decl {
        id: 0x07,
        name: "lonely",
        parent: None,
        kind: DeclKind::Module,
        visibility: Visibility::Public,
    }];

    let pkg = build_package("solo-pkg", DECLS, Insertion::Forward);
    let engine = Engine::start(EngineConfig::default(), StaticSource { items: vec![pkg] });
    wait_for_packages(&engine, 1).await;

    let rows = name_rows(&engine, "lonely", Gen(400)).await;
    assert_eq!(rows.len(), 1, "exactly one entry exists: {rows:?}");
    assert_eq!(&*rows[0].display_name, "lonely");
    assert_eq!(rows[0].key.intro.to_hex(), intro(0x07).to_hex());
}

/// A leaf name unique in the result set keeps its short form even though the
/// corpus is full of collisions.
#[tokio::test]
async fn unique_leaf_name_is_not_qualified() {
    let engine = engine_over_adversarial_corpus(false, Insertion::Forward);
    wait_for_packages(&engine, 2).await;

    let rows = name_rows(&engine, "solo", Gen(500)).await;
    assert_eq!(rows.len(), 1, "exactly one `solo` exists: {rows:?}");
    assert_eq!(&*rows[0].display_name, "solo");
}

/// Two symbols whose names differ only by Unicode case share one index bucket
/// and must come back in a fixed order, with both display forms preserved.
///
/// Case folding is where a name index most easily loses information; asserting
/// on the *display* strings proves the original casing survived the fold, and
/// asserting on the order proves the shared bucket is sorted rather than
/// arrival-ordered.
#[tokio::test]
async fn unicode_case_folded_collisions_are_ordered_and_keep_their_casing() {
    let engine = engine_over_adversarial_corpus(false, Insertion::Forward);
    wait_for_packages(&engine, 2).await;

    let rows = name_rows(&engine, "ünïcödé", Gen(600)).await;
    assert_eq!(
        rows.len(),
        2,
        "both Unicode-case variants must land in one bucket: {rows:?}"
    );

    // These two tie on score but *not* on leaf display name — the fold that
    // merged them into one bucket is not applied to the sort key, so the
    // declared spellings separate them: `Ü` is U+00DC (UTF-8 `C3 9C`) and `ü`
    // is U+00FC (`C3 BC`), so `Ünïcödé` sorts first. The identity term never
    // has to run here; that path is covered by the `dup` tie set.
    assert_eq!(rows[0].key.intro.to_hex(), intro(0x33).to_hex());
    assert_eq!(rows[1].key.intro.to_hex(), intro(0x11).to_hex());

    // The uppercase query must produce the identical ranking.
    let upper = name_rows(&engine, "ÜNÏCÖDÉ", Gen(601)).await;
    assert_eq!(
        fingerprint(&rows),
        fingerprint(&upper),
        "case-folding the query changed the ranking"
    );

    // Original casing survives the fold: the qualified names still carry the
    // declared spelling of each symbol.
    let joined: Vec<String> = rows.iter().map(|r| r.display_name.to_string()).collect();
    assert!(
        joined.iter().any(|n| n.ends_with("ünïcödé")) && joined.iter().any(|n| n.ends_with("Ünïcödé")),
        "both declared spellings must be recoverable from the rows; got {joined:?}"
    );
}
