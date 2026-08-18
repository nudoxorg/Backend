//! Regression tests for *how much work the store is asked to do*.
//!
//! Every test here would pass if the assertions on [`AdapterProbe`] were
//! deleted, because every optimisation in this crate returns exactly the same
//! rows as the scan it replaces. That is the point: rows cannot witness a
//! pushdown, so these tests assert on the round-trips instead — how many times
//! the corpus map was listed, how many packages were walked end to end, how
//! many index probes were made.
//!
//! Each test names the specific access path it pins and states, in its doc
//! comment, what the counter read *before* the optimisation landed.

use std::sync::Arc;

use futures::StreamExt as _;
use nudox_engine::graph::adapter::CorpusAdapter;
use nudox_engine::graph::probe::{AdapterProbe, StoreProbe};
use nudox_engine::store::{
    corpus::Corpus,
    package::{PackageView, Provenance},
    source::fixtures::{build_rich_view, rich_lineage},
};
use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
    entry::{Entry, Node, Symbol, Visibility},
    kind::Kind,
    kinds::{Function, Record},
    view::IrView,
};
use trustfall::{FieldValue, Schema, execute_query_async};

fn schema() -> Schema {
    Schema::parse(include_str!("../../schema.graphql")).expect("schema must parse")
}

fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

fn lineage(eco: &str, name: &str) -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new(eco), PackageName::new(name))
}

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

/// A two-entry package: one `Function` (`my_fn`, intro 1) and one `Record`
/// (`MyRec`, intro 2).
///
/// Two *different* kinds on purpose — a package where every entry shares one
/// discriminant cannot tell a `by_kind` probe apart from a full walk.
fn make_package(eco: &str, name: &str) -> Arc<PackageView> {
    let lid = lineage(eco, name);
    let mut table = PristineIntroTable::new();
    table.insert_live(
        intro(1),
        Entry::new(
            sym("my_fn"),
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Function(Function::builder().build()),
        ),
        None,
    );
    table.insert_live(
        intro(2),
        Entry::new(
            sym("MyRec"),
            Node::build(None::<nudox_ir::index::RawRef>, []),
            Kind::Record(Record::builder().build()),
        ),
        None,
    );
    let view = IrView::with_package(lid, table);
    Arc::new(PackageView::build(view, Provenance::TrustedLocal))
}

/// Two synthetic packages plus a probe wired into the adapter.
async fn two_package_corpus() -> (Corpus, Arc<AdapterProbe>) {
    let corpus = Corpus::new();
    corpus.insert(make_package("cargo", "pkg-alpha")).await;
    corpus.insert(make_package("cargo", "pkg-beta")).await;
    (corpus, Arc::new(AdapterProbe::new()))
}

/// Run a query to completion and return the probe's counters.
async fn drain(
    corpus: Corpus,
    probe: Arc<AdapterProbe>,
    query: &str,
    vars: impl IntoIterator<Item = (String, FieldValue)>,
) -> usize {
    let schema = schema();
    let adapter = Arc::new(CorpusAdapter::new_with_probe(corpus, Arc::clone(&probe)));
    let rows: Vec<_> = execute_query_async(
        &schema,
        adapter,
        query,
        vars.into_iter()
            .collect::<std::collections::BTreeMap<_, _>>(),
    )
    .expect("query must parse")
    .collect()
    .await;
    for row in &rows {
        assert!(row.is_ok(), "query produced an error row: {row:?}");
    }
    rows.len()
}

/// Assert one counter, reporting the whole snapshot on failure — a wrong
/// number is far easier to diagnose next to the other three.
fn assert_probe(probe: &AdapterProbe, which: StoreProbe, expected: usize, why: &str) {
    let actual = probe.count(which);
    assert_eq!(
        actual,
        expected,
        "{why}\n  expected {which:?} == {expected}, got {actual}\n  full snapshot: {:?}",
        probe.snapshot()
    );
}

fn key_of(eco: &str, name: &str, n: u8) -> String {
    StableRef::new(lineage(eco, name), intro(n)).to_string()
}

