//! Does the schema project the IR faithfully, or does it flatten it?
//!
//! Each test here names one piece of information the IR carries and asserts
//! that a query can recover it — and, where the IR value is a sum type, that
//! the *variant* survives rather than collapsing into a null.
//!
//! # On fixtures
//!
//! Doctrine §4 warns that a hand-authored fixture tests the author's
//! imagination. That warning is about *producers*, where the input space is
//! "every package anyone will ever write". These tests are about a projection
//! of closed enums — six `Visibility` variants, three `SourceLocation`
//! variants, four `Unlocated` reasons — so the input space is finite and the
//! fixtures below enumerate it exhaustively. Where the rich fixture already
//! contains the value (deprecations, `Unlocated(ProducerRecordsNoLocation)`),
//! it is used in preference.

use std::sync::Arc;

use futures::StreamExt as _;
use nudox_engine::graph::adapter::CorpusAdapter;
use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
    entry::{
        ByteSpan, Entry, LineCol, Node, SourceFile, SourceLocation, Symbol, Unlocated, Visibility,
    },
    kind::Kind,
    kinds::Function,
    view::IrView,
};
use nudox_engine::store::{
    corpus::Corpus,
    package::{PackageView, Provenance},
    source::fixtures::{build_rich_view, rich_lineage},
};
use serde::Deserialize;
use trustfall::{Schema, TryIntoStruct, execute_query_async};

fn schema() -> Schema {
    Schema::parse(include_str!("../../schema.graphql")).expect("schema must parse")
}

fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

fn no_args() -> std::collections::BTreeMap<String, String> {
    std::collections::BTreeMap::new()
}

/// Execute `query` against `corpus` and deserialize every row into `R`.
async fn rows<R: for<'de> Deserialize<'de>>(
    corpus: Corpus,
    query: &str,
    vars: impl IntoIterator<Item = (String, String)>,
) -> Vec<R> {
    let schema = schema();
    let adapter = Arc::new(CorpusAdapter::new(corpus));
    execute_query_async(
        &schema,
        adapter,
        query,
        vars.into_iter()
            .collect::<std::collections::BTreeMap<_, _>>(),
    )
    .expect("query must execute")
    .map(|row| {
        row.expect("no stream error")
            .try_into_struct::<R>()
            .expect("deserialize")
    })
    .collect()
    .await
}

async fn rich_corpus() -> Corpus {
    let corpus = Corpus::new();
    corpus
        .insert(Arc::new(PackageView::build(
            build_rich_view(),
            Provenance::TrustedLocal,
        )))
        .await;
    corpus
}

// ===========================================================================
// SourceLocation
// ===========================================================================

#[derive(Debug, Deserialize)]
struct LocRow {
    name: String,
    loc_kind: String,
    file: Option<String>,
    reason: Option<String>,
    byte_start: Option<i64>,
    start_line: Option<i64>,
    start_column: Option<i64>,
    end_line: Option<i64>,
}

const LOCATION_QUERY: &str = r#"{
    Symbols {
        name @output
        location {
            kind @output(name: "loc_kind")
            file @output(name: "file")
            unlocatedReason @output(name: "reason")
            byteStart @output(name: "byte_start")
            startLine @output(name: "start_line")
            startColumn @output(name: "start_column")
            endLine @output(name: "end_line")
        }
    }
}"#;

