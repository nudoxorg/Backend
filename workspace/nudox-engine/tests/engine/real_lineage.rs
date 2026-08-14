//! Real-generation lineage: does `Engine::start_with_versions` actually work
//! against on-disk producer output, or only against the synthetic
//! `StaticSource` fixtures in `versions_adversarial.rs`?
//!
//! # REGISTRAR's starting suspicion
//!
//! `docs/CORPUS-REPORT.md` records `memchr` 2.7.6, 2.8.0 and 2.8.3 each lowering to
//! **exactly** 11 329 entries. Three different release tarballs producing a
//! byte-identical count is either (a) a genuinely API-stable patch series, or
//! (b) a sign that `version` never reaches the producer and the same tree got
//! lowered three times under three labels. Nobody had distinguished these
//! before this file. docs/LIMITATIONS.md L10 says so explicitly.
//!
//! # What the on-disk diff established (before writing any test)
//!
//! `diff -rq` across `result/memchr-{2.7.6,2.8.0,2.8.3}/src` proves the
//! three checkouts are **not** the same tree: `Cargo.toml`'s `version` field
//! matches each label, and real `src/` files differ between every adjacent
//! pair (`cow.rs`, `memmem/mod.rs` for 2.7.6→2.8.0; `arch/all/rabinkarp.rs`,
//! `arch/all/twoway.rs`, `arch/generic/memchr.rs`, `arch/generic/packedpair.rs`,
//! `arch/x86_64/*`, `vector.rs`, `memmem/{mod,searcher}.rs` for 2.8.0→2.8.3).
//! So hypothesis (b) in its strongest form — "identical tree copied under
//! three names" — is already false; the checkouts are real, distinct releases.
//!
//! The finer question is whether those real differences are *visible to this
//! producer*, and inspecting them says no, for two independent reasons:
//!
//! 1. Every public-API-surface diff (`FinderBuilder::build_forward_owned`,
//!    `build_forward_with_ranker_owned`, `build_reverse_owned`, and
//!    `CowBytes::new_owned`'s `fn` → `pub(crate) fn`) added between 2.7.6 and
//!    2.8.0 sits behind `#[cfg(feature = "alloc")]`. `symbol_head_cfg.rs`
//!    (this crate, already committed) independently discovered and documented
//!    that the Rust producer's rust-analyzer workspace is loaded with **no
//!    cargo features active at all**, so every `#[cfg(feature = "...")]`
//!    branch is dead code from the producer's point of view — these items
//!    never reach the IR in *any* generation, 2.7.6 included. A pub-API diff
//!    that the producer cannot see cannot move an entry count.
//! 2. Every remaining diff between 2.8.0 and 2.8.3 (`.get(0)` → `.first()` in
//!    `rabinkarp.rs`/`twoway.rs`, an added `unreachable_unchecked` safety hint
//!    in `arch/generic/memchr.rs`, a rewritten bounds comparison in
//!    `packedpair.rs`, a dropped `.clone()` in `memmem/searcher.rs`, and an
//!    endian-cfg split of one NEON `movemask` fn into two mutually-exclusive
//!    arms in `vector.rs`) is a statement-level edit *inside* an existing
//!    function body. None of them adds, removes, or renames a top-level
//!    declaration, changes a signature, or changes a doc comment — grepping
//!    every `fn`/`struct`/`enum`/`trait`/`const`/`static`/`type`/`mod`/`impl`
//!    header in the whole `memchr-2.8.3/src` tree (including the x86_64/wasm32
//!    arms that don't even compile on this aarch64 host) finds only 551 of
//!    them, so whatever `entries` counts at 11 329 is far finer-grained than
//!    "one per header" — but every diffed line here sits *below* header
//!    granularity regardless of what the finer unit turns out to be.
//!
//! So a byte-identical entry count across these three specific releases is the
//! **expected, correct outcome** given (1) and (2), not evidence of a bug —
//! hypothesis (a). That conclusion is inferred from a source diff, though, not
//! observed from the producer. The rest of this file is the observation: it
//! boots the real `Engine` over all three checkouts and reads the entry count
//! `VersionRow::symbol_count` actually reports today, live, rather than
//! trusting a report that could be stale. See the `evidence` line in the
//! agent's own report for what that run actually returned.
//!
//! # Why `select_version`-switches-content is *not* asserted here
//!
//! The natural next test would be "search for something that only exists in
//! one generation, `select_version`, and watch the hit disappear" — exactly
//! how `versions_adversarial.rs` proves it against synthetic data. Point (1)
//! above rules that out for this specific fixture: there is no publicly
//! visible, producer-reachable name that differs between any two of these
//! three real generations. Asserting content-level switching honestly for
//! `memchr` needs a pair of real generations with a `#[cfg(target_arch =
//! "...")]`- or unconditionally-gated *name* change, which this corpus does
//! not have. What *is* asserted, on real data, is every layer `select_version`
//! is documented to move: the registry's `current` pointer
//! (`VersionEvent::Switched` + `versions().current()`), and the corpus's
//! resident `PackageView` (`open_symbol` continues to resolve the same
//! `IntroId` after the switch, which requires the corpus to actually hold a
//! `PackageView` for the newly-selected generation, not just a registry label).
//!
//! # Running
//!
//! ```text
//! cargo test -p nudox-engine --test real_lineage -- --ignored --nocapture
//! ```
//!
//! Each generation takes roughly the wall time `docs/CORPUS-REPORT.md` records for
//! `memchr` alone (~30-35s) because `nudox_engine::store::source::producer::ProducerSource`
//! loads packages with `Stream::then`, i.e. strictly serially — loading three
//! generations of one package is not parallelised. Total wall time for this
//! file is therefore on the order of four generation-loads (~2-3 minutes).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use nudox_engine::{
    Engine, EngineConfig, EngineHandle, PackageHistorySpec, PackageVersionSpec, ProducerLanguage,
    SearchQuery,
    wire::{DocEvent, Gen, HitRow, SymbolKey, Timeline, TimelineChange, VersionEvent, VersionList},
};
use nudox_ir::change::{EcosystemId, PackageLineageId, PackageName};

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

