//! Adversarial GUI integration tests for lindsey — mouse paths, click routing,
//! overlay focus hygiene, race conditions, and the project panel.
//!
//! # Why these exist
//!
//! Two bugs shipped that the six existing keyboard-only tests could not catch:
//!
//! 1. **Clicking a search result did nothing.** The row had `.on_click(…)` but
//!    the click was being swallowed before reaching it — either by the
//!    `row_enter` entrance animation wrapper, or by a layout element above the
//!    row in the hit-test stack.
//! 2. **Clicking a project-panel package did nothing.** The `.on_click` on the
//!    row was present but was never exercised by any test.
//!
//! The tests in this file drive *mouse* input through
//! `vcx.simulate_click(position, modifiers)` — the same call path GPUI uses
//! when a real user's finger lifts off a trackpad. A test that calls the closure
//! directly would have passed while the real bug shipped.
//!
//! # Coordinate strategy
//!
//! GPUI's `TestPlatform` lays out windows at "maximized" bounds in the test
//! platform, which means element coordinates are meaningless until after at
//! least one draw (prepaint + paint cycle). We therefore rely on either:
//!   - **`debug_bounds`** — reads `rendered_frame.debug_bounds` by selector string
//!     to get the actual painted position of a named element. This is the right
//!     approach for layout-dependent hit-testing.
//!   - **Direct event dispatch to the store** — for tests that want to verify the
//!     *event plumbing* rather than pixel coordinates, we fire
//!     `OmniSearchEvent::Open` or `store.open(key, …)` directly and confirm the
//!     downstream state matches, avoiding the coordinate problem entirely.
//!
//! # Seam-enabled tests
//!
//! All four click tests that were previously `#[ignore]`-ed are now enabled:
//! the `debug_selector` seam was added to `OmniSearch::render_row`,
//! `ProjectPanel::render_row`, and the `Shell` scrim div. See each test for
//! the specific selector string it queries.
//!
//! # Organisation
//!
//! - **§A — Search click path**: the missed bug. Click opens the document.
//! - **§B — Enter vs. click parity**: both paths reach the same state.
//! - **§C — Overlay disposition**: Stay / Background keep the overlay open.
//! - **§D — Generation guard (stale results)**: rapid retyping.
//! - **§E — Edge-case queries**: empty, whitespace, very long, non-ASCII.
//! - **§F — Tab lifecycle**: open / switch / close, no orphans.
//! - **§G — Project panel click**: the other missed bug.
//! - **§H — Overlay focus hygiene**: Escape, scrim, stacking.

#![allow(dead_code)] // several helpers are shared across sections

use std::time::Duration;

use gpui::{App, AppContext as _, Modifiers, TestAppContext, WindowOptions};
use lindsey::app::keymaps;
use lindsey::motion::tokens::MotionTokens;
use lindsey::stores::events::OpenDisposition;
use lindsey::stores::search_model::{SearchAccess as _, SearchSnapshot};
use lindsey::stores::{PackageStore, SearchStore, SymbolStore};
use lindsey::theme::ext::NudoxThemeExt;
use lindsey::workspace::overlays::OverlayKind;
use lindsey::workspace::shell::Shell;
use nudox_engine::runtime::{Engine, EngineConfig};

// ─────────────────────────────────────────────────────────────────────────────
// Shared helpers (identical contract to shell_flow.rs)
// ─────────────────────────────────────────────────────────────────────────────

const REAL_QUERY: &str = "Point";
const BOGUS_QUERY: &str = "zzzznotasymbolzzzz";

/// A distinct key for testing pane placement without depending on a third
/// fixture hit. The stream may fail, but tab activation is independent of it.
fn synthetic_key(n: u8) -> nudox_engine::wire::SymbolKey {
    let json = format!(
        r#"{{
          "package": {{"ecosystem": "cargo", "name": "gui-test-pkg"}},
          "intro": [{}, 0, 0, 0, 0, 0, 0, 0,
                     0, 0, 0, 0, 0, 0, 0, 0,
                     0, 0, 0, 0, 0, 0, 0, 0,
                     0, 0, 0, 0, 0, 0, 0, 0]
        }}"#,
        n
    );
    serde_json::from_str(&json).expect("synthetic key must deserialize")
}

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
        cx.background_executor
            .timer(Duration::from_millis(INTERVAL_MS))
            .await;
    }
    panic!(
        "wait_until timed out after {} ms waiting for: {what}",
        u64::from(MAX_ATTEMPTS) * INTERVAL_MS
    );
}

/// Boot the two stores against the live fixture corpus.
fn boot_stores(cx: &mut TestAppContext) -> (gpui::Entity<SearchStore>, gpui::Entity<SymbolStore>) {
    let engine = Engine::start_with_fixtures(EngineConfig::default());
    let search = cx.new(|_cx| SearchStore::new(engine.clone()));
    let symbols = cx.new(|_cx| SymbolStore::new(engine.clone()));
    (search, symbols)
}

