//! Exercises the GPUI test harness for `interface-gui`.
//! The windowed fixture wave lands with the `preview` feature; until its fixtures exist, these
//! laws keep the harness wired: a real GPUI application runs a store to a proven state.
//! Everything here is gated on `preview` and needs no window, so it stays hermetic on any host.

#![cfg(feature = "preview")]

use std::sync::atomic::{AtomicU64, Ordering};

use compiler_ir::{EntityId, Visibility};
use compiler_ir_vocabulary::{
    DeclarationFamilyId, DeclarationIdentity, EntityKind, VariantFingerprint,
};
use gpui::{AppContext, ClipboardItem, EntityInputHandler, TestAppContext};
use interface_core::PackageUrl;
use interface_documents::{Count, Name, Symbol};
use interface_gui::app::{EngineEvent, Errand, Startup, Submit, Workspace};
use interface_gui::prefs::Preferences;
use interface_gui::preview;
use interface_gui::store::document::PageKey;
use interface_gui::store::explore::ExploreSlot;
use interface_gui::store::library::{AddDraft, RowStatus};
use interface_gui::store::search::OmnibarMode;
use interface_identity::{ContentKey, ExactAddress, PackageCoordinate, PathSegment, SymbolPath};
use interface_library::{
    AddOutcome, AddRejection, CompilerAttachment, ExplorePackageName, ExploreQuery, Library,
    OpenOptions, RejectedAdd, Reply, WorkspaceRoot,
};
use interface_search::{
    Coverage, Cursor, Hit, Lane, LaneReport, LaneSet, QueryText, ResultLimit, Score, SearchRequest,
    SearchScope, SearchTerminal, merge_lanes,
};

/// The constant the results sheet pages by, so a silent change cannot shift two surfaces apart.
#[test]
fn the_results_sheet_pages_by_the_declared_page_rows() {
    assert_eq!(interface_gui::store::search::PAGE_ROWS, 8);
}

/// The store laws hold inside a real GPUI application loop, not only as bare values.
#[gpui::test]
fn a_store_law_runs_inside_the_test_application(cx: &mut TestAppContext) {
    let store = cx.update(|cx| cx.new(|_| interface_gui::store::SearchStore::default()));
    store.update(cx, |store, _| {
        store.retype("> add");
    });
    store.update(cx, |store, _| {
        assert_eq!(
            *store.mode(),
            OmnibarMode::Commands {
                query: "add".into()
            },
            "the entity observes the same mode law the headless tests prove"
        );
    });
}

/// The next unique temporary-root suffix in this process.
fn next_unique() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// One fresh absolute workspace root under the platform temp directory, unique per call.
fn temp_root(tag: &str) -> Option<WorkspaceRoot> {
    let path = std::env::temp_dir().join(format!(
        "nudox-interface-gui-{}-{}-{tag}",
        std::process::id(),
        next_unique()
    ));
    WorkspaceRoot::absolute(path, "test").ok()
}

/// Best-effort removal of one temporary root; the test has already asserted what it needed.
fn discard(root: &WorkspaceRoot) {
    let _ = std::fs::remove_dir_all(root.path());
}

/// Runs one body against the workspace window, refusing to continue if the window closed.
#[allow(
    clippy::panic,
    reason = "a closed window cannot carry the law's assertion, and the panic names the exact refusal"
)]
fn on_window<R>(
    window: &gpui::WindowHandle<Workspace>,
    cx: &mut TestAppContext,
    body: impl FnOnce(&mut Workspace, &mut gpui::Window, &mut gpui::Context<Workspace>) -> R,
) -> R {
    window.update(cx, body).unwrap_or_else(|error| {
        panic!("the workspace window refused the law: {error}");
    })
}

/// Opens one workspace window over a fresh detached root and hands back the root for cleanup.
fn open_workspace(
    tag: &str,
    cx: &mut TestAppContext,
) -> (WorkspaceRoot, gpui::WindowHandle<Workspace>) {
    let built = temp_root(tag);
    assert!(built.is_some(), "the temporary workspace root must resolve");
    let Some(root) = built else {
        unreachable_root();
    };
    let opened = Startup::detached(root.clone());
    assert!(opened.is_ok(), "a fresh temp root must open detached");
    let Ok(startup) = opened else {
        unreachable_root();
    };
    let window = cx.add_window(|window, cx| Workspace::new(startup, window, cx));
    (root, window)
}

/// Ends the test when an earlier assert should already have stopped it.
#[allow(
    clippy::panic,
    reason = "this branch is unreachable after the assertion on the same Option"
)]
fn unreachable_root() -> ! {
    panic!("the temporary workspace root failed an already-asserted check");
}

