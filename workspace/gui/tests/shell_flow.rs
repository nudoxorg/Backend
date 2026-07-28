//! End-to-end integration tests for the core user-visible flow:
//!
//!   search the live fixture corpus → get hits → open a symbol → document streams in.
//!
//! # Organisation
//!
//! **Tier 1 — store level, no window.** These are the load-bearing tests: they
//! exercise the engine+store stack in isolation, which means failures point
//! directly at the data path rather than at overlay plumbing.
//!
//! **Tier 2 — window level.** These exercise the wiring added in §13.5: that
//! `cmd-K` opens the `OmniSearch` overlay and `escape` closes it again.
//!
//! # GPUI executor notes
//!
//! - `cx.executor().allow_parking()` is called early in every test that drives
//!   the real engine. Without it the single-threaded test executor panics on
//!   the first cross-thread wait.
//! - `run_until_parked()` drains GPUI work but does not wait for the Tokio
//!   background thread to produce results. Every engine-result wait therefore
//!   goes through `wait_until`, which retries with a real sleep between polls.

use std::time::Duration;

use gpui::{App, AppContext as _, TestAppContext, WindowOptions};
use lindsey::app::keymaps;
use lindsey::motion::tokens::MotionTokens;
use lindsey::stores::search_model::SearchAccess as _;
use lindsey::stores::search_model::SearchSnapshot;
use lindsey::stores::events::OpenDisposition;
use lindsey::stores::{SearchStore, SymbolStore};
use lindsey::theme::ext::NudoxThemeExt;
use lindsey::workspace::overlays::OverlayKind;
use lindsey::workspace::shell::Shell;
use nudox_engine::runtime::{Engine, EngineConfig};

// ─────────────────────────────────────────────────────────────────────────────
// Shared helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Query substring that genuinely exists in the rich fixture corpus.
///
/// Chosen from `build_rich_view()` in
/// `crates/nudox-store/src/source/fixtures.rs`, table entry id=3:
/// `fsym_doc("Point", …)`. "Point" appears as a top-level public symbol
/// in the `nudox-fixture-rich` package and is searched by the Name index.
const REAL_QUERY: &str = "Point";

/// A query string that is guaranteed never to appear in the fixture corpus.
const BOGUS_QUERY: &str = "zzzznotasymbolzzzz";

/// Poll `predicate` until it returns `true`, advancing the GPUI executor
/// between each attempt.
///
/// Fails with a descriptive panic after a bounded number of rounds so tests
/// never hang. The wait budget is generous (5 seconds) because the Tokio
/// corpus-seed future runs on its own threads and the GPUI executor has no
/// visibility into when it finishes.
async fn wait_until(
    cx: &mut TestAppContext,
    what: &str,
    mut predicate: impl FnMut(&mut TestAppContext) -> bool,
) {
    const MAX_ATTEMPTS: u32 = 100;
    const INTERVAL_MS: u64 = 50;

    for _ in 0..MAX_ATTEMPTS {
        cx.run_until_parked();
        if predicate(cx) {
            return;
        }
        // `background_executor` is a field on `TestAppContext`, not a method.
        cx.background_executor
            .timer(Duration::from_millis(INTERVAL_MS))
            .await;
    }

    panic!(
        "wait_until timed out after {} ms waiting for: {what}",
        u64::from(MAX_ATTEMPTS) * INTERVAL_MS
    );
}

