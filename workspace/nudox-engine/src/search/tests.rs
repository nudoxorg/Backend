use std::{sync::Arc, time::Duration};

use crate::store::source::fixtures::FixtureSource;

use crate::{
    runtime::{Engine, EngineConfig},
    search::{SECTION_NAME, SECTION_TYPE, SearchQuery, collect_name_hits},
    wire::{Gen, SearchEvent, SharedStr},
};

fn make_engine() -> crate::runtime::EngineHandle {
    Engine::start(EngineConfig::default(), FixtureSource::rich())
}

async fn wait_for_corpus(engine: &crate::runtime::EngineHandle) {
    use crate::store::source::fixtures::rich_lineage;
    for _ in 0..50 {
        if engine.corpus().package(&rich_lineage()).await.is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("corpus never seeded");
}

/// Drain the search receiver into a vec of events.
async fn drain(rx: flume::Receiver<SearchEvent>) -> Vec<SearchEvent> {
    let mut events = Vec::new();
    while let Ok(ev) = rx.recv_async().await {
        let done = matches!(ev, SearchEvent::Done { .. } | SearchEvent::Failed { .. });
        events.push(ev);
        if done {
            break;
        }
    }
    events
}

#[tokio::test]
async fn local_name_hits_before_semantic() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    let query = SearchQuery {
        text: "Point".to_owned(),
        kinds: Vec::new(),
        exclude_kinds: Vec::new(),
        packages: Vec::new(),
        limit: 50,
    };
    let (_handle, rx) = engine.search(query, Gen(1));
    let events = drain(rx).await;

    // Find the position of SECTION_NAME and SECTION_SEMANTIC events.
    let name_pos = events.iter().position(
        |e| matches!(e, SearchEvent::Section { section, .. } if *section == SECTION_NAME),
    );
    let semantic_pos = events
        .iter()
        .position(|e| matches!(e, SearchEvent::Section { section, .. } if *section == crate::search::SECTION_SEMANTIC));

    let name_pos = name_pos.expect("must have a SECTION_NAME event");
    let semantic_pos = semantic_pos.expect("must have a SECTION_SEMANTIC event");

    assert!(
        name_pos < semantic_pos,
        "SECTION_NAME (pos {name_pos}) must precede SECTION_SEMANTIC (pos {semantic_pos})"
    );
}

/// The live name section agrees with a full-corpus walk of the same query.
///
/// The driver only opens packages the name index names. This oracle opens
/// every package. The two rankings have to be the same rows in the same
/// order; a prefix filter that dropped a real hit would diverge here.
#[tokio::test]
async fn name_section_matches_a_full_corpus_walk() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    let query = SearchQuery {
        text: "Po".to_owned(),
        kinds: Vec::new(),
        exclude_kinds: Vec::new(),
        packages: Vec::new(),
        limit: 50,
    };
    let oracle = collect_name_hits(&engine.corpus().packages().await, &query);
    assert!(
        !oracle.is_empty(),
        "the fixture declares Point; an empty oracle makes this comparison vacuous"
    );
    let (_handle, rx) = engine.search(query, Gen(4));
    let events = drain(rx).await;
    let live = events.into_iter().find_map(|event| match event {
        SearchEvent::Section { section, rows, .. } if section == SECTION_NAME => {
            Some(rows.to_vec())
        }
        _ => None,
    });
    let live = live.expect("name section");
    assert_eq!(live.len(), oracle.len());
    for (left, right) in live.iter().zip(oracle.iter()) {
        assert_eq!(left.key, right.key);
        assert_eq!(left.score, right.score);
    }
}