/// Path to one on-disk memchr generation, e.g. `memchr_root("2.8.3")`.
///
/// Mirrors `axum_root()` in `real_producer_links.rs`: honours
/// `CARGO_MANIFEST_DIR`-relative `../../result/`, no override env var
/// (the fixture set is fixed — three specific generations — rather than a
/// single swappable checkout).
fn memchr_root(version: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../result")
        .join(format!("memchr-{version}"))
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("/nonexistent"))
}

const ALL_VERSIONS: [&str; 3] = ["2.7.6", "2.8.0", "2.8.3"];

/// `true` only if every generation's checkout is present on disk.
fn all_checkouts_present() -> bool {
    ALL_VERSIONS
        .iter()
        .all(|v| memchr_root(v).join("Cargo.toml").is_file())
}

fn lineage() -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("memchr"))
}

/// Build a [`PackageHistorySpec`] for memchr with generations loaded in
/// exactly the order given by `versions` — the caller controls arrival order.
fn history_spec(versions: &[&str]) -> PackageHistorySpec {
    PackageHistorySpec {
        name: "memchr".to_owned(),
        language: ProducerLanguage::Rust,
        versions: versions
            .iter()
            .map(|v| PackageVersionSpec {
                root: memchr_root(v),
                version: (*v).to_owned(),
            })
            .collect(),
    }
}

