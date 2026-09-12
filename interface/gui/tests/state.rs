//! Proves the headless store laws for `interface-gui`.
//! These tests run with no window and no engine: the stores are the shell's whole behavioural
//! memory, so every law here is a law of the product. Each assertion states an exact value.
//! Faults are asserted as their typed slugs, per `interface/README.md`'s rendering discipline.

use compiler_ir::{EntityId, Visibility};
use compiler_ir_vocabulary::{
    DeclarationFamilyId, DeclarationIdentity, EntityKind, VariantFingerprint,
};
use interface_core::{CorrelationId, PackageCompilePhase};
use interface_documents::{Count, Name, Symbol};
use interface_identity::{ContentKey, ExactAddress, PackageCoordinate, PathSegment, SymbolPath};
use interface_library::{
    AddOutcome, AddProgress, AddRejection, CompilePhaseProgress, LibraryEpoch, RejectedAdd,
    RemoveOutcome, Shelf, ShelfEntry, ShelfStatus, Timestamp,
};
use interface_search::{
    Coverage, Hit, Lane, LaneReport, LaneSet, QueryText, ResultLimit, Score, SearchRequest,
    SearchScope, SearchTerminal, Truncation,
};

use interface_gui::store::document::{DocumentStore, PageKey, PageSlot};
use interface_gui::store::library::{LibraryStore, RowStatus};
use interface_gui::store::search::{LaneChip, OmnibarMode, SearchStore};

/// One coordinate with no free variables, so a fixture failure names its input.
fn coordinate(text: &str) -> Option<PackageCoordinate> {
    PackageCoordinate::parse(text).ok()
}

/// One hit row whose identity is fully minted, exactly as a projector would mint it.
fn hit(row: usize) -> Option<Hit> {
    let coordinate = coordinate("cargo:serde@1.0.196")?;
    let segment = PathSegment::new("serde", None)?;
    let path = SymbolPath::new(vec![segment]).ok()?;
    let key = ContentKey::new(DeclarationIdentity {
        family: DeclarationFamilyId::from_raw([0x5E; 16]),
        variant: VariantFingerprint::from_raw([0xA1; 16]),
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
        score: Score(0),
    })
}

/// One complete terminal for `rows` rows, with all four lanes reporting complete coverage.
fn terminal(rows: usize) -> Option<SearchTerminal> {
    let hits: Vec<Hit> = (0..rows).filter_map(hit).collect();
    if hits.len() != rows {
        return None;
    }
    let report = |lane| -> Option<LaneReport> {
        Some(LaneReport {
            lane,
            coverage: Coverage::Complete,
            hits: Count(u32::try_from(rows).ok()?),
            elapsed: None,
        })
    };
    Some(SearchTerminal {
        request: SearchRequest {
            text: QueryText::new("serde").ok()?,
            scope: SearchScope::default(),
            lanes: LaneSet::ALL,
            limit: ResultLimit::default(),
            cursor: None,
        },
        hits: hits.into_boxed_slice(),
        lanes: [
            report(Lane::Exact)?,
            report(Lane::Lexical)?,
            report(Lane::Graph)?,
            report(Lane::Semantic)?,
        ],
        truncation: Truncation::Complete,
    })
}

#[test]
fn the_omnibar_reads_its_mode_off_the_first_characters() {
    assert_eq!(OmnibarMode::detect(""), OmnibarMode::Idle);
    assert_eq!(
        OmnibarMode::detect("> add"),
        OmnibarMode::Commands {
            query: "add".into()
        }
    );
    assert_eq!(
        OmnibarMode::detect("@serde map"),
        OmnibarMode::Search {
            scope: Some("serde".into()),
            query: "map".into(),
        }
    );
    assert_eq!(
        OmnibarMode::detect("map"),
        OmnibarMode::Search {
            scope: None,
            query: "map".into(),
        }
    );
}