/// A later, lower-cosine neighbor outranks an earlier private hit.
///
/// `nearest` returns the private symbol first because its cosine is higher.
/// The section keeps one row. Stopping at that first neighbor would publish
/// the private symbol; ranking the over-fetch publishes the public one.
#[tokio::test]
async fn semantic_overfetch_prefers_a_later_public_symbol() {
    use std::{pin::Pin, sync::Arc};

    use nudox_ir::{
        apply::PristineIntroTable,
        change::{EcosystemId, IntroId, PackageLineageId, PackageName},
        entry::{Entry, Node, Symbol, Visibility},
        kind::Kind,
        kinds::Module,
        view::IrView,
    };

    use crate::{
        semantic::{EmbedRole, Embedder, EmbedderInfo, Error as EmbedError, SemanticIndex},
        store::package::{PackageView, Provenance},
    };

    struct Fixed;

    impl Embedder for Fixed {
        fn info(&self) -> EmbedderInfo {
            EmbedderInfo {
                model_id: SharedStr::from("fixed"),
                dimensions: 2,
                max_batch: 8,
                durable_canonical: false,
            }
        }

        fn embed_batch<'a>(
            &'a self,
            texts: &'a [String],
            _role: EmbedRole,
        ) -> Pin<Box<dyn std::future::Future<Output = Result<Vec<Vec<f32>>, EmbedError>> + Send + 'a>>
        {
            Box::pin(async move { Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect()) })
        }
    }

    fn intro(n: u8) -> IntroId {
        IntroId::from_raw([n; 32])
    }

    fn entry(name: &str, visibility: Visibility) -> Entry {
        Entry::new(
            Symbol {
                name: name.to_owned(),
                visibility,
                documentation: String::new(),
                source: std::path::PathBuf::new(),
                span: 0..0,
                aliases: Box::new([]),
                deprecation: None,
                doc_links: Box::new([]),
                attrs: Box::new([]),
                cfg: None,
            },
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Module(Module),
        )
    }

    let lineage = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("pkg"));
    let mut table = PristineIntroTable::new();
    table.insert_live(intro(1), entry("hidden", Visibility::Private), None);
    table.insert_live(intro(2), entry("shown", Visibility::Public), None);
    let pkg = Arc::new(PackageView::build(
        IrView::with_package(lineage.clone(), table),
        Provenance::TrustedLocal,
    ));

    let semantic = SemanticIndex::new();
    semantic
        .insert_package(
            lineage.clone(),
            vec![(intro(1), vec![1.0, 0.0]), (intro(2), vec![0.5, 0.8660254])],
            2,
        )
        .expect("vectors");

    let nearest = semantic.nearest(&[1.0, 0.0], 2);
    assert_eq!(
        nearest[0].1,
        intro(1),
        "the private symbol is the closer neighbor"
    );

    let query = SearchQuery {
        text: "anything".to_owned(),
        kinds: Vec::new(),
        exclude_kinds: Vec::new(),
        packages: Vec::new(),
        limit: 1,
    };
    let (_state, rows) = super::execute::collect_semantic_hits(
        std::slice::from_ref(&pkg),
        &semantic,
        Some(&Fixed),
        &query,
    )
    .await;
    assert_eq!(rows.len(), 1);
    assert_eq!(&*rows[0].display_name, "shown");
}

#[tokio::test]
async fn search_emits_done_as_terminal() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    let query = SearchQuery {
        text: "fn".to_owned(),
        ..Default::default()
    };
    let (_handle, rx) = engine.search(query, Gen(2));
    let events = drain(rx).await;

    assert!(
        matches!(events.last(), Some(SearchEvent::Done { .. })),
        "last event must be Done"
    );
}

#[tokio::test]
async fn dropping_handle_cancels_stream() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    let query = SearchQuery {
        text: "Point".to_owned(),
        ..Default::default()
    };
    let (handle, rx) = engine.search(query, Gen(3));

    // Drop handle immediately — cancels before any events land.
    drop(handle);

    // The receiver should close promptly; drain it.
    let mut count = 0usize;
    while rx.try_recv().is_ok() {
        count += 1;
        // Safety valve: no more than the channel capacity worth of events.
        if count > super::SEARCH_CHANNEL_CAP * 2 {
            break;
        }
    }
    // No assertion on count: cancel races with the async task. We assert
    // only that the loop terminates (does not hang).
}

