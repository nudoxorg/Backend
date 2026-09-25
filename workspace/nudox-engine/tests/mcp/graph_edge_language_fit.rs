//! A graph edge that cannot answer for a package's language must say so.
//!
//! # The incident
//!
//! Field report, 2026-08-16, from an agent exploring a Go service:
//!
//! > `graph implementors` — Rust-only edge, correctly documented, but meant my
//! > first guess for "what satisfies AuthDeps" was wrong. `graph subtypes` —
//! > the documented Go equivalent, but came back empty too. … So it took me
//! > **two wrong graph queries** before landing on `members`, which was the one
//! > that actually mattered.
//!
//! Both wrong guesses returned `[]`. An empty list is the same shape as a
//! correct answer, so neither told the caller it had asked a question this
//! edge cannot answer for this language — it just looked like the Go package
//! had no implementors and no subtypes. The documentation was right and the
//! caller had read it; documentation loses to a result that contradicts it.
//!
//! # What "by construction" means here
//!
//! The fix is not more documentation. It is that traversing an edge which is
//! **empty by construction** for every package in scope produces a *stated*
//! outcome rather than an empty one, naming the edge that does carry the
//! relationship for that language. A caller then cannot burn a turn on the
//! wrong edge without being told which one is right — the second wrong guess
//! becomes impossible, and the first one is self-correcting.
//!
//! This is the same discipline `refs` already applies with
//! `ReferenceCoverage::NotRecorded` and `search` with `excluded_kinds`: the
//! degraded case gets a shape of its own instead of borrowing the good case's.

use std::{sync::Arc, time::Duration};

use futures::{StreamExt, stream::BoxStream};
use nudox_engine::{
    Engine, EngineConfig, PackageLoadEvent,
    graph::{EdgeEmptyReason, diagnose_empty_edge},
    mcp::{
        NudoxTools, render_markdown,
        tools::{EdgeCoverageNote, GraphQueryArgs, QueryResult},
    },
    store::{
        package::{PackageView, Provenance},
        source::{Error, IrSource, LoadEvent, LoadRequest, PackageHint, SourceDescriptor},
    },
};
use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, IntroId, PackageLineageId, PackageName},
    entry::{Entry, Node, Symbol, Visibility},
    index::{RawRef, Ref},
    kind::Kind,
    kinds::{Function, Module, Param, Record, Trait, Type},
    view::IrView,
};

fn make_tools() -> NudoxTools {
    NudoxTools::new(Engine::start_with_fixtures(EngineConfig::default()))
}

async fn wait_for_corpus(tools: &NudoxTools) {
    let rx = tools.engine().packages();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv_async()).await {
            Ok(Ok(PackageLoadEvent::Loaded { .. })) => return,
            Ok(Ok(_)) => {}
            Ok(Err(_)) => return,
            Err(elapsed) => panic!("corpus never seeded within 5 s"),
        }
    }
}

/// Traverse `implementors` — the Rust-only edge — over a corpus that has no
/// Rust package, and require the result to say so.
///
/// The fixture corpus is `fixture:`-ecosystem, not `cargo:`, so `Impl` entries
/// do not exist in it and this edge is empty by construction — exactly the
/// situation the Go user was in.
#[tokio::test]
async fn an_edge_with_no_data_for_this_language_says_so() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    let result = tools
        .do_graph_query(GraphQueryArgs {
            query: r#"{
                Symbols {
                    ... on Trait {
                        implementors { name @output }
                    }
                }
            }"#
            .to_owned(),
            args: None,
            limit: Some(20),
            cursor: None,
        })
        .await
        .expect("a well-formed query must not error");

    if result.rows.is_empty() {
        let note = result.edge_coverage.as_ref().unwrap_or_else(|| {
            panic!(
                "`implementors` returned no rows and said nothing about why. \
                 An agent reads that as \"this type has no implementors\", \
                 which is what cost two wrong queries in the field. The \
                 response must state that the edge is empty by construction \
                 for the languages in scope."
            )
        });
        let rendered = format!("{note:?}");
        assert!(
            rendered.contains("subtypes"),
            "naming the dead edge is half the fix; the response must also name \
             the edge that DOES carry this relationship for these packages, or \
             the caller's next guess is still a guess: {rendered}",
        );
    }
}

