//! Pipeline part: **end-to-end** (compile → blob → index → search).
//!
//! The integration spec that ties every subsystem together: tracking a package
//! should make its symbols searchable across all three surfaces. Each test runs
//! the *whole* server (HTTP plane + background pollers) against the live stack
//! and real network acquisition — opt-in via `SERVER_TEST_BACKENDS`.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::TryStreamExt;
use heart::{PackageId, ResolutionState, Scored, Symbol};
use index::ecosystem::PackageNameExt as _;
use driver::registry::coordination::{OutboxSeq, SinkKind};
use driver::Server;

/// The tiny, dependency-free fixture crate the pipeline chews through.
const FIXTURE_NAME: &str = "either";
const FIXTURE_VERSION: &str = "1.15.0";
/// A symbol the fixture crate is certain to export.
const FIXTURE_SYMBOL: &str = "Either";

/// How long one full acquire→compile→emit→poll round may take.
const PIPELINE_DEADLINE: Duration = Duration::from_secs(300);
/// How often the probes re-check.
const PROBE_INTERVAL: Duration = Duration::from_millis(500);

/// A tracked package becomes searchable by name after sync.
///
/// Arrange: add the fixture package and wait for sync to finish.
/// Act: a precise (text) search for one of its symbols.
/// Assert: the symbol is returned carrying the tracked package's identity.
#[tokio::test]
async fn tracked_package_is_text_searchable() {
    let Some((server, _data)) =
        common::assembled_server("tracked_package_is_text_searchable").await
    else {
        return;
    };
    let package = track_and_await_stored(&server).await;

    let hit = await_text_hit(&server, FIXTURE_SYMBOL, package).await;
    assert_eq!(hit.value.package, package, "the hit carries the tracked package's identity");
    assert_eq!(hit.value.ecosystem, heart::Language::Rust);
}

/// The same package is reachable via semantic search.
///
/// Assert: a natural-language query surfaces the package's symbols via the
///   vector store (through the explicit semantic opt-in surface).
#[tokio::test]
async fn tracked_package_is_semantically_searchable() {
    let Some((server, _data)) =
        common::assembled_server("tracked_package_is_semantically_searchable").await
    else {
        return;
    };
    let package = track_and_await_stored(&server).await;
    // Text visibility first: the cheap proxy that fan-out has caught up.
    await_text_hit(&server, FIXTURE_SYMBOL, package).await;

    let request = driver::search::query::Search {
        query: driver::search::query::Query::Abstract(
            driver::search::query::AbstractQuery::NaturalLanguage(
                "a value that is one of two alternatives, left or right".into(),
            ),
        ),
        filter: driver::search::query::Filter::default(),
        page: common::page(16),
        _lifetime: std::marker::PhantomData,
    };
    let found = probe(PIPELINE_DEADLINE, || async {
        let cap = common::read_cap(&server);
        let hits: Vec<Scored<Symbol>> = server
            .search_symbols(&cap, &request)
            .await
            .ok()?
            .try_collect()
            .await
            .ok()?;
        hits.into_iter().find(|hit| hit.value.package == package)
    })
    .await;
    assert!(
        found.is_some(),
        "a natural-language query must surface the tracked package via the vector store"
    );
}

/// The same package is reachable via graph expansion.
///
/// Assert: expanding one of its symbols answers from the graph store (an
///   in-corpus symbol resolves and expands without error).
#[tokio::test]
async fn tracked_package_is_graph_expandable() {
    let Some((server, _data)) =
        common::assembled_server("tracked_package_is_graph_expandable").await
    else {
        return;
    };
    let package = track_and_await_stored(&server).await;
    let hit = await_text_hit(&server, FIXTURE_SYMBOL, package).await;

    let related = probe(PIPELINE_DEADLINE, || async {
        let cap = common::read_cap(&server);
        server.expand(&cap, &hit).await.ok()
    })
    .await
    .expect("expansion answers once the graph sink has materialized");
    assert!(
        related.iter().all(|neighbour| neighbour.value.package == package),
        "the fixture crate's neighbourhood stays inside the fixture crate"
    );
}

/// The package is parsed once and fanned out to every sink.
///
/// Assert: after one sync, every derived sink received (and consumed) exactly
///   the one projection — single parse, multiple sinks.
#[tokio::test]
async fn package_is_parsed_once_and_fanned_out() {
    let Some((server, _data)) =
        common::assembled_server("package_is_parsed_once_and_fanned_out").await
    else {
        return;
    };
    let package = track_and_await_stored(&server).await;

    for sink in [SinkKind::Text, SinkKind::Vector, SinkKind::Graph] {
        let intents = server
            .outbox()
            .read_since(sink, OutboxSeq(0), 1024)
            .await
            .expect("the outbox answers");
        let for_package: Vec<_> =
            intents.into_iter().filter(|entry| entry.package == package).collect();
        assert_eq!(
            for_package.len(),
            1,
            "one parse leaves exactly one {sink:?} intent — never zero, never a re-parse"
        );

        // And each sink's poller consumes it: the watermark passes the intent.
        let intent = for_package[0].id;
        let consumed = probe(PIPELINE_DEADLINE, || async {
            let watermark = server.outbox().read_watermark(sink).await.ok()?;
            (watermark >= intent).then_some(())
        })
        .await;
        assert!(consumed.is_some(), "the {sink:?} poller consumes the fan-out intent");
    }
}

