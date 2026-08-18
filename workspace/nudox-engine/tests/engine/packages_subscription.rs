//! Tests for `EngineHandle::packages()` — subscribe-then-snapshot ordering,
//! exactly-once delivery, and lag handling.
//!
//! All tests use the `FixtureSource` (behind the `fixtures` feature) so no
//! real language toolchain is needed. The fixture corpus loads synchronously,
//! which means the seeding task may complete before the test calls `packages()`.
//! That is the interesting case: the dedupe loop must surface packages that
//! finished loading before the subscriber arrived.
//!
//! # What these tests prove
//!
//! * **Already-loaded**: a corpus that finishes seeding before `packages()` is
//!   called still delivers all packages to the receiver.
//! * **Exactly-once**: if a package finished loading during the subscribe→
//!   snapshot window (simulated by the fixture source), it appears exactly once,
//!   not zero times and not twice.
//! * **No duplicate on early call**: calling `packages()` immediately after
//!   `Engine::start` (while the corpus may still be loading) returns each
//!   package exactly once even if it lands in both the snapshot and the live
//!   channel.

use nudox_engine::{Engine, EngineConfig, PackageLoadEvent};

// ---------------------------------------------------------------------------
// Helper: collect all events from a packages() receiver, stopping after `n`
// events or after the channel closes.  We use a timeout to avoid hanging the
// test suite if the channel never yields `n` events.
// ---------------------------------------------------------------------------

async fn collect_n_or_close(
    rx: flume::Receiver<PackageLoadEvent>,
    n: usize,
    timeout_ms: u64,
) -> Vec<PackageLoadEvent> {
    use std::time::Duration;
    use tokio::time::timeout;

    let mut out = Vec::new();
    let deadline = Duration::from_millis(timeout_ms);
    let _ = timeout(deadline, async {
        while out.len() < n {
            match rx.recv_async().await {
                Ok(ev) => out.push(ev),
                Err(_) => break, // channel closed
            }
        }
    })
    .await;
    out
}

// ---------------------------------------------------------------------------
// Test: packages already loaded before packages() is called
// ---------------------------------------------------------------------------

/// When the fixture corpus is fully seeded before `packages()` is subscribed,
/// the receiver must still deliver all loaded packages.
///
/// This exercises the snapshot half of the subscribe-then-snapshot protocol:
/// if only the live broadcast were forwarded, a caller that arrives after
/// seeding completes would receive nothing.
#[tokio::test]
async fn already_loaded_packages_delivered_to_late_subscriber() {
    let config = EngineConfig::default();
    let handle = Engine::start_with_fixtures(config);

    // Give the seeding task time to complete before we subscribe.
    // The fixture source is synchronous and tiny; 200 ms is generous.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let rx = handle.packages();

    // The rich fixture has exactly one package.
    let events = collect_n_or_close(rx, 1, 2000).await;

    assert!(
        !events.is_empty(),
        "packages() must deliver packages that loaded before it was called; \
         received no events (the snapshot path is broken)"
    );

    let loaded: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, PackageLoadEvent::Loaded { .. }))
        .collect();
    assert_eq!(
        loaded.len(),
        1,
        "rich fixture has exactly one package; got {} Loaded events",
        loaded.len()
    );

    if let PackageLoadEvent::Loaded {
        name,
        ecosystem,
        symbol_count,
        ..
    } = &loaded[0]
    {
        // `&**` rather than `.as_ref()`: `SharedStr` is `triomphe::Arc<str>`,
        // which has several `AsRef` impls, so the target type is ambiguous
        // (E0283). These bindings are `&SharedStr` (the match is on a
        // reference), so it takes two derefs to reach `str` and one reborrow
        // to name `&str`.
        assert_eq!(&**name, "nudox-fixture-rich");
        // `cargo`, not a synthetic `fixture` tag: see `FIXTURE_VERSION`'s doc
        // comment in `store/source/fixtures.rs`. `Language` models exactly the
        // seven supported ecosystems, so a fixture claiming an eighth cannot
        // produce a `SymbolHit` and silently vanishes from local search.
        assert_eq!(&**ecosystem, "cargo");
        assert!(
            *symbol_count > 0,
            "symbol_count must be positive; got {symbol_count}"
        );
    }
}

// ---------------------------------------------------------------------------
// Test: exactly-once delivery (no duplicates)
// ---------------------------------------------------------------------------