/// A query whose edges do apply carries no such note.
///
/// Same discipline as `SearchResult::semantic` and `excluded_kinds`: present
/// only when the answer is not the straightforward one, so it costs nothing in
/// the common case and does not become noise a reader learns to skip.
#[tokio::test]
async fn an_applicable_edge_carries_no_coverage_note() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    let result = tools
        .do_graph_query(GraphQueryArgs {
            query: r#"{
                Packages {
                    members { name @output }
                }
            }"#
            .to_owned(),
            args: None,
            limit: Some(5),
            cursor: None,
        })
        .await
        .expect("a well-formed query must not error");

    assert!(
        !result.rows.is_empty(),
        "`members` is the edge that worked in the field report; the fixture \
         corpus must exercise it",
    );
    assert!(
        result.edge_coverage.is_none(),
        "an edge that answered must not carry a coverage note: {:?}",
        result.edge_coverage,
    );
}

fn render_reason(
    edge: &str,
    symbol: &str,
    package: &Arc<PackageView>,
) -> (EdgeEmptyReason, String) {
    let diagnosis = diagnose_empty_edge(edge, symbol, package, std::slice::from_ref(package));
    let rendered = render_markdown(&QueryResult {
        columns: vec![],
        rows: vec![],
        truncated: false,
        next_cursor: None,
        edge_coverage: Some(EdgeCoverageNote {
            edge: diagnosis.edge,
            answers_instead: diagnosis.answers_instead.to_owned(),
            reason: diagnosis.reason.clone(),
        }),
    });
    assert!(
        rendered.contains(&format!("~graph:edge_empty({edge} → try ")),
        "the empty-edge signal must keep its shape: {rendered}"
    );
    assert!(
        !rendered.contains("signatureTypes"),
        "an empty reverse edge must not send the caller to signatureTypes: {rendered}"
    );
    (diagnosis.reason, rendered)
}

/// A package that records `Return` for `Other`, writes an unlinked `Widget`
/// at that same position, and never records occurrences or `ImplementedTrait`.
fn coverage_package() -> Arc<PackageView> {
    let lineage = PackageLineageId::new(EcosystemId::new("fixture"), PackageName::new("edges"));
    let mut table = PristineIntroTable::new();
    let root = intro_id(1);
    table.insert_live(root, named("root", Kind::Module(Module)), None);
    table.insert_live(
        intro_id(2),
        named("Widget", Kind::Record(Record::builder().build())),
        Some(root),
    );
    table.insert_live(
        intro_id(3),
        named("Other", Kind::Record(Record::builder().build())),
        Some(root),
    );
    table.insert_live(
        intro_id(4),
        named("Lonely", Kind::Record(Record::builder().build())),
        Some(root),
    );
    table.insert_live(
        intro_id(5),
        named("Marker", Kind::Trait(Trait::builder().build())),
        Some(root),
    );
    table.insert_live(
        intro_id(7),
        named(
            "linked",
            Kind::Function(
                Function::builder()
                    .output_params([Ref::Intro(intro_id(6))])
                    .build(),
            ),
        ),
        Some(root),
    );
    table.insert_live(
        intro_id(6),
        named(
            "linked_ret",
            Kind::Param(
                Param::builder()
                    .ty(Type::Nominal(Ref::Intro(intro_id(3))))
                    .build(),
            ),
        ),
        Some(intro_id(7)),
    );
    table.insert_live(
        intro_id(9),
        named(
            "broken",
            Kind::Function(
                Function::builder()
                    .output_params([Ref::Intro(intro_id(8))])
                    .build(),
            ),
        ),
        Some(root),
    );
    table.insert_live(
        intro_id(8),
        named(
            "broken_ret",
            Kind::Param(
                Param::builder()
                    .ty(Type::unresolved_external("Widget"))
                    .build(),
            ),
        ),
        Some(intro_id(9)),
    );
    let view = IrView::with_package(lineage, table);
    Arc::new(PackageView::build(view, Provenance::TrustedLocal))
}

