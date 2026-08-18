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

use nudox_engine::mcp::tools::GraphQueryArgs;
use nudox_engine::mcp::NudoxTools;
use nudox_engine::{Engine, EngineConfig, PackageLoadEvent};
use std::time::Duration;

fn make_tools() -> NudoxTools {
    NudoxTools::new(Engine::start_with_fixtures(EngineConfig::default()))
}

async fn wait_for_corpus(tools: &NudoxTools) {
    let rx = tools.engine().packages();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv_async()).await {
            Ok(Ok(PackageLoadEvent::Loaded { .. })) => return,
            Ok(Ok(_)) => {},
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
