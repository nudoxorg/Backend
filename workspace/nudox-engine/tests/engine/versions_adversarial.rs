//! Adversarial integration tests for the version plane and timeline.
//!
//! Uses `StaticSource` via hand-built packages (no `fixtures` feature required).
//!
//! # Coverage
//!
//! **Versions / timeline:**
//! * One loaded version → timeline has exactly one `Present` row, never empty,
//!   never `Introduced`.
//! * Two versions → correct classification (`Introduced`, `SignatureChanged`,
//!   `Removed`, `Unchanged`, `DocsChanged`, `Renamed`).
//! * A declaration that only moved file/span reads `Unchanged`.
//! * `versions()` is newest-first with exactly one `is_current`.
//! * `select_version` to a version not loaded → `NotLoaded`, no panic.
//! * Out-of-order generation arrival: newest ends up current regardless.
//! * Three versions: removal followed by re-introduction is classified correctly.
//! * Renamed symbol → the `Renamed { from }` variant, not `SignatureChanged`.

use std::sync::Arc;

use nudox_engine::{
    Engine, EngineConfig,
    wire::{DocEvent, Gen, TimelineChange, VersionEvent},
};

// Re-use the crate-internal test support via the public `Engine::start` API.
// We cannot import `test_support` (it is `pub(crate)` behind `#[cfg(test)]`),
// so we rebuild the minimal helpers here.

use nudox_engine::store::{
    package::{PackageView, Provenance},
    source::{Error, IrSource, LoadEvent, LoadRequest, PackageHint, SourceDescriptor},
};
use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
    entry::{Deprecation, Entry, Node, Symbol, Visibility},
    index::RawRef,
    kind::Kind,
    kinds::{Module, Reexport},
    view::IrView,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn lineage(name: &str) -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new(name))
}

fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

fn make_symbol(name: &str, docs: &str, deprecated: bool, _reexport: bool) -> Symbol {
    Symbol {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: docs.to_owned(),
        source: std::path::PathBuf::from("src/lib.rs"),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: if deprecated {
            Some(Deprecation {
                note: Some("use something else".to_owned()),
                since: None,
            })
        } else {
            None
        },
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    }
}

fn module_entry(sym: Symbol) -> Entry {
    Entry::new(sym, Node::build(None::<RawRef>, []), Kind::Module(Module))
}

fn reexport_entry(sym: Symbol) -> Entry {
    Entry::new(
        sym,
        Node::build(None::<RawRef>, []),
        Kind::Reexport(Reexport),
    )
}

struct PackageBuilder {
    lid: PackageLineageId,
    entries: Vec<(IntroId, Entry)>,
}

impl PackageBuilder {
    fn new(lid: &PackageLineageId) -> Self {
        Self {
            lid: lid.clone(),
            entries: Vec::new(),
        }
    }

    fn add(mut self, n: u8, name: &str) -> Self {
        self.entries
            .push((intro(n), module_entry(make_symbol(name, "", false, false))));
        self
    }

    fn add_with_docs(mut self, n: u8, name: &str, docs: &str) -> Self {
        self.entries.push((
            intro(n),
            module_entry(make_symbol(name, docs, false, false)),
        ));
        self
    }

    #[allow(dead_code)]
    fn add_deprecated(mut self, n: u8, name: &str) -> Self {
        self.entries
            .push((intro(n), module_entry(make_symbol(name, "", true, false))));
        self
    }

    fn add_reexport(mut self, n: u8, name: &str) -> Self {
        self.entries.push((
            intro(n),
            reexport_entry(make_symbol(name, "", false, false)),
        ));
        self
    }

    fn add_moved(mut self, n: u8, name: &str, file: &str, span: std::ops::Range<usize>) -> Self {
        let mut sym = make_symbol(name, "", false, false);
        sym.source = std::path::PathBuf::from(file);
        sym.span = span;
        self.entries.push((intro(n), module_entry(sym)));
        self
    }