/// Focuses the omnibar and lets one frame draw, so the native input bridge attaches to it.
///
/// The draw happens in the effect flush at the end of the update itself; the deterministic
/// scheduler is never stepped, so the resident engine's replies stay queued and inert.
fn focus_omnibar(window: &gpui::WindowHandle<Workspace>, cx: &mut TestAppContext) {
    on_window(window, cx, |workspace, window, cx| {
        workspace.omnibar_focus().focus(window, cx);
        window.refresh();
    });
}

/// The detached startup path opens the same fresh root read-only and with first-run defaults.
#[gpui::test]
fn a_detached_startup_opens_read_only_with_first_run_defaults(_cx: &mut TestAppContext) {
    let built = temp_root("startup");
    assert!(built.is_some(), "the temporary workspace root must resolve");
    let Some(root) = built else {
        unreachable_root();
    };

    let opened = Startup::detached(root.clone());
    assert!(opened.is_ok(), "a fresh temp root opens detached");

    let loaded = Preferences::load(&root);
    assert!(loaded.is_ok(), "a root without a prefs file still loads");
    let Some(decoded) = loaded.ok() else {
        unreachable_root();
    };
    assert_eq!(
        decoded.preferences,
        Preferences::default(),
        "first run resolves the documented defaults"
    );
    assert!(
        decoded.unreadable.is_empty(),
        "first run has no unreadable preference lines"
    );

    let library = Library::open(OpenOptions {
        root: root.clone(),
        compiler: CompilerAttachment::Detached,
    });
    assert!(
        library.is_ok(),
        "the detached library opens on a fresh root"
    );
    let Some(library) = library.ok() else {
        unreachable_root();
    };
    assert!(
        !library.has_compiler(),
        "the detached startup path never attaches a compiler"
    );

    discard(&root);
}

/// A workspace window renders idle with no field composing, and accepts text once the omnibar
/// is focused and one frame has drawn.
#[gpui::test]
fn a_window_renders_idle_and_accepts_text_once_the_omnibar_is_focused(cx: &mut TestAppContext) {
    let (root, window) = open_workspace("window", cx);

    let idle = on_window(&window, cx, |workspace, window, cx| {
        (
            workspace.accepts_text_input(window, cx),
            workspace.text_length_utf16(window, cx),
        )
    });
    assert_eq!(
        idle,
        (false, None),
        "nothing is focused, so nothing composes and no field bridge is attached"
    );

    focus_omnibar(&window, cx);

    let focused = on_window(&window, cx, |workspace, window, cx| {
        (
            workspace.accepts_text_input(window, cx),
            workspace.text_length_utf16(window, cx),
        )
    });
    assert_eq!(
        focused,
        (true, Some(0)),
        "the focused omnibar accepts text, and the drawn frame attached its input bridge"
    );

    discard(&root);
}

/// Typing through the platform bridge lands in the omnibar store: plain text is a search, a
/// leading `>` is the command registry, and the idle registry lists the shared registry verbatim.
#[gpui::test]
fn typing_through_the_bridge_switches_modes_and_lists_the_registry(cx: &mut TestAppContext) {
    let (root, window) = open_workspace("typing", cx);
    focus_omnibar(&window, cx);

    on_window(&window, cx, |workspace, window, cx| {
        workspace.replace_text_in_range(None, "ser", window, cx);
    });
    on_window(&window, cx, |workspace, _window, _cx| {
        assert_eq!(workspace.search().text(), "ser");
        assert_eq!(
            workspace.search().mode(),
            &OmnibarMode::Search {
                scope: None,
                query: "ser".into()
            }
        );
    });

    on_window(&window, cx, |workspace, window, cx| {
        workspace.replace_text_in_range(Some(0..3), "> ", window, cx);
    });
    on_window(&window, cx, |workspace, _window, _cx| {
        assert_eq!(workspace.search().text(), "> ");
        assert_eq!(
            workspace.search().mode(),
            &OmnibarMode::Commands { query: "".into() }
        );
        assert_eq!(
            workspace.registry_rows(),
            interface_library::COMMANDS.to_vec(),
            "an empty command query lists the shared registry verbatim"
        );
    });

    discard(&root);
}