#[test]
fn a_retyped_field_admits_its_own_text_as_a_request() {
    let mut store = SearchStore::default();
    store.retype("deserializer");
    let shelf: [PackageCoordinate; 0] = [];
    let request = store.request(&shelf);
    assert!(
        request.is_some(),
        "a non-empty search field must admit its own text"
    );
    let Some(request) = request else {
        return;
    };
    assert_eq!(request.text.as_str(), "deserializer");
    assert_eq!(request.scope.packages, None);

    store.retype("> add");
    assert!(
        store.request(&shelf).is_none(),
        "command mode is the registry, never a search request"
    );
}

#[test]
fn stepping_and_paging_keep_the_selection_inside_the_rows() {
    let mut store = SearchStore::default();
    let built = terminal(3);
    assert!(built.is_some(), "the 3-row terminal fixture must build");
    let Some(three_rows) = built else {
        return;
    };
    store.apply(three_rows);
    assert_eq!(store.hits().len(), 3);

    store.step(true);
    store.step(true);
    assert_eq!(store.selected(), 2);
    store.step(true);
    assert_eq!(
        store.selected(),
        2,
        "stepping forward saturates at the last row"
    );
    store.step(false);
    store.step(false);
    store.step(false);
    assert_eq!(
        store.selected(),
        0,
        "stepping back saturates at the first row"
    );

    store.page(true);
    assert_eq!(store.selected(), 2, "paging clamps to the last row");
    store.page(false);
    assert_eq!(store.selected(), 0, "paging clamps to the first row");

    store.select_last();
    assert_eq!(store.selected(), 2);
    store.select_first();
    assert_eq!(store.selected(), 0);
}

#[test]
fn folding_a_new_terminal_resets_the_selection() {
    let mut store = SearchStore::default();
    let built = terminal(3);
    assert!(built.is_some(), "the 3-row terminal fixture must build");
    let Some(three_rows) = built else {
        return;
    };
    store.apply(three_rows);
    store.select_last();
    assert_eq!(store.selected(), 2);
    let built_again = terminal(3);
    assert!(
        built_again.is_some(),
        "the terminal fixture must build twice"
    );
    let Some(three_rows_again) = built_again else {
        return;
    };
    store.apply(three_rows_again);
    assert_eq!(store.selected(), 0);
    store.retype("a different query");
    assert_eq!(store.selected(), 0, "retyping also resets the selection");
}

#[test]
fn an_empty_shelf_read_is_exactly_the_first_run_state() {
    let mut store = LibraryStore::default();
    store.apply_shelf(Ok(Shelf {
        entries: Box::new([]),
        epoch: LibraryEpoch(7),
    }));
    assert!(store.is_empty());
    assert_eq!(store.epoch(), LibraryEpoch(7));
    assert!(
        store.shelf_fault().is_none(),
        "a successful shelf read clears any stale fault"
    );
}

#[test]
fn a_detached_compiler_refusal_lands_on_the_row_with_its_slug() {
    let parsed = coordinate("cargo:serde@1.0.196");
    assert!(parsed.is_some(), "the fixture coordinate must parse");
    let Some(serde) = parsed else {
        return;
    };
    let url = interface_core::PackageUrl::try_from(serde.package_url_text());
    assert!(
        url.is_ok(),
        "the fixture coordinate must spell a package url"
    );
    let Ok(url) = url else {
        return;
    };

    let mut store = LibraryStore::default();
    store.begin_job(serde.clone());
    store.finish_job(&AddOutcome::Rejected(RejectedAdd {
        url,
        rejection: AddRejection::CompilerDetached,
    }));

    let row = store.row(&serde);
    assert!(
        row.is_some(),
        "a refused package keeps its row so the refusal is visible"
    );
    let Some(row) = row else {
        return;
    };
    assert_eq!(row.coordinate, serde);
    assert!(
        matches!(&row.status, RowStatus::Failed { fault } if fault.slug == "compiler-detached"),
        "the refusal is a Failed row carrying the engine's own slug"
    );
    assert_eq!(row.census_line(), "compiler-detached");
    assert!(!row.status.is_readable());
}

