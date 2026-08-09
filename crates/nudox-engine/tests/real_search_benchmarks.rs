//! Search latency + result-quality measurement for `EngineHandle::search`
//! (`crates/nudox-engine/src/search.rs`), run against a **real**
//! multi-package corpus lowered by the real Rust producer — not hand-typed
//! `StaticSource` fixtures.
//!
//! # Why this file exists
//!
//! Before this file, nothing in the repo measured search latency or proved
//! search result quality (see `search_adversarial.rs`'s own doc comment and
//! the survey this file accompanies). `search_adversarial.rs` proves
//! *protocol* correctness (ordering, cancellation, stability) against the
//! synthetic fixture corpus; it does not touch timing, corpus-size scaling,
//! or whether the *ranking* of results is any good. This file adds both,
//! using real crates already vendored under `.real-crates/` so the corpus,
//! the name collisions, and the module structure are not hand-authored —
//! per AGENTS-DOCTRINE §4, "a hand-authored fixture tests the fixture
//! author's imagination, not the code."
//!
//! # Scope discipline
//!
//! This file only *reads* `nudox-engine` (another agent owns that crate's
//! source right now). No file under `crates/nudox-engine/src/` was edited to
//! produce this test file or its results. Any defect this file's assertions
//! surface is reported in the accompanying task summary, not patched here.
//!
//! # Packages used
//!
//! * `memchr-2.8.3` — the workhorse. `search.rs`'s own committed test
//!   (`real_memchr_leaf_collisions_get_distinct_display_names`) already
//!   proves this crate has real, non-fixture leaf-name collisions between a
//!   public `fn memchr` / `struct Memchr` and several `arch::*::memchr`
//!   modules, some of which are declared `pub(crate)` (crate-private) in the
//!   real source — exactly the "public symbol vs. arbitrary internal module
//!   of the same name" shape this file is asked to test.
//! * `log-0.4.33` — a small, collision-free crate (`Record`, `Metadata`,
//!   `Log`), used as (a) a second real package for corpus-size scaling and
//!   (b) a "clean" contrast case.
//! * `itoa-1.0.18` — a third, tiny real package (`Buffer`), used only to
//!   round out the multi-package corpus for scaling measurements.
//!
//! All three are already present under `.real-crates/` in this checkout, so
//! every test below runs unmodified in this environment; the `Cargo.toml`
//! existence check is kept anyway so the file degrades gracefully (skips,
//! does not panic) on a checkout where they are absent.
//!
//! # Benchmark methodology (AGENTS-DOCTRINE §4)
//!
//! Every timed region is wrapped in `nudox_test_support::measured`, which
//! prints a `cost case=… wall_ms=… rss_bytes=… disk_delta_bytes=…` line. Two
//! *different* timings are reported for the same search, deliberately not
//! conflated:
//!
//! * the `measured(...)` wall-clock envelope — task spawn + channel + drain,
//!   i.e. what a caller actually waits on;
//! * `SearchEvent::Latency.elapsed` — the engine's own `Instant`-measured
//!   compute time for `collect_name_hits`/`collect_type_hits`, read directly
//!   off the wire event, with no channel/task overhead folded in.
//!
//! `PackageIndexes` (the `NameIndex`/`by_kind` structures `search.rs` queries)
//! are built once, eagerly, inside `PackageView::build` — *before* any
//! `PackageView` is published into a corpus (see
//! `crates/nudox-store/src/package.rs`'s module doc: "All mutable index
//! building happens inside `PackageIndexes::build` before the view is
//! wrapped in an `Arc`"). So "cold vs. warm" below does **not** measure index
//! construction — that already happened at load time, off the clock. It
//! measures the cost of the first `EngineHandle::search` dispatch through a
//! freshly-started engine (task scheduling, first touch of the corpus's
//! `RwLock`, page/cache warmth) versus immediately repeated identical
//! queries. This distinction is stated plainly rather than implied, per
//! AGENTS-DOCTRINE §6 ("do not describe intended behaviour as completed
//! behaviour").
//!
//! **Timing caveat, stated up front (AGENTS-DOCTRINE task brief):** this
//! environment runs many agents compiling concurrently, and wall-clock
//! timings taken under that load have been measured with up to a 4.6×
//! swing on identical work. Every `wall_ms`/`elapsed` number reported here
//! is real (pasted from an actual run, not invented), but the *absolute*
//! values should not be read as steady-state numbers; the *relative*
//! comparisons this file makes correctness assertions on (same content,
//! regardless of corpus size; stable repeat queries) are the load-bearing
//! claims, not the millisecond figures.
//!
//! # Running
//!
//! ```text
//! RUSTC_BOOTSTRAP=1 cargo test -p nudox-engine --test real_search_benchmarks \
//!   -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `--test-threads=1` is deliberate: these tests share a small set of
//! `OnceLock`-cached real `PackageView`s (each real crate is lowered through
//! rust-analyzer exactly once, not once per test), and running the timed
//! sections concurrently would have every test's CPU-bound work compete for
//! the same cores, which is exactly the kind of self-inflicted noise the
//! caveat above warns about from *other* agents — no reason to add more of
//! it ourselves.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use futures::stream::BoxStream;