/// The rich fixture's symbols have no recorded source position. The graph must
/// report *why* — `Unlocated(ProducerRecordsNoLocation)` — rather than a null
/// file with nothing to explain it.
///
/// This is the whole point of exposing the location as a sum rather than as
/// nullable columns: before this landed the schema had no location fields at
/// all, and the obvious way to add them (a nullable `sourceFile`) would have
/// made "this producer has not been migrated" indistinguishable from "this
/// symbol genuinely has no source position".
#[tokio::test]
async fn a_symbol_with_no_recorded_position_reports_the_reason_not_a_bare_null() {
    let results: Vec<LocRow> = rows(rich_corpus().await, LOCATION_QUERY, []).await;

    assert!(
        !results.is_empty(),
        "the rich fixture must yield symbols to inspect"
    );
    for row in &results {
        assert_eq!(
            row.loc_kind, "Unlocated",
            "every rich-fixture symbol is built through `Entry::new` with an empty \
             source path, which decodes to Unlocated; got {row:?}"
        );
        assert_eq!(
            row.reason.as_deref(),
            Some("ProducerRecordsNoLocation"),
            "the reason must survive the projection; a null here is the flattening \
             this field exists to prevent. row: {row:?}"
        );
        assert_eq!(row.file, None, "Unlocated carries no file; row: {row:?}");
        assert_eq!(
            row.byte_start, None,
            "Unlocated carries no byte span; row: {row:?}"
        );
        assert_eq!(
            row.start_line, None,
            "Unlocated carries no line index; row: {row:?}"
        );
    }
    // A named symbol, so this cannot pass against an adapter that returns
    // nothing.
    assert!(
        results.iter().any(|r| r.name == "distance"),
        "expected the fixture's `distance` function among the rows"
    );
}

/// A package whose entries carry each of the three `SourceLocation` variants
/// and each of the four `Unlocated` reasons.
fn located_package() -> Arc<PackageView> {
    fn sym(name: &str, loc: &SourceLocation) -> Symbol {
        // `Symbol::source`/`span` must be the location's own `legacy_pair()`
        // projection — building them any other way asserts two different
        // origins for one declaration.
        let (source, span) = loc.legacy_pair();
        Symbol {
            name: name.to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source,
            span,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        }
    }

    let file = SourceFile::new("src/lib.rs").expect("a non-empty relative path");
    let bytes = ByteSpan::new(120, 260).expect("a non-empty byte span");
    let locations = vec![
        (
            "declared_fn",
            SourceLocation::Declared {
                file: file.clone(),
                bytes,
                start: LineCol::from_zero_based(11, 4),
                end: LineCol::from_zero_based(14, 1),
            },
        ),
        ("bytes_only_fn", SourceLocation::BytesOnly { file, bytes }),
        (
            "synthesized_fn",
            SourceLocation::Unlocated(Unlocated::Synthesized),
        ),
        (
            "macro_fn",
            SourceLocation::Unlocated(Unlocated::MacroExpanded),
        ),
        (
            "unrecorded_fn",
            SourceLocation::Unlocated(Unlocated::ProducerRecordsNoLocation),
        ),
        (
            "outside_fn",
            SourceLocation::Unlocated(Unlocated::OutsideDocumentedPackage),
        ),
    ];

    let mut table = PristineIntroTable::new();
    for (i, (name, loc)) in locations.into_iter().enumerate() {
        table.insert_live(
            intro(i as u8 + 1),
            Entry::new_located(
                sym(name, &loc),
                Node::build(None::<nudox_ir::index::RawRef>, []),
                Kind::Function(Function::builder().build()),
                loc,
            ),
            None,
        );
    }
    let lid = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("located"));
    Arc::new(PackageView::build(
        IrView::with_package(lid, table),
        Provenance::TrustedLocal,
    ))
}

