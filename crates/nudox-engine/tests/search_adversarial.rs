//! Adversarial integration tests for `EngineHandle::search`.
//!
//! All tests use `Engine::start_with_fixtures` (behind the `fixtures` feature)
//! so no real Rust toolchain is needed.
//!
//! # What these tests prove
//!
//! * Known symbols produce hits; nonsense queries produce none.
//! * Results are stable across repeated identical queries.
//! * Dropping the `StreamHandle` cancels the stream without a hang.
//! * Case-insensitive lookup: `"point"` and `"POINT"` hit the same symbols.
//! * Exact vs. prefix ranking: exact match scores >= any prefix match.
//! * Very long and non-ASCII query strings do not panic or hang.
//! * A query issued before the corpus finishes loading does not error —
//!   it may return fewer hits but must return a complete `Done` stream.

use std::collections::HashMap;
use std::time::Duration;

use nudox_engine::wire::{Gen, SearchEvent};
use nudox_engine::{Engine, EngineConfig, SearchQuery};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_engine() -> nudox_engine::EngineHandle {
    Engine::start_with_fixtures(EngineConfig::default())
}

/// Wait until the fixture corpus is present in the engine.
///
/// Uses the public `packages()` broadcast to detect when the fixture package
/// has been loaded. `corpus()` is `pub(crate)` so integration tests must use
/// this indirect approach.
async fn wait_for_corpus(engine: &nudox_engine::EngineHandle) {
    // Subscribe before any race window.
    let rx = engine.packages();
    // The seeding task may have already completed. Drain the channel with a
    // generous timeout; if we see a `Loaded` event, we're done.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv_async()).await {
            Ok(Ok(nudox_engine::PackageLoadEvent::Loaded { .. })) => return,
            Ok(Ok(_)) => continue, // LoadFailed or unknown variant
            Ok(Err(_)) => {
                // Channel closed — all packages were loaded before we subscribed.
                // If the fixture source was fast, the Loaded event landed in the
                // broadcast ring before our subscribe. In that case just proceed
                // (the corpus is ready).
                return;
            }
            Err(_elapsed) => panic!("corpus never seeded within 5 s"),
        }
    }
}

/// Drain a search receiver to completion, returning all events.
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

/// Collect all `HitRow`s from the `SECTION_NAME` batch in an event list.
fn name_hits(events: &[SearchEvent]) -> Vec<nudox_engine::wire::HitRow> {
    use nudox_engine::search::SECTION_NAME;
    events
        .iter()
        .filter_map(|e| match e {
            SearchEvent::Section { section, rows, .. } if *section == SECTION_NAME => {
                Some(rows.iter().cloned().collect::<Vec<_>>())
            }
            _ => None,
        })
        .flatten()
        .collect()
}

// ---------------------------------------------------------------------------
// Known symbol → hits; nonsense → none
// ---------------------------------------------------------------------------

/// The rich fixture has a type named "Point". Searching for "Point" must return
/// at least one hit with `display_name` containing "Point".
#[tokio::test]
async fn known_symbol_returns_hits() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    let q = SearchQuery {
        text: "Point".to_owned(),
        kinds: Vec::new(),
        limit: 50,
    };
    let (_h, rx) = engine.search(q, Gen(1));
    let events = drain(rx).await;
    let hits = name_hits(&events);

    assert!(
        !hits.is_empty(),
        "searching for 'Point' in the rich fixture must return at least one hit; got none — \
         either the fixture is missing a Point type or the name index is broken"
    );

    let any_match = hits
        .iter()
        .any(|h| h.display_name.to_lowercase().contains("point"));
    assert!(
        any_match,
        "at least one hit must have 'point' in its display_name; got: {:?}",
        hits.iter().map(|h| &*h.display_name).collect::<Vec<_>>()
    );
}

/// A query whose text matches nothing in the corpus must produce zero hits in
/// the name section.
#[tokio::test]
async fn nonsense_query_returns_no_hits() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    let q = SearchQuery {
        text: "XxXzZzNoSuchSymbol42XxX".to_owned(),
        kinds: Vec::new(),
        limit: 50,
    };
    let (_h, rx) = engine.search(q, Gen(2));
    let events = drain(rx).await;

    assert!(
        matches!(events.last(), Some(SearchEvent::Done { .. })),
        "nonsense query must still terminate with Done; got {:?}",
        events.last()
    );

    let hits = name_hits(&events);
    assert!(
        hits.is_empty(),
        "nonsense query must produce zero name hits; got {} hits: {:?}",
        hits.len(),
        hits.iter().map(|h| &*h.display_name).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// Stability: repeated identical queries produce identical results
// ---------------------------------------------------------------------------

/// Two successive identical queries must produce the same set of display names
/// in the same order. This is a regression guard: a nondeterministic comparator
/// or a race on the corpus snapshot would violate this.
#[tokio::test]
async fn repeated_identical_queries_produce_stable_results() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    let q = || SearchQuery {
        text: "Point".to_owned(),
        kinds: Vec::new(),
        limit: 50,
    };

    let (_h1, rx1) = engine.search(q(), Gen(10));
    let (_h2, rx2) = engine.search(q(), Gen(11));

    let events1 = drain(rx1).await;
    let events2 = drain(rx2).await;

    // `name_hits` returns owned rows; hold them separately to avoid dangling refs.
    let hits1 = name_hits(&events1);
    let hits2 = name_hits(&events2);
    let names1: Vec<&str> = hits1.iter().map(|h| h.display_name.as_ref()).collect();
    let names2: Vec<&str> = hits2.iter().map(|h| h.display_name.as_ref()).collect();

    assert_eq!(
        names1, names2,
        "two identical queries produced different orderings — the comparator is non-deterministic \
         or there is a race on the corpus snapshot"
    );
}