use nudox_ir::change::{IntroId, PackageLineageId};
use nudox_ir::entry::Visibility;
use nudox_ir::kind::KindDiscriminant;
use nudox_ir::view::IrView;
use nudox_producer::produce;
use nudox_producer_rust::RustProducer;
use nudox_store::package::{PackageView, Provenance};
use nudox_store::source::producer::PackageDescriptor;
use nudox_store::source::{IrSource, LoadEvent, LoadRequest, PackageHint, SourceDescriptor, SourceError};

use nudox_engine::search::{SECTION_NAME, SECTION_TYPE};
use nudox_engine::wire::{Gen, HitRow, SearchEvent};
use nudox_engine::{Engine, EngineConfig, PackageLoadEvent, SearchQuery};

// ---------------------------------------------------------------------------
// Real-crate loading, cached once per package for the whole test binary
// ---------------------------------------------------------------------------

fn real_crate_root(name: &str, version: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../.real-crates/{name}-{version}"))
}

/// Lower one real crate through the real Rust producer, exactly like
/// `search.rs`'s own `real_memchr_leaf_collisions_get_distinct_display_names`
/// and `real_producer_links.rs`'s `try_lower_axum`. `direct_repo: false`
/// matches `ProducerRegistry::with_rust_pilot`, the constructor the real app
/// uses (same comment as those two files).
///
/// Wrapped in `nudox_test_support::measured` so the lowering cost itself is
/// on the record (case `"load/real/{name}-{version}"`), even though this
/// file's primary subject is search, not lowering.
fn try_lower(name: &str, version: &str) -> Option<Arc<PackageView>> {
    let root = real_crate_root(name, version);
    if !root.join("Cargo.toml").is_file() {
        eprintln!(
            "SKIP: no {name}-{version} checkout at {}. Run: scripts/fetch-real-crate.sh {name} {version}",
            root.display()
        );
        return None;
    }

    let descriptor = PackageDescriptor::cargo(&root, name, version);
    let (table, cost) = nudox_test_support::measured(
        &format!("load/real/{name}-{version}"),
        &root,
        || {
            produce(
                &RustProducer { direct_repo: false },
                &descriptor.source,
                &descriptor.lineage,
                &nudox_ir::foreign::Unlinked,
            )
            .unwrap_or_else(|err| {
                let mut chain = format!("{err}");
                let mut cursor: &dyn std::error::Error = &err;
                while let Some(source) = std::error::Error::source(cursor) {
                    chain.push_str(&format!("\n  caused by: {source}"));
                    cursor = source;
                }
                panic!("{name}-{version} must lower without error:\n{chain}");
            })
            .table
        },
    );
    eprintln!(
        "lowered {} entries from {name}-{version} in {:.2}s",
        table.len(),
        cost.wall.as_secs_f64()
    );

    let view = IrView::with_package(descriptor.lineage, table);
    Some(Arc::new(PackageView::build(view, Provenance::TrustedLocal)))
}

static MEMCHR: OnceLock<Option<Arc<PackageView>>> = OnceLock::new();
static LOG: OnceLock<Option<Arc<PackageView>>> = OnceLock::new();
static ITOA: OnceLock<Option<Arc<PackageView>>> = OnceLock::new();

fn memchr_pkg() -> Option<Arc<PackageView>> {
    MEMCHR.get_or_init(|| try_lower("memchr", "2.8.3")).clone()
}
fn log_pkg() -> Option<Arc<PackageView>> {
    LOG.get_or_init(|| try_lower("log", "0.4.33")).clone()
}
fn itoa_pkg() -> Option<Arc<PackageView>> {
    ITOA.get_or_init(|| try_lower("itoa", "1.0.18")).clone()
}

// ---------------------------------------------------------------------------
// StaticSource — replays a fixed list of already-built real `PackageView`s.
//
// Same shape as `multi_package_flows.rs`'s `StaticSource`; duplicated here
// (each integration-test binary is compiled separately, so there is no
// shared `tests/common` module in this crate) rather than imported.
// ---------------------------------------------------------------------------

struct StaticSource {
    items: Vec<(PackageLineageId, Option<String>, Arc<PackageView>)>,
}

impl IrSource for StaticSource {
    fn describe(&self) -> SourceDescriptor {
        SourceDescriptor {
            label: "real-static-multi".to_owned(),
            package_count_hint: Some(self.items.len() as u32),
        }
    }