/// Every `SourceLocation` variant, and every `Unlocated` reason, must be
/// distinguishable through the graph.
///
/// The assertion that matters most is the last one: the four `Unlocated`
/// reasons must produce four *different* answers. A projection that mapped
/// them all to `kind: "Unlocated", file: null` would satisfy every other
/// assertion in this file.
#[tokio::test]
async fn every_source_location_variant_survives_the_projection() {
    let corpus = Corpus::new();
    corpus.insert(located_package()).await;
    let results: Vec<LocRow> = rows(corpus, LOCATION_QUERY, []).await;

    let by_name = |n: &str| -> &LocRow {
        results
            .iter()
            .find(|r| r.name == n)
            .unwrap_or_else(|| panic!("expected a row for {n}; got {results:?}"))
    };

    let declared = by_name("declared_fn");
    assert_eq!(declared.loc_kind, "Declared");
    assert_eq!(declared.file.as_deref(), Some("src/lib.rs"));
    assert_eq!(declared.byte_start, Some(120));
    // `LineCol::from_zero_based(11, 4)` is 1-based line 12, column 5.
    assert_eq!(declared.start_line, Some(12));
    assert_eq!(declared.start_column, Some(5));
    assert_eq!(declared.end_line, Some(15));
    assert_eq!(
        declared.reason, None,
        "a Declared location has no unlocated reason to give"
    );

    let bytes_only = by_name("bytes_only_fn");
    assert_eq!(bytes_only.loc_kind, "BytesOnly");
    assert_eq!(bytes_only.file.as_deref(), Some("src/lib.rs"));
    assert_eq!(bytes_only.byte_start, Some(120));
    assert_eq!(
        bytes_only.start_line, None,
        "BytesOnly has no line index — reporting one would be inventing it"
    );
    assert_eq!(bytes_only.reason, None);

    for (name, expected_reason) in [
        ("synthesized_fn", "Synthesized"),
        ("macro_fn", "MacroExpanded"),
        ("unrecorded_fn", "ProducerRecordsNoLocation"),
        ("outside_fn", "OutsideDocumentedPackage"),
    ] {
        let row = by_name(name);
        assert_eq!(row.loc_kind, "Unlocated");
        assert_eq!(
            row.reason.as_deref(),
            Some(expected_reason),
            "the four Unlocated reasons must be four distinct answers, not one; row: {row:?}"
        );
    }

    // Stated as an invariant rather than left implicit: the four reasons are
    // four values, and a projection that collapsed them would still pass every
    // `kind == "Unlocated"` assertion above.
    let reasons: std::collections::BTreeSet<&str> = results
        .iter()
        .filter_map(|r| r.reason.as_deref())
        .collect();
    assert_eq!(
        reasons.len(),
        4,
        "expected four distinct unlocated reasons, got {reasons:?}"
    );
}

// ===========================================================================
// Visibility
// ===========================================================================

#[derive(Debug, Deserialize)]
struct VisRow {
    name: String,
    visibility: String,
    is_public: bool,
}

/// A package with one symbol per `Visibility` variant.
fn visibility_package() -> Arc<PackageView> {
    let variants = [
        ("public_fn", Visibility::Public),
        ("private_fn", Visibility::Private),
        ("protected_fn", Visibility::Protected),
        ("internal_fn", Visibility::Internal),
        ("package_fn", Visibility::Package),
        ("crate_fn", Visibility::Crate),
    ];
    let mut table = PristineIntroTable::new();
    for (i, (name, vis)) in variants.into_iter().enumerate() {
        table.insert_live(
            intro(i as u8 + 1),
            Entry::new(
                Symbol {
                    name: name.to_owned(),
                    visibility: vis,
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
                Kind::Function(Function::builder().build()),
            ),
            None,
        );
    }
    let lid = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("visibility"));
    Arc::new(PackageView::build(
        IrView::with_package(lid, table),
        Provenance::TrustedLocal,
    ))
}

/// `isPublic` answers one bit; the IR knows six values. The five non-public
/// visibilities must be distinguishable, because `pub(crate)` and genuinely
/// private are different facts about an API and a docs reader needs both.
#[tokio::test]
async fn all_six_visibilities_are_distinguishable_not_collapsed_to_a_bit() {
    let corpus = Corpus::new();
    corpus.insert(visibility_package()).await;
    let results: Vec<VisRow> = rows(
        corpus,
        r#"{ Symbols { name @output visibility @output is_public: isPublic @output } }"#,
        [],
    )
    .await;

    let mut seen: Vec<(&str, &str, bool)> = results
        .iter()
        .map(|r| (r.name.as_str(), r.visibility.as_str(), r.is_public))
        .collect();
    seen.sort_unstable();

    assert_eq!(
        seen,
        vec![
            ("crate_fn", "Crate", false),
            ("internal_fn", "Internal", false),
            ("package_fn", "Package", false),
            ("private_fn", "Private", false),
            ("protected_fn", "Protected", false),
            ("public_fn", "Public", true),
        ],
        "each visibility must project to its own name, and `isPublic` must stay \
         true only for Public"
    );

    // The five false-`isPublic` rows must not be five copies of one string:
    // that collapse is exactly what this field exists to undo.
    let non_public: std::collections::BTreeSet<&str> = results
        .iter()
        .filter(|r| !r.is_public)
        .map(|r| r.visibility.as_str())
        .collect();
    assert_eq!(
        non_public.len(),
        5,
        "expected five distinct non-public visibilities, got {non_public:?}"
    );
}