#[test]
fn a_begin_on_an_unknown_key_shows_loading_with_nothing_on_screen() {
    let parsed = coordinate("cargo:other@0.1.0");
    assert!(parsed.is_some(), "the fixture coordinate must parse");
    let Some(other) = parsed else {
        return;
    };
    let key = PageKey::package(&other);

    let mut fresh = DocumentStore::default();
    assert!(matches!(fresh.slot(), PageSlot::Idle));
    fresh.begin(&key);
    assert!(fresh.slot().is_loading());
    assert!(
        fresh.slot().visible().is_none(),
        "an unknown key has no previous page to keep on screen"
    );
}

#[test]
fn tabs_open_select_cycle_and_close_without_losing_the_reader() {
    let first = coordinate("cargo:a@1.0.0");
    let second = coordinate("cargo:b@2.0.0");
    let third = coordinate("cargo:c@3.0.0");
    let fixtures = (first.clone(), second.clone(), third.clone());
    assert!(
        matches!(fixtures, (Some(_), Some(_), Some(_))),
        "the fixture coordinates must parse"
    );
    let (Some(a), Some(b), Some(c)) = fixtures else {
        return;
    };
    let first = PageKey::package(&a);
    let second = PageKey::package(&b);
    let third = PageKey::package(&c);

    let mut documents = DocumentStore::default();
    documents.open_tab(first.clone(), "a", false);
    assert!(
        !documents.shows_tab_strip(),
        "one tab on screen is not a strip yet"
    );
    documents.open_tab(second.clone(), "b", true);
    assert_eq!(
        documents.active_index(),
        0,
        "a background tab does not steal the reader"
    );
    assert!(documents.shows_tab_strip(), "two tabs make a strip");
    assert_eq!(documents.tabs().len(), 2);

    documents.select_tab(1);
    assert_eq!(
        documents
            .active()
            .map(interface_gui::store::document::Tab::key),
        Some(&second)
    );

    documents.open_tab(third.clone(), "c", false);
    assert_eq!(documents.active_index(), 2);
    assert!(documents.shows_tab_strip());

    documents.cycle_tab(true);
    assert_eq!(documents.active_index(), 0, "cycling forward wraps");
    documents.cycle_tab(false);
    assert_eq!(documents.active_index(), 2, "cycling back wraps");

    let landed = documents.close_tab(2);
    assert_eq!(
        landed.as_ref(),
        Some(&second),
        "closing the front tab lands on its neighbour"
    );
    assert_eq!(documents.tabs().len(), 2);

    documents.close_tab(0);
    assert_eq!(documents.tabs().len(), 1);
    let landed = documents.close_tab(0);
    assert_eq!(landed, None, "closing the last tab leaves nothing to show");
    assert!(documents.tabs().is_empty());
    assert!(matches!(documents.slot(), PageSlot::Idle));
}

#[test]
fn progress_moves_the_active_job_and_its_row_to_compiling_at_the_reported_ordinal() {
    let parsed = coordinate("cargo:serde@1.0.196");
    assert!(parsed.is_some(), "the fixture coordinate must parse");
    let Some(serde) = parsed else {
        return;
    };

    let mut store = LibraryStore::default();
    store.apply_shelf(Ok(Shelf {
        entries: Box::new([ShelfEntry {
            coordinate: serde.clone(),
            status: ShelfStatus::Requested,
            requested_at: Timestamp(1_000_000),
            correlation: CorrelationId(1),
        }]),
        epoch: LibraryEpoch(4),
    }));
    store.begin_job(serde.clone());

    store.apply_progress(AddProgress::Admitted {
        correlation: CorrelationId(9),
    });
    let row = store.row(&serde);
    assert!(row.is_some(), "the requested row stays on the shelf");
    assert_eq!(
        row.map(|row| &row.status),
        Some(&RowStatus::Requested),
        "admission alone does not move the row"
    );
    assert_eq!(
        store
            .active()
            .and_then(|job| job.progress)
            .map(|progress| progress.ordinal),
        None,
        "admission carries no phase"
    );

    store.apply_progress(AddProgress::Phase(CompilePhaseProgress::of(
        PackageCompilePhase::Lower,
    )));
    let row = store.row(&serde);
    assert!(row.is_some(), "the compiling row stays on the shelf");
    let Some(row) = row else {
        return;
    };
    assert_eq!(
        row.status.ordinal(),
        Some(3),
        "lower is the fourth of the eight ordered phases"
    );
    assert!(
        matches!(
            &row.status,
            RowStatus::Compiling { progress }
                if progress.phase == PackageCompilePhase::Lower && progress.total == 8
        ),
        "the row carries the exact reported phase"
    );
    let active = store.active();
    assert!(active.is_some(), "the job stays active while compiling");
    let Some(active) = active else {
        return;
    };
    assert_eq!(
        active
            .progress
            .map(|progress| (progress.phase, progress.ordinal)),
        Some((PackageCompilePhase::Lower, 3))
    );
    assert_eq!(active.dots(), "●●●●○○○○");
    assert_eq!(active.label(), "lower");
}