    fn load(&self, _req: LoadRequest) -> BoxStream<'static, Result<LoadEvent, SourceError>> {
        use futures::StreamExt as _;
        let events: Vec<Result<LoadEvent, SourceError>> = self
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

/// Build an `EngineHandle` whose corpus is exactly `packages` — no more, no
/// less — so corpus size is under this file's control rather than an
/// artifact of whatever a fixture source happens to contain.
fn engine_over(packages: &[Arc<PackageView>]) -> nudox_engine::EngineHandle {
    let items = packages
        .iter()
        .map(|p| (p.lineage().clone(), None, Arc::clone(p)))
        .collect();
    Engine::start(EngineConfig::default(), StaticSource { items })
}

async fn wait_for_n_packages(engine: &nudox_engine::EngineHandle, n: usize) {
    let rx = engine.packages();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut seen = 0usize;
    while seen < n {
        match tokio::time::timeout_at(deadline, rx.recv_async()).await {
            Ok(Ok(PackageLoadEvent::Loaded { .. })) => seen += 1,
            Ok(Ok(_)) => continue,
            Ok(Err(_)) => break,
            Err(_) => panic!("corpus of {n} package(s) never fully seeded within 10s"),
        }
    }
}

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

fn section_rows(events: &[SearchEvent], section: nudox_engine::wire::SearchSectionId) -> Vec<HitRow> {
    events
        .iter()
        .filter_map(|e| match e {
            SearchEvent::Section { section: s, rows, .. } if *s == section => {
                Some(rows.iter().cloned().collect::<Vec<_>>())
            }
            _ => None,
        })
        .flatten()
        .collect()
}

fn section_latency(events: &[SearchEvent], section: nudox_engine::wire::SearchSectionId) -> Option<Duration> {
    events.iter().find_map(|e| match e {
        SearchEvent::Latency { section: s, elapsed, .. } if *s == section => Some(*elapsed),
        _ => None,
    })
}

/// Run one search to completion, returning (all events, wall time as
/// measured from outside — spawn + channel + drain).
async fn run_search_timed(
    engine: &nudox_engine::EngineHandle,
    query: SearchQuery,
    generation: Gen,
) -> (Vec<SearchEvent>, Duration) {
    let started = Instant::now();
    let (_handle, rx) = engine.search(query, generation);
    let events = drain(rx).await;
    (events, started.elapsed())
}

/// Ground-truth lookup: resolve a `HitRow` back to its real IR entry's kind
/// discriminant + declared visibility, independent of anything `search.rs`
/// computed. Used so quality assertions check the actual symbol, not just
/// its rendered string.
fn resolve<'a>(
    rows: &'a [HitRow],
    pkg: &PackageView,
) -> Vec<(usize, &'a HitRow, KindDiscriminant, Visibility)> {
    rows.iter()
        .enumerate()
        .filter_map(|(rank, row)| {
            let entry = pkg.view().entry(row.key.intro)?;
            let disc = entry.kind().discriminant()?;
            Some((rank, row, disc, entry.sym().visibility))
        })
        .collect()
}

fn scratch_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "nudox-engine-search-bench-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

// ---------------------------------------------------------------------------
// LATENCY: name search, single real package
// ---------------------------------------------------------------------------

/// Exact-name search against a single real package (`memchr` alone) must
/// complete and return the real `memchr` symbol(s); the search itself is the
/// measured region (AGENTS-DOCTRINE §4).
#[tokio::test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
async fn name_search_latency_single_real_package() {
    let Some(memchr) = memchr_pkg() else { return };
    let engine = engine_over(std::slice::from_ref(&memchr));
    wait_for_n_packages(&engine, 1).await;

    let dir = scratch_dir("name-1pkg");
    let query = SearchQuery {
        text: "memchr".to_owned(),
        kinds: Vec::new(),
            packages: Vec::new(),
        limit: 50,
    };
    let ((events, wall), _cost) = nudox_test_support::measured("search/name/1pkg/memchr", &dir, || {
        futures::executor::block_on(run_search_timed(&engine, query, Gen(1)))
    });

    let rows = section_rows(&events, SECTION_NAME);
    let engine_latency = section_latency(&events, SECTION_NAME);
    eprintln!(
        "name search over 1 real package: {} hits, outer wall={:?}, engine-reported SECTION_NAME latency={:?}",
        rows.len(),
        wall,
        engine_latency
    );

    assert!(
        !rows.is_empty(),
        "searching 'memchr' in the real memchr-2.8.3 corpus must return hits; got none"
    );
    let resolved = resolve(&rows, &memchr);
    assert!(
        resolved
            .iter()
            .any(|(_, _, disc, _)| *disc == KindDiscriminant::Function),
        "expected at least one real Function entry among the 'memchr' hits; got kinds: {:?}",
        resolved.iter().map(|(_, _, d, _)| d).collect::<Vec<_>>()
    );
    assert!(
        engine_latency.is_some(),
        "SECTION_NAME must emit a Latency event"
    );
}

// ---------------------------------------------------------------------------
// LATENCY: type (kind-facet) search, single real package
// ---------------------------------------------------------------------------