#[tokio::test]
async fn superseded_gen_search_still_produces_events_for_new_gen() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    let q = SearchQuery {
        text: "Color".to_owned(),
        ..Default::default()
    };

    // Gen 4: cancel immediately.
    let (handle4, _rx4) = engine.search(q.clone(), Gen(4));
    drop(handle4);

    // Gen 5: must produce a complete stream.
    let (_handle5, rx5) = engine.search(q, Gen(5));
    let events = drain(rx5).await;
    assert!(
        matches!(events.last(), Some(SearchEvent::Done { generation }) if generation.0 == 5),
        "gen 5 must terminate with Done"
    );
}

#[tokio::test]
async fn empty_query_produces_no_name_hits() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    let q = SearchQuery {
        text: String::new(),
        ..Default::default()
    };
    let (_handle, rx) = engine.search(q, Gen(6));
    let events = drain(rx).await;

    let name_section = events
        .iter()
        .find(|e| matches!(e, SearchEvent::Section { section, .. } if *section == SECTION_NAME));
    if let Some(SearchEvent::Section { rows, .. }) = name_section {
        assert!(rows.is_empty(), "empty query must produce no name hits");
    }
}

#[tokio::test]
async fn kind_keyword_produces_type_section_hits() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    let q = SearchQuery {
        text: "fn".to_owned(),
        ..Default::default()
    };
    let (_handle, rx) = engine.search(q, Gen(7));
    let events = drain(rx).await;

    let type_section = events
        .iter()
        .find(|e| matches!(e, SearchEvent::Section { section, .. } if *section == SECTION_TYPE));
    let Some(SearchEvent::Section { rows, .. }) = type_section else {
        panic!("must have SECTION_TYPE");
    };
    // The rich corpus has several functions; at least one must appear.
    assert!(
        !rows.is_empty(),
        "keyword 'fn' must produce type-section hits"
    );
}

// -----------------------------------------------------------------------
// L20: colliding leaf names must yield distinct display names, even
// within a single package.
// -----------------------------------------------------------------------

/// Build a single-package fixture with two functions named `foo` under
/// different modules (`a::foo`, `b::foo`) plus one uniquely-named
/// function (`zzunique`), calling `collect_name_hits` directly — it is
/// private to this module, so a test here can exercise it without going
/// through the whole engine/source plumbing.
///
/// This mirrors the real bug's shape (real memchr declares `mod memchr`
/// once per architecture backend, e.g. `arch::x86_64::memchr` and
/// `arch::aarch64::memchr`) without depending on a real crate checkout —
/// see `real_memchr_leaf_collisions_get_distinct_display_names` below for
/// the real-fixture counterpart.
fn package_with_leaf_collision() -> Arc<crate::store::package::PackageView> {
    use crate::store::package::{PackageView, Provenance};
    use nudox_ir::{
        apply::PristineIntroTable,
        change::{EcosystemId, IntroId, PackageLineageId, PackageName},
        entry::{Entry, Node, Symbol, Visibility},
        index::RawRef,
        kind::Kind,
        kinds::{Function, Module},
        view::IrView,
    };

    fn sym(name: &str) -> Symbol {
        Symbol {
            name: name.to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: std::path::PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        }
    }

    fn sym_with_alias(name: &str, alias: &str) -> Symbol {
        let mut symbol = sym(name);
        symbol.aliases = vec![alias.to_owned()].into_boxed_slice();
        symbol
    }

    let root_id = IntroId::from_raw([21u8; 32]);
    let mod_a_id = IntroId::from_raw([22u8; 32]);
    let mod_b_id = IntroId::from_raw([23u8; 32]);
    let foo_a_id = IntroId::from_raw([24u8; 32]);
    let foo_b_id = IntroId::from_raw([25u8; 32]);
    let unique_id = IntroId::from_raw([26u8; 32]);

    let mut table = PristineIntroTable::new();
    table.insert_live(
        root_id,
        Entry::new(
            sym("testpkg"),
            Node::build(None::<RawRef>, []),
            Kind::Module(Module),
        ),
        None,
    );
    table.insert_live(
        mod_a_id,
        Entry::new(
            sym("a"),
            Node::build(None::<RawRef>, []),
            Kind::Module(Module),
        ),
        Some(root_id),
    );
    table.insert_live(
        mod_b_id,
        Entry::new(
            sym("b"),
            Node::build(None::<RawRef>, []),
            Kind::Module(Module),
        ),
        Some(root_id),
    );
    table.insert_live(
        foo_a_id,
        Entry::new(
            sym("foo"),
            Node::build(None::<RawRef>, []),
            Kind::Function(Function::builder().build()),
        ),
        Some(mod_a_id),
    );
    table.insert_live(
        foo_b_id,
        Entry::new(
            sym("foo"),
            Node::build(None::<RawRef>, []),
            Kind::Function(Function::builder().build()),
        ),
        Some(mod_b_id),
    );
    table.insert_live(
        unique_id,
        Entry::new(
            sym_with_alias("zzunique", "testpkg::publicAlias"),
            Node::build(None::<RawRef>, []),
            Kind::Function(Function::builder().build()),
        ),
        Some(root_id),
    );

    let lineage = PackageLineageId::new(EcosystemId::new("test"), PackageName::new("testpkg"));
    let view = IrView::with_package(lineage, table);
    Arc::new(PackageView::build(view, Provenance::TrustedLocal))
}