#[test]
fn a_removed_row_erases_while_a_busy_refusal_keeps_the_row_failed() {
    let parsed_a = coordinate("cargo:a@1.0.0");
    let parsed_b = coordinate("cargo:b@2.0.0");
    assert!(
        matches!((parsed_a.as_ref(), parsed_b.as_ref()), (Some(_), Some(_))),
        "the fixture coordinates must parse"
    );
    let (Some(a), Some(b)) = (parsed_a, parsed_b) else {
        return;
    };

    let mut store = LibraryStore::default();
    store.apply_shelf(Ok(Shelf {
        entries: Box::new([
            ShelfEntry {
                coordinate: a.clone(),
                status: ShelfStatus::Requested,
                requested_at: Timestamp(1),
                correlation: CorrelationId(1),
            },
            ShelfEntry {
                coordinate: b.clone(),
                status: ShelfStatus::Requested,
                requested_at: Timestamp(2),
                correlation: CorrelationId(2),
            },
        ]),
        epoch: LibraryEpoch(2),
    }));

    store.apply_removed(&a, &RemoveOutcome::Removed);
    assert!(store.row(&a).is_none(), "a removed row leaves the shelf");
    assert_eq!(store.rows().len(), 1, "the other row is untouched");

    store.apply_removed(&b, &RemoveOutcome::Busy);
    let row = store.row(&b);
    assert!(
        row.is_some(),
        "a busy refusal keeps its row so the refusal stays visible"
    );
    let Some(row) = row else {
        return;
    };
    assert_eq!(store.rows().len(), 1);
    assert!(
        matches!(&row.status, RowStatus::Failed { fault } if fault.slug == "remove-refused"),
        "the busy refusal is a Failed row carrying its slug"
    );
    assert_eq!(row.census_line(), "remove-refused");
    assert!(!row.status.is_readable());
}

#[test]
fn a_scope_chip_confines_the_request_to_the_shelf_rows_its_substring_names() {
    let mut store = SearchStore::default();
    store.retype("@ser map");
    let shelf: Vec<PackageCoordinate> = [
        "cargo:serde@1.0.196",
        "cargo:serde_json@1.0.196",
        "cargo:toml@0.8.2",
    ]
    .iter()
    .filter_map(|text| coordinate(text))
    .collect();
    assert_eq!(shelf.len(), 3, "every fixture coordinate must parse");

    let request = store.request(&shelf);
    assert!(request.is_some(), "a chip plus a query admits a request");
    let Some(request) = request else {
        return;
    };
    assert_eq!(request.text.as_str(), "map");
    let mut expected: Vec<PackageCoordinate> = Vec::new();
    expected.extend(shelf.first().cloned());
    expected.extend(shelf.get(1).cloned());
    assert_eq!(
        request.scope.packages,
        Some(expected.into_boxed_slice()),
        "the chip keeps, in shelf order, only the rows whose name contains it"
    );

    store.clear_scope();
    let unscoped = store.request(&shelf);
    assert!(unscoped.is_some(), "the query alone still admits a request");
    assert_eq!(
        unscoped.and_then(|request| request.scope.packages),
        None,
        "dropping the chip searches every package"
    );
}