fn intro_id(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

fn named(name: &str, kind: Kind) -> Entry {
    Entry::new(
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
        },
        Node::build(None::<RawRef>, []),
        kind,
    )
}

#[test]
fn returned_by_unresolved_widget_names_the_spelling() {
    let package = coverage_package();
    assert!(
        package
            .indexes()
            .records_type_position(nudox_engine::store::package::TypePosition::Return),
        "Other's linked return must record Return, or this test does not prove \
         the note is per symbol"
    );
    let (reason, rendered) = render_reason("returnedBy", "Widget", &package);
    assert!(
        matches!(reason, EdgeEmptyReason::UnresolvedNominal { ref spelling } if spelling == "Widget"),
        "an unlinked Widget return is unresolved-nominal, even though Return \
         is recorded for Other: {reason:?}"
    );
    assert!(rendered.contains("unresolved-nominal"), "{rendered}");
    assert!(rendered.contains("Widget"), "{rendered}");
}

#[test]
fn usages_when_occurrences_were_not_recorded() {
    let package = coverage_package();
    assert!(!package.indexes().occurrences_recorded);
    let (reason, rendered) = render_reason("usages", "Widget", &package);
    assert!(
        matches!(reason, EdgeEmptyReason::OccurrencesNotAttached),
        "{reason:?}"
    );
    assert!(rendered.contains("occurrences-not-attached"), "{rendered}");
}

#[test]
fn returned_by_with_nothing_unresolved_is_no_such_edge() {
    let package = coverage_package();
    let (reason, rendered) = render_reason("returnedBy", "Lonely", &package);
    assert!(
        matches!(reason, EdgeEmptyReason::NoSuchEdge),
        "a symbol nothing returns, with nothing unresolved, is no-such-edge: {reason:?}"
    );
    assert!(rendered.contains("no-such-edge"), "{rendered}");
    assert!(
        !rendered.contains("rust-only"),
        "returnedBy is not the Rust-only implementors edge: {rendered}"
    );
}

#[test]
fn implementors_without_implemented_trait_still_points_at_subtypes() {
    let package = coverage_package();
    assert!(
        !package
            .indexes()
            .records_type_position(nudox_engine::store::package::TypePosition::ImplementedTrait)
    );
    let diagnosis = diagnose_empty_edge(
        "implementors",
        "Marker",
        &package,
        std::slice::from_ref(&package),
    );
    assert!(matches!(diagnosis.reason, EdgeEmptyReason::RustOnly));
    assert_eq!(diagnosis.answers_instead, "subtypes");
    let (_, rendered) = render_reason("implementors", "Marker", &package);
    assert!(rendered.contains("subtypes"), "{rendered}");
    assert!(rendered.contains("rust-only"), "{rendered}");
}

struct OnePackage {
    package: Arc<PackageView>,
}

impl IrSource for OnePackage {
    fn describe(&self) -> SourceDescriptor {
        SourceDescriptor {
            label: "edge-empty".into(),
            package_count_hint: Some(1),
        }
    }

    fn load(&self, _req: LoadRequest) -> BoxStream<'static, Result<LoadEvent, Error>> {
        let package = Arc::clone(&self.package);
        let lineage = package.lineage().clone();
        let events = vec![
            Ok(LoadEvent::Discovered {
                lineage: lineage.clone(),
                hint: PackageHint {
                    display_name: "edges".into(),
                    ecosystem: "fixture".into(),
                    version: None,
                },
            }),
            Ok(LoadEvent::Ready { package }),
        ];
        futures::stream::iter(events).boxed()
    }
}

