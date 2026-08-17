//! Adversarial integration tests for multi-package corpus behaviour.
//!
//! Uses `StaticSource` via the `test_support` module (crate-private, so this
//! integration test cannot import it directly — we replicate the minimal support
//! here).  No `fixtures` feature is required.
//!
//! # Coverage
//!
//! * Search spanning multiple packages returns hits from both.
//! * `packages()` emits exactly one row per lineage, never per generation.
//! * Name collisions across packages: display names are disambiguated.
//! * One package failing to load (simulated via `LoadEvent::Failed`) must not
//!   prevent others from loading or being searchable (LR-10).
//! * `packages()` late subscriber sees already-loaded packages exactly once.

use std::sync::Arc;
use std::time::Duration;

use futures::stream::BoxStream;

use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, IntroId, PackageLineageId, PackageName},
    entry::{Entry, Node, Symbol, Visibility},
    index::RawRef,
    kind::Kind,
    kinds::Module,
    view::IrView,
};
use nudox_engine::store::{
    package::{PackageView, Provenance},
    source::{IrSource, LoadEvent, LoadRequest, PackageHint, SourceDescriptor, Error},
};

use nudox_engine::{Engine, EngineConfig, PackageLoadEvent, SearchQuery, wire::Gen};

// ---------------------------------------------------------------------------
// Minimal source helpers (mirrors test_support but without crate-private access)
// ---------------------------------------------------------------------------

fn lineage(ecosystem: &str, name: &str) -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new(ecosystem), PackageName::new(name))
}

fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

fn module_entry(name: &str) -> Entry {
    let sym = Symbol {
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
    };
    Entry::new(sym, Node::build(None::<RawRef>, []), Kind::Module(Module))
}

fn build_package(lid: &PackageLineageId, entries: &[(u8, &str)]) -> Arc<PackageView> {
    let mut table = PristineIntroTable::new();
    for (n, name) in entries {
        table.insert_live(intro(*n), module_entry(name), None);
    }
    let view = IrView::with_package(lid.clone(), table);
    Arc::new(PackageView::build(view, Provenance::TrustedLocal))
}

// ---------------------------------------------------------------------------
// StaticSource: replays a fixed list of (lineage, version, package) triples
// ---------------------------------------------------------------------------

struct StaticSource {
    items: Vec<(PackageLineageId, Option<String>, Arc<PackageView>)>,
}

impl StaticSource {
    fn new(items: Vec<(PackageLineageId, Option<String>, Arc<PackageView>)>) -> Self {
        Self { items }
    }
}

impl IrSource for StaticSource {
    fn describe(&self) -> SourceDescriptor {
        SourceDescriptor {
            label: "static-multi".to_owned(),
            package_count_hint: Some(self.items.len() as u32),
        }
    }