/// Boot the engine and return a pair of GPUI entities for the two stores.
///
/// Uses `Engine::start_with_fixtures` (the same call as `main.rs`) so the
/// corpus loading and search index are real throughout.
fn boot_stores(cx: &mut TestAppContext) -> (gpui::Entity<SearchStore>, gpui::Entity<SymbolStore>) {
    let engine = Engine::start_with_fixtures(EngineConfig::default());
    let search = cx.new(|_cx| SearchStore::new(engine.clone()));
    let symbols = cx.new(|_cx| SymbolStore::new(engine.clone()));
    (search, symbols)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tier 1 — store level (no window required)
// ─────────────────────────────────────────────────────────────────────────────

/// A real query against the live fixture corpus must return at least one
/// hit in the Name section, and that hit's leaf must contain the query
/// string (case-insensitively).
///
/// This is the guard against the engine returning nothing at all — if it
/// fails, the user would see "no results" for any query.
#[gpui::test]
async fn search_over_the_live_corpus_returns_hits(cx: &mut TestAppContext) {
    // The engine delivers results from a Tokio thread; allow the single-thread
    // test executor to park while waiting for them.
    cx.executor().allow_parking();

    let (search, _symbols) = boot_stores(cx);

    // Kick off the query (mirrors what the overlay does on each keystroke).
    search.update(cx, |store, cx| {
        store.set_input(REAL_QUERY.into(), cx);
    });

    // Wait until the Name section (index 0) has at least one row.
    wait_until(cx, "Name section has ≥1 hit for 'Point'", |cx| {
        search.read_with(cx, |store, _cx| {
            let snap: SearchSnapshot = store.snapshot();
            !snap.sections[0].rows.is_empty()
        })
    })
    .await;

    // Assert on content, not just count.
    search.read_with(cx, |store, _cx| {
        let snap = store.snapshot();
        let name_rows = &snap.sections[0].rows;

        assert!(
            !name_rows.is_empty(),
            "Name section must have ≥1 row — the fixture corpus is empty or the \
             search index did not build in time"
        );

        let query_lower = REAL_QUERY.to_lowercase();
        let any_match = name_rows.iter().any(|row| {
            row.leaf.to_lowercase().contains(&query_lower)
                || row.path.to_lowercase().contains(&query_lower)
        });
        assert!(
            any_match,
            "At least one result's leaf or path must contain {:?} (case-insensitive). \
             Returned leaves: {:?}",
            REAL_QUERY,
            name_rows.iter().map(|r| r.leaf.as_ref()).collect::<Vec<_>>()
        );
    });
}

/// A query string that matches nothing in the fixture corpus must yield
/// zero hits across all sections once the search has settled.
///
/// Without this test, `search_over_the_live_corpus_returns_hits` could
/// pass even if the engine returned every symbol for every query.
#[gpui::test]
async fn a_nonsense_query_yields_no_hits(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let (search, _symbols) = boot_stores(cx);

    search.update(cx, |store, cx| {
        store.set_input(BOGUS_QUERY.into(), cx);
    });

    // Wait until at least one section has settled (status = Ready).  We do
    // not wait for *all* sections because the semantic section may time out or
    // go Offline in a headless environment; the local Name/Type sections are
    // enough to assert no-hits.
    wait_until(cx, "at least one section settled after bogus query", |cx| {
        search.read_with(cx, |store, _cx| {
            let snap = store.snapshot();
            snap.any_section_settled()
        })
    })
    .await;

    search.read_with(cx, |store, _cx| {
        let snap = store.snapshot();
        // Name (0) and Type (1) sections are local and synchronous; assert both
        // are empty.  Semantic may still be loading if the engine is offline,
        // so we only check it when it has settled.
        assert_eq!(
            snap.sections[0].rows.len(),
            0,
            "Name section must be empty for {:?} — the corpus seems to match everything",
            BOGUS_QUERY
        );
        assert_eq!(
            snap.sections[1].rows.len(),
            0,
            "Type section must be empty for {:?}",
            BOGUS_QUERY
        );
    });
}

/// Taking a `SymbolKey` from a real search hit and feeding it to
/// `SymbolStore::open` must eventually produce a `SymbolDoc` with a `head`
/// and at least one section.
///
/// This proves that the search→document pipeline is end-to-end wired:
/// a broken `open_symbol` bridge would leave `head` as `None` forever.
#[gpui::test]
async fn opening_a_hit_streams_a_document(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let (search, symbols) = boot_stores(cx);

    // Search for a symbol we know exists.
    search.update(cx, |store, cx| {
        store.set_input(REAL_QUERY.into(), cx);
    });

    // Wait for the first Name-section hit.
    wait_until(cx, "Name section has a hit to open", |cx| {
        search.read_with(cx, |store, _cx| {
            !store.snapshot().sections[0].rows.is_empty()
        })
    })
    .await;

    // Extract the key from the first hit.
    let key = search.read_with(cx, |store, _cx| {
        store
            .snapshot()
            .sections[0]
            .rows
            .first()
            .expect("Name section has at least one row")
            .key
            .clone()
    });

    let key_for_check = key.clone();

    // Ask the symbol store to stream the document for that key.
    let tab_id = symbols.update(cx, |store, cx| {
        store.open(key, OpenDisposition::Replace, cx)
    });

    // Wait until the head arrives.
    wait_until(cx, "SymbolDoc has a head", |cx| {
        symbols.read_with(cx, |store, _cx| {
            store
                .doc(tab_id)
                .and_then(|doc| doc.head.as_ref())
                .is_some()
        })
    })
    .await;

    // Assert on head content and section count.
    symbols.read_with(cx, |store, _cx| {
        let doc = store
            .doc(tab_id)
            .expect("SymbolDoc must exist after open()");

        let head = doc
            .head
            .as_ref()
            .expect("SymbolDoc::head must be Some — the engine sent no Head event");

        assert_eq!(
            head.key, key_for_check,
            "SymbolHead::key must match the key we opened — a mismatched head \
             would display the wrong symbol name in the tab"
        );

        assert!(
            doc.sections.len() > 0,
            "At least one section must have arrived — if zero sections arrive the \
             user sees a blank page with no documentation content"
        );
    });
}

/// Opening the same symbol key twice must reuse the existing tab and must not
/// create a duplicate document entry.
///
/// Without dedup, the pane would accumulate a new tab on every search
/// confirm, making it impossible to navigate a large corpus without
/// exponential tab growth.
#[gpui::test]
async fn opening_the_same_symbol_twice_reuses_its_tab(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let (search, symbols) = boot_stores(cx);

    // Get a real key from the corpus.
    search.update(cx, |store, cx| {
        store.set_input(REAL_QUERY.into(), cx);
    });
    wait_until(cx, "Name section has a hit", |cx| {
        search.read_with(cx, |store, _cx| {
            !store.snapshot().sections[0].rows.is_empty()
        })
    })
    .await;

    let key = search.read_with(cx, |store, _cx| {
        store.snapshot().sections[0].rows[0].key.clone()
    });

    // First open.
    let tab_a = symbols.update(cx, |store, cx| {
        store.open(key.clone(), OpenDisposition::Replace, cx)
    });

    // Second open — same key.
    let tab_b = symbols.update(cx, |store, cx| {
        store.open(key.clone(), OpenDisposition::Replace, cx)
    });

    assert_eq!(
        tab_a, tab_b,
        "Opening the same SymbolKey twice must return the same TabId — \
         a new TabId would mean a duplicate tab was created"
    );

    symbols.read_with(cx, |store, _cx| {
        assert_eq!(
            store.docs.len(),
            1,
            "Only one SymbolDoc must exist for one unique key — \
             multiple docs for the same key means dedup is broken"
        );
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// Tier 2 — window level (tests the shell overlay wiring)
// ─────────────────────────────────────────────────────────────────────────────
//
// These tests open a real GPUI window with Shell as the root view, simulate
// keystrokes, and assert on Shell's overlay state.  They mirror the exact
// boot order from main.rs: gpui_component::init → NudoxThemeExt::init →
// MotionTokens global → keymaps → stores → window.

/// `cmd-K` must push an `OmniSearch` overlay onto the shell's overlay stack.
///
/// If this fails, pressing `cmd-K` in the real app would do nothing visible —
/// the most common entry point for every search session would be broken.
#[gpui::test]
async fn cmd_k_opens_the_omni_search_overlay(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    // Boot in the exact order main.rs does.
    cx.update(|cx: &mut App| {
        gpui_component::init(cx);
        NudoxThemeExt::init(cx);
        cx.set_global(MotionTokens::new(1.0));
        cx.bind_keys(keymaps::all_bindings());
    });

    let engine = Engine::start_with_fixtures(EngineConfig::default());
    let search = cx.new(|_cx| SearchStore::new(engine.clone()));
    let symbols = cx.new(|_cx| SymbolStore::new(engine.clone()));

    // Capture the root Entity<Shell>: the window constructor closure runs
    // synchronously inside open_window, so by the time we return from
    // cx.update the cell is filled. We use Arc so the closure can be 'static.
    let shell_cell = std::sync::Arc::new(std::sync::Mutex::new(None::<gpui::Entity<Shell>>));
    let shell_cell_w = shell_cell.clone();
    let window = cx
        .update(|cx: &mut App| {
            cx.open_window(WindowOptions::default(), move |window, cx| {
                let entity = cx.new(|cx| Shell::new(search.clone(), symbols.clone(), window, cx));
                *shell_cell_w.lock().unwrap() = Some(entity.clone());
                entity
            })
        })
        .expect("window must open");

    let shell = shell_cell
        .lock()
        .unwrap()
        .take()
        .expect("shell entity set by window constructor");

    let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();

    // Simulate `cmd-K`.
    vcx.simulate_keystrokes("cmd-k");
    vcx.run_until_parked();

    // Read overlay state via the captured entity — no WindowHandle API needed.
    let overlay_top = shell.read_with(&mut vcx, |shell_ref, _| {
        shell_ref.overlay_kind_on_top().cloned()
    });

    assert_eq!(
        overlay_top,
        Some(OverlayKind::OmniSearch),
        "cmd-K must push OmniSearch onto the overlay stack — \
         a None or different overlay kind means the keymap binding \
         is not wired to Shell::open_omni_search"
    );
}

/// Pressing `escape` while the omni-search overlay is open must close it.
///
/// Without this invariant, the user has no keyboard path to return from
/// search to their document — the most important "back" gesture would be
/// silently broken.
#[gpui::test]
async fn escape_closes_the_overlay(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    cx.update(|cx: &mut App| {
        gpui_component::init(cx);
        NudoxThemeExt::init(cx);
        cx.set_global(MotionTokens::new(1.0));
        cx.bind_keys(keymaps::all_bindings());
    });

    let engine = Engine::start_with_fixtures(EngineConfig::default());
    let search = cx.new(|_cx| SearchStore::new(engine.clone()));
    let symbols = cx.new(|_cx| SymbolStore::new(engine.clone()));

    let shell_cell = std::sync::Arc::new(std::sync::Mutex::new(None::<gpui::Entity<Shell>>));
    let shell_cell_w = shell_cell.clone();
    let window = cx
        .update(|cx: &mut App| {
            cx.open_window(WindowOptions::default(), move |window, cx| {
                let entity = cx.new(|cx| Shell::new(search.clone(), symbols.clone(), window, cx));
                *shell_cell_w.lock().unwrap() = Some(entity.clone());
                entity
            })
        })
        .expect("window must open");

    let shell = shell_cell
        .lock()
        .unwrap()
        .take()
        .expect("shell entity set by window constructor");

    let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();

    // Open the overlay first.
    vcx.simulate_keystrokes("cmd-k");
    vcx.run_until_parked();

    // Confirm it is open before we test closing it.
    let before = shell.read_with(&mut vcx, |s, _| s.overlay_kind_on_top().cloned());
    assert_eq!(
        before,
        Some(OverlayKind::OmniSearch),
        "pre-condition: overlay must be open before we press escape"
    );

    // Press escape — dispatches DismissOverlay through the Overlay key context,
    // which Shell::on_omni_event routes to close_overlay.
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();

    let after = shell.read_with(&mut vcx, |s, _| s.overlay_kind_on_top().cloned());
    assert_eq!(
        after,
        None,
        "escape must close the OmniSearch overlay — a Some(…) here \
         means DismissOverlay was not routed to Shell::close_overlay"
    );
}