fn query_named(edge: &str) -> String {
    format!(
        r#"{{
            Symbols {{
                name @filter(op: "=", value: ["$name"])
                {edge} {{ name @output }}
            }}
        }}"#
    )
}

async fn query_edge(
    tools: &NudoxTools,
    name: &str,
    edge: &str,
) -> nudox_engine::mcp::tools::QueryResult {
    tools
        .do_graph_query(GraphQueryArgs {
            query: query_named(edge),
            args: Some(std::iter::once(("name".to_owned(), name.to_owned())).collect()),
            limit: Some(20),
            cursor: None,
        })
        .await
        .expect("a well-formed query must not error")
}

/// The posting list is empty for the symbol the query named, and another
/// symbol in the same package did link a return type.
#[tokio::test]
async fn unresolved_widget_query_is_not_a_bare_page() {
    let tools = NudoxTools::new(Engine::start(EngineConfig::default(), OnePackage {
        package: coverage_package(),
    }));
    wait_for_corpus(&tools).await;

    let widget = query_edge(&tools, "Widget", "returnedBy").await;
    assert!(widget.rows.is_empty(), "Widget's return type did not link");
    let rendered = render_markdown(&widget);
    assert!(rendered.contains("unresolved-nominal"), "{rendered}");
    assert!(rendered.contains("Widget"), "{rendered}");
    assert!(!rendered.contains("signatureTypes"), "{rendered}");

    let lonely = query_edge(&tools, "Lonely", "returnedBy").await;
    assert!(lonely.rows.is_empty());
    let lonely_rendered = render_markdown(&lonely);
    assert!(
        lonely_rendered.contains("no-such-edge"),
        "{lonely_rendered}"
    );
    assert!(!lonely_rendered.contains("rust-only"), "{lonely_rendered}");

    let usages = query_edge(&tools, "Widget", "usages").await;
    assert!(usages.rows.is_empty());
    assert!(
        render_markdown(&usages).contains("occurrences-not-attached"),
        "{}",
        render_markdown(&usages)
    );

    let implementors = tools
        .do_graph_query(GraphQueryArgs {
            query: r#"{
                Symbols {
                    ... on Trait {
                        name @filter(op: "=", value: ["$name"])
                        implementors { name @output }
                    }
                }
            }"#
            .to_owned(),
            args: Some(std::iter::once(("name".to_owned(), "Marker".to_owned())).collect()),
            limit: Some(20),
            cursor: None,
        })
        .await
        .expect("implementors query must parse");
    assert!(implementors.rows.is_empty());
    let implementors_rendered = render_markdown(&implementors);
    assert_eq!(
        implementors
            .edge_coverage
            .as_ref()
            .map(|note| note.answers_instead.as_str()),
        Some("subtypes")
    );
    assert!(
        implementors_rendered.contains("subtypes"),
        "{implementors_rendered}"
    );
    assert!(
        implementors_rendered.contains("rust-only"),
        "{implementors_rendered}"
    );
}

/// The fixture records `Return` for nobody and `ImplementedTrait` for Display.
/// Point's empty `returnedBy` is still a true negative, not Rust-only.
#[tokio::test]
async fn fixture_returned_by_with_no_posting_is_no_such_edge() {
    let tools = make_tools();
    wait_for_corpus(&tools).await;

    let result = query_edge(&tools, "Point", "returnedBy").await;
    assert!(
        result.rows.is_empty(),
        "the rich fixture does not return Point from any function"
    );
    let rendered = render_markdown(&result);
    assert!(
        rendered.contains("~graph:edge_empty(returnedBy → try "),
        "{rendered}"
    );
    assert!(rendered.contains("no-such-edge"), "{rendered}");
    assert!(!rendered.contains("rust-only"), "{rendered}");
}