/// A deferred external-library symbol becomes searchable after its lib resolves.
///
/// Assert: a symbol of an as-yet-untracked library is unfindable (deferred),
///   then becomes searchable once that library is registered and synced.
#[tokio::test]
async fn deferred_external_symbol_resolves_later() {
    let Some((server, _data)) =
        common::assembled_server("deferred_external_symbol_resolves_later").await
    else {
        return;
    };
    common::spawn_serving(Arc::clone(&server));

    // Before: the external library was never tracked, so its symbols defer.
    let request = literal(FIXTURE_SYMBOL);
    let read_cap = common::read_cap(&server);
    let before: Vec<Scored<Symbol>> = server
        .search_symbols(&read_cap, &request)
        .await
        .expect("an empty corpus answers cleanly")
        .try_collect()
        .await
        .expect("the stream yields");
    assert!(
        before.iter().all(|hit| hit.value.name.plain != FIXTURE_SYMBOL),
        "an untracked library's symbol must not resolve yet"
    );

    // Resolve the library: track it and let the pipeline run.
    let package = ensure(&server).await;
    await_stored(&server, package).await;
    let hit = await_text_hit(&server, FIXTURE_SYMBOL, package).await;
    assert_eq!(hit.value.name.plain, FIXTURE_SYMBOL, "the deferred symbol now resolves");
}

/// Track the fixture package, run the full server, and wait for `Stored`.
async fn track_and_await_stored(server: &Arc<Server<common::TestModel>>) -> PackageId {
    common::spawn_serving(Arc::clone(server));
    let package = ensure(server).await;
    await_stored(server, package).await;
    package
}

/// Ensure the fixture package is tracked (the queue worker picks it up).
async fn ensure(server: &Arc<Server<common::TestModel>>) -> PackageId {
    let coordinates = driver::registry::package::Coordinates {
        origin: heart::RegistryOrigin::CratesIo,
        name: driver::registry::package::PackageName::new(heart::Language::Rust, FIXTURE_NAME)
            .expect("fixture names are valid"),
        version: heart::PackageVersion::try_from((heart::Language::Rust, FIXTURE_VERSION))
            .expect("fixture versions are valid"),
    };
    let cap = common::write_cap(server);
    server
        .ensure_initialized(&cap, &coordinates)
        .await
        .expect("the fixture package ensures cleanly")
        .package
}

/// Wait until the pipeline records `Stored` for `package`.
async fn await_stored(server: &Arc<Server<common::TestModel>>, package: PackageId) {
    let stored = probe(PIPELINE_DEADLINE, || async {
        match server.parse_status(package).await.ok()?? {
            ResolutionState::Stored { .. } => Some(()),
            ResolutionState::DeadLettered(failure) | ResolutionState::Failed(failure) => {
                panic!("the pipeline failed: {failure:?}")
            }
            _ => None,
        }
    })
    .await;
    assert!(stored.is_some(), "the pipeline reaches Stored within the deadline");
}

/// Wait until a text search for `name` yields a hit from `package`.
async fn await_text_hit(
    server: &Arc<Server<common::TestModel>>,
    name: &str,
    package: PackageId,
) -> Scored<Symbol> {
    let request = literal(name);
    probe(PIPELINE_DEADLINE, || async {
        let cap = common::read_cap(server);
        let hits: Vec<Scored<Symbol>> = server
            .search_symbols(&cap, &request)
            .await
            .ok()?
            .try_collect()
            .await
            .ok()?;
        hits.into_iter().find(|hit| hit.value.package == package)
    })
    .await
    .expect("the text poller materializes the symbol within the deadline")
}

/// A literal search request for `query`.
fn literal(query: &str) -> driver::search::query::Search<'static> {
    driver::search::query::Search {
        query: driver::search::query::Query::Literal(
            driver::search::query::LiteralQuery::parse(query).expect("fixture queries are valid"),
        ),
        filter: driver::search::query::Filter::default(),
        page: common::page(16),
        _lifetime: std::marker::PhantomData,
    }
}

/// Re-run `check` until it yields or `deadline` passes.
async fn probe<T, F>(deadline: Duration, mut check: impl FnMut() -> F) -> Option<T>
where
    F: Future<Output = Option<T>>,
{
    let started = Instant::now();
    loop {
        if let Some(value) = check().await {
            return Some(value);
        }
        if started.elapsed() > deadline {
            return None;
        }
        tokio::time::sleep(PROBE_INTERVAL).await;
    }
}