/// The input bridge is UTF-16-first: a non-BMP character counts as two units, the caret sits at
/// the end, a mid-pair request collapses onto the pair, and the pair's span reads back the char.
#[gpui::test]
fn the_input_bridge_counts_and_spans_surrogate_pairs_utf16_first(cx: &mut TestAppContext) {
    let (root, window) = open_workspace("utf16", cx);
    focus_omnibar(&window, cx);

    on_window(&window, cx, |workspace, window, cx| {
        workspace.replace_text_in_range(None, "a\u{1D54A}b", window, cx);
    });
    on_window(&window, cx, |workspace, window, cx| {
        let length = workspace.text_length_utf16(window, cx);
        assert_eq!(
            length,
            Some(4),
            "the mathematical double-struck S counts as two UTF-16 units"
        );
        let selection = workspace.selected_text_range(false, window, cx);
        assert_eq!(
            selection.as_ref().map(|s| s.range.clone()),
            Some(4..4),
            "the caret rests at the end of the field"
        );
        assert_eq!(selection.map(|s| s.reversed), Some(false));

        let mut adjusted = None;
        let whole = workspace.text_for_range(0..4, &mut adjusted, window, cx);
        assert_eq!(whole.as_deref(), Some("a\u{1D54A}b"));
        assert_eq!(adjusted, Some(0..4));

        let mut adjusted = None;
        let pair = workspace.text_for_range(1..3, &mut adjusted, window, cx);
        assert_eq!(pair.as_deref(), Some("\u{1D54A}"));
        assert_eq!(
            adjusted,
            Some(1..3),
            "a request spanning the pair reads back the whole character"
        );

        let mut adjusted = None;
        let inside = workspace.text_for_range(1..2, &mut adjusted, window, cx);
        assert_eq!(
            (inside.as_deref(), adjusted),
            (Some(""), Some(1..1)),
            "a request landing inside the pair collapses onto its start"
        );
    });

    on_window(&window, cx, |workspace, window, cx| {
        workspace.set_selected_text_range(1..3, window, cx);
        let selection = workspace.selected_text_range(false, window, cx);
        assert_eq!(
            selection.map(|s| s.range),
            Some(1..3),
            "the platform's caret request lands on the surrogate-pair boundary"
        );
    });

    discard(&root);
}

/// The full search round trip: typing admits a request, a folded terminal fills the sheet with
/// four lane chips, and resubmitting the selected hit opens its page into a titled tab.
#[gpui::test]
fn a_typed_query_folds_a_terminal_and_the_selected_hit_opens_its_page(cx: &mut TestAppContext) {
    let (root, window) = open_workspace("search", cx);
    focus_omnibar(&window, cx);

    on_window(&window, cx, |workspace, window, cx| {
        workspace.replace_text_in_range(None, "ser", window, cx);
    });
    on_window(&window, cx, |workspace, _window, _cx| {
        workspace.submit();
    });

    let selected = on_window(&window, cx, |workspace, _window, cx| {
        workspace.fold(
            EngineEvent::Replied {
                errand: Errand::Search,
                reply: Reply::Searched(preview::terminal("ser")),
            },
            cx,
        );
        assert_eq!(
            workspace.search().hits().len(),
            2,
            "the merged terminal ranks both fixture declarations"
        );
        assert_eq!(
            workspace
                .search()
                .hits()
                .first()
                .map(|hit| hit.symbol.name.as_str()),
            Some("Deserializer"),
            "the exact-and-lexical row outranks the lexical-and-graph row"
        );
        assert_eq!(
            workspace.search().lanes().len(),
            4,
            "the sheet always shows one chip per lane"
        );
        workspace.search_mut().select_first();
        workspace.submit();
        let loading = workspace.documents().slot().is_loading();
        let visible = workspace.documents().slot().visible().is_none();
        let key = workspace
            .search()
            .selection()
            .map(|hit| PageKey::of(&hit.symbol.address));
        (loading, visible, key)
    });
    assert!(
        selected.0,
        "resubmitting the selected hit sends the reader into its loading slot"
    );
    assert!(
        selected.1,
        "a first open has no previous page to keep on screen"
    );
    assert!(
        selected.2.is_some(),
        "the selected hit names the page that was opened"
    );
    let Some(key) = selected.2 else {
        unreachable_root();
    };

    on_window(&window, cx, |workspace, _window, cx| {
        workspace.fold(
            EngineEvent::Replied {
                errand: Errand::Page { key: key.clone() },
                reply: Reply::Page(Ok(preview::page())),
            },
            cx,
        );
        assert_eq!(
            workspace
                .documents()
                .slot()
                .visible()
                .map(|page| page.symbol.name.as_str()),
            Some("Deserializer"),
            "the reply lands the fixture page in the reader"
        );
        let tab = workspace.documents().active();
        assert_eq!(
            tab.map(interface_gui::store::document::Tab::key),
            Some(&key),
            "the adopted tab sits on exactly the opened address"
        );
        assert_eq!(
            tab.map(interface_gui::store::document::Tab::title),
            Some("Deserializer"),
            "the tab's title is the page symbol's own name"
        );
    });

    discard(&root);
}