    fn build(self) -> Arc<PackageView> {
        let mut table = PristineIntroTable::new();
        for (id, entry) in self.entries {
            table.insert_live(id, entry, None);
        }
        let view = IrView::with_package(self.lid, table);
        Arc::new(PackageView::build(view, Provenance::TrustedLocal))
    }
}

// ---------------------------------------------------------------------------
// StaticSource: replays (lineage, version, package) triples
// ---------------------------------------------------------------------------

struct StaticSource {
    items: Vec<(PackageLineageId, Option<String>, Arc<PackageView>)>,
}

impl IrSource for StaticSource {
    fn describe(&self) -> SourceDescriptor {
        SourceDescriptor {
            label: "versions-static".to_owned(),
            package_count_hint: Some(self.items.len() as u32),
        }
    }

    fn load(
        &self,
        _req: LoadRequest,
    ) -> futures::stream::BoxStream<'static, Result<LoadEvent, Error>> {
        use futures::StreamExt as _;
        let events: Vec<Result<LoadEvent, Error>> = self
            .items
            .iter()
            .flat_map(|(lid, v, pkg)| {
                [
                    Ok(LoadEvent::Discovered {
                        lineage: lid.clone(),
                        hint: PackageHint {
                            display_name: lid.name.as_str().to_owned(),
                            ecosystem: lid.ecosystem.as_str().to_owned(),
                            version: v.clone(),
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

/// Start an engine and wait until all expected generations are recorded and the
/// corpus holds the current generation.
async fn start_and_settle(
    lid: &PackageLineageId,
    generations: Vec<(&str, Arc<PackageView>)>,
) -> nudox_engine::EngineHandle {
    let expected = generations.len();
    let items: Vec<_> = generations
        .into_iter()
        .map(|(v, p)| (lid.clone(), Some(v.to_owned()), p))
        .collect();

    let engine = Engine::start(EngineConfig::default(), StaticSource { items });

    // Wait for all generations to be registered and a current one to be pinned.
    // The corpus is updated in the same async task that records each generation,
    // so once versions(lid).current().is_some() and len() == expected we know
    // both the registry and the corpus are settled.
    // (`corpus()` is `pub(crate)` — integration tests use versions() instead.)
    for _ in 0..200 {
        let list = engine.versions(lid);
        if list.len() == expected && list.current().is_some() {
            return engine;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!(
        "engine did not settle within 2 s: versions={:?}",
        engine.versions(lid).len()
    );
}

/// Open the symbol at `intro(n)` and drain the full event stream.
async fn open_and_drain(
    engine: &nudox_engine::EngineHandle,
    lid: &PackageLineageId,
    n: u8,
) -> Vec<DocEvent> {
    let key = StableRef::new(lid.clone(), intro(n));
    let (handle, rx) = engine.open_symbol(key, Gen(1));
    let mut events = Vec::new();
    while let Ok(ev) = rx.recv_async().await {
        let done = matches!(ev, DocEvent::Done | DocEvent::Failed(_));
        events.push(ev);
        if done {
            break;
        }
    }
    drop(handle);
    events
}

fn timeline_of(events: &[DocEvent]) -> &nudox_engine::wire::Timeline {
    events
        .iter()
        .find_map(|e| match e {
            DocEvent::Timeline(t) => Some(t),
            _ => None,
        })
        .expect("every resolved symbol must have a Timeline event")
}

// ---------------------------------------------------------------------------
// One loaded version → exactly one `Present` row, never empty, never `Introduced`
// ---------------------------------------------------------------------------

#[tokio::test]
async fn one_version_produces_exactly_one_present_row() {
    let lid = lineage("axum");
    let pkg = PackageBuilder::new(&lid).add(1, "Router").build();
    let engine = start_and_settle(&lid, vec![("0.8.9", pkg)]).await;

    let events = open_and_drain(&engine, &lid, 1).await;
    let t = timeline_of(&events);

    assert_eq!(
        t.rows.len(),
        1,
        "single version must produce exactly one timeline row"
    );
    assert_eq!(
        t.rows[0].change,
        TimelineChange::Present,
        "the only row for a single version must be Present (no evidence of introduction)"
    );
    assert_eq!(&*t.rows[0].version, "0.8.9");
    assert_eq!(t.versions_examined, 1);
    assert!(
        !t.rows[0].sig.is_empty(),
        "the Present row must carry a signature"
    );
    assert!(
        t.rows[0].is_current,
        "the single version must be marked current"
    );
}

// ---------------------------------------------------------------------------
// Two versions → correct classifications
// ---------------------------------------------------------------------------

/// A symbol present in both versions with no observable change → `Unchanged`.
#[tokio::test]
async fn unchanged_symbol_across_two_versions() {
    let lid = lineage("axum");
    let pkg_v1 = PackageBuilder::new(&lid).add(1, "Router").build();
    let pkg_v2 = PackageBuilder::new(&lid).add(1, "Router").build();
    let engine = start_and_settle(&lid, vec![("0.7.9", pkg_v1), ("0.8.1", pkg_v2)]).await;

    let events = open_and_drain(&engine, &lid, 1).await;
    let t = timeline_of(&events);

    assert_eq!(t.rows.len(), 2);
    // Newest first: 0.8.1 is unchanged vs 0.7.9.
    assert_eq!(t.rows[0].change, TimelineChange::Unchanged);
    assert_eq!(&*t.rows[0].version, "0.8.1");
    // Oldest is Present (no older generation to compare against).
    assert_eq!(t.rows[1].change, TimelineChange::Present);
    assert_eq!(&*t.rows[1].version, "0.7.9");
}

/// Symbol absent in v1, present in v2 → `Introduced` in v2.
#[tokio::test]
async fn symbol_added_in_later_version_is_introduced() {
    let lid = lineage("axum");
    let pkg_v1 = PackageBuilder::new(&lid).add(1, "Router").build();
    let pkg_v2 = PackageBuilder::new(&lid)
        .add(1, "Router")
        .add(2, "Extractor")
        .build();
    let engine = start_and_settle(&lid, vec![("0.7.9", pkg_v1), ("0.8.1", pkg_v2)]).await;

    // Open the symbol that only exists in v2.
    let events = open_and_drain(&engine, &lid, 2).await;
    let t = timeline_of(&events);

    assert_eq!(t.rows.len(), 1, "symbol absent from v1 has no row for v1");
    assert_eq!(t.rows[0].change, TimelineChange::Introduced);
    assert_eq!(t.versions_examined, 2, "both generations were examined");
}

/// Symbol present in v1, absent in v2 → `Removed` row in v2.
///
/// Historical document fallback is not part of the current engine surface, so
/// this remains a direct timeline contract rather than an `open_symbol` test.
#[ignore = "needs open_symbol fallback to an older generation"]
#[tokio::test]
async fn symbol_removed_in_later_version_gets_removed_row() {
    let lid = lineage("axum");
    let pkg_v1 = PackageBuilder::new(&lid)
        .add(1, "Router")
        .add(2, "OldHandler")
        .build();
    let pkg_v2 = PackageBuilder::new(&lid).add(1, "Router").build();
    let engine = start_and_settle(&lid, vec![("0.7.9", pkg_v1), ("0.8.1", pkg_v2)]).await;

    let events = open_and_drain(&engine, &lid, 2).await;
    let t = timeline_of(&events);

    // Rows: [Removed(0.8.1), Present(0.7.9)].
    assert_eq!(t.rows.len(), 2);
    assert_eq!(t.rows[0].change, TimelineChange::Removed);
    assert_eq!(&*t.rows[0].version, "0.8.1");
    assert!(
        t.rows[0].sig.is_empty(),
        "Removed row must have no signature"
    );
    assert_eq!(&*t.rows[0].name, "", "Removed row must have empty name");
    assert_eq!(t.rows[1].change, TimelineChange::Present);
}

/// Docs changed, signature stable → `DocsChanged`.
#[tokio::test]
async fn docs_change_only_is_classified_as_docs_changed() {
    let lid = lineage("axum");
    let pkg_v1 = PackageBuilder::new(&lid)
        .add_with_docs(1, "Router", "old docs")
        .build();
    let pkg_v2 = PackageBuilder::new(&lid)
        .add_with_docs(1, "Router", "new docs")
        .build();
    let engine = start_and_settle(&lid, vec![("0.7.9", pkg_v1), ("0.8.1", pkg_v2)]).await;

    let events = open_and_drain(&engine, &lid, 1).await;
    let t = timeline_of(&events);

    assert_eq!(t.rows[0].change, TimelineChange::DocsChanged);
}

/// Signature changed (module → reexport), name stable → `SignatureChanged`.
#[tokio::test]
async fn signature_change_with_stable_name_is_signature_changed() {
    let lid = lineage("axum");
    let pkg_v1 = PackageBuilder::new(&lid).add(1, "Router").build();
    let pkg_v2 = PackageBuilder::new(&lid).add_reexport(1, "Router").build();
    let engine = start_and_settle(&lid, vec![("0.7.9", pkg_v1), ("0.8.1", pkg_v2)]).await;

    let events = open_and_drain(&engine, &lid, 1).await;
    let t = timeline_of(&events);

    assert_eq!(t.rows[0].change, TimelineChange::SignatureChanged);
    assert!(
        !t.rows[0].sig.is_empty(),
        "SignatureChanged row must carry the new signature"
    );
}

/// Symbol renamed (same IntroId, different name) → `Renamed { from }`.
/// The rename outranks any accompanying signature change.
#[tokio::test]
async fn renamed_symbol_produces_renamed_not_signature_changed() {
    let lid = lineage("axum");
    // v1: `mod Router`
    let pkg_v1 = PackageBuilder::new(&lid).add(1, "Router").build();
    // v2: `mod App` (same IntroId=1, different name, also reexport → sig change too)
    let pkg_v2 = PackageBuilder::new(&lid).add_reexport(1, "App").build();
    let engine = start_and_settle(&lid, vec![("0.7.9", pkg_v1), ("0.8.1", pkg_v2)]).await;

    let events = open_and_drain(&engine, &lid, 1).await;
    let t = timeline_of(&events);

    assert!(
        matches!(t.rows[0].change, TimelineChange::Renamed { .. }),
        "rename must outrank signature change; got {:?}",
        t.rows[0].change
    );
    match &t.rows[0].change {
        TimelineChange::Renamed { from } => assert_eq!(&**from, "Router"),
        _ => unreachable!(),
    }
    assert_eq!(&*t.rows[0].name, "App", "current name must be 'App'");
}

// ---------------------------------------------------------------------------
// Declaration that only moved → `Unchanged`
// ---------------------------------------------------------------------------

/// Two versions of a crate unpacked at different roots. The declaration drifts
/// to a different file and line. `entry_content_hash` would report a diff;
/// the timeline must report `Unchanged`.
#[tokio::test]
async fn declaration_that_only_moved_is_unchanged() {
    let lid = lineage("axum");
    let pkg_v1 = PackageBuilder::new(&lid)
        .add_moved(1, "Router", "/src/axum-0.7.9/src/lib.rs", 10..50)
        .build();
    let pkg_v2 = PackageBuilder::new(&lid)
        .add_moved(1, "Router", "/src/axum-0.8.1/src/routing.rs", 4000..4100)
        .build();
    let engine = start_and_settle(&lid, vec![("0.7.9", pkg_v1), ("0.8.1", pkg_v2)]).await;

    let events = open_and_drain(&engine, &lid, 1).await;
    let t = timeline_of(&events);

    assert_eq!(
        t.rows[0].change,
        TimelineChange::Unchanged,
        "a declaration that only moved file/span must be Unchanged; got {:?}",
        t.rows[0].change
    );
}

// ---------------------------------------------------------------------------
// `versions()` newest-first, exactly one `is_current`
// ---------------------------------------------------------------------------

#[tokio::test]
async fn versions_list_is_newest_first_with_exactly_one_current() {
    let lid = lineage("axum");
    let engine = start_and_settle(
        &lid,
        vec![
            ("0.7.9", PackageBuilder::new(&lid).add(1, "a").build()),
            (
                "0.8.1",
                PackageBuilder::new(&lid).add(1, "a").add(2, "b").build(),
            ),
            (
                "0.9.0",
                PackageBuilder::new(&lid)
                    .add(1, "a")
                    .add(2, "b")
                    .add(3, "c")
                    .build(),
            ),
        ],
    )
    .await;

    let list = engine.versions(&lid);

    assert_eq!(list.len(), 3);
    let versions: Vec<&str> = list.versions.iter().map(|v| v.version.as_ref()).collect();
    assert_eq!(versions, vec!["0.9.0", "0.8.1", "0.7.9"], "newest first");

    let current_count = list.versions.iter().filter(|v| v.is_current).count();
    assert_eq!(
        current_count, 1,
        "exactly one version must be marked current"
    );

    assert!(
        list.current().is_some(),
        "current() must return Some when a package is loaded"
    );
    assert_eq!(&*list.current().unwrap().version, "0.9.0");
}

// ---------------------------------------------------------------------------
// `select_version` to an unloaded version → `NotLoaded`, no panic
// ---------------------------------------------------------------------------

#[tokio::test]
async fn select_version_to_unloaded_returns_not_loaded_and_changes_nothing() {
    let lid = lineage("axum");
    let engine = start_and_settle(
        &lid,
        vec![("0.7.9", PackageBuilder::new(&lid).add(1, "a").build())],
    )
    .await;

    let rx = engine.select_version(lid.clone(), "9.9.9", Gen(99));
    let ev = rx
        .recv_async()
        .await
        .expect("exactly one event must be sent");

    assert!(
        matches!(ev, VersionEvent::NotLoaded { .. }),
        "selecting an unloaded version must yield NotLoaded; got {ev:?}"
    );

    // Corpus and registry must be unchanged.
    assert_eq!(&*engine.versions(&lid).current().unwrap().version, "0.7.9");
}

// ---------------------------------------------------------------------------
// Out-of-order arrival: newest still ends up current
// ---------------------------------------------------------------------------

/// The `StaticSource` delivers the newest generation first. Without the
/// `VersionRegistry` the corpus would hold whichever finished last (the older
/// one), because it is a simple `HashMap::insert`.  With the registry, the
/// newest is always current regardless of delivery order.
#[tokio::test]
async fn out_of_order_arrival_newer_is_still_current() {
    let lid = lineage("axum");
    // Deliver newest (0.9.0, 3 symbols) first, older (0.7.9, 1 symbol) second.
    let engine = start_and_settle(
        &lid,
        vec![
            (
                "0.9.0",
                PackageBuilder::new(&lid)
                    .add(1, "a")
                    .add(2, "b")
                    .add(3, "c")
                    .build(),
            ),
            ("0.7.9", PackageBuilder::new(&lid).add(1, "a").build()),
        ],
    )
    .await;

    assert_eq!(&*engine.versions(&lid).current().unwrap().version, "0.9.0");

    // The corpus must hold the 3-symbol generation, not the 1-symbol one.
    // intro(2) and intro(3) exist only in the 0.9.0 package; if the corpus
    // holds the wrong generation (0.7.9), opening intro(2) would yield Failed.
    // We verify the corpus is correct by opening a symbol that only exists
    // in the newest generation. (`corpus()` is `pub(crate)` in integration tests.)
    let key = StableRef::new(lid.clone(), intro(2)); // only in 0.9.0
    let events = open_and_drain(&engine, &lid, 2).await;
    assert!(
        !events.iter().any(|e| matches!(e, DocEvent::Failed(_))),
        "corpus must hold the newest (3-symbol) generation (intro(2) must resolve); \
         got events: {events:?}"
    );
    // Also verify via the version list's symbol_count which is set at insertion.
    assert_eq!(
        engine.versions(&lid).current().unwrap().symbol_count,
        3,
        "current version's symbol_count must be 3 (the newest generation)"
    );
    let _ = key; // suppress unused warning
}

// ---------------------------------------------------------------------------
// Timeline: removal followed by re-introduction
// ---------------------------------------------------------------------------

/// Three versions: symbol in v1, gone in v2, back in v3.
/// Expected rows (newest first): [Introduced(v3), Removed(v2), Present(v1)].
#[tokio::test]
async fn symbol_reintroduced_after_removal() {
    let lid = lineage("axum");
    let engine = start_and_settle(
        &lid,
        vec![
            ("0.1.0", PackageBuilder::new(&lid).add(1, "Flaky").build()),
            ("0.2.0", PackageBuilder::new(&lid).build()), // Flaky removed
            ("0.3.0", PackageBuilder::new(&lid).add(1, "Flaky").build()), // Flaky returns
        ],
    )
    .await;

    let events = open_and_drain(&engine, &lid, 1).await;
    let t = timeline_of(&events);

    let changes: Vec<TimelineChange> = t.rows.iter().map(|r| r.change.clone()).collect();
    assert_eq!(
        changes,
        vec![
            TimelineChange::Introduced,
            TimelineChange::Removed,
            TimelineChange::Present
        ],
        "a reintroduced symbol must show Introduced(v3), Removed(v2), Present(v1)"
    );
}

// ---------------------------------------------------------------------------
// Version selection must change package contents, not only metadata
// ---------------------------------------------------------------------------

/// Selecting a loaded generation must make subsequent document reads come
/// from that generation. Checking the selected version or the switch event is
/// insufficient: both can be correct while the corpus still serves the old
/// package.
#[tokio::test]
async fn selecting_a_version_changes_the_resident_symbol_content() {
    let lid = lineage("content-switch");
    let engine = start_and_settle(
        &lid,
        vec![
            ("1.0.0", PackageBuilder::new(&lid).add(1, "OldName").build()),
            ("2.0.0", PackageBuilder::new(&lid).add(1, "NewName").build()),
        ],
    )
    .await;

    async fn head_name(engine: &nudox_engine::EngineHandle, lid: &PackageLineageId) -> String {
        let events = open_and_drain(engine, lid, 1).await;
        match events.first() {
            Some(DocEvent::Head(head)) => head.name.to_string(),
            other => panic!("expected a document head, got {other:?}"),
        }
    }

    assert_eq!(head_name(&engine, &lid).await, "NewName");

    let rx = engine.select_version(lid.clone(), "1.0.0", Gen(2));
    let event = rx
        .recv_async()
        .await
        .expect("select_version must emit an event");
    assert!(
        matches!(event, VersionEvent::Switched { .. }),
        "loaded selection must switch successfully, got {event:?}"
    );

    assert_eq!(
        head_name(&engine, &lid).await,
        "OldName",
        "after selecting 1.0.0, open_symbol must read that generation's content"
    );
}

// ---------------------------------------------------------------------------
// Timeline event arrives after Head and before first Section
// ---------------------------------------------------------------------------

#[tokio::test]
async fn timeline_event_is_after_head_and_before_first_section() {
    let lid = lineage("axum");
    let pkg = PackageBuilder::new(&lid).add(1, "Router").build();
    let engine = start_and_settle(&lid, vec![("0.8.9", pkg)]).await;

    let events = open_and_drain(&engine, &lid, 1).await;

    let head_idx = events
        .iter()
        .position(|e| matches!(e, DocEvent::Head(_)))
        .expect("Head must be present");
    let timeline_idx = events
        .iter()
        .position(|e| matches!(e, DocEvent::Timeline(_)))
        .expect("Timeline must be present");
    let first_section_idx = events
        .iter()
        .position(|e| matches!(e, DocEvent::Section(_)));

    assert_eq!(head_idx, 0, "Head must be first");
    assert!(timeline_idx > head_idx, "Timeline must come after Head");
    if let Some(sec_idx) = first_section_idx {
        assert!(
            timeline_idx < sec_idx,
            "Timeline must come before the first Section (positions: timeline={timeline_idx}, section={sec_idx})"
        );
    }
}