#[test]
fn public_reexport_alias_is_visible_to_name_search() {
    let pkg = package_with_leaf_collision();
    let query = SearchQuery {
        text: "publicAlias".to_owned(),
        kinds: Vec::new(),
        exclude_kinds: Vec::new(),
        packages: Vec::new(),
        limit: 50,
    };

    let rows = collect_name_hits(std::slice::from_ref(&pkg), &query);

    assert_eq!(rows.len(), 1, "the public alias must produce one hit");
    assert_eq!(
        &*rows[0].display_name, "publicAlias",
        "the alias spelling should be visible in the name-search hit"
    );
}

// -----------------------------------------------------------------------
// The comparator is a *total* order, including over genuine ties
// -----------------------------------------------------------------------

/// Build a candidate that differs from its siblings only in `intro`.
fn tied_candidate(pkg: &str, intro_byte: u8, leaf: &str, score: f32) -> super::Candidate {
    use crate::wire::{HitRow, KindTag, Provenance};
    use nudox_ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName};

    let key = nudox_ir::change::StableRef::new(
        PackageLineageId::new(EcosystemId::new("test"), PackageName::new(pkg)),
        IntroId::from_raw([intro_byte; 32]),
    );
    super::Candidate {
        row: HitRow {
            key,
            display_name: SharedStr::from(leaf),
            sig_preview: Vec::new(),
            kind: KindTag::Known(nudox_ir::kind::KindDiscriminant::Function),
            provenance: Provenance::TrustedLocal,
            score,
        },
        qualified: SharedStr::from(leaf),
    }
}