// ---------------------------------------------------------------------------
// `Symbols` — key
// ---------------------------------------------------------------------------

/// A `key` equality filter must resolve through `Corpus::package` alone: the
/// corpus map is never listed and no package is walked, however many packages
/// are loaded.
#[tokio::test]
async fn key_equality_resolves_without_listing_or_walking_the_corpus() {
    let (corpus, probe) = two_package_corpus().await;
    let rows = drain(
        corpus,
        Arc::clone(&probe),
        r#"{ Symbols { key @filter(op: "=", value: ["$key"]) @output } }"#,
        [(
            "key".to_owned(),
            FieldValue::String(key_of("cargo", "pkg-alpha", 1).into()),
        )],
    )
    .await;

    assert_eq!(rows, 1, "exactly the one named symbol");
    assert_probe(
        &probe,
        StoreProbe::CorpusList,
        0,
        "a key names one package; listing the corpus to find it is the scan this pushdown exists to avoid",
    );
    assert_probe(
        &probe,
        StoreProbe::PackageEnumerated,
        0,
        "no package's entry table should be walked for a single-key lookup",
    );
    assert_probe(
        &probe,
        StoreProbe::CorpusLookup,
        1,
        "one key, one package probe",
    );
}

/// `@filter(op: "one_of")` over keys must cost one package probe per *distinct
/// package*, not a corpus walk.
///
/// Before `CandidateValue::Multiple` was decoded, `one_of` fell through the
/// `CandidateValue::Single` pattern to the full-scan arm: this test read
/// `CorpusList == 1` and `PackageEnumerated == 2`.
#[tokio::test]
async fn key_one_of_uses_one_lookup_per_package_not_a_scan() {
    let (corpus, probe) = two_package_corpus().await;
    let keys = FieldValue::List(
        vec![
            FieldValue::String(key_of("cargo", "pkg-alpha", 1).into()),
            FieldValue::String(key_of("cargo", "pkg-beta", 2).into()),
        ]
        .into(),
    );
    let rows = drain(
        corpus,
        Arc::clone(&probe),
        r#"{ Symbols { key @filter(op: "one_of", value: ["$keys"]) @output } }"#,
        [("keys".to_owned(), keys)],
    )
    .await;

    assert_eq!(rows, 2, "one row per named key");
    assert_probe(
        &probe,
        StoreProbe::CorpusList,
        0,
        "`one_of` over keys names its packages exactly; it must not list the corpus",
    );
    assert_probe(
        &probe,
        StoreProbe::PackageEnumerated,
        0,
        "`one_of` over keys must not walk any package's entry table",
    );
    assert_probe(
        &probe,
        StoreProbe::CorpusLookup,
        2,
        "two keys in two distinct packages: two probes",
    );
}

/// Two keys in the *same* package must cost one package probe, not two — the
/// memo deduplicates by lineage.
#[tokio::test]
async fn two_keys_in_one_package_cost_one_package_probe() {
    let (corpus, probe) = two_package_corpus().await;
    let keys = FieldValue::List(
        vec![
            FieldValue::String(key_of("cargo", "pkg-alpha", 1).into()),
            FieldValue::String(key_of("cargo", "pkg-alpha", 2).into()),
        ]
        .into(),
    );
    let rows = drain(
        corpus,
        Arc::clone(&probe),
        r#"{ Symbols { key @filter(op: "one_of", value: ["$keys"]) @output } }"#,
        [("keys".to_owned(), keys)],
    )
    .await;

    assert_eq!(rows, 2);
    assert_probe(
        &probe,
        StoreProbe::CorpusLookup,
        1,
        "both keys live in pkg-alpha; the second must be served from the memo",
    );
}

// ---------------------------------------------------------------------------
// `Symbols` — type coercion
// ---------------------------------------------------------------------------