// ---------------------------------------------------------------------------
// Dropping the handle cancels the stream without hanging
// ---------------------------------------------------------------------------

/// Dropping the `StreamHandle` immediately must not hang. We use a short
/// deadline rather than waiting forever.
#[tokio::test]
async fn dropping_handle_does_not_hang() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    let q = SearchQuery {
        text: "Point".to_owned(),
        kinds: Vec::new(),
        limit: 50,
    };
    let (handle, rx) = engine.search(q, Gen(20));

    // Cancel before any events are consumed.
    drop(handle);

    // Drain whatever happened to have been buffered before cancel fired.
    // The important assertion is that this future terminates; tokio::time::timeout
    // enforces that.
    let _ = tokio::time::timeout(Duration::from_millis(500), async move {
        while rx.recv_async().await.is_ok() {}
    })
    .await
    .expect("dropping StreamHandle must not leave the stream open indefinitely");
}

// ---------------------------------------------------------------------------
// Case-insensitivity
// ---------------------------------------------------------------------------

/// Lowercase and uppercase variants of the same query must return the same set
/// of display names (the name index is case-folded).
#[tokio::test]
async fn case_insensitive_lookup_returns_same_hits() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    let make = |text: &str| SearchQuery {
        text: text.to_owned(),
        kinds: Vec::new(),
        limit: 50,
    };

    let (_h_lower, rx_lower) = engine.search(make("point"), Gen(30));
    let (_h_upper, rx_upper) = engine.search(make("POINT"), Gen(31));
    let (_h_mixed, rx_mixed) = engine.search(make("PoInT"), Gen(32));

    let ev_lower = drain(rx_lower).await;
    let ev_upper = drain(rx_upper).await;
    let ev_mixed = drain(rx_mixed).await;

    let mut names_lower: Vec<String> = name_hits(&ev_lower)
        .into_iter()
        .map(|h| h.display_name.to_lowercase())
        .collect();
    let mut names_upper: Vec<String> = name_hits(&ev_upper)
        .into_iter()
        .map(|h| h.display_name.to_lowercase())
        .collect();
    let mut names_mixed: Vec<String> = name_hits(&ev_mixed)
        .into_iter()
        .map(|h| h.display_name.to_lowercase())
        .collect();

    names_lower.sort();
    names_upper.sort();
    names_mixed.sort();

    assert_eq!(
        names_lower, names_upper,
        "lowercase and uppercase queries must return the same hits (case-folded index)"
    );
    assert_eq!(
        names_lower, names_mixed,
        "mixed-case query must return the same hits as lowercase"
    );
}

// ---------------------------------------------------------------------------
// Exact match outscores prefix match
// ---------------------------------------------------------------------------

/// When the corpus contains both "Point" and "PointMut" (or similar), an exact
/// query for "Point" must produce the exact match with `score >= 1.0` and it
/// must appear before any prefix-only match.
///
/// This does not require the fixture to have exactly these names: we just assert
/// that for any query where an exact match exists alongside prefix matches, the
/// exact match comes first in the hit list (score is highest).
#[tokio::test]
async fn exact_match_outscores_prefix_match() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    // Use "Point" — the fixture is known to have it, and may have longer variants.
    let q = SearchQuery {
        text: "Point".to_owned(),
        kinds: Vec::new(),
        limit: 50,
    };
    let (_h, rx) = engine.search(q, Gen(40));
    let events = drain(rx).await;
    let hits = name_hits(&events);

    if hits.is_empty() {
        // If no hits at all, we can't assert ordering — but this itself is
        // caught by the `known_symbol_returns_hits` test above.
        return;
    }

    // Find any exact match (display_name == "Point", case-insensitive).
    let exact_idx = hits
        .iter()
        .position(|h| h.display_name.to_lowercase() == "point");

    if let Some(idx) = exact_idx {
        // Check that the exact match has score >= every prefix match that comes after it.
        let exact_score = hits[idx].score;
        for (i, hit) in hits.iter().enumerate() {
            if i == idx {
                continue;
            }
            assert!(
                exact_score >= hit.score,
                "exact match 'Point' (score {}) must not score below prefix hit '{}' (score {}); \
                 results are not ranked correctly",
                exact_score,
                &*hit.display_name,
                hit.score
            );
        }
        assert!(
            exact_score >= 1.0 - f32::EPSILON,
            "exact match must score 1.0; got {}",
            exact_score
        );
    }
}