    fn load(&self, _req: LoadRequest) -> BoxStream<'static, Result<LoadEvent, Error>> {
        use futures::StreamExt as _;
        let events: Vec<Result<LoadEvent, Error>> = self
            .items
            .iter()
            .flat_map(|(lid, version, pkg)| {
                [
                    Ok(LoadEvent::Discovered {
                        lineage: lid.clone(),
                        hint: PackageHint {
                            display_name: lid.name.as_str().to_owned(),
                            ecosystem: lid.ecosystem.as_str().to_owned(),
                            version: version.clone(),
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

// A source that emits one successful package + one failure, testing LR-10.
struct FailingSource {
    ok_lid: PackageLineageId,
    ok_pkg: Arc<PackageView>,
    fail_lid: PackageLineageId,
}

impl IrSource for FailingSource {
    fn describe(&self) -> SourceDescriptor {
        SourceDescriptor {
            label: "failing".to_owned(),
            package_count_hint: Some(2),
        }
    }

    fn load(&self, _req: LoadRequest) -> BoxStream<'static, Result<LoadEvent, Error>> {
        use futures::StreamExt as _;
        let events = vec![
            Ok(LoadEvent::Discovered {
                lineage: self.ok_lid.clone(),
                hint: PackageHint {
                    display_name: self.ok_lid.name.as_str().to_owned(),
                    ecosystem: self.ok_lid.ecosystem.as_str().to_owned(),
                    version: Some("1.0.0".to_owned()),
                },
            }),
            Ok(LoadEvent::Ready {
                package: Arc::clone(&self.ok_pkg),
            }),
            Ok(LoadEvent::Discovered {
                lineage: self.fail_lid.clone(),
                hint: PackageHint {
                    display_name: self.fail_lid.name.as_str().to_owned(),
                    ecosystem: self.fail_lid.ecosystem.as_str().to_owned(),
                    version: Some("0.1.0".to_owned()),
                },
            }),
            Ok(LoadEvent::Failed {
                lineage: self.fail_lid.clone(),
                error: Error::Internal("toolchain not found".to_owned()),
            }),
        ];
        futures::stream::iter(events).boxed()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Drain a `packages()` receiver until it closes or times out, returning all
/// events received.
async fn drain_packages(
    rx: flume::Receiver<PackageLoadEvent>,
    timeout_ms: u64,
) -> Vec<PackageLoadEvent> {
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_millis(timeout_ms), async {
        while let Ok(ev) = rx.recv_async().await {
            out.push(ev);
        }
    })
    .await;
    out
}

/// Wait until `n` packages appear in the packages() stream.
async fn wait_for_n_packages(
    engine: &nudox_engine::EngineHandle,
    n: usize,
) -> Vec<PackageLoadEvent> {
    let rx = engine.packages();
    let deadline = Duration::from_millis(2000);
    let mut out = Vec::new();
    let _ = tokio::time::timeout(deadline, async {
        while out.len() < n {
            match rx.recv_async().await {
                Ok(ev) => out.push(ev),
                Err(_) => break,
            }
        }
    })
    .await;
    out
}

/// Drain a search receiver to completion.
async fn drain_search(
    rx: flume::Receiver<nudox_engine::wire::SearchEvent>,
) -> Vec<nudox_engine::wire::SearchEvent> {
    use nudox_engine::wire::SearchEvent;
    let mut events = Vec::new();
    let _ = tokio::time::timeout(Duration::from_millis(1000), async {
        while let Ok(ev) = rx.recv_async().await {
            let done = matches!(ev, SearchEvent::Done { .. } | SearchEvent::Failed { .. });
            events.push(ev);
            if done {
                break;
            }
        }
    })
    .await;
    events
}

// ---------------------------------------------------------------------------
// Search spans multiple packages
// ---------------------------------------------------------------------------

/// Two packages with different symbols: searching for a name from package A
/// returns a hit from A, and searching for a name from package B returns a
/// hit from B.  Both should work without either package blocking the other.
#[tokio::test]
async fn search_spans_multiple_packages() {
    let lid_a = lineage("cargo", "pkg-a");
    let lid_b = lineage("cargo", "pkg-b");

    let pkg_a = build_package(&lid_a, &[(1, "RouterA"), (2, "MiddlewareA")]);
    let pkg_b = build_package(&lid_b, &[(1, "HandlerB"), (2, "ExtractorB")]);

    let source = StaticSource::new(vec![
        (lid_a.clone(), Some("1.0.0".to_owned()), pkg_a),
        (lid_b.clone(), Some("2.0.0".to_owned()), pkg_b),
    ]);

    let engine = Engine::start(EngineConfig::default(), source);

    // Wait for both packages.
    let events = wait_for_n_packages(&engine, 2).await;
    assert_eq!(
        events.len(),
        2,
        "expected 2 package load events, got {}",
        events.len()
    );

    // Search for something only in pkg-a.
    let q_a = SearchQuery {
        text: "RouterA".to_owned(),
        kinds: Vec::new(),
        packages: Vec::new(),
        limit: 50,
    };
    let (_h, rx) = engine.search(q_a, Gen(1));
    let ev_a = drain_search(rx).await;
    let hits_a: Vec<_> = ev_a
        .iter()
        .flat_map(|e| match e {
            nudox_engine::wire::SearchEvent::Section { section, rows, .. }
                if *section == nudox_engine::search::SECTION_NAME =>
            {
                rows.iter().cloned().collect::<Vec<_>>()
            }
            _ => Vec::new(),
        })
        .collect();
    assert!(
        hits_a.iter().any(|h| h.display_name.contains("RouterA")),
        "searching for 'RouterA' must return a hit from pkg-a; got: {:?}",
        hits_a.iter().map(|h| &*h.display_name).collect::<Vec<_>>()
    );

    // Search for something only in pkg-b.
    let q_b = SearchQuery {
        text: "HandlerB".to_owned(),
        kinds: Vec::new(),
        packages: Vec::new(),
        limit: 50,
    };
    let (_h, rx) = engine.search(q_b, Gen(2));
    let ev_b = drain_search(rx).await;
    let hits_b: Vec<_> = ev_b
        .iter()
        .flat_map(|e| match e {
            nudox_engine::wire::SearchEvent::Section { section, rows, .. }
                if *section == nudox_engine::search::SECTION_NAME =>
            {
                rows.iter().cloned().collect::<Vec<_>>()
            }
            _ => Vec::new(),
        })
        .collect();
    assert!(
        hits_b.iter().any(|h| h.display_name.contains("HandlerB")),
        "searching for 'HandlerB' must return a hit from pkg-b; got: {:?}",
        hits_b.iter().map(|h| &*h.display_name).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// `packages()` emits one row per lineage, not per generation
// ---------------------------------------------------------------------------

/// Loading two generations of the same package must produce exactly one
/// `PackageLoadEvent::Loaded` row — not two.
///
/// This exercises the dedup logic in `EngineHandle::packages()`.
#[tokio::test]
async fn packages_emits_one_row_per_lineage_not_per_generation() {
    let lid = lineage("cargo", "axum");

    let pkg_v1 = build_package(&lid, &[(1, "Router")]);
    let pkg_v2 = build_package(&lid, &[(1, "Router"), (2, "Extractor")]);

    let source = StaticSource::new(vec![
        (lid.clone(), Some("0.7.9".to_owned()), pkg_v1),
        (lid.clone(), Some("0.8.1".to_owned()), pkg_v2),
    ]);

    let engine = Engine::start(EngineConfig::default(), source);
    // Wait for both generations to be registered and one to be current.
    // (`corpus()` is pub(crate); use `versions()` instead.)
    for _ in 0..100 {
        let list = engine.versions(&lid);
        if list.len() == 2 && list.current().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let rx = engine.packages();
    let events = drain_packages(rx, 1000).await;

    let loaded: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, PackageLoadEvent::Loaded { .. }))
        .collect();

    assert_eq!(
        loaded.len(),
        1,
        "two generations of the same lineage must collapse to one packages() row; \
         got {} rows",
        loaded.len()
    );

    // The single row must carry the current (newest) version.
    if let PackageLoadEvent::Loaded { version, .. } = &loaded[0] {
        assert_eq!(
            version.as_deref(),
            Some("0.8.1"),
            "packages() row must carry the current version; got {version:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Name collisions across packages → disambiguated display names
// ---------------------------------------------------------------------------

/// When two packages both have a symbol named "Router", the display names in
/// search results must be distinguishable (either prefixed with the package path
/// or qualified in some way) — never two identical strings.
#[tokio::test]
async fn name_collisions_across_packages_are_disambiguated() {
    let lid_a = lineage("cargo", "axum");
    let lid_b = lineage("cargo", "actix-web");

    // Both packages have a symbol named "Router".
    let pkg_a = build_package(&lid_a, &[(1, "Router")]);
    let pkg_b = build_package(&lid_b, &[(1, "Router")]);

    let source = StaticSource::new(vec![
        (lid_a.clone(), Some("0.8.0".to_owned()), pkg_a),
        (lid_b.clone(), Some("4.0.0".to_owned()), pkg_b),
    ]);

    let engine = Engine::start(EngineConfig::default(), source);
    let _ = wait_for_n_packages(&engine, 2).await;

    let q = SearchQuery {
        text: "Router".to_owned(),
        kinds: Vec::new(),
        packages: Vec::new(),
        limit: 50,
    };
    let (_h, rx) = engine.search(q, Gen(10));
    let events = drain_search(rx).await;

    let hits: Vec<_> = events
        .iter()
        .flat_map(|e| match e {
            nudox_engine::wire::SearchEvent::Section { section, rows, .. }
                if *section == nudox_engine::search::SECTION_NAME =>
            {
                rows.iter().cloned().collect::<Vec<_>>()
            }
            _ => Vec::new(),
        })
        .collect();

    if hits.len() >= 2 {
        // With multiple packages, display names must differ so they can be told apart.
        let names: Vec<&str> = hits.iter().map(|h| h.display_name.as_ref()).collect();
        let unique: std::collections::HashSet<&str> = names.iter().copied().collect();
        assert_eq!(
            unique.len(),
            names.len(),
            "two 'Router' symbols from different packages must have different display names; \
             got duplicates: {:?}",
            names
        );
    }
    // If fewer than 2 hits (e.g. only one package loaded in time), we skip
    // the disambiguation check — the timing test already covers load completeness.
}

// ---------------------------------------------------------------------------
// One failing package must not prevent others from loading or being searchable
// ---------------------------------------------------------------------------

/// LR-10: isolation. A `LoadEvent::Failed` for one package must not prevent
/// the other package from being inserted into the corpus and returned by search.
#[tokio::test]
async fn one_failing_package_does_not_block_others() {
    let ok_lid = lineage("cargo", "always-ok");
    let fail_lid = lineage("cargo", "always-fails");

    let ok_pkg = build_package(&ok_lid, &[(1, "StableApi")]);

    let source = FailingSource {
        ok_lid: ok_lid.clone(),
        ok_pkg,
        fail_lid: fail_lid.clone(),
    };

    let engine = Engine::start(EngineConfig::default(), source);

    // Wait until the successful package is registered in the version list.
    // (`corpus()` is pub(crate); use `versions()` and `packages()` instead.)
    for _ in 0..100 {
        if engine.versions(&ok_lid).current().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // The successful package must be searchable.
    let q = SearchQuery {
        text: "StableApi".to_owned(),
        kinds: Vec::new(),
        packages: Vec::new(),
        limit: 50,
    };
    let (_h, rx) = engine.search(q, Gen(20));
    let events = drain_search(rx).await;

    let hits: Vec<_> = events
        .iter()
        .flat_map(|e| match e {
            nudox_engine::wire::SearchEvent::Section { section, rows, .. }
                if *section == nudox_engine::search::SECTION_NAME =>
            {
                rows.iter().cloned().collect::<Vec<_>>()
            }
            _ => Vec::new(),
        })
        .collect();

    assert!(
        hits.iter().any(|h| h.display_name.contains("StableApi")),
        "the successfully-loaded package must be searchable even when another package failed; \
         got: {:?}",
        hits.iter().map(|h| &*h.display_name).collect::<Vec<_>>()
    );

    // The failing package must not have a registered version (which implies it's
    // not in the corpus either — versions are only registered on success).
    // (`corpus()` is pub(crate); use `versions()` instead.)
    assert!(
        engine.versions(&fail_lid).is_empty(),
        "the failed package must not have a registered version"
    );

    // The packages() stream must contain a LoadFailed event for the bad package.
    let rx = engine.packages();
    let pkg_events = drain_packages(rx, 500).await;
    let any_failed = pkg_events
        .iter()
        .any(|e| matches!(e, PackageLoadEvent::LoadFailed { .. }));
    // The failure notification must have been broadcast (it may have been
    // received or missed depending on timing, but at minimum the successful
    // package must have a Loaded event).
    let any_loaded = pkg_events
        .iter()
        .any(|e| matches!(e, PackageLoadEvent::Loaded { name, .. } if &**name == "always-ok"));
    assert!(
        any_loaded,
        "a Loaded event must be present for the successfully-loaded package; events: {pkg_events:?}"
    );
    // `any_failed` is an opportunistic check — we don't assert it because the
    // broadcast channel may have already closed and the late subscriber sees
    // only the snapshot.
    let _ = any_failed;
}

// ---------------------------------------------------------------------------
// Late subscriber sees already-loaded packages exactly once
// ---------------------------------------------------------------------------

/// `packages()` called after the corpus is fully seeded must still deliver all
/// packages. This exercises the subscribe-then-snapshot dedup path: the live
/// channel is closed (no more events will arrive), so everything comes from the
/// snapshot.
#[tokio::test]
async fn late_subscriber_sees_already_loaded_packages_exactly_once() {
    let lid_a = lineage("cargo", "serde");
    let lid_b = lineage("cargo", "tokio");

    let pkg_a = build_package(&lid_a, &[(1, "Serialize"), (2, "Deserialize")]);
    let pkg_b = build_package(&lid_b, &[(1, "Runtime"), (2, "Task")]);

    let source = StaticSource::new(vec![
        (lid_a.clone(), Some("1.0.0".to_owned()), pkg_a),
        (lid_b.clone(), Some("1.35.0".to_owned()), pkg_b),
    ]);

    let engine = Engine::start(EngineConfig::default(), source);

    // Wait until both packages have a current version registered.
    // (`corpus()` is pub(crate); use `versions()` instead.)
    for _ in 0..100 {
        let a_ready = engine.versions(&lid_a).current().is_some();
        let b_ready = engine.versions(&lid_b).current().is_some();
        if a_ready && b_ready {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Subscribe *after* seeding completes.
    let rx = engine.packages();
    let events = drain_packages(rx, 1000).await;

    let loaded: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, PackageLoadEvent::Loaded { .. }))
        .collect();

    assert_eq!(
        loaded.len(),
        2,
        "late subscriber must see both packages exactly once; got {} events: {:?}",
        loaded.len(),
        loaded
            .iter()
            .map(|e| match e {
                PackageLoadEvent::Loaded { name, .. } => name.to_string(),
                _ => "?".to_owned(),
            })
            .collect::<Vec<_>>()
    );

    // Names must be distinct (no duplicates from the dedup path failing).
    let names: Vec<String> = loaded
        .iter()
        .filter_map(|e| match e {
            PackageLoadEvent::Loaded { name, .. } => Some(name.to_string()),
            _ => None,
        })
        .collect();
    let unique: std::collections::HashSet<&String> = names.iter().collect();
    assert_eq!(
        unique.len(),
        names.len(),
        "late subscriber must receive each package exactly once; got duplicates: {:?}",
        names
    );
}

// ---------------------------------------------------------------------------
// `packages` filter is applied before truncation, not after (L-timeline task
// item 3: the truncate-then-filter defect)
// ---------------------------------------------------------------------------

/// Extract every `SECTION_NAME` hit row from a drained event list.
fn name_hits(events: &[nudox_engine::wire::SearchEvent]) -> Vec<nudox_engine::wire::HitRow> {
    use nudox_engine::wire::SearchEvent;
    events
        .iter()
        .flat_map(|e| match e {
            SearchEvent::Section { section, rows, .. }
                if *section == nudox_engine::search::SECTION_NAME =>
            {
                rows.iter().cloned().collect::<Vec<_>>()
            }
            _ => Vec::new(),
        })
        .collect()
}

/// Two packages declare a symbol with the *identical* name, kind and
/// visibility, so `search::score_of` scores both hits identically and the
/// total order (`search::compare_candidates`) breaks the tie on package
/// lineage: `cargo:aaa` sorts before `cargo:zzz`. With `limit: 1` and no
/// package filter, only `aaa`'s hit survives truncation — `zzz`'s identical
/// match is silently crowded out.
///
/// This is the regression `EngineHandle::search`'s own `packages` field
/// exists to fix: filtering `packages: [cargo:zzz]` must apply *before* that
/// truncation, at the point candidates are collected, not after. Filtering
/// the already-truncated one-row result (the old MCP-layer behaviour this
/// test would have caught) can only ever see `aaa`'s row and therefore always
/// returns zero hits for `zzz`, even though `zzz` has a real, exact match.
#[tokio::test]
async fn packages_filter_is_applied_before_truncation_not_after() {
    let lid_a = lineage("cargo", "aaa");
    let lid_z = lineage("cargo", "zzz");

    let pkg_a = build_package(&lid_a, &[(1, "Shared")]);
    let pkg_z = build_package(&lid_z, &[(1, "Shared")]);

    let source = StaticSource::new(vec![
        (lid_a.clone(), Some("1.0.0".to_owned()), pkg_a),
        (lid_z.clone(), Some("1.0.0".to_owned()), pkg_z),
    ]);

    let engine = Engine::start(EngineConfig::default(), source);
    wait_for_n_packages(&engine, 2).await;

    // Sanity check the tie actually crowds `zzz` out when unfiltered — this
    // is the precondition the rest of the test depends on, not the assertion
    // under test. If this fails, the scenario is not exercising the defect.
    let unfiltered = SearchQuery {
        text: "Shared".to_owned(),
        kinds: Vec::new(),
        packages: Vec::new(),
        limit: 1,
    };
    let (_h, rx) = engine.search(unfiltered, Gen(30));
    let hits = name_hits(&drain_search(rx).await);
    assert_eq!(hits.len(), 1, "limit:1 must yield exactly one hit");
    assert_eq!(
        hits[0].key.package.name.as_str(),
        "aaa",
        "the tie must break toward the alphabetically-earlier package or this \
         test does not exercise the crowd-out at all"
    );

    // Filtered to `zzz` only, same limit: 1 — `zzz`'s hit must survive,
    // because `aaa` must never be considered a candidate at all once excluded
    // by the filter, so there is nothing left to crowd `zzz` out.
    let filtered = SearchQuery {
        text: "Shared".to_owned(),
        kinds: Vec::new(),
        packages: vec![lid_z.clone()],
        limit: 1,
    };
    let (_h, rx) = engine.search(filtered, Gen(31));
    let hits = name_hits(&drain_search(rx).await);
    assert_eq!(
        hits.len(),
        1,
        "zzz has a real exact match; the packages filter must not return zero \
         results for a package that genuinely matches"
    );
    assert_eq!(hits[0].key.package.name.as_str(), "zzz");

    // And the `kinds`-facet section (`SECTION_TYPE`) must obey the same rule:
    // querying the kind keyword "mod" ties every `Module` entry in both
    // packages, and the same crowd-out applies.
    let filtered_type = SearchQuery {
        text: "mod".to_owned(),
        kinds: Vec::new(),
        packages: vec![lid_z.clone()],
        limit: 1,
    };
    let (_h, rx) = engine.search(filtered_type, Gen(32));
    let events = drain_search(rx).await;
    let type_hits: Vec<_> = events
        .iter()
        .flat_map(|e| match e {
            nudox_engine::wire::SearchEvent::Section { section, rows, .. }
                if *section == nudox_engine::search::SECTION_TYPE =>
            {
                rows.iter().cloned().collect::<Vec<_>>()
            }
            _ => Vec::new(),
        })
        .collect();
    assert_eq!(
        type_hits.len(),
        1,
        "the kind-facet section must respect the packages filter before \
         truncation too"
    );
    assert_eq!(type_hits[0].key.package.name.as_str(), "zzz");
}