/// No two *distinct* symbols may compare `Equal`, even when they agree on
/// every scoring input.
///
/// This is what "total" buys: `sort_by` is stable, so any pair that
/// compares `Equal` silently keeps whatever relative order the input had —
/// and the input order is a `HashMap` walk. A comparator that bottoms out
/// in `display_name` reports `Equal` for the entire set below, which is the
/// exact shape a real query produces (every case-folded exact match scores
/// identically and carries the same leaf name at sort time).
#[test]
fn comparator_never_reports_equal_for_two_distinct_symbols() {
    let set = vec![
        tied_candidate("aaa", 0x40, "dup", 1.0),
        tied_candidate("aaa", 0x10, "dup", 1.0),
        tied_candidate("zzz", 0x10, "dup", 1.0),
        tied_candidate("aaa", 0x70, "dup", 1.0),
        tied_candidate("zzz", 0xb0, "dup", 1.0),
    ];

    for (i, a) in set.iter().enumerate() {
        for (j, b) in set.iter().enumerate() {
            let ord = super::compare_candidates(a, b);
            if i == j {
                assert_eq!(
                    ord,
                    std::cmp::Ordering::Equal,
                    "a candidate must compare Equal to itself"
                );
                continue;
            }
            assert_ne!(
                ord,
                std::cmp::Ordering::Equal,
                "candidates {i} and {j} tie on every scoring input and the \
                 comparator cannot separate them — their order is therefore \
                 whatever the index walk produced"
            );
            // Antisymmetry: reversing the arguments must reverse the result.
            assert_eq!(
                ord.reverse(),
                super::compare_candidates(b, a),
                "comparator is not antisymmetric for {i} vs {j}"
            );
        }
    }

    // Transitivity across the whole set: sorting must produce a sequence in
    // which every adjacent pair is strictly Less.
    let mut sorted = set;
    sorted.sort_by(super::compare_candidates);
    for pair in sorted.windows(2) {
        assert_eq!(
            super::compare_candidates(&pair[0], &pair[1]),
            std::cmp::Ordering::Less,
            "sorted output contains an adjacent pair that is not strictly ordered"
        );
    }

    // And the order really is the identity order: package, then IntroId.
    let ids: Vec<(String, u8)> = sorted
        .iter()
        .map(|c| {
            (
                c.row.key.package.name.as_str().to_owned(),
                c.row.key.intro.as_bytes()[0],
            )
        })
        .collect();
    assert_eq!(ids, vec![
        ("aaa".to_owned(), 0x10),
        ("aaa".to_owned(), 0x40),
        ("aaa".to_owned(), 0x70),
        ("zzz".to_owned(), 0x10),
        ("zzz".to_owned(), 0xb0),
    ]);
}

/// Search candidates commonly originate in hash-backed indexes. The
/// rendered order must therefore be invariant to equivalent insertion
/// orders before the ranking comparator runs.
#[test]
fn hash_map_insertion_orders_produce_identical_hit_keys() {
    fn ranked(insertion: impl IntoIterator<Item = u8>) -> Vec<u8> {
        let mut candidates = std::collections::HashMap::new();
        for id in insertion {
            candidates.insert(id, tied_candidate("aaa", id, "dup", 1.0));
        }
        let mut candidates: Vec<_> = candidates.into_values().collect();
        candidates.sort_by(super::compare_candidates);
        candidates
            .into_iter()
            .map(|candidate| candidate.row.key.intro.as_bytes()[0])
            .collect()
    }

    let forward = ranked(0x10..=0x70);
    let reversed = ranked((0x10..=0x70).rev());
    let expected: Vec<u8> = (0x10..=0x70).collect();

    assert_eq!(forward, expected);
    assert_eq!(
        reversed, expected,
        "equivalent HashMap contents must not inherit traversal order"
    );
}

/// A higher score always wins, whatever the identities say.
///
/// Guards the ordering of the comparator's terms: identity is the *last*
/// resort, not a co-equal key that could reorder real relevance.
#[test]
fn score_dominates_identity_in_the_comparator() {
    // The lower-scoring candidate has the smaller package name and the
    // smaller intro — it wins on every tiebreak and must still lose.
    let strong = tied_candidate("zzz", 0xff, "dup", 1.0);
    let weak = tied_candidate("aaa", 0x00, "dup", 0.4);
    assert_eq!(
        super::compare_candidates(&strong, &weak),
        std::cmp::Ordering::Less,
        "the higher-scoring row must sort first"
    );
}

// -----------------------------------------------------------------------
// Scoring model
// -----------------------------------------------------------------------