/// A kind-keyword query ("fn") against a single real package must complete
/// and return real, named function entries — not just a non-empty count.
#[tokio::test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
async fn type_search_latency_single_real_package() {
    let Some(memchr) = memchr_pkg() else { return };
    let engine = engine_over(std::slice::from_ref(&memchr));
    wait_for_n_packages(&engine, 1).await;

    let dir = scratch_dir("type-1pkg");
    let query = SearchQuery {
        text: "fn".to_owned(),
        kinds: Vec::new(),
            packages: Vec::new(),
        limit: 0, // unlimited: we want the true kind-facet size
    };
    let ((events, wall), _cost) = nudox_test_support::measured("search/type/1pkg/fn", &dir, || {
        futures::executor::block_on(run_search_timed(&engine, query, Gen(2)))
    });

    let rows = section_rows(&events, SECTION_TYPE);
    let engine_latency = section_latency(&events, SECTION_TYPE);
    eprintln!(
        "type search 'fn' over 1 real package: {} hits, outer wall={:?}, engine-reported SECTION_TYPE latency={:?}",
        rows.len(),
        wall,
        engine_latency
    );

    assert!(
        !rows.is_empty(),
        "kind keyword 'fn' must return function hits from the real memchr corpus"
    );
    let resolved = resolve(&rows, &memchr);
    assert!(
        resolved
            .iter()
            .all(|(_, _, disc, _)| *disc == KindDiscriminant::Function),
        "every SECTION_TYPE hit for keyword 'fn' must be a real Function entry; \
         found a non-Function kind: {:?}",
        resolved
            .iter()
            .filter(|(_, _, d, _)| *d != KindDiscriminant::Function)
            .map(|(_, r, d, _)| (&*r.display_name, d))
            .collect::<Vec<_>>()
    );
    // Ground truth for the real name, independent of the search path:
    // `memchr` itself must be one of the Function entries returned.
    let has_memchr_fn = resolved.iter().any(|(_, r, disc, _)| {
        *disc == KindDiscriminant::Function && r.display_name.to_lowercase().contains("memchr")
    });
    assert!(
        has_memchr_fn,
        "expected the type-facet 'fn' query to include a function whose name contains \
         'memchr'; got {} function rows total",
        resolved.len()
    );
}

// ---------------------------------------------------------------------------
// LATENCY: a prefix that matches many symbols
// ---------------------------------------------------------------------------

/// A short, broad prefix ("mem") must return many matches from a real
/// package with a large related-name family (memchr/memchr2/memchr3,
/// Memchr/Memchr2/Memchr3, memmem, plus every architecture backend's
/// internal `memchr` module) — this is the "prefix that matches many
/// symbols" latency case, and the count is a real, ground-truthed number,
/// not an estimate.
#[tokio::test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
async fn prefix_search_many_matches_latency() {
    let Some(memchr) = memchr_pkg() else { return };
    let engine = engine_over(std::slice::from_ref(&memchr));
    wait_for_n_packages(&engine, 1).await;

    let dir = scratch_dir("prefix-many");
    let query = SearchQuery {
        text: "mem".to_owned(),
        kinds: Vec::new(),
            packages: Vec::new(),
        limit: 0, // unlimited: measure the true match volume
    };
    let ((events, wall), _cost) = nudox_test_support::measured("search/prefix/1pkg/mem", &dir, || {
        futures::executor::block_on(run_search_timed(&engine, query, Gen(3)))
    });

    let rows = section_rows(&events, SECTION_NAME);
    let engine_latency = section_latency(&events, SECTION_NAME);
    eprintln!(
        "prefix 'mem' over 1 real package: {} hits, outer wall={:?}, engine-reported latency={:?}",
        rows.len(),
        wall,
        engine_latency
    );

    // Ground truth: independently count how many real, *kind-bearing* entries
    // in the corpus actually case-fold-prefix-match "mem", and require the
    // search to find at least that many distinct IntroIds. `collect_name_hits`
    // deliberately skips entries whose `EntryInner` is `Reference` (a
    // re-export alias pointing at another entry, not an owned `Kind` — see
    // `entry.rs::EntryInner`), so the ground truth must too, or it is not
    // actually the invariant `collect_name_hits` implements. (First draft of
    // this test did not exclude `Reference` entries and found 15 "missing"
    // hits that turned out to be exactly this — re-export aliases for
    // `memchr`/`memchr2`/.../`memrchr3_iter` at the crate root, each with
    // `kind_discriminant=None`. That is real, verified behavior, not a
    // ground-truth bug in the abstract — see
    // `memchr_reexport_aliases_are_invisible_to_name_search` below for the
    // dedicated test and finding.)
    let real_prefix_matches: std::collections::HashSet<IntroId> = memchr
        .view()
        .entries()
        .filter(|(_, e)| {
            e.sym().name.to_lowercase().starts_with("mem") && e.kind().discriminant().is_some()
        })
        .map(|(id, _)| id)
        .collect();

    assert!(
        rows.len() >= 15,
        "prefix 'mem' against real memchr-2.8.3 must return 'many' matches (>=15); got {}. \
         Rows: {:?}",
        rows.len(),
        rows.iter().map(|r| &*r.display_name).collect::<Vec<_>>()
    );

    let found_intros: std::collections::HashSet<IntroId> = rows.iter().map(|r| r.key.intro).collect();
    let missed: Vec<_> = real_prefix_matches.difference(&found_intros).collect();
    if !missed.is_empty() {
        for id in &missed {
            let entry = memchr.view().entry(**id);
            eprintln!(
                "MISSED: intro={:?} name={:?} kind_discriminant={:?} raw_kind_present={}",
                id,
                entry.map(|e| e.sym().name.clone()),
                entry.and_then(|e| e.kind().discriminant()),
                entry.map(|e| e.kind().as_owned_kind().is_some()).unwrap_or(false),
            );
        }
    }
    assert!(
        missed.is_empty(),
        "search missed {} real entries whose name case-fold-prefixes 'mem': {:?}",
        missed.len(),
        missed
    );
}

// ---------------------------------------------------------------------------
// LATENCY: how it scales with corpus size (1 real package vs. several)
// ---------------------------------------------------------------------------