/// `visibility` must be usable as a filter, not merely as an output — the
/// point of naming the value is that a query can select on it.
#[tokio::test]
async fn visibility_is_filterable() {
    let corpus = Corpus::new();
    corpus.insert(visibility_package()).await;

    #[derive(Debug, Deserialize)]
    struct Row {
        name: String,
    }

    let results: Vec<Row> = rows(
        corpus,
        r#"{ Symbols { visibility @filter(op: "=", value: ["$v"]) name @output } }"#,
        [("v".to_owned(), "Crate".to_owned())],
    )
    .await;

    let names: Vec<&str> = results.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, vec!["crate_fn"]);
}

// ===========================================================================
// Deprecation
// ===========================================================================

/// `isDeprecated` says *that* a symbol is deprecated; the IR also knows the
/// note and the version. Both were dropped by the schema, which is the whole
/// substance of a deprecation notice.
///
/// Asserted against the rich fixture's real content: `legacy_fn` (intro 17)
/// carries note `"Use `distance` instead."` and since `"0.2.0"`.
#[tokio::test]
async fn a_deprecation_note_and_version_reach_the_graph() {
    #[derive(Debug, Deserialize)]
    struct Row {
        name: String,
        deprecated: bool,
        note: Option<String>,
        since: Option<String>,
    }

    let key = StableRef::new(rich_lineage(), intro(17)).to_string();
    let results: Vec<Row> = rows(
        rich_corpus().await,
        r#"{
            Symbols {
                key @filter(op: "=", value: ["$key"])
                name @output
                deprecated: isDeprecated @output
                note: deprecationNote @output
                since: deprecationSince @output
            }
        }"#,
        [("key".to_owned(), key)],
    )
    .await;

    let row = results.first().expect("legacy_fn must resolve");
    assert_eq!(row.name, "legacy_fn");
    assert!(row.deprecated);
    assert_eq!(
        row.note.as_deref(),
        Some("Use `distance` instead."),
        "the deprecation note must reach the graph verbatim"
    );
    assert_eq!(row.since.as_deref(), Some("0.2.0"));
}

/// A symbol that is not deprecated must report nulls, not empty strings — an
/// empty string is a value someone could have written.
#[tokio::test]
async fn a_live_symbol_reports_null_deprecation_fields() {
    #[derive(Debug, Deserialize)]
    struct Row {
        deprecated: bool,
        note: Option<String>,
        since: Option<String>,
    }

    let key = StableRef::new(rich_lineage(), intro(6)).to_string(); // `distance`
    let results: Vec<Row> = rows(
        rich_corpus().await,
        r#"{
            Symbols {
                key @filter(op: "=", value: ["$key"])
                deprecated: isDeprecated @output
                note: deprecationNote @output
                since: deprecationSince @output
            }
        }"#,
        [("key".to_owned(), key)],
    )
    .await;

    let row = results.first().expect("distance must resolve");
    assert!(!row.deprecated);
    assert_eq!(row.note, None);
    assert_eq!(row.since, None);
}

// ===========================================================================
// Occurrence → owner
// ===========================================================================