/// A public, top-level, exactly-matched symbol scores exactly 1.0, and
/// every weight is a discount from it.
///
/// This is the contract `HitRow::score` documents and that the MCP layer
/// and the adversarial suite both assume. Asserting the *bound* rather than
/// each constant means the weights can be re-argued without the test
/// having to be edited to agree with them.
#[test]
#[allow(clippy::float_cmp)] // the reference point must be *exactly* 1.0
fn public_top_level_exact_match_is_the_scoring_reference_point() {
    use nudox_ir::{entry::Visibility, kind::KindDiscriminant as K};

    assert_eq!(
        super::score_of(1.0, Visibility::Public, K::Function),
        1.0,
        "an exact match on a public function is the 1.0 reference point"
    );

    let all_visibilities = [
        Visibility::Public,
        Visibility::Protected,
        Visibility::Internal,
        Visibility::Package,
        Visibility::Crate,
        Visibility::Private,
    ];
    let all_kinds = [
        K::Module,
        K::Record,
        K::Field,
        K::Function,
        K::Alias,
        K::Trait,
        K::Impl,
        K::Enum,
        K::Variant,
        K::Const,
        K::Static,
        K::Reexport,
        K::Param,
    ];
    for v in all_visibilities {
        for k in all_kinds {
            let s = super::score_of(1.0, v, k);
            assert!(
                s > 0.0 && s <= 1.0,
                "score for ({v:?}, {k:?}) escaped (0, 1]: {s} — a weight above \
                 1.0 would let a quality factor promote a worse text match"
            );
        }
    }
}

/// Visibility separates two symbols that are identical to the text matcher.
#[test]
fn less_visible_symbols_score_below_public_ones() {
    use nudox_ir::{entry::Visibility, kind::KindDiscriminant as K};

    let public = super::score_of(1.0, Visibility::Public, K::Module);
    for lesser in [
        Visibility::Protected,
        Visibility::Internal,
        Visibility::Package,
        Visibility::Crate,
        Visibility::Private,
    ] {
        assert!(
            super::score_of(1.0, lesser, K::Module) < public,
            "{lesser:?} must score below Public on an otherwise identical match"
        );
    }
}

/// Two entries sharing the leaf name `foo` in different modules of the
/// *same* package must render with distinct `display_name`s.
///
/// This is the regression the old code missed: disambiguation only ran
/// when `packages.len() > 1`, so a single-package corpus (the common
/// case — open one crate's docs and search it) never qualified anything,
/// no matter how many leaf names collided inside that one package.
#[test]
fn colliding_leaf_names_within_one_package_get_distinct_display_names() {
    let pkg = package_with_leaf_collision();

    let query = SearchQuery {
        text: "foo".to_owned(),
        kinds: Vec::new(),
        exclude_kinds: Vec::new(),
        packages: Vec::new(),
        limit: 50,
    };
    let rows = collect_name_hits(std::slice::from_ref(&pkg), &query);

    assert_eq!(rows.len(), 2, "both `foo` entries must be returned");
    assert_ne!(
        rows[0].display_name, rows[1].display_name,
        "two entries with the same leaf name in one package must not \
         render identical display names; got {:?} and {:?}",
        rows[0].display_name, rows[1].display_name
    );
    // Qualified, not just "different by accident" — each must still
    // read as `foo`, distinguished by its module path.
    for row in &rows {
        assert!(
            row.display_name.contains("foo"),
            "qualified display name must still contain the leaf name, \
             got {:?}",
            row.display_name
        );
    }
}

/// A leaf name that does not collide with anything in the result set
/// must keep its short, unqualified form — disambiguation is a cost we
/// pay only where the reader actually needs it.
#[test]
fn unique_leaf_name_keeps_short_display_name() {
    let pkg = package_with_leaf_collision();

    let query = SearchQuery {
        text: "zzunique".to_owned(),
        kinds: Vec::new(),
        exclude_kinds: Vec::new(),
        packages: Vec::new(),
        limit: 50,
    };
    let rows = collect_name_hits(std::slice::from_ref(&pkg), &query);

    assert_eq!(rows.len(), 1, "exactly one `zzunique` entry exists");
    assert_eq!(
        &*rows[0].display_name, "zzunique",
        "a non-colliding leaf name must not be qualified"
    );
}

#[test]
fn zero_limit_returns_all_name_hits() {
    let pkg = package_with_leaf_collision();

    let query = SearchQuery {
        text: "foo".to_owned(),
        kinds: Vec::new(),
        exclude_kinds: Vec::new(),
        packages: Vec::new(),
        limit: 0,
    };
    let rows = collect_name_hits(std::slice::from_ref(&pkg), &query);

    assert_eq!(
        rows.len(),
        2,
        "limit zero means unlimited, so both matching entries must be returned"
    );
}