/// A `... on Function` coercion with no filter at all must route to the
/// `by_kind` index, because the coercion pins the discriminant exactly as
/// tightly as `kind @filter(op: "=", value: ["Function"])` does.
///
/// Before the coercion was consulted this read `PackageEnumerated == 2`: the
/// query walked every entry of every package and then discarded the
/// non-functions in the coercion step. The five `... on Variant` /
/// `... on Static` / … tests in `fixture_queries.rs` all took that path.
#[tokio::test]
async fn type_coercion_alone_pushes_down_to_the_kind_index() {
    let (corpus, probe) = two_package_corpus().await;
    let rows = drain(
        corpus,
        Arc::clone(&probe),
        r#"{ Symbols { ... on Function { name @output } } }"#,
        [],
    )
    .await;

    assert_eq!(rows, 2, "one function per package");
    assert_probe(
        &probe,
        StoreProbe::PackageEnumerated,
        0,
        "`... on Function` is a kind constraint; walking every entry to serve it is the regression",
    );
    assert_probe(
        &probe,
        StoreProbe::IndexProbe,
        2,
        "one `by_kind` probe per package",
    );
    assert_probe(
        &probe,
        StoreProbe::CorpusList,
        1,
        "the package list is read once, not once per package",
    );
}

/// `kind @filter(op: "one_of")` must probe `by_kind` once per (package, kind)
/// pair rather than falling back to a walk.
#[tokio::test]
async fn kind_one_of_probes_the_kind_index_per_kind() {
    let (corpus, probe) = two_package_corpus().await;
    let kinds = FieldValue::List(
        vec![
            FieldValue::String("Function".into()),
            FieldValue::String("Record".into()),
        ]
        .into(),
    );
    let rows = drain(
        corpus,
        Arc::clone(&probe),
        r#"{ Symbols { kind @filter(op: "one_of", value: ["$kinds"]) @output name @output } }"#,
        [("kinds".to_owned(), kinds)],
    )
    .await;

    assert_eq!(rows, 4, "two kinds × two packages");
    assert_probe(
        &probe,
        StoreProbe::PackageEnumerated,
        0,
        "`one_of` over kinds is answerable from `by_kind`; a walk is the regression",
    );
    assert_probe(
        &probe,
        StoreProbe::IndexProbe,
        4,
        "two kinds probed in each of two packages",
    );
}

// ---------------------------------------------------------------------------
// `Symbols` — provably empty
// ---------------------------------------------------------------------------

/// A query the engine has already proven unsatisfiable must touch the store
/// zero times.
///
/// `one_of` with an empty operand list is the reachable form. Before
/// `CandidateValue::Impossible` was decoded this walked the entire corpus to
/// produce, necessarily, nothing.
#[tokio::test]
async fn a_provably_empty_filter_touches_the_store_zero_times() {
    let (corpus, probe) = two_package_corpus().await;
    let rows = drain(
        corpus,
        Arc::clone(&probe),
        r#"{ Symbols { key @filter(op: "one_of", value: ["$keys"]) @output } }"#,
        [("keys".to_owned(), FieldValue::List(Vec::new().into()))],
    )
    .await;

    assert_eq!(rows, 0);
    for which in StoreProbe::ALL {
        assert_probe(
            &probe,
            which,
            0,
            "nothing can satisfy this query; the store must not be consulted at all",
        );
    }
}

// ---------------------------------------------------------------------------
// `Packages`
// ---------------------------------------------------------------------------

/// `Packages { lineage @filter(op: "=") }` must resolve through
/// `Corpus::package`, not by listing the corpus.
///
/// This is the shape of `list_package_functions.trustfall`, one of the five
/// shipped queries. Before the `Packages` entrypoint had any plan at all this
/// read `CorpusList == 1` and `CorpusLookup == 0` — it read the whole map to
/// find one entry, on every invocation.
#[tokio::test]
async fn package_lineage_equality_probes_one_package_and_never_lists() {
    let (corpus, probe) = two_package_corpus().await;
    let rows = drain(
        corpus,
        Arc::clone(&probe),
        r#"{ Packages { lineage @filter(op: "=", value: ["$lineage"]) @output name @output } }"#,
        [(
            "lineage".to_owned(),
            FieldValue::String("cargo:pkg-alpha".into()),
        )],
    )
    .await;

    assert_eq!(rows, 1);
    assert_probe(
        &probe,
        StoreProbe::CorpusList,
        0,
        "a lineage is the corpus map's own key; listing the map to find it is the regression",
    );
    assert_probe(
        &probe,
        StoreProbe::CorpusLookup,
        1,
        "one lineage, one probe",
    );
}