/// `Occurrence.spanStart`/`spanEnd` are offsets relative to the owning
/// symbol's own span start (`nudox_ir::vocab::RelSpan`). Until `owner` existed
/// the graph published two numbers whose base was unreachable, so no caller
/// could turn them into a position.
#[tokio::test]
async fn an_occurrence_can_reach_the_owner_its_span_is_relative_to() {
    #[derive(Debug, Deserialize)]
    struct Row {
        owner_name: String,
        target_name: String,
        span_start: i64,
    }

    let key = StableRef::new(rich_lineage(), intro(23)).to_string(); // `format_point`
    let results: Vec<Row> = rows(
        rich_corpus().await,
        r#"{
            Symbols {
                key @filter(op: "=", value: ["$key"])
                occurrencesOf {
                    span_start: spanStart @output
                    owner { owner_name: name @output }
                    target { target_name: name @output }
                }
            }
        }"#,
        [("key".to_owned(), key)],
    )
    .await;

    assert!(
        !results.is_empty(),
        "format_point must have occurrences to inspect"
    );
    for row in &results {
        assert_eq!(
            row.owner_name, "format_point",
            "the owner edge must lead back to the symbol the occurrence is stored on; row: {row:?}"
        );
        assert!(
            row.span_start >= 0,
            "a relative span start cannot be negative; row: {row:?}"
        );
    }
    let targets: Vec<&str> = results.iter().map(|r| r.target_name.as_str()).collect();
    assert!(
        targets.contains(&"distance"),
        "format_point calls distance; got targets {targets:?}"
    );
}

// ===========================================================================
// Row order
// ===========================================================================