/// A reply filed on the wrong errand never folds into a store: the shell states the crossing in
/// its own fault line, naming both the reply's command and the errand's command.
#[gpui::test]
fn a_misfiled_health_reply_lands_on_the_fault_line_naming_both_commands(cx: &mut TestAppContext) {
    let (root, window) = open_workspace("misroute", cx);

    let health = Library::open(OpenOptions {
        root: root.clone(),
        compiler: CompilerAttachment::Detached,
    })
    .ok()
    .map(|library| library.health());
    assert!(
        health.is_some(),
        "the detached library reports its own capability health"
    );
    let Some(health) = health else {
        unreachable_root();
    };

    on_window(&window, cx, |workspace, _window, cx| {
        workspace.fold(
            EngineEvent::Replied {
                errand: Errand::Shelf,
                reply: Reply::Health(health),
            },
            cx,
        );
        assert_eq!(
            workspace.action_fault(),
            Some("a health reply arrived on the errand of a packages"),
            "the crossed pair is named by both registry spellings"
        );
        assert!(
            workspace.search().hits().is_empty(),
            "a misfiled reply never folds into a store"
        );
    });

    discard(&root);
}

/// One minted hit row with its own key, name, and lane score, so a merged page's order is exact.
fn hit(row: usize, score: u32) -> Option<Hit> {
    let coordinate = PackageCoordinate::parse("cargo:serde@1.0.196").ok()?;
    let segment = PathSegment::new("serde", None)?;
    let path = SymbolPath::new(vec![segment]).ok()?;
    let mut variant = [0xA1; 16];
    variant[0] ^= u8::try_from(row).ok()?;
    let key = ContentKey::new(DeclarationIdentity {
        family: DeclarationFamilyId::from_raw([0x5E; 16]),
        variant: VariantFingerprint::from_raw(variant),
    });
    let name = Name::exact(format!("serde{row}").as_bytes()).ok()?;
    Some(Hit {
        symbol: Symbol {
            address: ExactAddress::mint(coordinate, path, key),
            entity: EntityId::new(1),
            name,
            kind: EntityKind::Function,
            visibility: Visibility::Public,
        },
        signature: None,
        summary: None,
        lane: Lane::Exact,
        score: Score(score),
    })
}

/// The two-page continuation fixture: six honest exact-lane rows merged through `merge_lanes`
/// with a three-row page, so the first page truncates at cursor three and the second page
/// completes the sheet.
fn continuation_pages() -> Option<(SearchTerminal, SearchTerminal)> {
    let scored = [(0, 600), (1, 500), (2, 400), (3, 300), (4, 200), (5, 100)];
    let all: Vec<Hit> = scored
        .iter()
        .filter_map(|(row, score)| hit(*row, *score))
        .collect();
    if all.len() != scored.len() {
        return None;
    }
    let (first_rows, first_truncation) = merge_lanes(
        [
            (Lane::Exact, all.clone()),
            (Lane::Lexical, Vec::new()),
            (Lane::Graph, Vec::new()),
            (Lane::Semantic, Vec::new()),
        ],
        ResultLimit::clamped(3),
        None,
    );
    let (second_rows, second_truncation) = merge_lanes(
        [
            (Lane::Exact, all),
            (Lane::Lexical, Vec::new()),
            (Lane::Graph, Vec::new()),
            (Lane::Semantic, Vec::new()),
        ],
        ResultLimit::clamped(3),
        Some(Cursor(3)),
    );
    let report = |lane: Lane| LaneReport {
        lane,
        coverage: Coverage::Complete,
        hits: Count(3),
        elapsed: None,
    };
    let request = SearchRequest {
        text: QueryText::new("serde").ok()?,
        scope: SearchScope::default(),
        lanes: LaneSet::ALL,
        limit: ResultLimit::clamped(3),
        cursor: None,
    };
    Some((
        SearchTerminal {
            request: request.clone(),
            hits: first_rows,
            lanes: [
                report(Lane::Exact),
                report(Lane::Lexical),
                report(Lane::Graph),
                report(Lane::Semantic),
            ],
            truncation: first_truncation,
        },
        SearchTerminal {
            request: SearchRequest {
                cursor: Some(Cursor(3)),
                ..request
            },
            hits: second_rows,
            lanes: [
                report(Lane::Exact),
                report(Lane::Lexical),
                report(Lane::Graph),
                report(Lane::Semantic),
            ],
            truncation: second_truncation,
        },
    ))
}