/// The claim above, made against the *shipped* query text rather than a query
/// written to suit the test.
///
/// `list_package_functions.trustfall` is what `nudox-mcp` serves; if the
/// pushdown only fired for hand-written variants of its shape it would be a
/// benchmark, not an improvement.
#[tokio::test]
async fn the_shipped_list_package_functions_query_takes_the_pushdown() {
    let corpus = Corpus::new();
    corpus
        .insert(Arc::new(PackageView::build(
            build_rich_view(),
            Provenance::TrustedLocal,
        )))
        .await;
    let probe = Arc::new(AdapterProbe::new());

    let rows = drain(
        corpus,
        Arc::clone(&probe),
        nudox_engine::graph::queries::LIST_PACKAGE_FUNCTIONS,
        [(
            "package".to_owned(),
            FieldValue::String(rich_lineage().to_string().into()),
        )],
    )
    .await;

    assert!(rows > 0, "the fixture package must contain functions");
    assert_probe(
        &probe,
        StoreProbe::CorpusList,
        0,
        "the shipped query names its package; it must not list the corpus to find it",
    );
    assert_probe(
        &probe,
        StoreProbe::CorpusLookup,
        1,
        "one package, one probe",
    );
}

/// An unloaded lineage yields no row and no error — `Packages` enumerates what
/// is present, so "not loaded" is a legitimate empty answer rather than a
/// malformed query. (`Symbols { key … }` is the opposite case and does error.)
#[tokio::test]
async fn packages_filter_on_an_unloaded_lineage_yields_no_rows_and_no_error() {
    let (corpus, probe) = two_package_corpus().await;
    let rows = drain(
        corpus,
        Arc::clone(&probe),
        r#"{ Packages { lineage @filter(op: "=", value: ["$lineage"]) @output } }"#,
        [(
            "lineage".to_owned(),
            FieldValue::String("cargo:not-loaded".into()),
        )],
    )
    .await;

    assert_eq!(rows, 0);
    assert_probe(&probe, StoreProbe::CorpusList, 0, "still no list");
}