/// End-to-end regression against the **real** `memchr` crate (L20): the
/// exact case the limitations ledger used to demonstrate the bug —
/// `memchr` declares `mod memchr` once per architecture backend
/// (`arch::x86_64::memchr`, `arch::aarch64::memchr`, …), so a search for
/// `memchr` returns several same-kind, same-name rows that were
/// previously pixel-identical apart from vertical position.
///
/// # Running
///
/// ```text
/// cargo test -p nudox-engine --lib \
///   search::tests::real_memchr_leaf_collisions_get_distinct_display_names \
///   -- --ignored --nocapture
/// ```
#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn real_memchr_leaf_collisions_get_distinct_display_names() {
    use crate::store::{
        package::{PackageView, Provenance},
        source::producer::PackageDescriptor,
    };
    use nudox_ir::view::IrView;
    use nudox_languages::{produce, rust::RustProducer};

    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../result/memchr-2.8.3")
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from("/nonexistent"));

    if !root.join("Cargo.toml").is_file() {
        eprintln!(
            "SKIP: no memchr checkout at {}. \
             Run: scripts/fetch-real-crate.sh memchr 2.8.3",
            root.display()
        );
        return;
    }

    // `direct_repo: false` matches `ProducerRegistry::with_rust_pilot`,
    // the constructor the real app uses.
    let descriptor = PackageDescriptor::cargo(&root, "memchr", "2.8.3");
    let table = produce(
        &RustProducer { direct_repo: false },
        &descriptor.source,
        &descriptor.lineage,
        &nudox_ir::foreign::Unlinked,
    )
    .expect("memchr must lower without error for a real checkout")
    .table;

    let view = IrView::with_package(descriptor.lineage, table);
    let pkg = Arc::new(PackageView::build(view, Provenance::TrustedLocal));

    let query = SearchQuery {
        text: "memchr".to_owned(),
        kinds: Vec::new(),
        exclude_kinds: Vec::new(),
        packages: Vec::new(),
        limit: 0, // unlimited — we need the full collision set to exist
    };
    let rows = collect_name_hits(std::slice::from_ref(&pkg), &query);
    eprintln!(
        "memchr search rows: {:?}",
        rows.iter().map(|r| &*r.display_name).collect::<Vec<_>>()
    );

    // Ground truth, independent of `collect_name_hits`: group the
    // *actual* IR entries by leaf name (case-sensitive, as displayed).
    let mut leaf_of_intro: std::collections::HashMap<nudox_ir::change::IntroId, &str> =
        std::collections::HashMap::new();
    for (id, entry) in pkg.view().entries() {
        leaf_of_intro.insert(id, entry.sym().name.as_str());
    }
    let mut groups: std::collections::HashMap<&str, Vec<SharedStr>> =
        std::collections::HashMap::new();
    for row in &rows {
        let leaf = leaf_of_intro
            .get(&row.key.intro)
            .copied()
            .expect("every hit row must resolve back to a real entry");
        groups
            .entry(leaf)
            .or_default()
            .push(row.display_name.clone());
    }

    let mut proved_a_collision = false;
    for (leaf, display_names) in &groups {
        if display_names.len() < 2 {
            continue;
        }
        proved_a_collision = true;
        let unique: std::collections::HashSet<&SharedStr> = display_names.iter().collect();
        assert_eq!(
            unique.len(),
            display_names.len(),
            "leaf name {leaf:?} has {} colliding rows but only {} distinct \
             display names: {display_names:?}",
            display_names.len(),
            unique.len(),
        );
    }

    assert!(
        proved_a_collision,
        "expected at least one leaf-name collision among real memchr's \
         `memchr`-prefixed symbols (the L20 evidence found seven) — \
         found none, so this run does not actually exercise the \
         invariant. Rows: {:?}",
        rows.iter().map(|r| &*r.display_name).collect::<Vec<_>>()
    );
}