/// The same query against a 1-package corpus and a 3-package real corpus
/// must return *identical* name-section content (searching "memchr" — a
/// name that exists only in the `memchr` package — must not be affected by
/// the presence of unrelated packages). Timing for both is measured and
/// reported; per this file's header, the millisecond figures are informative
/// only, not asserted on, because wall-clock is unreliable under concurrent
/// build load in this environment. What *is* asserted is the scale-invariant
/// correctness property: fan-out over more packages must not change *what*
/// is found, and `search.rs`'s own documented fan-out shape (`for pkg in
/// packages { … }`, no cross-package merged index) predicts that the *work
/// done* scales with package count even when the *hits* do not.
#[tokio::test]
#[ignore = "loads three real Cargo workspaces through rust-analyzer; run with --ignored"]
async fn latency_one_package_vs_several_real_packages() {
    let (Some(memchr), Some(log), Some(itoa)) = (memchr_pkg(), log_pkg(), itoa_pkg()) else {
        return;
    };

    let engine1 = engine_over(std::slice::from_ref(&memchr));
    wait_for_n_packages(&engine1, 1).await;

    let engine3 = engine_over(&[Arc::clone(&memchr), Arc::clone(&log), Arc::clone(&itoa)]);
    wait_for_n_packages(&engine3, 3).await;

    let query = || SearchQuery {
        text: "memchr".to_owned(),
        kinds: Vec::new(),
            packages: Vec::new(),
        limit: 0,
    };

    let dir1 = scratch_dir("scale-1pkg");
    let ((events1, wall1), _c1) = nudox_test_support::measured("search/scale/1pkg/memchr", &dir1, || {
        futures::executor::block_on(run_search_timed(&engine1, query(), Gen(10)))
    });

    let dir3 = scratch_dir("scale-3pkg");
    let ((events3, wall3), _c3) = nudox_test_support::measured("search/scale/3pkg/memchr", &dir3, || {
        futures::executor::block_on(run_search_timed(&engine3, query(), Gen(11)))
    });

    let rows1 = section_rows(&events1, SECTION_NAME);
    let rows3 = section_rows(&events3, SECTION_NAME);
    let lat1 = section_latency(&events1, SECTION_NAME);
    let lat3 = section_latency(&events3, SECTION_NAME);

    eprintln!(
        "corpus size scaling — 1 package: {} hits, outer wall={:?}, engine latency={:?}",
        rows1.len(),
        wall1,
        lat1
    );
    eprintln!(
        "corpus size scaling — 3 packages: {} hits, outer wall={:?}, engine latency={:?}",
        rows3.len(),
        wall3,
        lat3
    );
    eprintln!(
        "NOTE: absolute timings are unreliable under concurrent build load in this \
         environment (observed up to 4.6x swings on identical work elsewhere in this \
         program); only the content-identity assertion below is load-bearing."
    );

    let mut names1: Vec<&str> = rows1.iter().map(|r| r.display_name.as_ref()).collect();
    let mut names3: Vec<&str> = rows3.iter().map(|r| r.display_name.as_ref()).collect();
    names1.sort_unstable();
    names3.sort_unstable();
    assert_eq!(
        names1, names3,
        "adding unrelated packages (log, itoa) to the corpus must not change the \
         'memchr' name-section results — every hit still belongs to the memchr package"
    );
    assert!(!rows1.is_empty(), "sanity: the query must actually hit something");
}

// ---------------------------------------------------------------------------
// LATENCY: first query (cold) vs. repeated identical queries (warm)
// ---------------------------------------------------------------------------

/// The very first search dispatched through a freshly-started engine over an
/// already-loaded 3-package real corpus, versus four immediately repeated
/// identical searches. Per this file's header, `PackageIndexes` are already
/// built before this point (index construction is off the clock); this
/// isolates dispatch/runtime/cache warmth, not index-build cost. All five
/// runs must return identical content — cold vs. warm must be a timing
/// difference only, never a correctness difference.
#[tokio::test]
#[ignore = "loads three real Cargo workspaces through rust-analyzer; run with --ignored"]
async fn first_query_cold_vs_warm_real_corpus() {
    let (Some(memchr), Some(log), Some(itoa)) = (memchr_pkg(), log_pkg(), itoa_pkg()) else {
        return;
    };
    let engine = engine_over(&[memchr, log, itoa]);
    wait_for_n_packages(&engine, 3).await;

    let query = || SearchQuery {
        text: "memchr".to_owned(),
        kinds: Vec::new(),
            packages: Vec::new(),
        limit: 50,
    };

    let cold_dir = scratch_dir("cold");
    let ((cold_events, cold_wall), _c) = nudox_test_support::measured("search/cold/first", &cold_dir, || {
        futures::executor::block_on(run_search_timed(&engine, query(), Gen(20)))
    });
    let cold_names: Vec<String> = section_rows(&cold_events, SECTION_NAME)
        .iter()
        .map(|r| r.display_name.to_string())
        .collect();

    let mut warm_walls = Vec::new();
    for i in 0..4 {
        let dir = scratch_dir("warm");
        let ((events, wall), _c) =
            nudox_test_support::measured(&format!("search/warm/iter{i}"), &dir, || {
                futures::executor::block_on(run_search_timed(&engine, query(), Gen(21 + i)))
            });
        let names: Vec<String> = section_rows(&events, SECTION_NAME)
            .iter()
            .map(|r| r.display_name.to_string())
            .collect();
        assert_eq!(
            names, cold_names,
            "warm iteration {i} returned different content than the cold first query"
        );
        warm_walls.push(wall);
    }

    let warm_mean = warm_walls.iter().sum::<Duration>() / warm_walls.len() as u32;
    eprintln!(
        "cold first query wall={:?}; warm mean over {} repeats={:?}; per-iter={:?}",
        cold_wall,
        warm_walls.len(),
        warm_mean,
        warm_walls
    );
    assert!(!cold_names.is_empty(), "sanity: the query must actually hit something");
}