/// One `CompilerDetached` rejection over the fixture URL, exactly as a detached engine answers.
fn rejected_detached() -> AddOutcome {
    let url = PackageUrl::try_from("pkg:cargo/serde@1.0.196".to_owned());
    assert!(url.is_ok(), "the fixture package url must parse");
    let Ok(url) = url else {
        unreachable_root();
    };
    AddOutcome::Rejected(RejectedAdd {
        url,
        rejection: AddRejection::CompilerDetached,
    })
}

/// The registry-index workflow: `#ser` is index mode, the folded page fills the explore slot in
/// rank order, two cursor steps land on the third row, Enter opens that row's profile, and the
/// folded profile names the newest active version as latest with the yanked row inactive.
#[gpui::test]
fn an_index_query_folds_a_page_and_submit_opens_the_selected_profile(cx: &mut TestAppContext) {
    let (root, window) = open_workspace("index", cx);
    focus_omnibar(&window, cx);

    on_window(&window, cx, |workspace, window, cx| {
        workspace.replace_text_in_range(None, "#ser", window, cx);
    });
    on_window(&window, cx, |workspace, _window, _cx| {
        assert_eq!(
            workspace.search().mode(),
            &OmnibarMode::Index {
                query: "ser".into()
            },
            "a leading hash is the registry-index mode with the rest as its query"
        );
    });

    on_window(&window, cx, |workspace, _window, _cx| {
        let built = ExploreQuery::new("ser");
        assert!(built.is_ok(), "the typed query must be admitted");
        let Some(query) = built.ok() else {
            return;
        };
        workspace.search_index(query);
        assert_eq!(
            workspace.explore().index_page(),
            Some(&ExploreSlot::Loading),
            "dispatching the index search sends the slot into its loading state"
        );
    });

    on_window(&window, cx, |workspace, _window, cx| {
        let built = ExploreQuery::new("ser");
        let Some(query) = built.ok() else {
            return;
        };
        workspace.fold(
            EngineEvent::Replied {
                errand: Errand::IndexSearch { query },
                reply: Reply::IndexSearched(Ok(preview::index_page())),
            },
            cx,
        );
        assert!(
            matches!(
                workspace.explore().index_page(),
                Some(ExploreSlot::Ready(_))
            ),
            "the folded page lands ready in the explore slot"
        );
        let Some(ExploreSlot::Ready(page)) = workspace.explore().index_page() else {
            return;
        };
        let names: Vec<&str> = page.hits.iter().map(|row| row.document.as_str()).collect();
        assert_eq!(
            page.hits.len(),
            3,
            "the fixture page carries three ranked rows"
        );
        assert_eq!(
            names,
            vec!["serde", "serde_json", "tokio"],
            "the rows keep the projection's deterministic rank order"
        );
    });

    on_window(&window, cx, |workspace, _window, _cx| {
        workspace.search_mut().step_index_selection(true);
        workspace.search_mut().step_index_selection(true);
        assert_eq!(
            workspace.search().index_cursor(),
            2,
            "two forward steps land the cursor on the third row"
        );
        let Some(ExploreSlot::Ready(page)) = workspace.explore().index_page() else {
            return;
        };
        assert_eq!(
            page.hits
                .get(workspace.search().index_cursor())
                .map(|row| row.document.as_str()),
            Some("tokio"),
            "the cursor points at the third row's package"
        );
    });

    on_window(&window, cx, |_workspace, window, cx| {
        window.refresh();
        window.dispatch_action(Box::new(Submit), cx);
    });
    on_window(&window, cx, |workspace, _window, _cx| {
        let built = ExplorePackageName::new("tokio");
        assert!(
            built.is_ok(),
            "the selected row's document text must be an admitted name"
        );
        let Some(tokio) = built.ok() else {
            return;
        };
        assert_eq!(
            workspace.explore().profile(&tokio),
            Some(&ExploreSlot::Loading),
            "Enter in index mode dispatches the selected row's profile"
        );
    });

    on_window(&window, cx, |workspace, _window, cx| {
        let built = ExplorePackageName::new("tokio");
        let Some(tokio) = built.ok() else {
            return;
        };
        workspace.fold(
            EngineEvent::Replied {
                errand: Errand::Profile {
                    name: tokio.clone(),
                },
                reply: Reply::Profiled(Ok(preview::profile())),
            },
            cx,
        );
    });
    on_window(&window, cx, |workspace, _window, _cx| {
        let built = ExplorePackageName::new("tokio");
        let Some(tokio) = built.ok() else {
            return;
        };
        assert!(
            matches!(
                workspace.explore().profile(&tokio),
                Some(ExploreSlot::Ready(_))
            ),
            "the folded profile lands ready in its package's slot"
        );
        let Some(ExploreSlot::Ready(profile)) = workspace.explore().profile(&tokio) else {
            return;
        };
        assert_eq!(
            profile.latest.as_ref().map(|row| row.version.as_str()),
            Some("1.0.210"),
            "the profile's latest is the newest active version"
        );
        assert_eq!(
            profile.versions.rows.len(),
            3,
            "the profile carries every recorded version row"
        );
        let yanked = profile
            .versions
            .rows
            .iter()
            .find(|row| row.version.as_str() == "1.0.104");
        assert!(yanked.is_some(), "the yanked row is present");
        let Some(row) = yanked else {
            return;
        };
        assert!(
            !row.active.is_active(),
            "the yanked row is present as inactive"
        );
    });

    discard(&root);
}