#[test]
fn the_hash_prefix_opens_the_registry_index_and_the_truth_table_agrees() {
    assert_eq!(
        OmnibarMode::detect("#ser"),
        OmnibarMode::Index {
            query: "ser".into()
        }
    );
    assert_eq!(
        OmnibarMode::detect("#"),
        OmnibarMode::Index { query: "".into() },
        "a bare hash is the index mode with an empty query"
    );
    assert_eq!(
        OmnibarMode::detect("> x"),
        OmnibarMode::Commands { query: "x".into() },
        "the command registry keeps its greater-than spelling"
    );
    assert_eq!(
        OmnibarMode::detect("@s q"),
        OmnibarMode::Search {
            scope: Some("s".into()),
            query: "q".into(),
        },
        "the scope chip keeps its at-and-space spelling"
    );
    assert!(!OmnibarMode::Idle.is_index(), "idle is not the index");
    assert!(
        !OmnibarMode::Commands { query: "x".into() }.is_index(),
        "the command registry is not the index"
    );
    assert!(
        OmnibarMode::Index {
            query: "ser".into()
        }
        .is_index(),
        "the hash prefix is the index"
    );
    assert!(
        !OmnibarMode::Search {
            scope: None,
            query: "q".into(),
        }
        .is_index(),
        "an ordinary search is not the index"
    );
}

#[test]
fn the_index_cursor_never_steps_below_the_first_row_and_select_sets_exactly() {
    let mut store = SearchStore::default();
    assert_eq!(store.index_cursor(), 0);
    store.step_index_selection(false);
    assert_eq!(
        store.index_cursor(),
        0,
        "stepping back on the first row stays on it"
    );
    store.step_index_selection(true);
    store.step_index_selection(true);
    assert_eq!(store.index_cursor(), 2);
    store.step_index_selection(false);
    assert_eq!(store.index_cursor(), 1);
    store.select_index(4);
    assert_eq!(
        store.index_cursor(),
        4,
        "select sets exactly what the row click named; the shell clamps against the loaded page"
    );
    store.retype("a different query");
    assert_eq!(
        store.index_cursor(),
        0,
        "retyping resets the index selection with the rest of the sheet"
    );
}

#[test]
fn lane_chips_after_a_continuation_describe_the_newest_terminal() {
    let mut store = SearchStore::default();
    let built_first = terminal(3);
    assert!(
        built_first.is_some(),
        "the 3-row terminal fixture must build"
    );
    let Some(first) = built_first else {
        return;
    };
    store.apply(first);
    assert_eq!(
        store
            .lanes()
            .first()
            .map(|chip| (chip.glyph, chip.note.as_str())),
        Some(("✓", "3")),
        "before the continuation the chips are the first terminal's own"
    );

    let built_second = terminal(3);
    assert!(
        built_second.is_some(),
        "the terminal fixture must build twice"
    );
    let Some(mut second) = built_second else {
        return;
    };
    if let Some(report) = second.lanes.get_mut(0) {
        *report = LaneReport {
            lane: Lane::Exact,
            coverage: Coverage::Partial {
                searched: Count(1),
                total: Count(2),
            },
            hits: Count(1),
            elapsed: None,
        };
    }
    let expected: Vec<LaneChip> = second.lanes.iter().map(LaneChip::of).collect();
    assert_eq!(
        expected
            .first()
            .map(|chip| (chip.glyph, chip.note.as_str())),
        Some(("◐", "1/2")),
        "the continuation fixture must report differently than the page it continues"
    );
    store.apply_continuation(second);
    assert_eq!(
        store.lanes(),
        expected,
        "the chips are the newest terminal's own reports, not the page they continue"
    );
}