/// A `lineage` operand that is not `"ecosystem:name"` must surface a typed
/// error, not an empty result set — a silently-empty answer to a typo is a
/// wrong answer.
#[tokio::test]
async fn a_malformed_lineage_yields_an_error_row_not_silence() {
    let (corpus, _probe) = two_package_corpus().await;
    let schema = schema();
    let adapter = Arc::new(CorpusAdapter::new(corpus));
    let vars = [(
        "lineage".to_owned(),
        FieldValue::String("no-colon-here".into()),
    )];
    let mut stream = execute_query_async(
        &schema,
        adapter,
        r#"{ Packages { lineage @filter(op: "=", value: ["$lineage"]) @output } }"#,
        vars.into_iter().collect(),
    )
    .expect("query must parse");

    let first = stream
        .next()
        .await
        .expect("stream must yield an item, not be empty");
    let err = first.expect_err("expected an error for a malformed lineage");
    assert!(
        err.to_string().contains("invalid package lineage"),
        "expected InvalidLineage, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Laziness
// ---------------------------------------------------------------------------

/// The unconstrained `Symbols` scan must be lazy per package: a consumer that
/// stops after the first row must have caused exactly one package to be
/// walked, not all of them.
///
/// Before the resolver collected every `(package, intro)` pair into one `Vec`
/// up front, this read `PackageEnumerated == 2` — the "scan" was eager, so
/// `.next()` on the result stream cost the whole corpus.
#[tokio::test]
async fn an_abandoned_full_scan_walks_only_the_packages_it_reached() {
    let (corpus, probe) = two_package_corpus().await;
    let schema = schema();
    let adapter = Arc::new(CorpusAdapter::new_with_probe(corpus, Arc::clone(&probe)));

    let mut stream = execute_query_async(
        &schema,
        adapter,
        r#"{ Symbols { name @output } }"#,
        std::collections::BTreeMap::<String, FieldValue>::new(),
    )
    .expect("query must parse");

    let first = stream.next().await.expect("at least one row");
    first.expect("no error");
    drop(stream);

    assert_probe(
        &probe,
        StoreProbe::PackageEnumerated,
        1,
        "one row was taken from a two-package corpus; the second package must not have been walked",
    );
}

// ---------------------------------------------------------------------------
// Reverse edges — the N+1
// ---------------------------------------------------------------------------

/// A reverse edge traversed from many source symbols must list the corpus once
/// for the whole edge, not once per source symbol.
///
/// This is the crate's N+1: `usages`, `mentions` and `Trait.implementors` each
/// called `Corpus::packages()` *inside* their per-vertex resolver closure.
/// `Corpus::packages()` takes a read lock over the whole map and allocates a
/// fresh `Vec` with one `Arc` clone per package, every call — so the cost was
/// `sources × packages`, to re-read a table that cannot change mid-query.
///
/// The rich fixture has many functions; the assertion is deliberately
/// independent of how many, because a count that tracked the fixture's size
/// would be the very thing under test.
#[tokio::test]
async fn a_reverse_edge_lists_the_corpus_once_for_the_whole_edge() {
    let corpus = Corpus::new();
    corpus
        .insert(Arc::new(PackageView::build(
            build_rich_view(),
            Provenance::TrustedLocal,
        )))
        .await;
    let probe = Arc::new(AdapterProbe::new());

    let rows = drain(
        corpus,
        Arc::clone(&probe),
        // `@fold`, not a plain traversal: a mandatory `usages` edge would drop
        // every function that nothing calls, leaving one source symbol and
        // therefore no N+1 to observe.
        r#"{ Symbols { ... on Function { name @output usages @fold { key @output } } } }"#,
        [],
    )
    .await;

    // Two lists total, and this is the whole claim: one for the `Symbols`
    // entrypoint's `by_kind` fan-out over packages, one for the `usages` edge's
    // fan-out over packages. Neither scales with the number of functions the
    // fixture happens to contain.
    assert_probe(
        &probe,
        StoreProbe::CorpusList,
        2,
        "one list for the entrypoint and one for the edge; a third means the edge is listing per row",
    );
    assert!(
        rows > 1,
        "the fixture must exercise more than one source symbol or this test proves nothing; got {rows} rows"
    );
    assert!(
        probe.count(StoreProbe::IndexProbe) > rows,
        "each source symbol must still probe the posting list; got {} probes for {rows} rows",
        probe.count(StoreProbe::IndexProbe)
    );
}

/// Occurrence targets must cost one package probe per *distinct target
/// package*, not one per occurrence.
///
/// `format_point` (intro 23) has two occurrences, both targeting symbols in
/// the same package. Before `Occurrence.target` went through the memo it
/// called `Corpus::package` once per occurrence.
#[tokio::test]
async fn occurrence_targets_probe_each_package_once_not_each_occurrence() {
    let corpus = Corpus::new();
    corpus
        .insert(Arc::new(PackageView::build(
            build_rich_view(),
            Provenance::TrustedLocal,
        )))
        .await;
    let probe = Arc::new(AdapterProbe::new());
    let key = StableRef::new(rich_lineage(), intro(23)).to_string();

    let rows = drain(
        corpus,
        Arc::clone(&probe),
        r#"{
            Symbols {
                key @filter(op: "=", value: ["$key"])
                occurrencesOf { target { name @output } }
            }
        }"#,
        [("key".to_owned(), FieldValue::String(key.into()))],
    )
    .await;

    assert!(
        rows >= 2,
        "format_point must have at least two resolvable occurrence targets; got {rows}"
    );
    // One lookup for the entrypoint's `key` pushdown, one for the target
    // edge's first (and only distinct) package. Both occurrences live in the
    // fixture's single package, so a second target lookup would mean the memo
    // is not deduplicating.
    assert_probe(
        &probe,
        StoreProbe::CorpusLookup,
        2,
        "one lookup for the key pushdown and one for the shared target package",
    );
    assert_probe(
        &probe,
        StoreProbe::CorpusList,
        0,
        "neither the key pushdown nor the target edge needs the package list",
    );
}

// ---------------------------------------------------------------------------
// Signature-position edges — the N+1 must not come back
// ---------------------------------------------------------------------------

/// A one-package corpus holding the rich fixture, plus a fresh probe.
async fn rich_corpus_with_probe() -> (Corpus, Arc<AdapterProbe>) {
    let corpus = Corpus::new();
    corpus
        .insert(Arc::new(PackageView::build(
            build_rich_view(),
            Provenance::TrustedLocal,
        )))
        .await;
    (corpus, Arc::new(AdapterProbe::new()))
}

/// **Every** signature reverse edge must share the corpus listing with the
/// whole edge, not take one per source symbol.
///
/// The N+1 that `CorpusMemo` removed was not a property of `usages`; it was a
/// property of *how a reverse edge was written*, and five more reverse edges
/// are five more chances to write it the old way. The loop is the point: an
/// edge added later that calls `Corpus::packages()` inside its per-vertex
/// closure fails here as soon as it is added to the list, and one that is
/// added and not listed is caught by
/// `every_type_reference_edge_in_the_schema_is_covered_by_the_n_plus_1_guard`
/// below.
///
/// Before `posting_neighbors` existed this read `CorpusList == 4` for a
/// three-function fixture (one for the entrypoint, one per source symbol).
#[tokio::test]
async fn every_signature_reverse_edge_lists_the_corpus_once_for_the_whole_edge() {
    for edge in SIGNATURE_REVERSE_EDGES {
        let (corpus, probe) = rich_corpus_with_probe().await;
        // `... on Function` gives several source symbols whatever the edge is,
        // and `@fold` keeps the ones with no neighbors — a mandatory traversal
        // would collapse the fan-out and leave no N+1 to observe.
        let query = format!(
            r#"{{ Symbols {{ ... on Function {{ name @output {edge} @fold {{ key @output }} }} }} }}"#
        );
        let rows = drain(corpus, Arc::clone(&probe), &query, []).await;

        assert!(
            rows > 1,
            "`{edge}` must be exercised from more than one source symbol or this \
             test proves nothing; got {rows} rows"
        );
        assert_probe(
            &probe,
            StoreProbe::CorpusList,
            2,
            &format!(
                "one list for the `... on Function` entrypoint and one for the \
                 `{edge}` edge; a third means `{edge}` is listing per row"
            ),
        );
        assert!(
            probe.count(StoreProbe::IndexProbe) > rows,
            "`{edge}` must still probe the posting list once per source symbol; \
             got {} probes for {rows} rows",
            probe.count(StoreProbe::IndexProbe)
        );
    }
}

/// The five signature reverse edges this file guards.
///
/// Named once, next to the test that reads them, so that the coverage test
/// below can compare it against the schema rather than against a second copy.
const SIGNATURE_REVERSE_EDGES: [&str; 5] = [
    "implementedBy",
    "subtypes",
    "returnedBy",
    "acceptedBy",
    "heldBy",
];

/// Every reverse type-reference edge the schema declares must appear in
/// [`SIGNATURE_REVERSE_EDGES`], so that adding an edge to the SDL without
/// adding it to the N+1 guard is a failing test rather than an unmeasured
/// access path.
///
/// This is the guard on the guard. A list of edge names hand-maintained
/// alongside a schema is exactly the shape that rots: the edge lands, the list
/// does not, and the new edge is free to regress forever. Reading the schema
/// makes the list impossible to under-populate.
#[test]
fn every_type_reference_edge_in_the_schema_is_covered_by_the_n_plus_1_guard() {
    let sdl = include_str!("../../schema.graphql");
    // The `Symbol` interface declares each edge exactly once; the concrete
    // types repeat it. Any `<name>: [Symbol!]` field that is not one of the
    // pre-existing traversals is a type-reference edge.
    let pre_existing = [
        "members",
        "parent",
        "usages",
        "mentions",
        "implementors",
        "signatureTypes",
    ];
    let mut found: Vec<&str> = sdl
        .lines()
        .filter_map(|line| line.trim().strip_suffix(": [Symbol!]"))
        .filter(|name| !name.contains(' ') && !name.contains('#'))
        .filter(|name| !pre_existing.contains(name))
        .collect();
    found.sort_unstable();
    found.dedup();

    let mut expected: Vec<&str> = SIGNATURE_REVERSE_EDGES.to_vec();
    expected.sort_unstable();

    assert_eq!(
        found, expected,
        "the schema declares reverse type-reference edges that the N+1 guard \
         above does not exercise (or vice versa). Add the edge to \
         SIGNATURE_REVERSE_EDGES — an edge outside the guard is an access path \
         nothing measures."
    );
}

/// The forward edge must not list the corpus at all, and must probe each
/// *target package* once for the whole edge rather than once per source
/// symbol.
///
/// `signatureTypes` reads the declaration itself, so the only store round-trip
/// it can make is resolving a target's package. Written without the memo it
/// would be one `Corpus::package` per referenced type — the same N+1 in the
/// forward direction.
#[tokio::test]
async fn the_forward_signature_edge_probes_each_target_package_once() {
    let (corpus, probe) = rich_corpus_with_probe().await;
    let rows = drain(
        corpus,
        Arc::clone(&probe),
        // Both `@output`s must be aliased: trustfall rejects a query that
        // outputs two fields under one name, and `name` appears at both the
        // `Function` scope and the folded `signatureTypes` scope.
        r#"{
            Symbols {
                ... on Function {
                    name @output(name: "fnName")
                    signatureTypes @fold { name @output(name: "refNames") }
                }
            }
        }"#,
        [],
    )
    .await;

    assert!(rows > 1, "several functions must be traversed; got {rows}");
    assert_probe(
        &probe,
        StoreProbe::CorpusList,
        1,
        "only the `... on Function` entrypoint lists the corpus; the forward \
         edge reads the entry it already holds",
    );
    assert_probe(
        &probe,
        StoreProbe::CorpusLookup,
        1,
        "every referenced type in this fixture lives in the one loaded package; \
         a second lookup means the memo is not deduplicating by lineage",
    );
}

/// A single-key traversal of the forward edge must not enumerate anything.
///
/// `distance` (intro 6) takes one `Point`. The whole query is two probes: the
/// key pushdown, and the target's package.
#[tokio::test]
async fn a_keyed_forward_signature_traversal_never_enumerates() {
    let (corpus, probe) = rich_corpus_with_probe().await;
    let key = StableRef::new(rich_lineage(), intro(6)).to_string();
    let rows = drain(
        corpus,
        Arc::clone(&probe),
        r#"{
            Symbols {
                key @filter(op: "=", value: ["$key"])
                signatureTypes { name @output }
            }
        }"#,
        [("key".to_owned(), FieldValue::String(key.into()))],
    )
    .await;

    assert_eq!(rows, 1, "distance names exactly one type: Point");
    assert_probe(
        &probe,
        StoreProbe::CorpusList,
        0,
        "neither the key pushdown nor the forward edge needs the package list",
    );
    assert_probe(
        &probe,
        StoreProbe::PackageEnumerated,
        0,
        "a named symbol's own signature is not a reason to walk an entry table",
    );
    assert_probe(
        &probe,
        StoreProbe::CorpusLookup,
        2,
        "one lookup for the key pushdown, one for the target's package",
    );
}