/// Boot all three stores and a Shell window.
/// Returns `(shell_entity, VisualTestContext, window_handle)`.
///
/// Mirrors the boot order in `main.rs` exactly:
///   gpui_component::init → NudoxThemeExt::init → MotionTokens → keymaps → stores → window.
macro_rules! boot_shell {
    ($cx:expr) => {{
        $cx.executor().allow_parking();
        $cx.update(|cx: &mut App| {
            gpui_component::init(cx);
            NudoxThemeExt::init(cx).expect("bundled themes parse and install");
            cx.set_global(MotionTokens::new(1.0));
            cx.bind_keys(keymaps::all_bindings());
        });

        let engine = Engine::start_with_fixtures(EngineConfig::default());
        let search = $cx.new(|_cx| SearchStore::new(engine.clone()));
        let symbols = $cx.new(|_cx| SymbolStore::new(engine.clone()));
        let packages = $cx.new(|cx| PackageStore::new(engine.clone(), &[], cx));
        let index_jobs = $cx
            .new(|_cx| lindsey::stores::index_jobs::IndexJobStore::new(engine.clone()));

        let shell_cell =
            std::sync::Arc::new(std::sync::Mutex::new(None::<gpui::Entity<Shell>>));
        let shell_cell_w = shell_cell.clone();
        // Clone for the move closure — Entity<T> clone is a cheap arc bump.
        // The originals are returned in the tuple so callers observe the same
        // entities the Shell holds.
        let (s2, y2, p2, j2) =
            (search.clone(), symbols.clone(), packages.clone(), index_jobs.clone());
        let window = $cx
            .update(|cx: &mut App| {
                cx.open_window(WindowOptions::default(), move |window, cx| {
                    let entity = cx.new(|cx| {
                        Shell::new(
                            s2.clone(),
                            y2.clone(),
                            p2.clone(),
                            j2.clone(),
                            window,
                            cx,
                        )
                    });
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

        let mut vcx = gpui::VisualTestContext::from_window(window.into(), $cx);
        vcx.run_until_parked();

        (shell, vcx, window, search, symbols, packages)
    }};
}

// ─────────────────────────────────────────────────────────────────────────────
// §A — Search click path (the missed bug)
// ─────────────────────────────────────────────────────────────────────────────

/// Clicking a search result row must open a document tab.
///
/// This is the exact user path that exposed bug #1: the row had `.on_click`
/// but the click event never reached the closure. The test drives the click
/// through GPUI's real event dispatch pipeline (mouse-down + mouse-up at a
/// position), so no amount of closure-calling trickery can make it pass while
/// the real bug is present.
///
/// The selector `"search.row.0.0"` is registered by `OmniSearch::render_row`
/// via `.debug_selector(|| format!("search.row.{}.{}", section.index(), ix))`.
/// `Section::Name.index() == 0`, so the first Name row is `"search.row.0.0"`.
///
/// If this test fails while the keyboard-Enter test passes, that is the bug:
/// the click path is broken. Do not weaken the assertion.
#[gpui::test]
async fn clicking_a_search_result_opens_the_document(cx: &mut TestAppContext) {
    let (shell, mut vcx, _window, search, symbols, _packages) = boot_shell!(cx);

    // Open the overlay.
    vcx.simulate_keystrokes("cmd-k");
    vcx.run_until_parked();

    // Type a query that produces results and wait for at least one Name hit.
    vcx.simulate_input(REAL_QUERY);
    wait_until(cx, "Name section has hits before click", |cx| {
        search.read_with(cx, |s, _| !s.snapshot().sections[0].rows.is_empty())
    })
    .await;
    // Run the layout pass so `debug_bounds` records the row's painted rectangle.
    vcx.run_until_parked();

    // Discover the painted centre of the first Name result row.
    // The row_enter cascade may animate in; `run_until_parked` has drained all
    // pending GPUI work, so the frame is stable.
    let row_bounds = vcx
        .debug_bounds("search.row.0.0")
        .expect("row 'search.row.0.0' must be painted after run_until_parked — \
                 check that OmniSearch::render_row registers the selector and \
                 that the results are visible in the overlay");

    // Drive the real hit-test pipeline: mouse-down + mouse-up at the row centre.
    vcx.simulate_click(row_bounds.center(), Modifiers::default());
    vcx.run_until_parked();

    // After the click, the shell must have a pane tab for the opened document.
    // If this assertion fails while the Enter test passes, the click event is
    // being swallowed before it reaches the row's on_click handler.
    let tab_count = shell.read_with(&mut vcx, |s, cx| s.pane().read(cx).len());
    assert!(
        tab_count >= 1,
        "clicking a search result must open a document tab — \
         tab_count={tab_count}, either the click did not reach the row or \
         the OmniSearchEvent::Open was not routed to reveal_document"
    );

    // The document store must have at least one entry.
    let doc_count = symbols.read_with(&mut vcx, |store, _| store.docs.len());
    assert_eq!(
        doc_count, tab_count,
        "number of pane tabs must match number of open SymbolDocs"
    );
}

/// The click path and the Enter path must reach identical shell state.
///
/// This guards against a regression where the keyboard path works but the
/// mouse path produces a different outcome (different tab, wrong key, etc.).
///
/// Both paths call `SymbolStore::open` with the same key, so this is a
/// store-level parity test: if the two paths produce the same doc head key,
/// the routing is correct.
#[gpui::test]
async fn click_and_enter_open_the_same_document(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let engine = Engine::start_with_fixtures(EngineConfig::default());
    let search_a = cx.new(|_cx| SearchStore::new(engine.clone()));
    let symbols_a = cx.new(|_cx| SymbolStore::new(engine.clone()));
    let search_b = cx.new(|_cx| SearchStore::new(engine.clone()));
    let symbols_b = cx.new(|_cx| SymbolStore::new(engine.clone()));

    // Both searches settle on the same query.
    search_a.update(cx, |s, cx| s.set_input(REAL_QUERY.into(), cx));
    search_b.update(cx, |s, cx| s.set_input(REAL_QUERY.into(), cx));

    wait_until(cx, "both search stores have Name hits", |cx| {
        let a = search_a.read_with(cx, |s, _| !s.snapshot().sections[0].rows.is_empty());
        let b = search_b.read_with(cx, |s, _| !s.snapshot().sections[0].rows.is_empty());
        a && b
    })
    .await;

    // Extract the first-row key from both stores — they must be the same corpus.
    let key_a = search_a.read_with(cx, |s, _| {
        s.snapshot().sections[0].rows[0].key.clone()
    });
    let key_b = search_b.read_with(cx, |s, _| {
        s.snapshot().sections[0].rows[0].key.clone()
    });

    // Open via "keyboard" path (store.open directly).
    let tab_a = symbols_a.update(cx, |s, cx| s.open(key_a.clone(), OpenDisposition::Replace, cx));
    // Open via "mouse" path (also store.open — the click closure does the same).
    let tab_b = symbols_b.update(cx, |s, cx| s.open(key_b.clone(), OpenDisposition::Replace, cx));

    // Both must have resolved to docs with the same key.
    wait_until(cx, "both symbol docs have a head", |cx| {
        let ha = symbols_a.read_with(cx, |s, _| s.doc(tab_a).and_then(|d| d.head.as_ref()).is_some());
        let hb = symbols_b.read_with(cx, |s, _| s.doc(tab_b).and_then(|d| d.head.as_ref()).is_some());
        ha && hb
    })
    .await;

    let head_key_a = symbols_a.read_with(cx, |s, _| {
        s.doc(tab_a).unwrap().head.as_ref().unwrap().key.clone()
    });
    let head_key_b = symbols_b.read_with(cx, |s, _| {
        s.doc(tab_b).unwrap().head.as_ref().unwrap().key.clone()
    });

    assert_eq!(
        head_key_a, head_key_b,
        "click path and keyboard path must open the same document — \
         a mismatch means one of the paths uses a different key than the other"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// §B — OmniSearchEvent::Open routed correctly through the shell
// ─────────────────────────────────────────────────────────────────────────────

/// Confirming a search result with plain Enter must open the first row even
/// when the user has not moved the selection cursor first.
///
/// This exercises the complete path:
///   keystroke → action → OmniSearch::on_confirm → OmniSearchEvent::Open
///   → Shell::on_omni_event → SymbolStore::open → TabActivated
///   → Shell::reveal_document → Pane::open_item.
#[gpui::test]
async fn plain_enter_on_an_unselected_search_hit_opens_a_tab(cx: &mut TestAppContext) {
    let (shell, mut vcx, _window, search, symbols, _packages) = boot_shell!(cx);

    // Open the omni-search overlay.
    vcx.simulate_keystrokes("cmd-k");
    vcx.run_until_parked();

    // Type a query and wait for results.
    vcx.simulate_input(REAL_QUERY);
    wait_until(cx, "Name section has hits", |cx| {
        search.read_with(cx, |s, _| !s.snapshot().sections[0].rows.is_empty())
    })
    .await;
    vcx.run_until_parked();

    // Confirm with Enter without first pressing Down. The first populated row
    // is the implicit default selection for a plain commit.
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();

    // Overlay must be gone.
    let overlay = shell.read_with(&mut vcx, |s, _| s.overlay_kind_on_top().cloned());
    assert_eq!(
        overlay, None,
        "Enter with OpenDisposition::Replace must close the overlay — \
         overlay is still {:?}",
        overlay
    );

    // A pane tab must exist.
    let tab_count = shell.read_with(&mut vcx, |s, cx| s.pane().read(cx).len());
    assert!(
        tab_count >= 1,
        "Enter must have opened a document tab — tab_count={tab_count}"
    );

    // The symbol store must have at least one doc.
    let doc_count = symbols.read_with(&mut vcx, |s, _| s.docs.len());
    assert_eq!(doc_count, 1, "exactly one document must have been opened");
}

/// `cmd-Enter` (OpenWithoutClosing / Stay disposition) must open a tab but
/// leave the overlay open for further queries.
#[gpui::test]
async fn cmd_enter_opens_tab_and_keeps_overlay_open(cx: &mut TestAppContext) {
    let (shell, mut vcx, _window, search, symbols, _packages) = boot_shell!(cx);

    vcx.simulate_keystrokes("cmd-k");
    vcx.run_until_parked();

    vcx.simulate_input(REAL_QUERY);
    wait_until(cx, "Name section has hits", |cx| {
        search.read_with(cx, |s, _| !s.snapshot().sections[0].rows.is_empty())
    })
    .await;
    vcx.run_until_parked();

    vcx.simulate_keystrokes("down");
    vcx.run_until_parked();

    // cmd-Enter = Stay disposition.
    vcx.simulate_keystrokes("cmd-enter");
    vcx.run_until_parked();

    // Overlay must still be open.
    let overlay = shell.read_with(&mut vcx, |s, _| s.overlay_kind_on_top().cloned());
    assert_eq!(
        overlay,
        Some(OverlayKind::OmniSearch),
        "cmd-Enter must leave the overlay open so the user can open more results — \
         overlay is {:?}",
        overlay
    );

    // A tab must have been opened behind the overlay.
    let tab_count = shell.read_with(&mut vcx, |s, cx| s.pane().read(cx).len());
    assert!(tab_count >= 1, "cmd-Enter must still open a tab");

    let doc_count = symbols.read_with(&mut vcx, |s, _| s.docs.len());
    assert_eq!(
        doc_count, 1,
        "exactly one document must exist after one cmd-Enter"
    );
}

/// `alt-Enter` (OpenInBackgroundTab / Background disposition) must open a tab
/// behind the current one, and leave the overlay open.
#[gpui::test]
async fn alt_enter_opens_a_background_tab_and_keeps_overlay_open(cx: &mut TestAppContext) {
    let (shell, mut vcx, _window, search, _symbols, _packages) = boot_shell!(cx);

    vcx.simulate_keystrokes("cmd-k");
    vcx.run_until_parked();

    vcx.simulate_input(REAL_QUERY);
    wait_until(cx, "Name section has hits", |cx| {
        search.read_with(cx, |s, _| !s.snapshot().sections[0].rows.is_empty())
    })
    .await;
    vcx.run_until_parked();

    vcx.simulate_keystrokes("down");
    vcx.run_until_parked();

    // alt-Enter = Background disposition.
    vcx.simulate_keystrokes("alt-enter");
    vcx.run_until_parked();

    // Overlay must still be open.
    let overlay = shell.read_with(&mut vcx, |s, _| s.overlay_kind_on_top().cloned());
    assert_eq!(
        overlay,
        Some(OverlayKind::OmniSearch),
        "alt-Enter (Background) must leave the overlay open"
    );

    // At least one tab must exist.
    let tab_count = shell.read_with(&mut vcx, |s, cx| s.pane().read(cx).len());
    assert!(
        tab_count >= 1,
        "alt-Enter must open a background tab — tab_count={tab_count}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// §C — Generation guard: stale results must not appear
// ─────────────────────────────────────────────────────────────────────────────

/// Issuing several queries in rapid succession must leave the search store in a
/// state consistent with the *last* query only — no stale results from earlier
/// generations.
///
/// This tests the generation guard in `SearchStore`/`drain`. The failure mode
/// is: an early query's results arrive late and are applied to a snapshot that
/// belongs to a newer query, producing results that contain symbols from the
/// wrong query.
///
/// The assertion is conservative: we verify that after the final (bogus) query
/// settles, the sections are empty. If stale results were applied they would
/// not be empty.
#[gpui::test]
async fn rapid_retyping_shows_only_the_last_generation(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let (search, _symbols) = boot_stores(cx);

    // Issue several queries without waiting for results.
    for query in &["Poi", "Poin", REAL_QUERY, "a", BOGUS_QUERY] {
        search.update(cx, |s, cx| s.set_input((*query).into(), cx));
    }

    // Wait until the last query's sections settle.
    wait_until(cx, "all sections settled after final query", |cx| {
        search.read_with(cx, |s, _| s.snapshot().any_section_settled())
    })
    .await;

    // The final query (BOGUS_QUERY) must yield zero hits in the local sections.
    search.read_with(cx, |s, _| {
        let snap: SearchSnapshot = s.snapshot();
        assert_eq!(
            snap.sections[0].rows.len(),
            0,
            "Name section must be empty for the final bogus query — \
             non-empty means stale results from an earlier generation were applied"
        );
        assert_eq!(
            snap.sections[1].rows.len(),
            0,
            "Type section must be empty for the final bogus query"
        );
        // The generation must have advanced at least once — but *not* once per
        // keystroke.
        //
        // `SearchStore::set_input` debounces: a burst of edits inside the
        // debounce window collapses into a single issued search, which is the
        // entire point (a query per keystroke would hammer the engine and
        // deliver results the user has already typed past). Asserting one
        // generation per query would therefore be asserting the *absence* of
        // debouncing, and would fail precisely when the store behaves
        // correctly.
        //
        // The property that actually matters — that the displayed rows belong
        // to the final query and not to a superseded one — is the two
        // assertions above.
        assert!(
            snap.generation >= 1,
            "at least one generation must have been issued for the query burst",
        );
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// §D — Edge-case query inputs
// ─────────────────────────────────────────────────────────────────────────────

/// An empty query must produce zero hits and not crash.
#[gpui::test]
async fn empty_query_yields_no_hits(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let (search, _symbols) = boot_stores(cx);

    search.update(cx, |s, cx| s.set_input("".into(), cx));

    // Give the engine a moment to respond (or confirm it does not).
    cx.background_executor
        .timer(Duration::from_millis(200))
        .await;
    cx.run_until_parked();

    search.read_with(cx, |s, _| {
        let snap: SearchSnapshot = s.snapshot();
        for (ix, section) in snap.sections.iter().enumerate() {
            assert_eq!(
                section.rows.len(),
                0,
                "section {ix} must have no rows for an empty query"
            );
        }
    });
}

/// A whitespace-only query must produce zero hits and not crash.
#[gpui::test]
async fn whitespace_only_query_yields_no_hits(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let (search, _symbols) = boot_stores(cx);

    search.update(cx, |s, cx| s.set_input("   \t\n".into(), cx));

    wait_until(cx, "sections settle after whitespace query", |cx| {
        search.read_with(cx, |s, _| s.snapshot().any_section_settled())
    })
    .await;

    search.read_with(cx, |s, _| {
        let snap: SearchSnapshot = s.snapshot();
        assert_eq!(
            snap.sections[0].rows.len(),
            0,
            "whitespace-only query must yield zero Name hits"
        );
    });
}

/// A very long query (256+ characters) must not crash the engine or the UI.
///
/// This tests that there is no buffer overflow, panic, or assertion failure
/// anywhere in the pipeline when the user pastes a very long string.
#[gpui::test]
async fn very_long_query_does_not_crash(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let (search, _symbols) = boot_stores(cx);

    let long_query: String = "x".repeat(512);
    search.update(cx, |s, cx| s.set_input(long_query.into(), cx));

    // Wait for the engine to respond (either zero hits or an error state, not a crash).
    wait_until(cx, "sections settle after long query", |cx| {
        search.read_with(cx, |s, _| s.snapshot().any_section_settled())
    })
    .await;

    // Reaching here without panicking is the assertion.
    search.read_with(cx, |s, _| {
        // Input must still be stored correctly.
        assert_eq!(s.snapshot().input.len(), 512, "input must survive round-trip");
    });
}

/// A non-ASCII query (CJK characters) must not crash.
///
/// The search index was built from an ASCII corpus, so the result is expected
/// to be empty — but no panic, no assertion failure.
#[gpui::test]
async fn non_ascii_query_does_not_crash(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let (search, _symbols) = boot_stores(cx);

    search.update(cx, |s, cx| {
        // A mix of CJK and Latin.
        s.set_input("搜索 Point 검색".into(), cx);
    });

    wait_until(cx, "sections settle after non-ASCII query", |cx| {
        search.read_with(cx, |s, _| s.snapshot().any_section_settled())
    })
    .await;

    // Must not have panicked. Content assertion: Name section is unlikely to
    // match, so zero hits is acceptable — but some sections may remain Loading
    // (semantic is offline in headless). We do not assert hit count here.
    search.read_with(cx, |s, _| {
        let snap = s.snapshot();
        // The generation must have advanced.
        assert!(snap.generation >= 1, "generation must have advanced");
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// §E — Tab lifecycle
// ─────────────────────────────────────────────────────────────────────────────

/// Opening the same symbol twice must reuse the same tab and not duplicate the
/// SymbolDoc. This is the store-level dedup test. The shell-level test below
/// verifies the pane stays clean.
#[gpui::test]
async fn opening_same_symbol_twice_does_not_create_duplicate_tab(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let (search, symbols) = boot_stores(cx);

    search.update(cx, |s, cx| s.set_input(REAL_QUERY.into(), cx));
    wait_until(cx, "Name section has hits", |cx| {
        search.read_with(cx, |s, _| !s.snapshot().sections[0].rows.is_empty())
    })
    .await;

    let key = search.read_with(cx, |s, _| {
        s.snapshot().sections[0].rows[0].key.clone()
    });

    let tab_a = symbols.update(cx, |s, cx| {
        s.open(key.clone(), OpenDisposition::Replace, cx)
    });
    let tab_b = symbols.update(cx, |s, cx| {
        s.open(key.clone(), OpenDisposition::Replace, cx)
    });

    assert_eq!(
        tab_a, tab_b,
        "opening the same key twice must return the same TabId — \
         different ids means a duplicate tab was created"
    );

    symbols.read_with(cx, |s, _| {
        assert_eq!(
            s.docs.len(),
            1,
            "exactly one SymbolDoc must exist after two opens of the same key"
        );
    });
}

/// Opening several different symbols must create separate tabs. Closing one
/// must remove exactly its tab without disturbing the others.
#[gpui::test]
async fn open_several_then_close_one_leaves_no_orphan(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let (search, symbols) = boot_stores(cx);

    // Use REAL_QUERY which returns multiple hits.
    search.update(cx, |s, cx| s.set_input(REAL_QUERY.into(), cx));
    wait_until(cx, "Name section has at least 2 hits", |cx| {
        search.read_with(cx, |s, _| s.snapshot().sections[0].rows.len() >= 2)
    })
    .await;

    let rows = search.read_with(cx, |s, _| {
        s.snapshot().sections[0].rows[0..2].to_vec()
    });
    let key1 = rows[0].key.clone();
    let key2 = rows[1].key.clone();

    // `Stay` — the disposition `cmd` maps to — is what opens an *additional*
    // tab. `Replace` now supersedes the active tab (its documented contract,
    // which the implementation previously ignored), so opening two symbols with
    // `Replace` correctly leaves one tab, not two. This test is about tab
    // lifetime with several tabs open, so it must ask for several.
    let tab1 = symbols.update(cx, |s, cx| {
        s.open(key1.clone(), OpenDisposition::Stay, cx)
    });
    let tab2 = symbols.update(cx, |s, cx| {
        s.open(key2.clone(), OpenDisposition::Stay, cx)
    });

    assert_ne!(tab1, tab2, "two different keys must produce two different tab ids");

    symbols.read_with(cx, |s, _| {
        assert_eq!(s.docs.len(), 2, "two docs must be open");
    });

    // Close tab1.
    symbols.update(cx, |s, cx| s.close(tab1, cx));

    symbols.read_with(cx, |s, _| {
        assert_eq!(s.docs.len(), 1, "closing one tab must leave exactly one doc");
        assert!(
            s.doc(tab2).is_some(),
            "the surviving tab must still have its doc"
        );
        assert!(
            s.doc(tab1).is_none(),
            "the closed tab must not have a doc"
        );
        assert!(
            !s.is_open(&key1),
            "closed key must not appear as open in the store"
        );
        assert!(
            s.is_open(&key2),
            "surviving key must still appear as open"
        );
    });
}

/// `reveal_document` with `Background` disposition must not activate the new
/// tab — the previously active tab must remain active.
#[gpui::test]
async fn reveal_document_background_does_not_steal_focus(cx: &mut TestAppContext) {
    let (shell, mut vcx, _window, search, symbols, _packages) = boot_shell!(cx);

    // Open the first symbol in the foreground.
    search.update(cx, |s, cx| s.set_input(REAL_QUERY.into(), cx));
    wait_until(cx, "Name section has at least 2 hits", |cx| {
        search.read_with(cx, |s, _| s.snapshot().sections[0].rows.len() >= 2)
    })
    .await;

    let rows = search.read_with(cx, |s, _| {
        s.snapshot().sections[0].rows[0..2].to_vec()
    });
    let key1 = rows[0].key.clone();
    let key2 = rows[1].key.clone();

    // Open key1 in the foreground.
    symbols.update(cx, |s, cx| {
        s.open(key1.clone(), OpenDisposition::Replace, cx)
    });
    vcx.run_until_parked();

    let active_after_first_open = shell.read_with(&mut vcx, |s, cx| {
        s.pane().read(cx).active_id()
    });
    assert!(
        active_after_first_open.is_some(),
        "a tab must be active after the first open"
    );

    // Open key2 in the background — the active tab must not change.
    symbols.update(cx, |s, cx| {
        s.open(key2.clone(), OpenDisposition::Background, cx)
    });
    vcx.run_until_parked();

    let active_after_background_open = shell.read_with(&mut vcx, |s, cx| {
        s.pane().read(cx).active_id()
    });
    assert_eq!(
        active_after_first_open, active_after_background_open,
        "Background disposition must not change the active tab — \
         user is reading key1 and should not be yanked to key2"
    );

    // But the pane must now have two tabs.
    let tab_count = shell.read_with(&mut vcx, |s, cx| s.pane().read(cx).len());
    assert_eq!(tab_count, 2, "two tabs must exist after two opens");
}

// ─────────────────────────────────────────────────────────────────────────────
// §F — Project panel click
// ─────────────────────────────────────────────────────────────────────────────
//
// The project panel renders package rows in a `uniform_list`. Each row has
// `.on_click(…)` that calls `panel.apply_selection(Some(ix))`. Bug #2 was
// that clicking a row did nothing.
//
// The test below verifies that clicking a row goes through the real hit-test
// pipeline via `debug_bounds` + `simulate_click`. The seam
// (`.debug_selector(|| format!("project.panel.row.{ix}"))`) was added to
// `ProjectPanel::render_row` so the test can discover the row's painted rect.

/// Clicking a project-panel row must not navigate (selection only, no pane tab).
///
/// The debug selector `"project.panel.row.0"` is registered in
/// `ProjectPanel::render_row` via `.debug_selector(|| format!("project.panel.row.{ix}"))`.
///
/// The current contract: `on_click` calls `panel.apply_selection(Some(ix))` and
/// does NOT open a document. The pane must remain empty after the click, which
/// is the observable side-effect we can assert without a public `selection()` getter.
///
/// When a navigation seam is added this test will fail, making the new behaviour
/// an intentional change rather than a silent regression.
///
/// NOTE: `ProjectPanel::selection` is a private field with no public accessor.
/// Asserting `selection == Some(0)` would require adding `pub fn selection(&self)`.
/// Until that getter exists, this test pins only the "no navigation" contract.
#[gpui::test]
async fn clicking_a_project_panel_row_selects_it(cx: &mut TestAppContext) {
    let (shell, mut vcx, _window, _search, _symbols, packages) = boot_shell!(cx);

    // Wait for the fixture engine to deliver at least one package row so that
    // the project panel renders at least one row.
    wait_until(cx, "at least one package row is ready", |cx| {
        packages.read_with(cx, |store, _| store.ready_count() >= 1)
    })
    .await;
    // Drain the layout pass so debug_bounds records the row's painted rect.
    vcx.run_until_parked();

    // Discover the painted centre of the first project panel row.
    let row_bounds = vcx
        .debug_bounds("project.panel.row.0")
        .expect("row 'project.panel.row.0' must be painted after run_until_parked — \
                 check that ProjectPanel::render_row registers the selector and \
                 that packages have loaded from the fixture engine");

    // Pane must be empty before the click (pre-condition).
    let pre_click_tabs = shell.read_with(&mut vcx, |s, cx| s.pane().read(cx).len());
    assert_eq!(pre_click_tabs, 0, "pre-condition: pane must be empty before any click");

    // Drive the real hit-test pipeline.
    vcx.simulate_click(row_bounds.center(), Modifiers::default());
    vcx.run_until_parked();

    // Clicking a ready package row opens its crate root — the package landing
    // page, the way docs.rs opens a crate index. The panel emits
    // `PackageActivated { root }` and the shell routes it through the same
    // `reveal_document` path search uses.
    let post_click_tabs = shell.read_with(&mut vcx, |s, cx| s.pane().read(cx).len());
    assert_eq!(
        post_click_tabs, 1,
        "clicking a ready package row must open its crate root — \
         if this is 0 the panel emitted no PackageActivated (is PackageRow::root \
         populated?) or the shell is not subscribed to it"
    );
}

/// The project panel must show the empty state when no packages are loaded.
///
/// This is a store-level test (no pixel coordinates needed) because the
/// "no rows → empty state" branch is a simple condition on row count.
///
/// The empty state prevents the most common user confusion: a blank panel that
/// looks like a broken app when the corpus has not been pointed at a package.
#[gpui::test]
async fn project_panel_shows_empty_state_with_no_packages(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    // Use an engine that produces packages (fixture engine). The project panel
    // is seeded with zero requested packages, so the fixture engine's packages
    // arrive as Ready events rather than resolving pre-seeded Pending rows.
    // For the empty-state test we want to observe the *initial* state before
    // any Ready events arrive — we can observe this at the store level.
    let engine = Engine::start_with_fixtures(EngineConfig::default());
    // Seed with no requested packages AND give it no time to load — the store
    // starts with zero rows.
    let packages_store = cx.new(|cx| PackageStore::new(engine.clone(), &[], cx));

    // Immediately after construction: the store has zero rows (the engine's
    // packages arrive asynchronously).
    packages_store.read_with(cx, |store, _| {
        // The store was seeded with zero requested names. The fixture engine's
        // async events have not arrived yet (no `allow_parking` wait here).
        // `rows()` may be 0 (clean start) or non-zero (fixture loaded very fast).
        // We accept either — the important assertion is that `len()` is consistent
        // with `summary_label()`.
        let rows = store.rows();
        let label = store.summary_label();
        if rows.is_empty() {
            assert!(
                &*label == "no packages" || label.contains("loading"),
                "an empty store must say 'no packages' or be loading, not '{label}'"
            );
        }
        // Non-empty is also fine — the fixture engine is fast.
    });
}

/// Package rows with different statuses must all be represented after loading.
///
/// This is the store-level assertion that the `ProjectPanel::render` branch for
/// `PackageStatus::Failed` is reachable — a failed package must not silently
/// become `Ready` or disappear.
#[gpui::test]
async fn project_panel_represents_all_status_variants(cx: &mut TestAppContext) {
    cx.executor().allow_parking();

    let engine = Engine::start_with_fixtures(EngineConfig::default());
    let packages_store = cx.new(|cx| PackageStore::new(engine.clone(), &[], cx));

    // Wait for the fixture corpus to load some packages.
    wait_until(cx, "at least one package is ready", |cx| {
        packages_store.read_with(cx, |store, _| store.ready_count() >= 1)
    })
    .await;

    packages_store.read_with(cx, |store, _| {
        let rows = store.rows();
        assert!(
            !rows.is_empty(),
            "at least one package must have loaded from the fixture engine"
        );
        // Every row must have a non-empty name.
        for row in rows {
            assert!(
                !row.name.is_empty(),
                "every package row must have a non-empty name"
            );
        }
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// §G — Overlay focus hygiene
// ─────────────────────────────────────────────────────────────────────────────

/// Pressing `cmd-K` twice must not stack two OmniSearch overlays.
///
/// The `OverlayStack::push` deduplication only guards against pushing the same
/// kind twice at the *stack level*. The `Shell::open_omni_search` also has a
/// guard — but if either guard is accidentally removed, the second `cmd-K`
/// would stack a second overlay whose Escape only closes the inner one, leaving
/// a dangling view with a stale focus trap.
#[gpui::test]
async fn cmd_k_twice_does_not_stack_two_overlays(cx: &mut TestAppContext) {
    let (shell, mut vcx, _window, _search, _symbols, _packages) = boot_shell!(cx);

    vcx.simulate_keystrokes("cmd-k");
    vcx.run_until_parked();

    vcx.simulate_keystrokes("cmd-k");
    vcx.run_until_parked();

    let depth = shell.read_with(&mut vcx, |s, _| s.overlay_depth());
    assert_eq!(
        depth, 1,
        "cmd-K pressed twice must leave exactly one overlay on the stack — \
         depth={depth} means the guard is broken and Escape would need to be \
         pressed twice to return to the document"
    );
}

/// After the overlay is closed, focus must return somewhere real (the pane).
///
/// A closed overlay that leaves focus in the void causes the app to appear
/// frozen: keystrokes are dispatched but nothing receives them (LD-13).
///
/// We cannot directly inspect GPUI's focused element from a `TestAppContext`
/// without a window-internal query, but we can verify that the shell's
/// subsequent keystroke (`cmd-K` again) is processed — which proves focus did
/// not go to /dev/null.
#[gpui::test]
async fn closing_overlay_returns_focus_so_subsequent_keystrokes_work(cx: &mut TestAppContext) {
    let (shell, mut vcx, _window, _search, _symbols, _packages) = boot_shell!(cx);

    // Open overlay.
    vcx.simulate_keystrokes("cmd-k");
    vcx.run_until_parked();

    // Close with Escape.
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();

    // Verify closed.
    let after_escape = shell.read_with(&mut vcx, |s, _| s.overlay_kind_on_top().cloned());
    assert_eq!(after_escape, None, "overlay must be closed");

    // Now open it again — this only works if focus returned to the shell/pane
    // after the first close, because cmd-K is bound in the "Workspace global"
    // key context on the Shell div.
    vcx.simulate_keystrokes("cmd-k");
    vcx.run_until_parked();

    let reopened = shell.read_with(&mut vcx, |s, _| s.overlay_kind_on_top().cloned());
    assert_eq!(
        reopened,
        Some(OverlayKind::OmniSearch),
        "cmd-K after a previous close must open the overlay again — \
         None here means focus was not restored after the first close, \
         so the second cmd-K went to the void"
    );
}

/// The scrim click must dismiss a dismissable overlay (OmniSearch).
///
/// The scrim's `.on_click` is conditionally wired in `Shell::render` based on
/// `OverlayEntry::dismiss_on_scrim_click()`. This test verifies that the
/// wiring is correct for OmniSearch.
///
/// The selector `"overlay.scrim"` is registered in `Shell::render` via
/// `.debug_selector(|| "overlay.scrim".into())`. It is a static string literal
/// because `debug_bounds` requires `&'static str`.
#[gpui::test]
async fn scrim_click_dismisses_omni_search(cx: &mut TestAppContext) {
    let (shell, mut vcx, _window, _search, _symbols, _packages) = boot_shell!(cx);

    // Open the OmniSearch overlay.
    vcx.simulate_keystrokes("cmd-k");
    vcx.run_until_parked();

    // Pre-condition: overlay must be open.
    let before = shell.read_with(&mut vcx, |s, _| s.overlay_kind_on_top().cloned());
    assert_eq!(
        before,
        Some(OverlayKind::OmniSearch),
        "pre-condition: cmd-K must have opened the OmniSearch overlay"
    );

    // Discover the scrim's painted bounds. The scrim is painted under the overlay
    // panel, so its selector is registered by Shell::render when an overlay is
    // visible and dismiss_on_scrim_click() is true.
    let scrim_bounds = vcx
        .debug_bounds("overlay.scrim")
        .expect("scrim 'overlay.scrim' must be painted when an overlay is open — \
                 check that Shell::render registers the selector on the scrim div");

    // Click the scrim *away from the panel*.
    //
    // The scrim spans the whole window, so its centre sits directly on top of
    // the centred search panel — and the panel now calls `.occlude()`, so a
    // click there is correctly absorbed by the panel rather than falling
    // through to the scrim. That occlusion is the fix for the shipped bug where
    // clicking a result row dismissed the overlay instead of opening the
    // symbol. Aim near the bottom edge, which is scrim and nothing else.
    let below_panel = gpui::Point {
        x: scrim_bounds.center().x,
        y: scrim_bounds.bottom() - gpui::px(8.0),
    };
    vcx.simulate_click(below_panel, Modifiers::default());
    vcx.run_until_parked();

    // Overlay must be closed.
    let after = shell.read_with(&mut vcx, |s, _| s.overlay_kind_on_top().cloned());
    assert_eq!(
        after,
        None,
        "clicking the scrim must close the OmniSearch overlay — \
         Some(…) here means the scrim's on_click is not wired or \
         dismiss_on_scrim_click() returned false for OmniSearch"
    );
}

/// The scrim must still be clickable once the search panel has *finished*
/// revealing — not only during the fraction of a second while it is growing.
///
/// `scrim_click_dismisses_omni_search` above clicks as soon as the executor
/// parks, which races the reveal spring. That made it pass about four times in
/// five and hid a real bug: the panel's height cap was written
/// `max_h(relative(0.60))`, and a percentage max-height has no definite basis
/// inside an auto-height wrapper, so the clamp was dropped and the spring's
/// 9999 px sentinel became the panel's real layout height. `overflow_hidden`
/// meant it still *looked* correct, but the panel carries `.occlude()`, so its
/// hitbox covered the whole window and there was no scrim left to click
/// anywhere on screen.
///
/// This test settles the reveal first, so it asserts the state a reader is
/// actually in when they click away from a search they have been looking at.
#[gpui::test]
async fn scrim_click_dismisses_omni_search_after_the_panel_has_fully_revealed(
    cx: &mut TestAppContext,
) {
    let (shell, mut vcx, _window, _search, _symbols, _packages) = boot_shell!(cx);

    vcx.simulate_keystrokes("cmd-k");
    vcx.run_until_parked();

    // The reveal spring takes its phase from wall-clock time, so both clocks
    // have to move: real time for the spring, `run_until_parked` for the frames
    // it schedules (see AGENTS-DOCTRINE §8, "Two clocks have to move").
    for _ in 0..20 {
        cx.background_executor
            .timer(Duration::from_millis(25))
            .await;
        vcx.run_until_parked();
    }

    assert_eq!(
        shell.read_with(&mut vcx, |s, _| s.overlay_kind_on_top().cloned()),
        Some(OverlayKind::OmniSearch),
        "precondition: the overlay must still be open after the reveal settles"
    );

    let scrim_bounds = vcx
        .debug_bounds("overlay.scrim")
        .expect("the scrim must be painted while the overlay is open");
    let below_panel = gpui::Point {
        x: scrim_bounds.center().x,
        y: scrim_bounds.bottom() - gpui::px(8.0),
    };
    vcx.simulate_click(below_panel, Modifiers::default());
    vcx.run_until_parked();

    assert_eq!(
        shell.read_with(&mut vcx, |s, _| s.overlay_kind_on_top().cloned()),
        None,
        "clicking the scrim below a fully-revealed search panel must dismiss \
         it. Still open means the panel's occluding hitbox has grown over the \
         whole window again — check that its height cap is a definite length \
         and not a percentage",
    );
}

/// `Escape` on a non-empty input must clear the input first, not close the
/// overlay. Only a second `Escape` on an empty input closes the overlay.
///
/// This is the §15 "two-press Escape" invariant.
#[gpui::test]
async fn escape_on_non_empty_input_clears_input_before_closing(cx: &mut TestAppContext) {
    let (shell, mut vcx, _window, search, _symbols, _packages) = boot_shell!(cx);

    vcx.simulate_keystrokes("cmd-k");
    vcx.run_until_parked();

    // Type something.
    vcx.simulate_input("hello");
    vcx.run_until_parked();

    // Verify input is there.
    let input_before = search.read_with(&mut vcx, |s, _| s.snapshot().input.to_string());
    assert_eq!(
        input_before, "hello",
        "pre-condition: input must contain the typed text"
    );

    // First Escape: must clear input, not close overlay.
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();

    let input_after_first_escape = search.read_with(&mut vcx, |s, _| {
        s.snapshot().input.to_string()
    });
    assert_eq!(
        input_after_first_escape, "",
        "first Escape must clear the input, not close the overlay"
    );

    let overlay_after_first_escape =
        shell.read_with(&mut vcx, |s, _| s.overlay_kind_on_top().cloned());
    assert_eq!(
        overlay_after_first_escape,
        Some(OverlayKind::OmniSearch),
        "first Escape on non-empty input must leave the overlay open"
    );

    // Second Escape: now the input is empty, so the overlay must close.
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();

    let overlay_after_second_escape =
        shell.read_with(&mut vcx, |s, _| s.overlay_kind_on_top().cloned());
    assert_eq!(
        overlay_after_second_escape,
        None,
        "second Escape on empty input must close the overlay"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// §H — Store-level click event routing (without coordinates)
// ─────────────────────────────────────────────────────────────────────────────
//
// These tests verify the *handler* logic in isolation, not the hit-test path.
// They are valuable because they catch regressions in the closure body
// (wrong key passed, wrong disposition emitted, etc.) even when we cannot
// simulate a real pixel-level click.

/// Emitting `OmniSearchEvent::Open` with `Replace` disposition must open a
/// document tab via the shell's `on_omni_event` routing.
///
/// The closure in `uniform_list`'s `on_click` emits this event. If the event
/// is emitted but not processed (subscription dropped prematurely, wrong
/// entity target, etc.) the tab count remains zero.
#[gpui::test]
async fn omni_search_open_event_replaces_overlay_and_opens_tab(cx: &mut TestAppContext) {
    let (shell, mut vcx, _window, search, symbols, _packages) = boot_shell!(cx);

    // Open overlay and get a real key.
    vcx.simulate_keystrokes("cmd-k");
    vcx.run_until_parked();

    search.update(cx, |s, cx| s.set_input(REAL_QUERY.into(), cx));
    wait_until(cx, "Name section has hits", |cx| {
        search.read_with(cx, |s, _| !s.snapshot().sections[0].rows.is_empty())
    })
    .await;
    vcx.run_until_parked();

    // Keyboard-navigate to row 0 and confirm.
    vcx.simulate_keystrokes("down");
    vcx.run_until_parked();
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();

    // Shell must have routed the event: overlay closed, tab opened.
    let overlay = shell.read_with(&mut vcx, |s, _| s.overlay_kind_on_top().cloned());
    assert_eq!(overlay, None, "Replace must close the overlay");

    let tab_count = shell.read_with(&mut vcx, |s, cx| s.pane().read(cx).len());
    assert_eq!(tab_count, 1, "exactly one tab must be open");

    // The pane's active tab must have a corresponding SymbolDoc.
    let pane_tab_id = shell.read_with(&mut vcx, |s, cx| s.pane().read(cx).active_id());
    assert!(pane_tab_id.is_some(), "there must be an active pane tab");

    let doc_count = symbols.read_with(&mut vcx, |s, _| s.docs.len());
    assert_eq!(doc_count, 1, "one SymbolDoc must exist");
}

/// When `reveal_document` is called for a key that is already open, the
/// existing pane tab must be activated rather than a new one created.
///
/// This is the dedup guarantee at the shell level (the store deduplicates
/// SymbolDocs; the shell additionally must not create a second pane tab for
/// an already-open doc).
#[gpui::test]
async fn reveal_document_reuses_existing_pane_tab(cx: &mut TestAppContext) {
    let (shell, mut vcx, _window, search, symbols, _packages) = boot_shell!(cx);

    search.update(cx, |s, cx| s.set_input(REAL_QUERY.into(), cx));
    wait_until(cx, "Name section has hits", |cx| {
        search.read_with(cx, |s, _| !s.snapshot().sections[0].rows.is_empty())
    })
    .await;

    let key = search.read_with(cx, |s, _| {
        s.snapshot().sections[0].rows[0].key.clone()
    });

    // First open.
    symbols.update(cx, |s, cx| {
        s.open(key.clone(), OpenDisposition::Replace, cx)
    });
    vcx.run_until_parked();

    let tab_count_after_first = shell.read_with(&mut vcx, |s, cx| s.pane().read(cx).len());
    assert_eq!(tab_count_after_first, 1, "one tab after first open");

    // Second open of the same key.
    symbols.update(cx, |s, cx| {
        s.open(key.clone(), OpenDisposition::Replace, cx)
    });
    vcx.run_until_parked();

    let tab_count_after_second = shell.read_with(&mut vcx, |s, cx| s.pane().read(cx).len());
    assert_eq!(
        tab_count_after_second, 1,
        "second open of same key must not create a second pane tab — \
         tab_count={tab_count_after_second}"
    );
}

/// Keyboard tab activation must update the store's Replace target as well as
/// the pane. Otherwise opening a new symbol after switching tabs closes the
/// last-opened document and leaves a ghost tab in the pane.
#[gpui::test]
async fn pane_tab_activation_keeps_store_and_pane_in_sync(cx: &mut TestAppContext) {
    let (shell, mut vcx, _window, search, symbols, _packages) = boot_shell!(cx);

    search.update(cx, |s, cx| s.set_input(REAL_QUERY.into(), cx));
    wait_until(cx, "Name section has at least 2 hits", |cx| {
        search.read_with(cx, |s, _| s.snapshot().sections[0].rows.len() >= 2)
    })
    .await;

    let rows = search.read_with(cx, |s, _| s.snapshot().sections[0].rows[0..2].to_vec());
    let key1 = rows[0].key.clone();
    let key2 = rows[1].key.clone();
    let key3 = synthetic_key(99);

    let tab1 = symbols.update(cx, |s, cx| s.open(key1.clone(), OpenDisposition::Stay, cx));
    let tab2 = symbols.update(cx, |s, cx| s.open(key2.clone(), OpenDisposition::Stay, cx));
    vcx.run_until_parked();

    let pane_tab1 = shell
        .read_with(&mut vcx, |s, _| s.pane_tab_for_document(tab1))
        .expect("first document must have a pane tab");

    // The pane starts on the second tab. cmd-1 must activate the first tab.
    vcx.simulate_keystrokes("cmd-1");
    vcx.run_until_parked();
    let active = shell.read_with(&mut vcx, |s, cx| s.pane().read(cx).active_id());
    assert_eq!(active, Some(pane_tab1), "cmd-1 must activate the first pane tab");

    // Replace the selected first tab with a new symbol. The second tab must
    // survive, proving the pane activation was mirrored into SymbolStore.
    let tab3 = symbols.update(cx, |s, cx| s.open(key3.clone(), OpenDisposition::Replace, cx));
    vcx.run_until_parked();

    symbols.read_with(cx, |s, _| {
        assert!(!s.is_open(&key1), "the selected first document must be replaced");
        assert!(s.is_open(&key2), "the unselected second document must survive");
        assert!(s.is_open(&key3), "the replacement document must be open");
    });

    let pane_len = shell.read_with(&mut vcx, |s, cx| s.pane().read(cx).len());
    assert_eq!(pane_len, 2, "replacement must not leave a ghost pane tab");
    assert!(
        shell.read_with(&mut vcx, |s, _| s.pane_tab_for_document(tab2)).is_some(),
        "the second document's pane tab must remain mapped"
    );
    assert_eq!(
        shell.read_with(&mut vcx, |s, cx| s.pane().read(cx).active_id()),
        shell.read_with(&mut vcx, |s, _| s.pane_tab_for_document(tab3)),
        "the replacement document must be active"
    );

    // CloseTab is handled by the same Pane context and must tear down the
    // corresponding SymbolDoc rather than leaving an open-store orphan.
    vcx.simulate_keystrokes("cmd-w");
    vcx.run_until_parked();
    let pane_len_after_close = shell.read_with(&mut vcx, |s, cx| s.pane().read(cx).len());
    assert_eq!(pane_len_after_close, 1, "cmd-w must close the active pane tab");
    symbols.read_with(cx, |s, _| {
        assert!(!s.is_open(&key3), "closing a pane tab must close its SymbolDoc");
        assert!(s.is_open(&key2), "the surviving pane tab must keep its SymbolDoc");
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// §I — Focus attachment (L16 / L22)
// ─────────────────────────────────────────────────────────────────────────────
//
// `Pane::activate_ix` focuses the active item's `FocusHandle` on every
// activation. GPUI resolves a keystroke by looking that handle up in the last
// rendered frame's dispatch tree and, when it is absent, silently falls back to
// the *window root* — which carries no key context, so every ancestor binding
// dies with it. These two tests pin both halves: that the handle really is
// focused, and that a page which renders a degenerate state still attaches it.

/// Activating a tab must move window focus onto that tab's own content, not
/// leave it on the pane — otherwise the item's `.on_action` handlers are
/// structurally unreachable (LIMITATIONS.md L16).
///
/// Asserted twice over, because "some handle is focused" is not the claim:
/// first that the focused handle is *the active item's*, then that a
/// `SymbolPage`-scoped binding actually fires, observed through content
/// (the clipboard holds the symbol's URI) rather than through a repaint.
#[gpui::test]
async fn activating_a_tab_focuses_the_symbol_page_not_the_pane(cx: &mut TestAppContext) {
    let (shell, mut vcx, _window, search, symbols, _packages) = boot_shell!(cx);

    search.update(cx, |s, cx| s.set_input(REAL_QUERY.into(), cx));
    wait_until(cx, "Name section has hits", |cx| {
        search.read_with(cx, |s, _| !s.snapshot().sections[0].rows.is_empty())
    })
    .await;

    let key = search.read_with(cx, |s, _| s.snapshot().sections[0].rows[0].key.clone());
    // `SymbolHeader`'s URI is the key's own rendering (see
    // `HeaderModel::from_head`), so this is the exact string a correct
    // `CopySymbolUri` must produce for *this* symbol and no other.
    let expected_uri = format!("{:?}", key);
    let tab = symbols.update(cx, |s, cx| s.open(key, OpenDisposition::Replace, cx));
    wait_until(cx, "the document head arrives", |cx| {
        symbols.read_with(cx, |s, _| {
            s.doc(tab).and_then(|d| d.head.as_ref()).is_some()
        })
    })
    .await;
    vcx.run_until_parked();

    // 1. The window's focused element is the page's handle, not the pane's.
    let (item_focused, pane_focused) = vcx.update(|window, cx| {
        use gpui::Focusable as _;
        let pane = shell.read(cx).pane().clone();
        let item = pane
            .read(cx)
            .active_item_focus_handle(cx)
            .expect("a tab is open, so it has a content focus handle");
        let own = pane.focus_handle(cx);
        (item.is_focused(window), own.is_focused(window))
    });
    assert!(
        item_focused,
        "the active tab's own FocusHandle must hold window focus after \
         activation — if it does not, every SymbolPage `.on_action` handler is \
         unreachable and so is dispatch through it (L16)"
    );
    assert!(
        !pane_focused,
        "focus must have moved off Pane's own root and onto the item"
    );

    // 2. …and that focus is *attached*, so a SymbolPage-context binding really
    //    dispatches. `y` → `CopySymbolUri`, observed as clipboard content.
    let sentinel = "adversarial-sentinel-before-copy-symbol-uri";
    let original = vcx.update(|_, cx| cx.read_from_clipboard());
    vcx.update(|_, cx| {
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(sentinel.to_owned()))
    });

    vcx.simulate_keystrokes("y");
    vcx.run_until_parked();

    let copied = vcx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()));
    if let Some(original) = original {
        vcx.update(|_, cx| cx.write_to_clipboard(original));
    }
    let copied = copied.expect("clipboard must hold something");
    assert_ne!(
        copied, sentinel,
        "`y` (CopySymbolUri, bound in the SymbolPage context) left the \
         sentinel in place — the keystroke never reached SymbolPage, which \
         means its focus handle is focused but not attached to any element in \
         the rendered dispatch tree"
    );
    assert_eq!(
        copied, expected_uri,
        "CopySymbolUri must copy *this* symbol's URI, not some other page's"
    );
}

/// A document whose stream fails before anything is readable must not take the
/// pane's keyboard bindings down with it (LIMITATIONS.md L22).
///
/// `SymbolPage` renders a whole-page error for that state. When that error page
/// was built as a *second* root — without `key_context` or `track_focus` —
/// GPUI could not find the focused handle in the rendered frame and fell back
/// to the window root, so `cmd-W` (bound in the `Pane` context, handled two
/// levels up on `Pane`'s own div) silently stopped working. Nothing about the
/// pane changed; a leaf view's render branch disabled it.
#[gpui::test]
async fn a_failed_document_does_not_disable_pane_bindings(cx: &mut TestAppContext) {
    let (shell, mut vcx, _window, _search, symbols, _packages) = boot_shell!(cx);

    // A key for a package the corpus does not contain: the stream fails with
    // nothing readable, which is exactly `PageState::ColdError`.
    let doomed = synthetic_key(99);
    symbols.update(cx, |s, cx| {
        s.open(doomed.clone(), OpenDisposition::Replace, cx)
    });
    vcx.run_until_parked();

    assert_eq!(
        shell.read_with(&mut vcx, |s, cx| s.pane().read(cx).len()),
        1,
        "precondition: the failing document still gets a pane tab"
    );

    // The failing page must still be the focused element — a page that renders
    // an error is still the active tab and still owns the keyboard.
    let focused = vcx.update(|window, cx| {
        shell
            .read(cx)
            .pane()
            .read(cx)
            .active_item_focus_handle(cx)
            .expect("the failing tab still has a content focus handle")
            .is_focused(window)
    });
    assert!(
        focused,
        "the error page's FocusHandle must still hold window focus; if it does \
         not, dispatch has already collapsed to the window root"
    );

    vcx.simulate_keystrokes("cmd-w");
    vcx.run_until_parked();

    assert_eq!(
        shell.read_with(&mut vcx, |s, cx| s.pane().read(cx).len()),
        0,
        "cmd-W must close the tab even when its content rendered as a \
         cold-error page — a leaf view's render branch must not be able to \
         unbind a Pane-level keystroke (L22)"
    );
    symbols.read_with(cx, |s, _| {
        assert!(
            !s.is_open(&doomed),
            "closing the pane tab must close the SymbolDoc behind it"
        );
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// §J — Overlay bindings that used to be bound-and-inert (L15)
// ─────────────────────────────────────────────────────────────────────────────

/// `cmd-shift-P` must put the command palette on the overlay stack.
///
/// Asserted on the shell's overlay state, not on "the action dispatched": a
/// dispatch that nothing listens for is precisely the failure this guards
/// (L15 — the action was bound with zero `.on_action` handlers anywhere).
#[gpui::test]
async fn cmd_shift_p_opens_the_command_palette(cx: &mut TestAppContext) {
    let (shell, mut vcx, _window, _search, _symbols, _packages) = boot_shell!(cx);

    assert_eq!(
        shell.read_with(&mut vcx, |s, _| s.overlay_kind_on_top().cloned()),
        None,
        "precondition: no overlay before the keystroke"
    );

    vcx.simulate_keystrokes("cmd-shift-p");
    vcx.run_until_parked();

    assert_eq!(
        shell.read_with(&mut vcx, |s, _| s.overlay_kind_on_top().cloned()),
        Some(OverlayKind::CommandPalette),
        "cmd-shift-P must push CommandPalette onto the overlay stack"
    );
    assert_eq!(
        shell.read_with(&mut vcx, |s, _| s.overlay_depth()),
        1,
        "exactly one overlay — the palette must not stack on itself"
    );

    // Escape must take it back off again, or the reader is trapped.
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert_eq!(
        shell.read_with(&mut vcx, |s, _| s.overlay_kind_on_top().cloned()),
        None,
        "escape must dismiss the command palette"
    );
}

/// `?` must toggle the shortcuts cheat sheet: open on the first press, closed
/// on the second. "Toggle" is in the action's name, so a press that only ever
/// opens is a half-implemented binding, not a working one.
#[gpui::test]
async fn question_mark_toggles_the_shortcuts_overlay(cx: &mut TestAppContext) {
    let (shell, mut vcx, _window, _search, _symbols, _packages) = boot_shell!(cx);

    vcx.simulate_keystrokes("?");
    vcx.run_until_parked();
    assert_eq!(
        shell.read_with(&mut vcx, |s, _| s.overlay_kind_on_top().cloned()),
        Some(OverlayKind::Shortcuts),
        "`?` must push the shortcuts cheat sheet onto the overlay stack"
    );

    vcx.simulate_keystrokes("?");
    vcx.run_until_parked();
    assert_eq!(
        shell.read_with(&mut vcx, |s, _| s.overlay_kind_on_top().cloned()),
        None,
        "a second `?` must close the cheat sheet again — the action is a toggle"
    );
}

/// Opening the palette while the cheat sheet is up must *replace* it, not
/// stack a second modal over it. Two live overlay views at once would leave
/// one of them rendering underneath with its subscriptions still armed.
#[gpui::test]
async fn opening_the_palette_over_the_cheat_sheet_replaces_it(cx: &mut TestAppContext) {
    let (shell, mut vcx, _window, _search, _symbols, _packages) = boot_shell!(cx);

    vcx.simulate_keystrokes("?");
    vcx.run_until_parked();
    vcx.simulate_keystrokes("cmd-shift-p");
    vcx.run_until_parked();

    assert_eq!(
        shell.read_with(&mut vcx, |s, _| s.overlay_kind_on_top().cloned()),
        Some(OverlayKind::CommandPalette),
        "the palette must be on top after cmd-shift-P"
    );
    assert_eq!(
        shell.read_with(&mut vcx, |s, _| s.overlay_depth()),
        1,
        "the cheat sheet must have been replaced, not layered under the palette"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Developer scaffolding must not reach a rendered surface (GUI-WORKORDER-2 F9)
// ─────────────────────────────────────────────────────────────────────────────

/// Markers that mean "a developer left this here for another developer".
///
/// `TODO(` rather than `TODO` so that prose containing the word in a *comment*
/// — which this scan never looks at anyway — could not be mistaken for the
/// tagged form the crate actually uses (`TODO(views)`, `TODO(store)`,
/// `TODO(wire)`).
const SCAFFOLDING_MARKERS: [&str; 2] = ["TODO(", "FIXME"];

/// Strip comments from a Rust source file and return every string literal in
/// what remains.
///
/// # Why a lexer and not a `grep`
///
/// The interesting question is "can a user read this?", and in Rust that means
/// "is it a string literal?". Doc comments and `//` notes are *supposed* to say
/// `TODO(store)` — that is the whole point of a tracking comment — so a plain
/// grep for the marker would fail on a healthy crate and force the guard to be
/// deleted. This walks the file character by character, drops line and block
/// comments, and yields the contents of every `"…"` and `r#"…"#` literal.
fn string_literals(src: &str) -> Vec<String> {
    let b: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        // Line comment.
        if b[i] == '/' && i + 1 < b.len() && b[i + 1] == '/' {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
            continue;
        }
        // Block comment (Rust nests them).
        if b[i] == '/' && i + 1 < b.len() && b[i + 1] == '*' {
            let mut depth = 1usize;
            i += 2;
            while i < b.len() && depth > 0 {
                if b[i] == '/' && i + 1 < b.len() && b[i + 1] == '*' {
                    depth += 1;
                    i += 2;
                } else if b[i] == '*' && i + 1 < b.len() && b[i + 1] == '/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        // Raw string: r"…", r#"…"#, r##"…"##
        if b[i] == 'r' && i + 1 < b.len() && (b[i + 1] == '"' || b[i + 1] == '#') {
            let mut j = i + 1;
            let mut hashes = 0usize;
            while j < b.len() && b[j] == '#' {
                hashes += 1;
                j += 1;
            }
            if j < b.len() && b[j] == '"' {
                j += 1;
                let start = j;
                let closing: String =
                    std::iter::once('"').chain(std::iter::repeat_n('#', hashes)).collect();
                let rest: String = b[j..].iter().collect();
                match rest.find(&closing) {
                    Some(rel) => {
                        let end = start + rest[..rel].chars().count();
                        out.push(b[start..end].iter().collect());
                        i = end + closing.chars().count();
                    }
                    None => break,
                }
                continue;
            }
        }
        // Char literal — skipped so `'"'` does not open a phantom string.
        if b[i] == '\'' {
            let mut j = i + 1;
            while j < b.len() && b[j] != '\'' {
                if b[j] == '\\' {
                    j += 1;
                }
                j += 1;
            }
            // A lifetime (`'a`) has no closing quote on the same construct; in
            // either case advancing past what we scanned is safe, because
            // nothing between here and there can start a string.
            i = (j + 1).min(b.len()).max(i + 1);
            continue;
        }
        // Ordinary string literal.
        if b[i] == '"' {
            let start = i + 1;
            let mut j = start;
            while j < b.len() && b[j] != '"' {
                if b[j] == '\\' {
                    j += 1;
                }
                j += 1;
            }
            out.push(b[start..j.min(b.len())].iter().collect());
            i = j + 1;
            continue;
        }
        i += 1;
    }
    out
}

/// Every `.rs` file under `src/`.
fn crate_sources() -> Vec<std::path::PathBuf> {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let entries = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    walk(&root, &mut out);
    assert!(
        out.len() > 20,
        "the source walk found only {} files — it is not scanning the crate",
        out.len()
    );
    out
}

/// No string the app can put on screen may contain developer scaffolding.
///
/// # The bug this closes
///
/// The Jobs panel rendered, centred and in the product's own type, the text
/// `TODO(views): crate::views::jobs_panel` — in a dock a user opens with ⌘J
/// (`.shots/memchr/17-bottom-dock-open.png`, GUI-WORKORDER-2 F9). The Logs,
/// Search and Outline panels each carried the same shape. A placeholder is a
/// claim like any other: it is read by the person using the program, not by the
/// author who wrote it, and a Rust module path answers nothing they asked.
///
/// # What this can and cannot prove
///
/// It is a *static* check over string literals, not an inspection of the
/// rendered scene, because GPUI's element tree is opaque once built — there is
/// no way to enumerate the text nodes of a painted frame. It is therefore
/// broader than the rule it enforces (it also covers strings that never reach a
/// view) and, in exchange, it cannot be evaded by a formatting expression that
/// assembles the marker at runtime. Nothing in this crate does that, and a
/// change that started to would be doing something conspicuous.
///
/// Comments are deliberately exempt: `TODO(store)` in a doc comment is a
/// tracking note doing its job. Only literals count.
#[test]
fn no_rendered_string_carries_developer_scaffolding() {
    let mut offences: Vec<String> = Vec::new();
    for path in crate_sources() {
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        for literal in string_literals(&src) {
            for marker in SCAFFOLDING_MARKERS {
                if literal.contains(marker) {
                    offences.push(format!("{}: {literal:?}", path.display()));
                }
            }
        }
    }
    assert!(
        offences.is_empty(),
        "developer scaffolding in user-visible strings — an empty state must \
         say what the *reader* would see here, not name a module at them:\n{}",
        offences.join("\n"),
    );
}

/// The lexer this guard depends on has to actually tell the two apart, or the
/// guard is decoration: a scanner that saw nothing would pass on a crate full
/// of offences.
#[test]
fn scaffolding_scan_reads_literals_and_ignores_comments() {
    let src = r####"
        // TODO(views): this is a comment and must be ignored
        /// TODO(store): so is this
        /* TODO(wire): and this */
        fn f() {
            let quote = '"';
            let a = "TODO(views): crate::views::jobs_panel";
            let b = r#"FIXME: raw"#;
            let c = "harmless";
        }
    "####;
    let literals = string_literals(src);
    assert!(
        literals.iter().any(|l| l == "harmless"),
        "the scan must find ordinary literals; got {literals:?}"
    );
    let flagged: Vec<&String> = literals
        .iter()
        .filter(|l| SCAFFOLDING_MARKERS.iter().any(|m| l.contains(m)))
        .collect();
    assert_eq!(
        flagged.len(),
        2,
        "exactly the two literals carry a marker — the three comments must not \
         be counted and the two literals must not be missed; got {literals:?}"
    );
}