/// An unfiltered `Symbols` scan is a user-visible row order, so it must be
/// deterministic across runs.
///
/// `IrView::entries()` is `HashMap` order — explicitly documented as unstable
/// across process launches — and both the scan and `Package.members` used it.
/// `nudox-store` had already grown `entries_sorted()` and routed its own index
/// builds through it; this adapter had not followed.
#[tokio::test]
async fn an_unfiltered_scan_returns_symbols_in_a_deterministic_order() {
    #[derive(Debug, Deserialize)]
    struct Row {
        key: String,
    }

    let first: Vec<Row> = rows(rich_corpus().await, r#"{ Symbols { key @output } }"#, []).await;
    let second: Vec<Row> = rows(rich_corpus().await, r#"{ Symbols { key @output } }"#, []).await;

    let a: Vec<&str> = first.iter().map(|r| r.key.as_str()).collect();
    let b: Vec<&str> = second.iter().map(|r| r.key.as_str()).collect();
    assert!(!a.is_empty(), "the fixture must yield rows");
    assert_eq!(a, b, "two scans of one corpus must agree on order");

    // Sorted by IntroId, which is what `entries_sorted` guarantees and what
    // makes the order reproducible across *processes*, not merely within one.
    let mut sorted = a.clone();
    sorted.sort_unstable();
    assert_eq!(
        a, sorted,
        "the scan must emit symbols in StableRef order; an unsorted order that \
         merely happens to repeat within one process is the HashMap-iteration bug"
    );
}

/// `Package.members` is the other user-visible order that came off
/// `entries()`.
#[tokio::test]
async fn package_members_are_returned_in_a_deterministic_order() {
    #[derive(Debug, Deserialize)]
    struct Row {
        key: String,
    }

    let query = r#"{ Packages { members { key @output } } }"#;
    let first: Vec<Row> = rows(rich_corpus().await, query, []).await;
    let a: Vec<&str> = first.iter().map(|r| r.key.as_str()).collect();
    let mut sorted = a.clone();
    sorted.sort_unstable();
    assert!(!a.is_empty(), "the fixture package must have members");
    assert_eq!(a, sorted, "Package.members must be in StableRef order");
}

/// A guard against the schema and the adapter drifting apart: every type the
/// SDL declares must be a `Vertex` variant the adapter can actually emit, or a
/// query coercing to it silently returns nothing.
#[test]
fn every_sdl_object_type_has_a_vertex_variant() {
    let sdl = nudox_engine::graph::SCHEMA_SDL;
    let declared: Vec<&str> = sdl
        .lines()
        .filter_map(|l| l.strip_prefix("type "))
        .filter_map(|l| l.split_whitespace().next())
        .filter(|t| *t != "RootSchemaQuery")
        .collect();

    // Mirrors `Vertex`'s variant list; `typename_invariant.rs` pins that these
    // strings really are what `typename()` returns.
    let emitted = [
        "Package",
        "Function",
        "Record",
        "Trait",
        "Impl",
        "Enum",
        "Field",
        "Const",
        "Alias",
        "Static",
        "Variant",
        "Module",
        "Reexport",
        "Param",
        "OtherSymbol",
        "Occurrence",
        "SourceLocation",
    ];
    for ty in &declared {
        assert!(
            emitted.contains(ty),
            "schema.graphql declares `type {ty}` but no Vertex variant emits it; \
             any query coercing to it would silently return nothing"
        );
    }
    for ty in emitted {
        assert!(
            declared.contains(&ty),
            "Vertex emits `{ty}` but schema.graphql declares no such type"
        );
    }
}

// ===========================================================================
// Rendered type text
//
// `Field.typeStr` was `format!("{t:?}")` and nothing asserted on its content,
// so it shipped `Primitive(Float(W64))` where the answer is `f64` and
// `Nominal(Intro(intro:3f1a9c2b…))` where the answer is `Point`. Five sibling
// fields were left off the schema entirely rather than replicate it.
//
// Every assertion below is an exact expected string. `assert!(!s.is_empty())`
// would have passed against the defect, and did — for as long as the defect
// existed.
// ===========================================================================

#[derive(Debug, Deserialize)]
struct TypeTextRow {
    name: String,
    rendered: Option<String>,
}

/// Run `query` and collect its `name`/`rendered` pairs into a lookup.
async fn type_text(query: &str) -> std::collections::HashMap<String, Option<String>> {
    let results: Vec<TypeTextRow> = rows(rich_corpus().await, query, []).await;
    assert!(
        !results.is_empty(),
        "the rich fixture must yield rows for this query, or every assertion \
         below is vacuous"
    );
    results
        .into_iter()
        .map(|r| (r.name, r.rendered))
        .collect()
}

/// A primitive must render as its written spelling, not as its IR shape.
///
/// `Point.x: f64` is `Type::Primitive(Float(Width::W64))`. Under `Debug` that
/// was `Primitive(Float(W64))`; under a renderer it is `f64`.
#[tokio::test]
async fn a_field_type_renders_as_source_syntax_not_as_an_ir_dump() {
    let by_name = type_text(
        r#"{ Symbols { ... on Field { name @output typeStr @output(name: "rendered") } } }"#,
    )
    .await;

    assert_eq!(
        by_name.get("x").and_then(Clone::clone).as_deref(),
        Some("f64"),
        "`Point.x` is an f64. rows: {by_name:?}"
    );
    for (name, rendered) in &by_name {
        let Some(text) = rendered else { continue };
        for leak in ["Primitive(", "Nominal(", "Intro(", "Width::", "W64"] {
            assert!(
                !text.contains(leak),
                "field `{name}` leaked IR Debug syntax {leak:?}: {text}"
            );
        }
    }
}

/// A same-package nominal must render as the declaration's **name**, resolved
/// through the package's entry table — not as the `?ref(…)` digest placeholder
/// the resolver-less path falls back to, and certainly not as a hex `IntroId`.
///
/// This is the single assertion that pins the resolver is actually wired: the
/// renderer produces honest output either way, so a `type_str` that forgot to
/// pass one would still return a plausible non-empty string.
#[tokio::test]
async fn a_same_package_nominal_renders_by_name_not_by_digest() {
    let fields = type_text(
        r#"{ Symbols { ... on Field { name @output typeStr @output(name: "rendered") } } }"#,
    )
    .await;
    // `Pair.1` is `Type::Apply { base: Point, args: [i32] }`.
    assert_eq!(
        fields.get("1").and_then(Clone::clone).as_deref(),
        Some("Point<i32>"),
        "the generic application's base must resolve to the record's name and \
         its argument must render too. rows: {fields:?}"
    );

    let params = type_text(
        r#"{ Symbols { ... on Param { name @output typeStr @output(name: "rendered") } } }"#,
    )
    .await;
    assert_eq!(
        params.get("p").and_then(Clone::clone).as_deref(),
        Some("Point"),
        "`distance(p: Point)` — the parameter type is a bare same-package \
         nominal, the exact case that renders as `?ref(3f1a9c2b)` without a \
         resolver. rows: {params:?}"
    );

    let impls = type_text(
        r#"{ Symbols { ... on Impl { name @output selfTypeStr @output(name: "rendered") } } }"#,
    )
    .await;
    assert_eq!(
        impls.get("PointDisplay").and_then(Clone::clone).as_deref(),
        Some("Point"),
        "`impl Display for Point` — `self_ty` is the field that had no schema \
         projection at all. rows: {impls:?}"
    );
}

/// The four remaining withheld fields, each asserted on a distinct type shape
/// so that one renderer arm cannot cover for another.
#[tokio::test]
async fn the_withheld_type_valued_fields_all_render_real_syntax() {
    let consts = type_text(
        r#"{ Symbols { ... on Const { name @output typeStr @output(name: "rendered") } } }"#,
    )
    .await;
    assert_eq!(
        consts.get("MAX_SIZE").and_then(Clone::clone).as_deref(),
        Some("u64"),
        "rows: {consts:?}"
    );

    let statics = type_text(
        r#"{ Symbols { ... on Static { name @output typeStr @output(name: "rendered") } } }"#,
    )
    .await;
    assert_eq!(
        statics.get("REGISTRY").and_then(Clone::clone).as_deref(),
        Some("any"),
        "`Type::Any` is the *top* type and renders bare; it must not collide \
         with any `?`-prefixed unknown. rows: {statics:?}"
    );

    let aliases = type_text(
        r#"{ Symbols { ... on Alias { name @output targetStr @output(name: "rendered") } } }"#,
    )
    .await;
    assert_eq!(
        aliases.get("Callback").and_then(Clone::clone).as_deref(),
        Some("fn(i32) -> bool"),
        "a function-pointer alias renders as a function type, not as a struct \
         dump. rows: {aliases:?}"
    );
    assert_eq!(
        aliases.get("AnyUnion").and_then(Clone::clone).as_deref(),
        Some("str | i32 | bool"),
        "a union renders infix. rows: {aliases:?}"
    );
}

// ===========================================================================
// Key provenance
// ===========================================================================

#[derive(Debug, Deserialize)]
struct KeyTierRow {
    name: String,
    tier: String,
    derived: Option<bool>,
}

/// A `PackageView` built without a `SealReport` must say `Unrecorded`, and must
/// leave `keyIsContentDerived` null.
///
/// This is the honest half of the contract and the easy one to get wrong: the
/// tempting default is `Structural`/`true`, which would tell every caller its
/// key is sound on a corpus that never looked. Doctrine §8 — a silent repair is
/// the same defect class as a silent failure.
#[tokio::test]
async fn a_view_with_no_seal_report_reports_unrecorded_not_structural() {
    let results: Vec<KeyTierRow> = rows(
        rich_corpus().await,
        r#"{
            Symbols {
                name @output
                keyTier @output(name: "tier")
                keyIsContentDerived @output(name: "derived")
            }
        }"#,
        [],
    )
    .await;

    assert!(!results.is_empty(), "the rich fixture must yield symbols");
    for row in &results {
        assert_eq!(
            row.tier, "Unrecorded",
            "the rich fixture is a hand-built table that never ran `seal`, so \
             nothing is known about how its keys were minted. row: {row:?}"
        );
        assert_eq!(
            row.derived, None,
            "null, never false: \"we did not look\" is not \"this key is \
             fragile\". row: {row:?}"
        );
    }
    assert!(
        results.iter().any(|r| r.name == "distance"),
        "a named symbol must be among the rows, so this cannot pass against an \
         adapter that returns nothing"
    );
}
