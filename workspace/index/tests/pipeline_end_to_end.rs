#![cfg(feature = "server")]
//! Pipeline part: **end-to-end** (compile → blob → index → search).
//!
//! The integration spec that ties every subsystem together: tracking a package
//! should make its symbols searchable across all three surfaces. Each test
//! drives the server in-process (no HTTP) but with its real background
//! pollers running (`server_common::spawn_pollers`) against the live stack and
//! real network acquisition — opt-in via `SERVER_TEST_BACKENDS`.

mod server_common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use heart::{PackageId, ResolutionState, Scored, Symbol};
use index::ecosystem::PackageNameExt as _;
use index::server::registry::coordination::{OutboxSeq, SinkKind};
use index::server::{BackgroundWorkers, Server};

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
    let (server, _data) =
        server_common::required_assembled_server("tracked_package_is_text_searchable").await;
    let (package, _pollers) = track_and_await_stored(&server).await;

    let hit = await_text_hit(&server, FIXTURE_SYMBOL, package).await;
    assert_eq!(
        hit.value.package, package,
        "the hit carries the tracked package's identity"
    );
    assert_eq!(hit.value.ecosystem, heart::Language::Rust);
}

/// The same package is reachable via semantic search.
///
/// Assert: a natural-language query surfaces the package's symbols via the
///   vector store (through the explicit semantic opt-in surface).
#[tokio::test]
async fn tracked_package_is_semantically_searchable() {
    let (server, _data) =
        server_common::required_assembled_server("tracked_package_is_semantically_searchable")
            .await;
    let (package, _pollers) = track_and_await_stored(&server).await;
    // Text visibility first: the cheap proxy that fan-out has caught up.
    await_text_hit(&server, FIXTURE_SYMBOL, package).await;

    let request = index::server::search::Search {
        query: index::server::search::Query::Abstract(
            index::server::search::AbstractQuery::NaturalLanguage(
                "a value that is one of two alternatives, left or right".into(),
            ),
        ),
        filter: index::server::search::Filter::default(),
        page: server_common::page(16),
        _lifetime: std::marker::PhantomData,
    };
    let found = probe(PIPELINE_DEADLINE, || async {
        let cap = server_common::read_cap(&server);
        let answer = server.search_symbols(&cap, &request).await.ok()?;
        let hits: Vec<Scored<Symbol>> = server_common::collect_symbol_answer(answer).await.ok()?;
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
    let (server, _data) =
        server_common::required_assembled_server("tracked_package_is_graph_expandable").await;
    let (package, _pollers) = track_and_await_stored(&server).await;
    let hit = await_text_hit(&server, FIXTURE_SYMBOL, package).await;

    let related = probe(PIPELINE_DEADLINE, || async {
        let cap = server_common::read_cap(&server);
        server.expand(&cap, &hit).await.ok()
    })
    .await
    .expect("expansion answers once the graph sink has materialized");
    assert!(
        related
            .iter()
            .all(|neighbour| neighbour.value.package == package),
        "the fixture crate's neighbourhood stays inside the fixture crate"
    );
}

/// The package is parsed once and fanned out to every sink.
///
/// Assert: after one sync, Vector/Graph each received (and consumed) exactly
///   one projection intent, and Text received exactly two.
///
/// # Why Text is two, not one (unlike Vector/Graph)
///
/// This used to assert exactly one intent for every sink, including Text.
/// That was itself the "lexical search is permanently empty for every newly
/// ingested package" defect wearing a green test: the *only* Text intent a
/// freshly registered package ever got was the one `store::apply::apply_run`
/// emits at `CatalogOp::UpsertVersion` — registration time, before any
/// symbol exists. `runtime::text::poll::Poller::poll_once` consumed it,
/// found zero symbols, and advanced the watermark past it forever; nothing
/// ever re-signalled Text once symbols actually landed. This test's own
/// "one parse leaves exactly one intent" assertion was, unintentionally,
/// exercising exactly that bug and calling it correct.
///
/// The fix (`coordination::Outbox::record_stored`) makes the terminal
/// `Stored` transition emit a Text intent too — symmetric with the
/// pre-existing Vector/Graph emits there, and never a no-op because symbols
/// genuinely exist by `Stored` time. So Text now gets two: the
/// registration-time one (still emitted — it is the right signal for a
/// metadata-only update that never touches `Stored` again) and the
/// `Stored`-time one (the one that actually carries symbols). See the fast,
/// non-gated unit-level regression for this exact sequence:
/// `tests/text_sink_recovers_after_stored.rs`.
#[tokio::test]
async fn package_is_parsed_once_and_fanned_out() {
    let (server, _data) =
        server_common::required_assembled_server("package_is_parsed_once_and_fanned_out").await;
    let (package, _pollers) = track_and_await_stored(&server).await;

    for (sink, expected_intents) in [
        (SinkKind::Text, 2),
        (SinkKind::Vector, 1),
        (SinkKind::Graph, 1),
    ] {
        let intents = server
            .outbox()
            .read_since(sink, OutboxSeq(0), 1024)
            .await
            .expect("the outbox answers");
        let for_package: Vec<_> = intents
            .into_iter()
            .filter(|entry| entry.package == package)
            .collect();
        assert_eq!(
            for_package.len(),
            expected_intents,
            "one parse must leave exactly {expected_intents} {sink:?} intent(s) — \
             never zero, never a silently-stuck-at-one-empty-intent regression"
        );

        // And each sink's poller consumes every intent: the watermark passes
        // the last (highest-sequence) one.
        let last_intent = for_package
            .iter()
            .map(|entry| entry.id)
            .max()
            .expect("non-empty");
        let consumed = probe(PIPELINE_DEADLINE, || async {
            let watermark = server.outbox().read_watermark(sink).await.ok()?;
            (watermark >= last_intent).then_some(())
        })
        .await;
        assert!(
            consumed.is_some(),
            "the {sink:?} poller consumes every fan-out intent, including the last"
        );
    }
}

/// A deferred external-library symbol becomes searchable after its lib resolves.
///
/// Assert: a symbol of an as-yet-untracked library is unfindable (deferred),
///   then becomes searchable once that library is registered and synced.
#[tokio::test]
async fn deferred_external_symbol_resolves_later() {
    let (server, _data) =
        server_common::required_assembled_server("deferred_external_symbol_resolves_later").await;
    let _pollers = server_common::spawn_pollers(&server);

    // Before: the external library was never tracked, so its symbols defer.
    let request = literal(FIXTURE_SYMBOL);
    let read_cap = server_common::read_cap(&server);
    let answer = server
        .search_symbols(&read_cap, &request)
        .await
        .expect("an empty corpus answers cleanly");
    let before: Vec<Scored<Symbol>> = server_common::collect_symbol_answer(answer)
        .await
        .expect("the stream yields");
    assert!(
        before
            .iter()
            .all(|hit| hit.value.name.plain != FIXTURE_SYMBOL),
        "an untracked library's symbol must not resolve yet"
    );

    // Resolve the library: track it and let the pipeline run.
    let package = ensure(&server).await;
    await_stored(&server, package).await;
    let hit = await_text_hit(&server, FIXTURE_SYMBOL, package).await;
    assert_eq!(
        hit.value.name.plain, FIXTURE_SYMBOL,
        "the deferred symbol now resolves"
    );
}

/// Track the fixture package, start the pollers, and wait for `Stored`.
///
/// Returns the poller guard alongside the package id: the caller must hold it
/// for as long as it still needs materialization to keep happening (most
/// callers go on to probe `search_symbols`/`expand`, which only see results
/// once the pollers that are consuming the outbox have run) and let it drop
/// only once the test is done — dropping it immediately would stop the
/// pollers right after `Stored`, before anything downstream materializes.
async fn track_and_await_stored(
    server: &Arc<Server<server_common::TestModel>>,
) -> (PackageId, BackgroundWorkers) {
    let pollers = server_common::spawn_pollers(server);
    let package = ensure(server).await;
    await_stored(server, package).await;
    (package, pollers)
}

/// Ensure the fixture package is tracked (the queue worker picks it up).
async fn ensure(server: &Arc<Server<server_common::TestModel>>) -> PackageId {
    let coordinates = index::server::registry::package::Coordinates {
        origin: heart::RegistryOrigin::CratesIo,
        name: index::server::registry::package::PackageName::new(
            heart::Language::Rust,
            FIXTURE_NAME,
        )
        .expect("fixture names are valid"),
        version: heart::PackageVersion::try_from((heart::Language::Rust, FIXTURE_VERSION))
            .expect("fixture versions are valid"),
    };
    let cap = server_common::write_cap(server);
    server
        .ensure_initialized(&cap, &coordinates)
        .await
        .expect("the fixture package ensures cleanly")
        .package
}

/// Wait until the pipeline records `Stored` for `package`.
async fn await_stored(server: &Arc<Server<server_common::TestModel>>, package: PackageId) {
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
    assert!(
        stored.is_some(),
        "the pipeline reaches Stored within the deadline"
    );
}

/// Wait until a text search for `name` yields a hit from `package`.
async fn await_text_hit(
    server: &Arc<Server<server_common::TestModel>>,
    name: &str,
    package: PackageId,
) -> Scored<Symbol> {
    let request = literal(name);
    probe(PIPELINE_DEADLINE, || async {
        let cap = server_common::read_cap(server);
        let answer = server.search_symbols(&cap, &request).await.ok()?;
        let hits: Vec<Scored<Symbol>> = server_common::collect_symbol_answer(answer).await.ok()?;
        hits.into_iter().find(|hit| hit.value.package == package)
    })
    .await
    .expect("the text poller materializes the symbol within the deadline")
}

/// A literal search request for `query`.
fn literal(query: &str) -> index::server::search::Search<'static> {
    index::server::search::Search {
        query: index::server::search::Query::Literal(
            index::server::search::LiteralQuery::parse(query).expect("fixture queries are valid"),
        ),
        filter: index::server::search::Filter::default(),
        page: server_common::page(16),
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