/// Calling `packages()` must never emit the same package twice, even when the
/// package lands in both the broadcast channel (captured before the snapshot)
/// and the snapshot (taken after subscribe).
///
/// We call `packages()` immediately after `Engine::start` — before seeding has
/// had time to complete — to maximise the chance that a package arrives in the
/// overlap window.  Then we wait long enough for all packages to finish and
/// verify the count.
#[tokio::test]
async fn packages_delivered_exactly_once() {
    let config = EngineConfig::default();
    let handle = Engine::start_with_fixtures(config);

    // Subscribe immediately, before seeding has time to complete.
    let rx = handle.packages();

    // Collect up to 10 events (far more than the fixture has) with a generous
    // timeout.  We use "10 or close" because the channel closes when the
    // seeding task drops the broadcast sender.
    let events = collect_n_or_close(rx, 10, 3000).await;

    // Count Loaded events by name.
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for ev in &events {
        if let PackageLoadEvent::Loaded { name, .. } = ev {
            *counts.entry(name.to_string()).or_insert(0) += 1;
        }
    }

    for (name, count) in &counts {
        assert_eq!(
            *count, 1,
            "package '{name}' appeared {count} times in packages() — \
             the dedup logic has a bug: either snapshot and live both emitted it, \
             or the broadcast replayed old events"
        );
    }

    // Sanity: at least one package must have been loaded.
    assert!(
        !counts.is_empty(),
        "no Loaded events received; either the fixture failed to produce any \
         package or the snapshot + live channel both missed it"
    );
}

// ---------------------------------------------------------------------------
// Test: multiple subscribers each get a complete independent view
// ---------------------------------------------------------------------------

/// Two independent calls to `packages()` must each receive the full set of
/// loaded packages.  Each call returns a fresh `flume` receiver backed by its
/// own reconciler task.
#[tokio::test]
async fn multiple_subscribers_each_see_all_packages() {
    let config = EngineConfig::default();
    let handle = Engine::start_with_fixtures(config);

    // Give seeding time to complete so both subscribers hit the snapshot path.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let rx1 = handle.packages();
    let rx2 = handle.packages();

    let (events1, events2) = tokio::join!(
        collect_n_or_close(rx1, 1, 2000),
        collect_n_or_close(rx2, 1, 2000),
    );

    let loaded1 = events1
        .iter()
        .filter(|e| matches!(e, PackageLoadEvent::Loaded { .. }))
        .count();
    let loaded2 = events2
        .iter()
        .filter(|e| matches!(e, PackageLoadEvent::Loaded { .. }))
        .count();

    assert_eq!(
        loaded1, 1,
        "subscriber 1 must see exactly one Loaded event; got {loaded1}"
    );
    assert_eq!(
        loaded2, 1,
        "subscriber 2 must see exactly one Loaded event; got {loaded2}"
    );
}

// ---------------------------------------------------------------------------
// Test: start_with_producer constructor compiles and emits type-correct events
// ---------------------------------------------------------------------------

/// `Engine::start_with_producer` must accept a `Vec<PackageSpec>` and the
/// returned handle must yield a `packages()` receiver — this test is a
/// compile-time and shape check, not a full integration test (which would
/// require a real Rust workspace on disk).
///
/// We pass an empty package list so no actual producer work happens.
#[tokio::test]
async fn start_with_producer_is_callable_with_empty_list() {
    use nudox_engine::{PackageSpec, ProducerLanguage};

    let config = EngineConfig::default();
    let handle = Engine::start_with_producer(config, vec![]);

    let rx = handle.packages();

    // With no packages, no Loaded events arrive; the channel closes when the
    // seeding task finishes (immediately, since the list is empty).
    let events = collect_n_or_close(rx, 10, 500).await;

    // No packages means no events — but this must not panic or deadlock.
    let loaded = events
        .iter()
        .filter(|e| matches!(e, PackageLoadEvent::Loaded { .. }))
        .count();
    assert_eq!(
        loaded, 0,
        "empty package list must produce zero Loaded events; got {loaded}"
    );

    // Silence the "unused variable" lint for the PackageSpec shape test below.
    let _spec = PackageSpec {
        root: std::path::PathBuf::from("/tmp/does-not-exist"),
        name: "test-crate".to_owned(),
        version: "0.1.0".to_owned(),
        language: ProducerLanguage::Rust,
    };
}