/// The easy-add workflow: a coordinate seed opens the flow valid, submitting begins the active
/// job on that coordinate, and a rejection that arrives with no begun job still derives its
/// Failed row from the URL it names instead of being dropped.
#[gpui::test]
fn a_quick_add_submits_a_job_and_an_orphan_rejection_still_lands_its_row(cx: &mut TestAppContext) {
    let (root, window) = open_workspace("quick-add", cx);
    let built = PackageCoordinate::parse("cargo:serde@1.0.196");
    assert!(built.is_ok(), "the fixture coordinate must parse");
    let Some(serde) = built.ok() else {
        return;
    };

    on_window(&window, cx, |workspace, window, cx| {
        workspace.quick_add("cargo:serde@1.0.196", window, cx);
        assert!(
            workspace.store().add.open,
            "the quick add opens the add flow"
        );
        assert_eq!(
            workspace.store().add.text,
            "cargo:serde@1.0.196",
            "the field is seeded with the coordinate as offered"
        );
        assert_eq!(
            workspace.store().add.draft,
            AddDraft::Valid {
                coordinate: serde.clone()
            },
            "the seeded coordinate drafts valid"
        );
    });
    on_window(&window, cx, |workspace, _window, _cx| {
        workspace.submit_add();
        assert_eq!(
            workspace.store().active().map(|job| job.coordinate.clone()),
            Some(serde.clone()),
            "submitting begins the active job on the seeded coordinate"
        );
    });

    on_window(&window, cx, |workspace, _window, cx| {
        workspace.fold(
            EngineEvent::Replied {
                errand: Errand::Add {
                    coordinate: serde.clone(),
                },
                reply: Reply::Added(rejected_detached()),
            },
            cx,
        );
        let row = workspace.store().row(&serde);
        assert!(row.is_some(), "the refusal keeps a row for its coordinate");
        let Some(row) = row else {
            return;
        };
        assert!(
            matches!(&row.status, RowStatus::Failed { fault } if fault.slug == "compiler-detached"),
            "the refusal is a Failed row carrying the engine's own slug"
        );
        assert!(
            workspace.store().active().is_none(),
            "the terminal consumed the job it began"
        );

        workspace.fold(
            EngineEvent::Replied {
                errand: Errand::Add {
                    coordinate: serde.clone(),
                },
                reply: Reply::Added(rejected_detached()),
            },
            cx,
        );
        assert_eq!(
            workspace.store().rows().len(),
            1,
            "the orphan refusal derives the same row instead of opening a second"
        );
        assert!(
            workspace.store().last_orphan_fault().is_none(),
            "a rejection names its URL, so no orphan fault is retained"
        );
    });

    discard(&root);
}