/// Block until `expected` generations are recorded and one is current, or
/// panic past `budget`. `ProducerSource` loads serially (see module docs), so
/// the budget must scale with the number of generations requested.
fn settle(engine: &EngineHandle, lid: &PackageLineageId, expected: usize) -> VersionList {
    let budget = Duration::from_secs(90 * expected.max(1) as u64);
    let started = Instant::now();
    loop {
        let list = engine.versions(lid);
        if list.len() == expected && list.current().is_some() {
            return list;
        }
        if started.elapsed() > budget {
            panic!(
                "engine did not settle within {budget:?}: got {} of {expected} generations",
                list.len()
            );
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Run a name search on whichever generation is currently resident and return
/// every hit whose *exact* (case-sensitive) display name is `name`.
///
/// Search is prefix-matched, so a plain `SearchQuery::text` lookup for
/// `"memchr2"` also returns `"memchr2_iter"`; exact filtering here is what
/// makes the caller's assertion about *one specific* symbol meaningful.
fn search_exact(engine: &EngineHandle, name: &str) -> Vec<HitRow> {
    let (handle, rx) = engine.search(
        SearchQuery {
            text: name.to_owned(),
            ..Default::default()
        },
        Gen(1),
    );
    let mut hits = Vec::new();
    loop {
        match rx.recv() {
            Ok(nudox_engine::wire::SearchEvent::Section { rows, .. })
            | Ok(nudox_engine::wire::SearchEvent::Merge { rows, .. }) => {
                hits.extend(rows.iter().filter(|r| &*r.display_name == name).cloned());
            }
            Ok(nudox_engine::wire::SearchEvent::Done { .. }) => break,
            Ok(nudox_engine::wire::SearchEvent::Failed { error, .. }) => {
                panic!("search for {name:?} failed: {error:?}")
            }
            Ok(nudox_engine::wire::SearchEvent::Latency { .. }) => {}
            // `SearchEvent` is `#[non_exhaustive]`; any future variant is
            // neither a hit nor a terminator, so it is ignored rather than
            // matched away with a bare `_` that would also swallow `Done`.
            Ok(_) => {}
            Err(_) => break, // channel closed
        }
    }
    drop(handle);
    hits
}

/// Open `key` and drain the full `DocEvent` stream, returning it.
fn open_and_drain(engine: &EngineHandle, key: SymbolKey, generation: Gen) -> Vec<DocEvent> {
    let (handle, rx) = engine.open_symbol(key, generation);
    let mut events = Vec::new();
    loop {
        match rx.recv() {
            Ok(ev) => {
                let done = matches!(ev, DocEvent::Done | DocEvent::Failed(_));
                events.push(ev);
                if done {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    drop(handle);
    events
}

fn timeline_of(events: &[DocEvent]) -> &Timeline {
    events
        .iter()
        .find_map(|e| match e {
            DocEvent::Timeline(t) => Some(t),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "no Timeline event in stream: {:?}",
                events.iter().map(std::mem::discriminant).collect::<Vec<_>>()
            )
        })
}

// ---------------------------------------------------------------------------
// The comprehensive test: three real generations, scrambled arrival order
// ---------------------------------------------------------------------------

/// Boots the real engine over memchr 2.7.6 / 2.8.0 / 2.8.3, listed in a
/// deliberately scrambled order (oldest, newest, middle), and checks every
/// contract `Engine::start_with_versions`'s doc comment makes:
///
/// * all three generations are loaded and listed newest-first regardless of
///   the order they were given in (out-of-order arrival, order independence);
/// * the newest (2.8.3) is current on load;
/// * `select_version` actually repoints both the version registry *and* the
///   corpus (§ module docs on why this is checked at the corpus level, not
///   just via a disappearing search hit);
/// * **the load-bearing assertion**: a symbol present in all three real
///   generations (`memchr2`, a top-level function, byte-identical across all
///   three checkouts — verified by `diff` before this test was written) keeps
///   the *same* `IntroId`, so the timeline the engine builds by looking that
///   one id up in each resident generation actually finds it in all three.
#[test]
#[ignore = "loads three real Cargo workspaces through rust-analyzer; run with --ignored"]
fn real_memchr_lineage_loads_all_three_generations_with_stable_intro_ids() {
    if !all_checkouts_present() {
        eprintln!(
            "SKIP: not all of memchr {ALL_VERSIONS:?} are checked out under result/. \
             Expected e.g. {}",
            memchr_root("2.8.3").display()
        );
        return;
    }

    let dir = memchr_root("2.8.3");
    let (_, _cost) = heart::cost::measured(
        "real_lineage/memchr-3gen-scrambled-order",
        &dir,
        || {
            let lid = lineage();
            // Deliberately not sorted: oldest, newest, middle. If ordering
            // were "last writer wins" (the bug the version registry exists to
            // prevent — see `versions.rs` module docs) the corpus would end
            // up serving 2.8.0, not 2.8.3.
            let spec = history_spec(&["2.7.6", "2.8.3", "2.8.0"]);
            let engine = Engine::start_with_versions(EngineConfig::default(), vec![spec]);

            let list = settle(&engine, &lid, 3);

            // --- versions(): three rows, newest first, exactly one current ---
            assert_eq!(list.len(), 3, "all three generations must be recorded");
            let seen: Vec<&str> = list.versions.iter().map(|v| v.version.as_ref()).collect();
            assert_eq!(
                seen,
                vec!["2.8.3", "2.8.0", "2.7.6"],
                "versions() must be newest-first regardless of load order; got {seen:?}"
            );
            let current_count = list.versions.iter().filter(|v| v.is_current).count();
            assert_eq!(current_count, 1, "exactly one version must be current");
            assert_eq!(
                &*list.current().unwrap().version,
                "2.8.3",
                "the newest generation must be current regardless of arrival order \
                 (2.7.6 arrived first, 2.8.3 second, 2.8.0 last in this run)"
            );

            // Evidence for L10: the real, live entry count per generation,
            // read straight off the running corpus rather than trusted from
            // docs/CORPUS-REPORT.md.
            let counts: Vec<(String, u64)> = list
                .versions
                .iter()
                .map(|v| (v.version.to_string(), v.symbol_count))
                .collect();
            eprintln!("real memchr symbol_count per generation: {counts:?}");
            for (version, count) in &counts {
                assert!(
                    *count > 0,
                    "generation {version} lowered to zero entries — that is a \
                     load failure, not an empty package"
                );
            }

            // --- the newest generation actually answers open_symbol/search ---
            let hits = search_exact(&engine, "memchr2");
            assert_eq!(
                hits.len(),
                1,
                "expected exactly one exact-name hit for 'memchr2' (a real, \
                 unique top-level function in memchr) while 2.8.3 is current; \
                 got {hits:?}"
            );
            let key = hits[0].key.clone();

            // --- open_symbol + Timeline: THE load-bearing IntroId assertion ---
            let events = open_and_drain(&engine, key.clone(), Gen(1));
            assert!(
                !events.iter().any(|e| matches!(e, DocEvent::Failed(_))),
                "opening memchr2 on the current (2.8.3) generation must not fail: {events:?}"
            );
            let t = timeline_of(&events);
            assert_eq!(
                t.versions_examined, 3,
                "the timeline must have examined all three loaded generations"
            );
            assert_eq!(
                t.rows.len(),
                3,
                "MAJOR FINDING if this fails: `memchr2` is a real top-level \
                 function present, and byte-for-byte identical (verified by \
                 `diff` before this test existed), in all three real memchr \
                 checkouts (2.7.6, 2.8.0, 2.8.3). `timeline::build` finds a \
                 symbol in an older generation by looking up the SAME IntroId \
                 in that generation's table (see `timeline.rs` module docs: \
                 \"is the point of IntroId\"). If `rows.len()` here is less \
                 than 3, the real Rust producer is NOT assigning the same \
                 IntroId to the same declaration across independent lowerings \
                 of different checkouts of the same package — i.e. IntroId \
                 stability, the entire premise docs/LIMITATIONS.md L10 is about, \
                 does not hold for real producer output. Got {} row(s): {:?}",
                t.rows.len(),
                t.rows.iter().map(|r| (&*r.version, r.change.clone())).collect::<Vec<_>>(),
            );
            let changes: Vec<(&str, TimelineChange)> = t
                .rows
                .iter()
                .map(|r| (r.version.as_ref(), r.change.clone()))
                .collect();
            assert_eq!(
                changes,
                vec![
                    ("2.8.3", TimelineChange::Unchanged),
                    ("2.8.0", TimelineChange::Unchanged),
                    ("2.7.6", TimelineChange::Present),
                ],
                "memchr2's declaration and doc comment are identical across all \
                 three checkouts (verified by `diff` before this test was \
                 written), so the newest two rows must classify as Unchanged \
                 and the oldest (nothing older to compare against) as Present; \
                 got {changes:?}"
            );

            // --- select_version repoints the registry AND the corpus ---
            let rx = engine.select_version(lid.clone(), "2.7.6", Gen(2));
            let ev = rx.recv().expect("exactly one VersionEvent is sent");
            match ev {
                VersionEvent::Switched {
                    generation,
                    version,
                    ..
                } => {
                    assert_eq!(generation, Gen(2));
                    assert_eq!(&*version, "2.7.6");
                }
                other => panic!("expected Switched, got {other:?}"),
            }
            assert_eq!(
                &*engine.versions(&lid).current().unwrap().version,
                "2.7.6",
                "versions() must report the new selection"
            );

            // The SAME SymbolKey (same IntroId) must still resolve, now against
            // the 2.7.6 generation the corpus is serving — proving the switch
            // moved the corpus's resident PackageView, not just a registry
            // label (`corpus()` is not reachable from this integration test,
            // so `open_symbol` succeeding under the old key against the new
            // generation is the only externally-observable proof available).
            let events_after_switch = open_and_drain(&engine, key.clone(), Gen(2));
            assert!(
                !events_after_switch
                    .iter()
                    .any(|e| matches!(e, DocEvent::Failed(_))),
                "the same SymbolKey must still resolve after select_version to \
                 2.7.6 — IntroId is documented to be stable across the switch; \
                 got {events_after_switch:?}"
            );

            // --- switch again, to the middle generation, for good measure ---
            let rx2 = engine.select_version(lid.clone(), "2.8.0", Gen(3));
            let ev2 = rx2.recv().expect("exactly one VersionEvent is sent");
            assert!(
                matches!(ev2, VersionEvent::Switched { generation: Gen(3), .. }),
                "expected Switched(Gen(3)), got {ev2:?}"
            );
            assert_eq!(&*engine.versions(&lid).current().unwrap().version, "2.8.0");
        },
    );
}

// ---------------------------------------------------------------------------
// Adversarial: one real version, and a version that was never loaded
// ---------------------------------------------------------------------------

/// A lineage loaded with exactly one real generation must still produce a
/// valid, one-row timeline (never an empty one), and asking for a version
/// that was never loaded must answer `NotLoaded` without disturbing the
/// resident generation.
#[test]
#[ignore = "loads a real Cargo workspace through rust-analyzer; run with --ignored"]
fn real_memchr_single_version_and_unloaded_version_request() {
    let root = memchr_root("2.8.3");
    if !root.join("Cargo.toml").is_file() {
        eprintln!("SKIP: no memchr-2.8.3 checkout at {}", root.display());
        return;
    }

    let (_, _cost) = heart::cost::measured("real_lineage/memchr-1gen", &root, || {
        let lid = lineage();
        let spec = history_spec(&["2.8.3"]);
        let engine = Engine::start_with_versions(EngineConfig::default(), vec![spec]);

        let list = settle(&engine, &lid, 1);
        assert_eq!(
            list.len(),
            1,
            "a one-generation lineage must still populate the registry with one row"
        );
        assert_eq!(&*list.versions[0].version, "2.8.3");
        assert!(list.versions[0].is_current, "the single version must be current");
        assert!(
            list.versions[0].symbol_count > 0,
            "a real lowering must not silently produce zero entries"
        );

        // Real content check (doctrine §4: never assert only is_ok()/non-zero
        // on a stub — assert on a symbol that really exists).
        let hits = search_exact(&engine, "memchr2");
        assert_eq!(
            hits.len(),
            1,
            "the single loaded generation must answer a real search; got {hits:?}"
        );
        let events = open_and_drain(&engine, hits[0].key.clone(), Gen(1));
        let t = timeline_of(&events);
        assert_eq!(
            t.rows.len(),
            1,
            "one loaded generation must produce exactly one timeline row"
        );
        assert_eq!(t.rows[0].change, TimelineChange::Present);
        assert_eq!(&*t.rows[0].version, "2.8.3");

        // Adversarial: a version that was never loaded.
        let rx = engine.select_version(lid.clone(), "0.0.1-never-loaded", Gen(2));
        let ev = rx.recv().expect("exactly one VersionEvent is sent");
        match ev {
            VersionEvent::NotLoaded {
                generation,
                version,
                ..
            } => {
                assert_eq!(generation, Gen(2));
                assert_eq!(&*version, "0.0.1-never-loaded");
            }
            other => panic!("expected NotLoaded, got {other:?}"),
        }
        // Nothing must have changed.
        assert_eq!(
            &*engine.versions(&lid).current().unwrap().version,
            "2.8.3",
            "selecting an unloaded version must not disturb the resident generation"
        );
    });
}