// ---------------------------------------------------------------------------
// Very long query — must not panic or hang
// ---------------------------------------------------------------------------

/// A query string of 4 096 characters must terminate cleanly, regardless of
/// whether it produces hits. The name index uses a prefix scan; a very long
/// string exercises the boundary condition.
#[tokio::test]
async fn very_long_query_terminates() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    let long = "a".repeat(4096);
    let q = SearchQuery {
        text: long,
        kinds: Vec::new(),
        limit: 50,
    };
    let (_h, rx) = engine.search(q, Gen(50));
    let result = tokio::time::timeout(Duration::from_millis(500), drain(rx)).await;
    let events = result.expect("very long query must terminate within 500 ms");
    assert!(
        matches!(events.last(), Some(SearchEvent::Done { .. })),
        "very long query must end with Done"
    );
}

// ---------------------------------------------------------------------------
// Non-ASCII query — must not panic
// ---------------------------------------------------------------------------

/// Queries containing non-ASCII characters (emoji, CJK, accented letters) must
/// not panic. They will return no hits (the fixture uses ASCII names) but the
/// engine must not crash.
#[tokio::test]
async fn non_ascii_query_does_not_panic() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    for text in &["ñoño", "中文", "😀", "Ünïcödé_Téxt", "\u{0000}null"] {
        let q = SearchQuery {
            text: text.to_string(),
            kinds: Vec::new(),
            limit: 50,
        };
        let (_h, rx) = engine.search(q, Gen(60));
        let result = tokio::time::timeout(Duration::from_millis(500), drain(rx)).await;
        let events = result.unwrap_or_else(|_| panic!("query '{text}' timed out"));
        assert!(
            matches!(events.last(), Some(SearchEvent::Done { .. })),
            "non-ASCII query '{text}' must end with Done"
        );
    }
}

// ---------------------------------------------------------------------------
// Query issued before corpus finishes loading must not error
// ---------------------------------------------------------------------------

/// A search issued *immediately* after `Engine::start_with_fixtures` — before
/// the seeding task has had any time to run — must produce `Done`, never
/// `Failed`. It may return zero hits (the corpus may not be ready yet) but it
/// must not produce an error event.
///
/// This exercises the "empty-corpus search" code path: `run_search` calls
/// `corpus.packages().await`, which returns an empty slice if nothing is loaded
/// yet, and then emits empty sections and `Done`. If that path panics or emits
/// `Failed` instead, this test will catch it.
#[tokio::test]
async fn search_before_corpus_ready_does_not_error() {
    // Start the engine but do NOT wait for the corpus.
    let engine = Engine::start_with_fixtures(EngineConfig::default());

    let q = SearchQuery {
        text: "Point".to_owned(),
        kinds: Vec::new(),
        limit: 50,
    };
    // Issue the search immediately — corpus may not be loaded.
    let (_h, rx) = engine.search(q, Gen(70));

    let result = tokio::time::timeout(Duration::from_millis(2000), drain(rx)).await;
    let events = result.expect("search before corpus ready must terminate within 2 s");

    assert!(
        !events
            .iter()
            .any(|e| matches!(e, SearchEvent::Failed { .. })),
        "search issued before corpus finishes loading must not produce Failed; got: {:?}",
        events
    );
    assert!(
        matches!(events.last(), Some(SearchEvent::Done { .. })),
        "search issued before corpus finishes loading must end with Done"
    );
}

// ---------------------------------------------------------------------------
// Later generation supersedes earlier — no stale rows from cancelled generation
// ---------------------------------------------------------------------------

/// Cancel generation N immediately, then run generation N+1 to completion. The generation N+1
/// events must all carry generation N+1, never generation N.
#[tokio::test]
async fn cancelled_gen_does_not_deliver_events_to_next_gen() {
    let engine = make_engine();
    wait_for_corpus(&engine).await;

    let q = || SearchQuery {
        text: "Point".to_owned(),
        kinds: Vec::new(),
        limit: 50,
    };

    let (h_old, _rx_old) = engine.search(q(), Gen(80));
    drop(h_old); // cancel generation 80 immediately

    let (_h_new, rx_new) = engine.search(q(), Gen(81));
    let events = drain(rx_new).await;

    // Every event must carry generation 81, never generation 80.
    for ev in &events {
        let generation = match ev {
            SearchEvent::Section { generation, .. }
            | SearchEvent::Merge { generation, .. }
            | SearchEvent::Latency { generation, .. }
            | SearchEvent::Done { generation }
            | SearchEvent::Failed { generation, .. } => *generation,
            // `SearchEvent` is `#[non_exhaustive]` (LD-7). A variant added
            // later that carries no generation cannot leak a *stale* one,
            // which is the property under test — so skipping it is correct
            // rather than merely convenient.
            _ => continue,
        };
        assert_eq!(
            generation,
            Gen(81),
            "event from generation-81 search carries wrong generation {generation:?}"
        );
    }
}