/// The clipboard path: a canonical package URL on the clipboard seeds the flow with the URL's
/// derived coordinate, and garbage on the clipboard still opens the flow with a fault line
/// naming exactly what was offered.
#[gpui::test]
fn a_clipboard_quick_add_seeds_the_flow_and_garbage_names_its_source(cx: &mut TestAppContext) {
    let (root, window) = open_workspace("clipboard", cx);

    cx.update(|cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(
            "pkg:cargo/tokio@1.35.0".to_owned(),
        ));
    });
    on_window(&window, cx, |workspace, window, cx| {
        workspace.quick_add_from_clipboard(window, cx);
        assert!(
            workspace.store().add.open,
            "the clipboard quick add opens the flow"
        );
        assert_eq!(
            workspace.store().add.text,
            "cargo:tokio@1.35.0",
            "the flow holds the URL's derived coordinate, the spelling the draft grammar admits"
        );
        let built = PackageCoordinate::parse("cargo:tokio@1.35.0");
        assert!(built.is_ok(), "the derived coordinate must parse");
        let Some(tokio) = built.ok() else {
            return;
        };
        assert_eq!(
            workspace.store().add.draft,
            AddDraft::Valid { coordinate: tokio },
            "the derived coordinate drafts valid"
        );
        assert!(
            workspace.store().add.fault.is_none(),
            "a coordinate seed opens with no fault line"
        );
    });

    cx.update(|cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("not a package".to_owned()));
    });
    on_window(&window, cx, |workspace, window, cx| {
        workspace.quick_add_from_clipboard(window, cx);
        assert!(
            workspace.store().add.open,
            "garbage still opens the flow for editing"
        );
        let fault = workspace.store().add.fault.as_ref();
        assert!(
            fault.is_some(),
            "the garbage names itself on the fault line"
        );
        let Some(fault) = fault else {
            return;
        };
        assert_eq!(fault.slug, "clipboard-not-a-package");
        assert_eq!(
            fault.operand, "not a package",
            "the fault names exactly what was offered"
        );
        assert!(
            matches!(workspace.store().add.draft, AddDraft::Invalid { .. }),
            "the refused text sits in the field for the reader to edit"
        );
    });

    discard(&root);
}

/// The continuation workflow: a truncated sheet walked past its last row continues from the
/// truncation cursor, and the folded second page appends its rows, replaces the truncation
/// line, and keeps the reader's selection on the row it held.
#[gpui::test]
fn paging_past_the_last_row_continues_the_search_and_appends_the_next_page(
    cx: &mut TestAppContext,
) {
    let (root, window) = open_workspace("continuation", cx);
    let built = continuation_pages();
    assert!(
        built.is_some(),
        "the 3+3-row continuation fixture must build"
    );
    let Some((first, second)) = built else {
        return;
    };

    on_window(&window, cx, |workspace, _window, cx| {
        workspace.fold(
            EngineEvent::Replied {
                errand: Errand::Search,
                reply: Reply::Searched(first),
            },
            cx,
        );
        assert_eq!(
            workspace.search().hits().len(),
            3,
            "the first page carries exactly its limit in rows"
        );
        assert!(
            workspace.search().is_truncated(),
            "the first page states its truncation"
        );
    });

    on_window(&window, cx, |workspace, _window, _cx| {
        workspace.search_mut().select_last();
        workspace.page_results(true);
        assert_eq!(
            workspace.search().selected(),
            2,
            "paging past the last row clamps the selection to it"
        );
    });

    on_window(&window, cx, |workspace, _window, _cx| {
        let request = workspace.search().continuation_request();
        assert!(
            request.is_some(),
            "a walked-past truncated sheet owes a continuation"
        );
        let Some(request) = request else {
            return;
        };
        assert_eq!(
            request.cursor,
            Some(Cursor(3)),
            "the continuation resumes at the truncation cursor"
        );
        assert_eq!(request.text.as_str(), "serde");
        assert_eq!(request.limit, ResultLimit::clamped(3));
    });

    on_window(&window, cx, |workspace, _window, cx| {
        workspace.fold(
            EngineEvent::Replied {
                errand: Errand::Search,
                reply: Reply::Searched(second),
            },
            cx,
        );
        assert_eq!(
            workspace.search().hits().len(),
            6,
            "the continuation appends its rows to the loaded page"
        );
        assert!(
            !workspace.search().is_truncated(),
            "the truncation line is replaced by the continuation's own complete state"
        );
        assert!(
            workspace.search().continuation_request().is_none(),
            "a complete sheet offers no further continuation"
        );
        assert_eq!(
            workspace.search().selected(),
            2,
            "the selection stays where the reader left it"
        );
        assert_eq!(
            workspace
                .search()
                .selection()
                .map(|hit| hit.symbol.name.as_str()),
            Some("serde2"),
            "the held selection is still the same row"
        );
    });

    discard(&root);
}