// ---------------------------------------------------------------------------
// QUALITY: the right symbol is actually returned
// ---------------------------------------------------------------------------

/// Querying "memchr" against the real memchr-2.8.3 corpus must return the
/// real `pub fn memchr` and the real `pub struct Memchr` — verified by
/// resolving each hit back to its real IR entry's kind, not by string
/// matching alone (AGENTS-DOCTRINE §4: "assert on content ... never just
/// is_ok() or a count being non-zero").
#[tokio::test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
async fn memchr_query_returns_the_real_function_and_the_real_struct() {
    let Some(memchr) = memchr_pkg() else { return };
    let engine = engine_over(std::slice::from_ref(&memchr));
    wait_for_n_packages(&engine, 1).await;

    let query = SearchQuery {
        text: "memchr".to_owned(),
        kinds: Vec::new(),
            packages: Vec::new(),
        limit: 0,
    };
    let (events, _wall) = run_search_timed(&engine, query, Gen(30)).await;
    let rows = section_rows(&events, SECTION_NAME);
    let resolved = resolve(&rows, &memchr);

    let has_function = resolved.iter().any(|(_, r, disc, vis)| {
        *disc == KindDiscriminant::Function
            && r.display_name.to_lowercase().contains("memchr")
            && *vis == Visibility::Public
    });
    let has_struct = resolved.iter().any(|(_, r, disc, vis)| {
        *disc == KindDiscriminant::Record
            && r.display_name.to_lowercase().contains("memchr")
            && *vis == Visibility::Public
    });

    assert!(
        has_function,
        "expected a public Function entry named 'memchr' among the hits; got: {:?}",
        resolved
            .iter()
            .map(|(rank, r, d, v)| format!("#{rank} {:?} {:?} {:?}", r.display_name, d, v))
            .collect::<Vec<_>>()
    );
    assert!(
        has_struct,
        "expected a public Record (struct) entry named 'Memchr' among the hits; got: {:?}",
        resolved
            .iter()
            .map(|(rank, r, d, v)| format!("#{rank} {:?} {:?} {:?}", r.display_name, d, v))
            .collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// QUALITY: ranking — public API vs. an arbitrary same-named internal module
// ---------------------------------------------------------------------------

/// The core ranking-quality claim this file was asked to test: querying
/// "memchr" must rank the public `memchr` function (or the public `Memchr`
/// struct) *above* an arbitrary non-public module of the same name — a
/// private "arch::*::memchr" implementation-detail module must not
/// outrank the actual public API surface a user is searching for.
///
/// This is checked against real, ground-truthed data: each hit is resolved
/// back to its real `Kind` + `Visibility` (not inferred from display text),
/// and "rank" is each hit's position in the array `search.rs` actually
/// returns (the same order a client renders top-to-bottom).
///
/// **If this assertion fails, that is a reportable finding about
/// `collect_name_hits`'s scoring, not a bug in this test.** Per the task
/// brief: "Report any query where ranking is visibly wrong. That is a
/// finding, not a failure." See the accompanying report for the mechanism:
/// `collect_name_hits` scores every case-folded exact match at the same
/// 1.0 regardless of kind or visibility, and sorts ties by the pre-
/// qualification leaf string — which does not distinguish "the public
/// function" from "an internal module that happens to share its name."
#[tokio::test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
async fn memchr_query_ranks_public_api_above_same_named_internal_module() {
    let Some(memchr) = memchr_pkg() else { return };
    let engine = engine_over(std::slice::from_ref(&memchr));
    wait_for_n_packages(&engine, 1).await;

    let query = SearchQuery {
        text: "memchr".to_owned(),
        kinds: Vec::new(),
            packages: Vec::new(),
        limit: 0,
    };
    let (events, _wall) = run_search_timed(&engine, query, Gen(40)).await;
    let rows = section_rows(&events, SECTION_NAME);
    let resolved = resolve(&rows, &memchr);

    eprintln!(
        "full ranked result set for 'memchr' ({} rows):\n{}",
        resolved.len(),
        resolved
            .iter()
            .map(|(rank, r, d, v)| format!(
                "  #{rank:>2} score={:.3} kind={:?} vis={:?} name={:?}",
                r.score, d, v, r.display_name
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );

    let best_public_api_rank = resolved
        .iter()
        .filter(|(_, r, disc, vis)| {
            matches!(disc, KindDiscriminant::Function | KindDiscriminant::Record)
                && *vis == Visibility::Public
                && r.display_name.to_lowercase().contains("memchr")
        })
        .map(|(rank, ..)| *rank)
        .min();

    let best_internal_module_rank = resolved
        .iter()
        .filter(|(_, _, disc, vis)| {
            *disc == KindDiscriminant::Module && *vis != Visibility::Public
        })
        .map(|(rank, ..)| *rank)
        .min();

    let (Some(pub_rank), Some(mod_rank)) = (best_public_api_rank, best_internal_module_rank) else {
        panic!(
            "test does not exercise the invariant it claims to: need at least one public \
             Function/Record hit AND at least one non-public Module hit among the 'memchr' \
             results. public_rank={best_public_api_rank:?} internal_module_rank={best_internal_module_rank:?}"
        );
    };

    assert!(
        pub_rank < mod_rank,
        "querying 'memchr' ranks a non-public internal module (rank #{mod_rank}) at or \
         above the public function/struct (rank #{pub_rank}) — a user searching for the \
         real API sees internal implementation detail ranked equal-or-higher. See the \
         full ranked list printed above."
    );
}

// ---------------------------------------------------------------------------
// QUALITY: cross-package isolation in a real multi-package corpus
// ---------------------------------------------------------------------------

/// In a real 3-package corpus, a query that matches only in one package
/// (`memchr`) must return hits that all genuinely belong to that package —
/// verified via `SymbolKey.package`, not display text.
#[tokio::test]
#[ignore = "loads three real Cargo workspaces through rust-analyzer; run with --ignored"]
async fn cross_package_search_returns_only_the_owning_real_package() {
    let (Some(memchr), Some(log), Some(itoa)) = (memchr_pkg(), log_pkg(), itoa_pkg()) else {
        return;
    };
    let engine = engine_over(&[memchr, log, itoa]);
    wait_for_n_packages(&engine, 3).await;

    let query = SearchQuery {
        text: "memchr".to_owned(),
        kinds: Vec::new(),
            packages: Vec::new(),
        limit: 0,
    };
    let (events, _wall) = run_search_timed(&engine, query, Gen(50)).await;
    let rows = section_rows(&events, SECTION_NAME);

    assert!(!rows.is_empty(), "sanity: 'memchr' must hit something in the 3-package corpus");
    let foreign: Vec<_> = rows
        .iter()
        .filter(|r| r.key.package.name.as_str() != "memchr")
        .map(|r| (r.key.package.name.as_str().to_owned(), r.display_name.to_string()))
        .collect();
    assert!(
        foreign.is_empty(),
        "query 'memchr' returned hits from a package other than memchr: {foreign:?}"
    );
}

// ---------------------------------------------------------------------------
// QUALITY: display-name repeated-segment sanity (independent evidence for
// the already-tracked qualified_display_name path-repetition issue)
// ---------------------------------------------------------------------------

/// A qualified display name may legitimately repeat one path segment once —
/// `memchr`'s crate root and its private `crate::memchr` submodule are both
/// literally named "memchr", so "memchr.memchr.<leaf>" is expected. Three or
/// more *consecutive* repeats of the same segment is never the correct
/// rendering of any real Rust module nesting; it is a chain-construction
/// defect. This is independent, freshly-generated evidence for the
/// separately-tracked "qualified_display_name repeats ancestor segments"
/// item — reported here as a finding, not fixed (this file does not touch
/// engine source).
#[tokio::test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
async fn memchr_query_display_names_do_not_triple_repeat_a_path_segment() {
    let Some(memchr) = memchr_pkg() else { return };
    let engine = engine_over(std::slice::from_ref(&memchr));
    wait_for_n_packages(&engine, 1).await;

    let query = SearchQuery {
        text: "memchr".to_owned(),
        kinds: Vec::new(),
            packages: Vec::new(),
        limit: 0,
    };
    let (events, _wall) = run_search_timed(&engine, query, Gen(60)).await;
    let rows = section_rows(&events, SECTION_NAME);

    let offenders: Vec<&str> = rows
        .iter()
        .map(|r| -> &str { &r.display_name })
        .filter(|name| {
            let segs: Vec<&str> = name.split(['.', ':']).filter(|s| !s.is_empty()).collect();
            segs.windows(3).any(|w| w[0] == w[1] && w[1] == w[2])
        })
        .collect();

    assert!(
        offenders.is_empty(),
        "found {} display name(s) with the same path segment repeated 3+ times in a row \
         (never correct for real Rust module nesting): {:?}. This corroborates the \
         separately-tracked qualified_display_name ancestor-repetition issue with fresh \
         evidence from a real crate.",
        offenders.len(),
        offenders
    );
}

// ---------------------------------------------------------------------------
// QUALITY: the *specific* public function, not "function or struct", vs. an
// internal module collision
// ---------------------------------------------------------------------------

/// `memchr_query_ranks_public_api_above_same_named_internal_module` (above)
/// checks "the best-ranked public Function *or* Record" against "the
/// best-ranked non-public Module", and passes. This test isolates the
/// *function* alone, and is expected to demonstrate that the previous test's
/// pass is not because ranking correctly prioritizes public API — it is
/// because `Memchr` (capitalized) happens to sort ahead of the lowercase
/// `memchr` family under the plain byte-wise tie-break `collect_name_hits`
/// uses. Take away the capitalized struct and ask specifically "does the
/// real, exported `memchr()` function outrank a crate-private
/// `arch::*::memchr` implementation module?" and the real answer — verified
/// against the printed ranked list in the sibling test above — is no: the
/// function is out-ranked by `arch::generic::memchr` (`vis=Crate`), a module
/// no caller of this crate can even name.
///
/// This is the more honest statement of the finding this file was asked to
/// prove or disprove. **A failure here is a finding about
/// `collect_name_hits`'s scoring (every case-folded exact match ties at
/// 1.0, broken only by an incidental capitalization-sensitive string
/// compare), not a bug in this test.**
#[tokio::test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
async fn memchr_function_specifically_outranks_internal_module_collision() {
    let Some(memchr) = memchr_pkg() else { return };
    let engine = engine_over(std::slice::from_ref(&memchr));
    wait_for_n_packages(&engine, 1).await;

    let query = SearchQuery {
        text: "memchr".to_owned(),
        kinds: Vec::new(),
            packages: Vec::new(),
        limit: 0,
    };
    let (events, _wall) = run_search_timed(&engine, query, Gen(41)).await;
    let rows = section_rows(&events, SECTION_NAME);
    let resolved = resolve(&rows, &memchr);

    let function_rank = resolved
        .iter()
        .filter(|(_, r, disc, vis)| {
            *disc == KindDiscriminant::Function
                && *vis == Visibility::Public
                && r.display_name.to_lowercase().contains("memchr")
        })
        .map(|(rank, ..)| *rank)
        .min();
    let internal_module_rank = resolved
        .iter()
        .filter(|(_, _, disc, vis)| *disc == KindDiscriminant::Module && *vis != Visibility::Public)
        .map(|(rank, ..)| *rank)
        .min();

    let (Some(fn_rank), Some(mod_rank)) = (function_rank, internal_module_rank) else {
        panic!(
            "test does not exercise the invariant it claims to: fn_rank={function_rank:?} \
             internal_module_rank={internal_module_rank:?}"
        );
    };

    assert!(
        fn_rank < mod_rank,
        "the real public `memchr()` function (rank #{fn_rank}) is out-ranked by a \
         non-public internal module of the same name (rank #{mod_rank}). See the full \
         ranked list eprintln'd by memchr_query_ranks_public_api_above_same_named_internal_module."
    );
}

// ---------------------------------------------------------------------------
// QUALITY: re-export aliases are invisible to name search (descriptive
// finding, not an asserted "should")
// ---------------------------------------------------------------------------

/// `memchr`'s crate root re-exports its public API with
/// `pub use crate::memchr::{memchr, memchr2, ...};` (`.real-crates/memchr-2.8.3/src/lib.rs:203`).
/// That creates, per real symbol, **two** IR entries: the physical
/// definition inside the private `crate::memchr` submodule (an
/// `EntryInner::Owned(Kind::Function(..))`, the one search finds — see the
/// ranking test above for how ugly its resolved path is), and a re-export
/// alias at the crate root (`EntryInner::Reference`, pointing at the real
/// one). `collect_name_hits` filters on `ir_entry.kind().discriminant()`,
/// which is `None` for a `Reference` entry, so **the clean, crate-root
/// re-export alias is structurally invisible to name search** — a user can
/// never be shown the short, public path a real `use memchr::memchr;`
/// resolves to; only the long, private-module-qualified physical path.
///
/// This test documents that fact precisely (it is expected to pass — it
/// describes real, current behavior, not a violated invariant): a `Reference`
/// entry named "memchr" genuinely exists in the sealed table, and genuinely
/// never appears among the search hits for "memchr" by `IntroId`.
#[tokio::test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
async fn memchr_reexport_aliases_are_invisible_to_name_search() {
    let Some(memchr) = memchr_pkg() else { return };

    let reexport_aliases: Vec<IntroId> = memchr
        .view()
        .entries()
        .filter(|(_, e)| e.sym().name == "memchr" && e.kind().discriminant().is_none())
        .map(|(id, _)| id)
        .collect();
    assert!(
        !reexport_aliases.is_empty(),
        "test does not exercise the invariant it claims to: expected at least one \
         kindless (Reference) entry literally named 'memchr' in the real sealed table; \
         found none — the re-export-alias shape this test documents may no longer exist"
    );

    let engine = engine_over(std::slice::from_ref(&memchr));
    wait_for_n_packages(&engine, 1).await;
    let query = SearchQuery {
        text: "memchr".to_owned(),
        kinds: Vec::new(),
            packages: Vec::new(),
        limit: 0,
    };
    let (events, _wall) = run_search_timed(&engine, query, Gen(42)).await;
    let found_intros: std::collections::HashSet<IntroId> =
        section_rows(&events, SECTION_NAME).iter().map(|r| r.key.intro).collect();

    let visible_aliases: Vec<_> = reexport_aliases
        .iter()
        .filter(|id| found_intros.contains(id))
        .collect();
    eprintln!(
        "{} re-export-alias 'memchr' Reference entries exist in the IR; {} of them are \
         reachable through name search (expected: 0, they are filtered by the \
         kind().discriminant() check in collect_name_hits)",
        reexport_aliases.len(),
        visible_aliases.len()
    );
    assert!(
        visible_aliases.is_empty(),
        "expected zero re-export Reference entries to be reachable through name search \
         (this is describing current behavior); found {} reachable: {:?} — the behavior \
         this test documents has changed and the surrounding doc comment is now stale",
        visible_aliases.len(),
        visible_aliases
    );
}