/// The kind chips: toggling one kind flips the set the shell holds, and the next admitted
/// request carries exactly that set as its kinds scope.
#[gpui::test]
fn a_kind_chip_rescopes_the_next_request(cx: &mut TestAppContext) {
    let (root, window) = open_workspace("kinds", cx);
    focus_omnibar(&window, cx);

    on_window(&window, cx, |workspace, window, cx| {
        workspace.replace_text_in_range(None, "ser", window, cx);
    });
    on_window(&window, cx, |workspace, _window, cx| {
        let shelf: [PackageCoordinate; 0] = [];
        assert!(
            !workspace.search().kinds().contains(EntityKind::Field),
            "fields start excluded from the browsable kinds"
        );
        workspace.toggle_search_kind(EntityKind::Field, cx);
        assert!(
            workspace.search().kinds().contains(EntityKind::Field),
            "the chip flips the set the shell holds"
        );
        let request = workspace.search().request(&shelf);
        assert!(request.is_some(), "the typed query still admits a request");
        let Some(request) = request else {
            return;
        };
        assert_eq!(
            request.scope.kinds,
            workspace.search().kinds(),
            "the next request carries the kinds scope the chips hold"
        );
        workspace.toggle_search_kind(EntityKind::Field, cx);
        assert!(
            !workspace.search().kinds().contains(EntityKind::Field),
            "toggling twice restores the browsable set"
        );
    });

    discard(&root);
}

/// The explore misroute law: a versions reply filed on an index-search errand never folds into
/// an explore slot; the shell names both commands on its fault line.
#[gpui::test]
fn a_versions_reply_on_an_index_search_errand_names_both_commands(cx: &mut TestAppContext) {
    let (root, window) = open_workspace("misroute-explore", cx);

    on_window(&window, cx, |workspace, _window, cx| {
        let built = ExploreQuery::new("ser");
        assert!(built.is_ok(), "the fixture query must be admitted");
        let Some(query) = built.ok() else {
            return;
        };
        workspace.fold(
            EngineEvent::Replied {
                errand: Errand::IndexSearch { query },
                reply: Reply::Versions(Ok(preview::version_rows())),
            },
            cx,
        );
        assert_eq!(
            workspace.action_fault(),
            Some("a package-versions reply arrived on the errand of a index-search"),
            "the crossed pair is named by both registry spellings"
        );
        assert!(
            workspace.explore().index_page().is_none(),
            "a misfiled reply never folds into an explore slot"
        );
    });

    discard(&root);
}

#[gpui::test]
fn a_debounced_search_fires_once_and_a_superseded_arm_does_nothing(
    cx: &mut TestAppContext,
) {
    let (root, window) = open_workspace("debounce", cx);
    let _ = root;
    focus_omnibar(&window, cx);

    on_window(&window, cx, |workspace, window, cx| {
        workspace
            .replace_text_in_range(None, "serde", window, cx);
        assert_eq!(
            workspace.search_dispatches(),
            0,
            "an armed debounce dispatches nothing until its timer fires"
        );
        let generation = workspace.search_generation();
        workspace.fire_debounced_search(generation);
        assert_eq!(
            workspace.search_dispatches(),
            1,
            "the armed generation dispatches exactly one search"
        );
    });

    on_window(&window, cx, |workspace, window, cx| {
        workspace
            .replace_text_in_range(Some(0..5), "serde_json", window, cx);
        let superseded = workspace.search_generation().saturating_sub(1);
        workspace.fire_debounced_search(superseded);
        assert_eq!(
            workspace.search_dispatches(),
            1,
            "a superseded arm fires nothing; only the newest generation may dispatch"
        );
        workspace.fire_debounced_search(workspace.search_generation());
        assert_eq!(
            workspace.search_dispatches(),
            2,
            "the newest arm still dispatches exactly one search"
        );
    });
}

#[gpui::test]
fn stepping_past_a_truncated_sheet_dispatches_the_continuation_it_owes(
    cx: &mut TestAppContext,
) {
    let (root, window) = open_workspace("continuation-dispatch", cx);
    let _ = root;
    let Some((first, _)) = continuation_pages() else {
        assert!(false, "the continuation fixture must build");
        return;
    };

    on_window(&window, cx, |workspace, _window, cx| {
        workspace.fold(
            EngineEvent::Replied {
                errand: Errand::Search,
                reply: Reply::Searched(first),
            },
            cx,
        );
        assert_eq!(workspace.search_dispatches(), 0, "nothing was dispatched yet");
        workspace.search_mut().select_last();
        workspace.page_results(true);
        assert_eq!(
            workspace.search_dispatches(),
            1,
            "stepping past a truncated sheet dispatches the continuation request"
        );
        let owed = workspace.search().continuation_request();
        assert!(
            owed.is_some(),
            "the dispatched continuation is the one the sheet owes"
        );
    });
}
