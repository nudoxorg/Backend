//! Shell behaviour, end to end through the real window root: regions
//! re-render only for their own slice, keys walk and act, held modifiers
//! reveal, the descent plays, storms settle to exactly what a fresh window
//! shows, and an idle window costs nothing.
//!
//! Page data comes from a fixture reader behind the real read pool and
//! store, so every landing goes through the same wake path as the product.

#![allow(clippy::expect_used, clippy::panic, clippy::too_many_lines)]

use super::root::Shell;
use crate::core::{LocalProjectId, VersionedRoot};
use crate::model::pages::{
    Arrival, DeclRef, DocFragment, Excerpt, Gap, GapReason, HealthModel, IndexedPackage,
    IngestModel, Known, LineSpan, Member, Members, MethodGroup, OrbitModel, OutlineNode,
    OutlineTree, PackageDossier, PackageRecord, PackageRef, PageValue, Provenance, Readiness,
    ReadFailure, Receiver, RecordSource, Relation, RelationKind, Rose, SearchPage, SearchRow,
    MatchReason, SignatureText, SourceLocation, SourceOrigin, SourceSite, SourceText, SourceView,
    SymbolPage, SymbolRef,
};
use crate::model::{AppSnapshot, DensityPreference, SessionState};
use crate::navigation::{Coordinate, Intent, Route, SymbolRoute, View};
use crate::runtime::actor::{EngineActor, EngineClient, EngineDto, EngineFault, EngineRequest};
use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use crate::runtime::{DesktopRuntime, UiEntityGraph};
use backend_library::DeclarationKind;
use gpui::{
    AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext, WindowHandle, point, px,
    size,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The fixture package root.
pub(crate) const PACKAGE: &str = "/fixture/present";

pub(crate) fn coordinate(name: &str) -> String {
    format!("{PACKAGE}::glyph.rs:138::{name}")
}

pub(crate) fn symbol(name: &str) -> SymbolRef {
    SymbolRef::new(&coordinate(name)).expect("symbol")
}

fn package() -> PackageRef {
    PackageRef::parse(PACKAGE).expect("package")
}

fn decl(name: &str, kind: DeclarationKind) -> DeclRef {
    DeclRef::from_label(&coordinate(name), None, Some(kind), None).expect("decl")
}

fn unknown(reason: GapReason) -> Gap {
    Gap::new(reason, "")
}

fn member(name: &str, kind: DeclarationKind, signature: &str, summary: &str) -> Member {
    Member {
        decl: decl(name, kind),
        signature: Known::Known(SignatureText {
            text: Arc::from(signature),
            tokens: Arc::from([]),
        }),
        summary: Some(Arc::from(summary)),
    }
}

fn relation(name: &str, kind: DeclarationKind) -> Relation {
    Relation {
        decl: decl(name, kind),
        kind: RelationKind::Contains,
        provenance: Provenance::Structural,
        arrival: Arrival::NotReported,
        via: None,
    }
}

/// A declaration page with members, docs, relations and a real excerpt.
pub(crate) fn page(name: &str) -> SymbolPage {
    SymbolPage {
        identity: decl(name, DeclarationKind::Enum),
        package: Known::Known(package()),
        signature: Known::Known(SignatureText {
            text: Arc::from(format!("pub enum {name}")),
            tokens: Arc::from([]),
        }),
        docs: Arc::from([DocFragment::Text(Arc::from(format!(
            "The readable label of {name}.\n\nIt names one relation group."
        )))]),
        site: SourceSite {
            location: Known::Known(SourceLocation {
                path: Arc::from("glyph.rs"),
                line: 138,
            }),
            excerpt: Known::Known(Excerpt {
                text: Arc::from(format!("pub enum {name} {{\n    Typed(SemanticLinkKind),\n    Related,\n}}")),
                lines: Some(LineSpan { first: 138, last: 141 }),
                complete: true,
            }),
        },
        members: Known::Known(Members {
            made_of: Arc::from([
                member("Typed", DeclarationKind::Variant, "Typed(SemanticLinkKind)", "A relation whose kind is known."),
                member("Related", DeclarationKind::Variant, "Related", "Related, and nothing more is known."),
            ]),
            does: Arc::from([MethodGroup {
                receiver: Receiver::Reads,
                members: Arc::from([
                    member("as_str", DeclarationKind::Method, "pub fn as_str(self) -> &'static str", "The words this group prints."),
                    member("is_typed", DeclarationKind::Method, "pub fn is_typed(&self) -> bool", "Whether the compiler proved the kind."),
                ]),
            }]),
            other: Arc::from([]),
        }),
        rose: Rose {
            up: Known::Known(Arc::from([relation("Display", DeclarationKind::Trait)])),
            down: Known::Known(Arc::from([relation("Typed", DeclarationKind::Variant), relation("Related", DeclarationKind::Variant)])),
            left: Known::Unknown(unknown(GapReason::NoSemanticPublication)),
            right: Known::Known(Arc::from([])),
            implemented_by: Known::Known(Arc::from([])),
        },
        references: Known::Unknown(unknown(GapReason::NoSemanticPublication)),
        outline: Known::Unknown(unknown(GapReason::NotServed)),
    }
}

fn dossier() -> PackageDossier {
    let node = |name: &str, kind: DeclarationKind, children: Vec<OutlineNode>| OutlineNode {
        decl: decl(name, kind),
        children: Arc::from(children),
    };
    PackageDossier {
        package: package(),
        record: Known::Known(PackageRecord {
            package: package(),
            source: RecordSource::LocalManifest,
            name: Arc::from("present"),
            version: Known::Known(Arc::from("0.4.2")),
            ecosystem: Known::Known(Arc::from("cargo")),
            standing: Known::Unknown(unknown(GapReason::LocalProject)),
            downloads: Known::Unknown(unknown(GapReason::LocalProject)),
            bytes: Known::Unknown(unknown(GapReason::LocalProject)),
            advisory: Known::Unknown(unknown(GapReason::LocalProject)),
            description: Known::Known(Arc::from("How one symbol page reads.")),
            license: Known::Known(Arc::from("MIT")),
        }),
        versions: Known::Unknown(unknown(GapReason::LocalProject)),
        dependencies: Known::Known(Arc::from([])),
        dependents: Known::Unknown(unknown(GapReason::LocalProject)),
        outline: Known::Known(OutlineTree {
            roots: Arc::from([
                node("identity", DeclarationKind::Module, vec![node("Identity", DeclarationKind::Struct, vec![])]),
                node(
                    "glyph",
                    DeclarationKind::Module,
                    vec![
                        node("RelationLabel", DeclarationKind::Enum, vec![]),
                        node("RelationDirection", DeclarationKind::Enum, vec![]),
                        node("KindGlyph", DeclarationKind::Struct, vec![]),
                        node("relation_label", DeclarationKind::Function, vec![]),
                    ],
                ),
                node("outline", DeclarationKind::Module, vec![node("Outline", DeclarationKind::Struct, vec![])]),
            ]),
            complete: true,
        }),
        readme: Known::Unknown(unknown(GapReason::NotCaptured)),
    }
}

fn health() -> HealthModel {
    HealthModel {
        lanes: backend_present::CoverageLine::new(&[], Some(1_234)),
        rows: 1_234,
        ingest: IngestModel {
            files_discovered: 20,
            files_indexed: 20,
            files_unavailable: 0,
            declarations: 1_234,
            languages: Arc::from([]),
            faults: Arc::from([]),
        },
        ready_capabilities: Arc::from([]),
        missing_capabilities: Arc::from([]),
    }
}

/// Answers every page from the fixtures; searches name their query.
pub(crate) struct Fixture;

impl PageReader for Fixture {
    fn read(&mut self, request: &ReadRequest, _: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        Ok(match request {
            ReadRequest::Symbol(symbol) => PageValue::Symbol(page(symbol.identity().name())),
            ReadRequest::Source(symbol) => {
                let name = symbol.identity().name().to_owned();
                PageValue::Source(SourceView {
                    symbol: decl(&name, DeclarationKind::Enum),
                    file: Known::Known(Arc::from("glyph.rs")),
                    text: Known::Known(SourceText {
                        text: Arc::from(format!("// lead\npub enum {name} {{\n    Typed,\n}}\n// tail\n")),
                        first_line: 137,
                        origin: SourceOrigin::LocalFile,
                        complete: true,
                    }),
                    declaration: Known::Known(LineSpan { first: 138, last: 140 }),
                    identifiers: Known::Known(Arc::from([])),
                    uses: Known::Known(Arc::from([])),
                    uses_elsewhere: Arc::from([]),
                })
            }
            ReadRequest::Package(_) => PageValue::Package(dossier()),
            ReadRequest::Orbit => PageValue::Orbit(OrbitModel {
                indexed: Known::Known(Arc::from([IndexedPackage {
                    package: package(),
                    name: Arc::from("present"),
                    readiness: Readiness::Ready,
                }])),
                projects: Known::Known(Arc::from([])),
                explore: Known::Unknown(unknown(GapReason::NotServed)),
                tree: Known::Unknown(unknown(GapReason::NotServed)),
            }),
            ReadRequest::Health => PageValue::Health(health()),
            ReadRequest::Search(query) | ReadRequest::SearchMore { query, .. } => PageValue::Search(SearchPage {
                query: Arc::clone(&query.text),
                rows: Arc::from([SearchRow {
                    rank: 0,
                    decl: decl(&query.text, DeclarationKind::Struct),
                    package: Some(Arc::from(PACKAGE)),
                    score: Known::Unknown(unknown(GapReason::NotServed)),
                    signature: Known::Unknown(unknown(GapReason::NotServed)),
                    snippet: None,
                    reason: MatchReason::ExactName,
                }]),
                coverage: backend_present::CoverageLine::new(&[], None),
                next: None,
            }),
        })
    }
}

/// Answers the root at once; nothing else is asked of it here.
pub(crate) struct RootOnly;

impl EngineClient for RootOnly {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        match request {
            EngineRequest::Root { request, basis, .. } => {
                let revision = backend_library::Cursor::at(basis.root(), basis.generation().saturating_add(1));
                Ok(EngineDto::Root {
                    request: *request,
                    basis: *basis,
                    key: VersionedRoot::from_revision(basis.producer_epoch(), revision, basis.observation()),
                    revision,
                    delta: None,
                    project: None,
                    catalog: None,
                })
            }
            _ => Err(EngineFault::Cancelled),
        }
    }
}

/// The route of the fixture's `name` page.
pub(crate) fn page_route(name: &str) -> Route {
    view_route(name, View::Page)
}

/// The route of the fixture's `name` declaration shown as `view`.
pub(crate) fn view_route(name: &str, view: View) -> Route {
    Route::Symbol(SymbolRoute {
        project: None,
        package: crate::core::PackageId::new(PACKAGE).expect("package id"),
        id: Coordinate::new(&coordinate(name)).expect("coordinate"),
        at: None,
        view,
        line: None,
        selected: None,
    })
}

pub(crate) struct Rig {
    pub window: WindowHandle<Shell>,
    pub shell: Entity<Shell>,
    pub graph: UiEntityGraph,
    pub cx: &'static mut VisualTestContext,
}

/// Opens a real shell window at `route` (after an Orbit start, so the
/// thread has a bead behind), at `width`×`height`.
pub(crate) fn rig(cx: &mut TestAppContext, route: Option<Route>, width: f32, height: f32) -> Rig {
    cx.executor().allow_parking();
    cx.update(|cx| {
        gpui_component::init(cx);
        let _ = facet::fonts::install(cx);
    });
    let mut snapshot = AppSnapshot::empty(VersionedRoot::synthetic(
        backend_library::view_state_root(&[("shell".to_owned(), "tests".to_owned())]),
        4,
    ));
    let folder = std::env::temp_dir().join(format!("nudox-shell-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&folder);
    let mut workspace = snapshot.workspace().clone();
    workspace.host = LocalProjectId::from_path(&folder).ok();
    snapshot = snapshot.with_workspace(workspace);
    snapshot = snapshot.with_session(SessionState::default());
    let actor = EngineActor::start(RootOnly, 8).expect("actor");
    let runtime = DesktopRuntime::new(snapshot, actor);
    let pool = ReadPool::start(2, |_| Fixture).expect("pool");
    let graph = cx.update(|cx| UiEntityGraph::install_with_reads(cx, runtime, None, Some(pool)));
    let window_graph = UiEntityGraph {
        root: graph.root.clone(),
        store: graph.store.clone(),
    };
    let window = cx.update(|cx| {
        cx.bind_keys(super::keys::bindings());
        cx.open_window(
            gpui::WindowOptions {
                window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds::new(
                    point(px(0.0), px(0.0)),
                    size(px(width), px(height)),
                ))),
                ..gpui::WindowOptions::default()
            },
            |window, cx| cx.new(|cx| Shell::new(&window_graph, window, cx)),
        )
        .expect("window")
    });
    let shell = window.root(cx).expect("shell");
    let visual = VisualTestContext::from_window(window.into(), cx).into_mut();
    visual.update(|window, _| window.activate_window());
    let mut rig = Rig {
        window,
        shell,
        graph,
        cx: visual,
    };
    rig.settle();
    if let Some(route) = route {
        rig.go(Intent::Navigate(route));
    }
    rig
}

impl Rig {
    /// Draws one frame.
    pub fn draw(&mut self) {
        self.cx.update(|window, cx| window.draw(cx).clear(cx));
        self.cx.run_until_parked();
    }

    /// Redraws every view (cached ones included): the debug-bounds map is
    /// rebuilt only by views that actually paint.
    pub fn repaint(&mut self) {
        self.cx.update(|window, _| window.refresh());
        self.draw();
    }

    /// Lets reads land and motion finish, drawing as a platform would.
    pub fn settle(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            self.cx.run_until_parked();
            self.draw();
            // Advance the virtual clock past every motion budget.
            self.cx.executor().advance_clock(Duration::from_millis(700));
            self.cx.run_until_parked();
            let frames = self.cx.update(|window, cx| window.simulate_next_frame(cx));
            self.draw();
            let (queued, running) = self.graph.store.read_with(self.cx, |store, _| store.pool_load());
            if frames == 0 && queued == 0 && running == 0 {
                self.draw();
                return;
            }
            assert!(Instant::now() < deadline, "the shell never settled");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Queues an intent on the state owner and settles.
    pub fn go(&mut self, intent: Intent) {
        self.graph.root.update(self.cx, |root, cx| root.queue(intent, cx));
        self.settle();
    }

    pub fn counts(&mut self) -> super::RenderCounts {
        self.shell.read_with(self.cx, |shell, cx| shell.render_counts(cx))
    }

    pub fn said(&mut self) -> Vec<String> {
        self.shell
            .read_with(self.cx, |shell, cx| shell.reader_text(cx))
            .into_iter()
            .map(|text| text.to_string())
            .collect()
    }

    pub fn route(&mut self) -> Route {
        self.graph.store.read_with(self.cx, |store, _| store.snapshot().route().clone())
    }

    pub fn keys(&mut self, keys: &str) {
        self.cx.simulate_keystrokes(keys);
        self.settle();
    }
}

#[gpui::test]
fn a_page_renders_its_real_content_through_the_shell(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    let said = rig.said();
    for expected in [
        "RelationLabel",
        "The readable label of RelationLabel.",
        "pub enum RelationLabel {\n    Typed(SemanticLinkKind),\n    Related,\n}",
        "Made of",
        "Typed(SemanticLinkKind)",
        "A relation whose kind is known.",
        "Does",
        "reads",
        "as_str(self) -> &'static str",
        "Display",
        "Relations need a compiler publication; this package has none.",
    ] {
        assert!(said.iter().any(|line| line == expected), "{expected:?} is not on screen: {said:#?}");
    }
    // The route and the store agree, and the thread has Orbit behind.
    assert_eq!(rig.route(), page_route("RelationLabel"));
}

#[gpui::test]
fn a_hover_wave_in_the_reader_re_renders_only_the_reader(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    let before = rig.counts();
    // Sweep the pointer down the reader, across every row.
    for step in 0..60 {
        let y = 120.0 + step as f32 * 12.0;
        rig.cx.simulate_mouse_move(point(px(760.0), px(y)), None, Modifiers::default());
        rig.draw();
    }
    let after = rig.counts();
    assert!(after.reader > before.reader, "the reader drew its hover states: {before:?} → {after:?}");
    assert_eq!(
        (after.titlebar, after.shelf, after.status, after.pins),
        (before.titlebar, before.shelf, before.status, before.pins),
        "nothing outside the reader re-rendered"
    );
}

#[gpui::test]
fn a_page_landing_re_renders_only_the_regions_that_show_it(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    let before = rig.counts();
    // Re-read the page: a resource event for the reader's key only.
    let key = crate::model::pages::PageKey::Symbol(symbol("RelationLabel"));
    rig.graph.store.update(rig.cx, |store, cx| store.retry(key, cx));
    rig.settle();
    let after = rig.counts();
    assert!(after.reader > before.reader, "the reader redrew its page");
    assert_eq!(after.status, before.status, "the address did not change");
    assert_eq!(after.shelf, before.shelf, "the shelf does not show the page's slot");
}

#[gpui::test]
fn j_and_k_walk_focus_inside_the_reader_only(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    // The first key after the pointer switches GPUI's input modality, which
    // repaints the whole window once (its hover suppression); measure after.
    rig.keys("x");
    let before = rig.counts();
    rig.keys("j");
    let (_, first) = rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
    rig.keys("j j");
    let (zone, third) = rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
    assert_eq!(zone, super::focus::Zone::Reader);
    assert!(first.is_some() && third.is_some() && first != third, "{first:?} → {third:?}");
    rig.keys("k k");
    let (_, back) = rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
    assert_eq!(back, first, "K walks back the way J came");
    let after = rig.counts();
    assert_eq!(
        (after.titlebar, after.shelf, after.status),
        (before.titlebar, before.shelf, before.status),
        "a focus walk re-renders only its zone"
    );
    // Tab moves the keyboard to the next zone.
    rig.keys("tab");
    let (zone, _) = rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
    assert_eq!(zone, super::focus::Zone::Titlebar);
}

#[gpui::test]
fn enter_descends_and_the_descent_plays_down_then_up(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(Route::Package(crate::navigation::PackageRoute {
        project: None,
        at: None,
        package: crate::core::PackageId::new(PACKAGE).expect("package"),
        lane: crate::navigation::PackageLane::Overview,
        selected: None,
    })), 1440.0, 900.0);
    let (before, _) = rig.shell.read_with(rig.cx, |shell, cx| shell.descent(cx));
    // Walk to the first "start with" row and open it.
    rig.keys("j");
    rig.keys("enter");
    let route = rig.route();
    assert!(matches!(route, Route::Symbol(_)), "enter opened a page: {route:?}");
    let (after, way) = rig.shell.read_with(rig.cx, |shell, cx| shell.descent(cx));
    assert_eq!((after, way), (before + 1, Some(super::reader::Way::Down)));
    // ⌘- surfaces one depth: the descent plays up.
    rig.keys("cmd-up");
    assert!(matches!(rig.route(), Route::Package(_)));
    let (_, way) = rig.shell.read_with(rig.cx, |shell, cx| shell.descent(cx));
    assert_eq!(way, Some(super::reader::Way::Up));
    // ⌘[ walks back along the thread.
    rig.keys("cmd-[");
    assert!(matches!(rig.route(), Route::Symbol(_)));
}

#[gpui::test]
fn holding_command_shows_keys_only_after_the_hold_and_only_where_keys_are(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    let before = rig.counts();
    rig.cx.simulate_modifiers_change(Modifiers {
        platform: true,
        ..Modifiers::default()
    });
    rig.cx.run_until_parked();
    assert!(!rig.cx.update(|_, cx| facet::ActiveFacet::facet(cx).reveal.keys), "nothing shows before the hold");
    rig.cx.executor().advance_clock(super::reveal::HOLD + Duration::from_millis(10));
    rig.cx.run_until_parked();
    rig.draw();
    assert!(rig.cx.update(|_, cx| facet::ActiveFacet::facet(cx).reveal.keys), "keys show after the hold");
    let held = rig.counts();
    assert!(held.titlebar > before.titlebar, "the titlebar raised its caps");
    assert_eq!((held.reader, held.status), (before.reader, before.status), "regions without keys did not re-render");
    rig.cx.simulate_modifiers_change(Modifiers::default());
    rig.cx.run_until_parked();
    assert!(!rig.cx.update(|_, cx| facet::ActiveFacet::facet(cx).reveal.keys), "release hides them at once");

    // A chord never flashes caps.
    rig.cx.simulate_modifiers_change(Modifiers {
        platform: true,
        ..Modifiers::default()
    });
    rig.cx.simulate_keystrokes("cmd-c");
    rig.cx.executor().advance_clock(super::reveal::HOLD * 2);
    rig.cx.run_until_parked();
    assert!(!rig.cx.update(|_, cx| facet::ActiveFacet::facet(cx).reveal.keys), "⌘C is a chord");
    rig.cx.simulate_modifiers_change(Modifiers::default());

    // ⌥ x-rays the reader, not the titlebar.
    let before = rig.counts();
    rig.cx.simulate_modifiers_change(Modifiers {
        alt: true,
        ..Modifiers::default()
    });
    rig.cx.executor().advance_clock(super::reveal::HOLD + Duration::from_millis(10));
    rig.cx.run_until_parked();
    rig.draw();
    let xray = rig.counts();
    assert!(rig.cx.update(|_, cx| facet::ActiveFacet::facet(cx).reveal.xray));
    assert!(xray.reader > before.reader, "the reader rose a rung");
    assert_eq!(xray.titlebar, before.titlebar, "the titlebar has nothing to x-ray");
    // Losing focus releases everything.
    rig.cx.deactivate_window();
    rig.cx.run_until_parked();
    assert_eq!(rig.cx.update(|_, cx| facet::ActiveFacet::facet(cx).reveal), facet::Reveal::default());
}

#[gpui::test]
fn every_setting_applies_live(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    let before = rig.counts();
    rig.go(Intent::SetDensity(DensityPreference::Dense));
    let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
    rig.go(Intent::ZoomTo {
        display: display.clone(),
        percent: 125,
    });
    rig.go(Intent::SetAppearance(crate::model::AppearancePreference::Glacier));
    rig.go(Intent::SetContrast(crate::model::ContrastPreference::High));
    rig.go(Intent::SetMotion(crate::model::MotionPreference::Reduced));
    let facet = rig.cx.update(|_, cx| facet::ActiveFacet::facet(cx));
    assert_eq!(facet.density, facet::Density::Dense);
    assert!((facet.text_scale - 1.25).abs() < 1e-6);
    assert_eq!(facet.appearance, facet::Appearance::Glacier);
    assert_eq!(facet.contrast, facet::Contrast::High);
    assert!(facet.reduced_motion);
    // Every region re-resolved against the new facet: none is stale.
    let after = rig.counts();
    assert!(after.titlebar > before.titlebar && after.shelf > before.shelf && after.reader > before.reader && after.status > before.status);
    // Dense folds the ledger's summaries away.
    assert!(!rig.said().iter().any(|line| line == "A relation whose kind is known."));
    // At 125 % a 1440 px window is 1152 effective: still a shelf.
    let frame = rig.shell.read_with(rig.cx, |shell, _| shell.frame()).expect("frame");
    assert_eq!(frame.shelf, super::ShelfMode::Shelf);
    rig.go(Intent::ZoomTo { display, percent: 200 });
    let frame = rig.shell.read_with(rig.cx, |shell, _| shell.frame()).expect("frame");
    assert_eq!(frame.shelf, super::ShelfMode::Spine, "200 % text behaves like a 720 px window");
}

#[gpui::test]
fn escape_closes_the_topmost_transient_first(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    // Stand on a member row and peek it.
    rig.keys("j j j j j j");
    rig.keys("space");
    let (_, peek, _) = rig.shell.read_with(rig.cx, |shell, _| shell.transients());
    assert!(peek, "space opened a peek");
    rig.keys("f");
    let (_, peek, hints) = rig.shell.read_with(rig.cx, |shell, _| shell.transients());
    assert!(peek && hints);
    rig.keys("escape");
    let (_, peek, hints) = rig.shell.read_with(rig.cx, |shell, _| shell.transients());
    assert!(peek && !hints, "esc closed the hints first");
    rig.keys("escape");
    let (_, peek, _) = rig.shell.read_with(rig.cx, |shell, _| shell.transients());
    assert!(!peek, "then the peek");
    rig.keys("cmd-k");
    let (ask, _, _) = rig.shell.read_with(rig.cx, |shell, _| shell.transients());
    assert!(ask, "⌘K opened Ask");
    rig.keys("escape");
    let (ask, _, _) = rig.shell.read_with(rig.cx, |shell, _| shell.transients());
    assert!(!ask, "esc closed Ask");
    assert_eq!(rig.route(), page_route("RelationLabel"), "and nothing else moved");
}

#[gpui::test]
fn hint_mode_labels_every_visible_target_and_a_code_activates_one(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    rig.keys("f");
    let (_, _, hints) = rig.shell.read_with(rig.cx, |shell, _| shell.transients());
    assert!(hints);
    // Type codes until one fires: every code is at most two letters.
    rig.keys("a");
    let (_, _, still) = rig.shell.read_with(rig.cx, |shell, _| shell.transients());
    if still {
        rig.keys("a");
    }
    let (_, _, hints) = rig.shell.read_with(rig.cx, |shell, _| shell.transients());
    assert!(!hints, "a full code ended hint mode");
}

#[gpui::test]
fn the_regions_degrade_with_the_window_and_the_text(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 2560.0, 1440.0);
    for (width, shelf) in [
        (2560.0, super::ShelfMode::Shelf),
        (1440.0, super::ShelfMode::Shelf),
        (1100.0, super::ShelfMode::Shelf),
        (899.0, super::ShelfMode::Spine),
        (760.0, super::ShelfMode::Spine),
        (639.0, super::ShelfMode::Hidden),
        (480.0, super::ShelfMode::Hidden),
    ] {
        rig.cx.simulate_resize(size(px(width), px(900.0)));
        rig.settle();
        let frame = rig.shell.read_with(rig.cx, |shell, _| shell.frame()).expect("frame");
        assert_eq!(frame.shelf, shelf, "at {width}");
        // The page is on screen at every width.
        assert!(rig.said().iter().any(|line| line == "RelationLabel"), "at {width}");
    }
}

/// Seeded storm: navigation, lenses, resizes, shelf toggles, modifier holds,
/// peeks and Ask, dozens per second. No panic, the keyboard stays live, and
/// after settling the reader shows exactly what a fresh window at the same
/// place shows.
#[gpui::test]
fn a_storm_settles_to_exactly_what_a_fresh_window_shows(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    let names = ["RelationLabel", "RelationDirection", "KindGlyph", "Identity", "Outline"];
    let mut seed: u64 = 0x5eed_cafe;
    let mut next = move |bound: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % bound
    };
    for _ in 0..240 {
        match next(16) {
            0 | 1 => {
                let name = names[next(names.len() as u64) as usize];
                rig.graph.root.update(rig.cx, |root, cx| root.queue(Intent::Navigate(page_route(name)), cx));
            }
            2 => rig.cx.simulate_keystrokes("cmd-["),
            3 => rig.cx.simulate_keystrokes("cmd-]"),
            4 => rig.cx.simulate_keystrokes("cmd-up"),
            5 => rig.cx.simulate_keystrokes("j j k"),
            6 => rig.cx.simulate_resize(size(px(420.0 + next(2200) as f32), px(900.0))),
            7 => rig.cx.simulate_keystrokes("cmd-\\"),
            8 => {
                rig.cx.simulate_modifiers_change(Modifiers {
                    platform: next(2) == 0,
                    alt: next(2) == 0,
                    ..Modifiers::default()
                });
            }
            9 => rig.cx.simulate_keystrokes("space"),
            10 => rig.cx.simulate_keystrokes("cmd-k"),
            12 => rig.cx.simulate_keystrokes("g"),
            13 => rig.cx.simulate_keystrokes("cmd-."),
            14 => rig.cx.simulate_keystrokes(if next(2) == 0 { "cmd-=" } else { "cmd--" }),
            15 => {
                let release = crate::navigation::ReleaseId::new("9.9.9").expect("release");
                rig.graph.root.update(rig.cx, |root, cx| root.queue(Intent::SetRelease(Some(release)), cx));
            }
            _ => rig.cx.simulate_keystrokes("escape"),
        }
        rig.cx.run_until_parked();
        if next(3) == 0 {
            rig.draw();
        }
        rig.cx.executor().advance_clock(Duration::from_millis(next(40)));
    }
    // Put the window somewhere definite and let everything land.
    rig.cx.simulate_modifiers_change(Modifiers::default());
    rig.cx.simulate_keystrokes("escape escape escape cmd-0");
    rig.cx.simulate_resize(size(px(1440.0), px(900.0)));
    rig.go(Intent::Navigate(page_route("KindGlyph")));
    rig.settle();
    let storm = rig.said();
    let (transients, reveal) = (
        rig.shell.read_with(rig.cx, |shell, _| shell.transients()),
        rig.cx.update(|_, cx| facet::ActiveFacet::facet(cx).reveal),
    );
    assert_eq!(transients, (false, false, false), "nothing transient is left open");
    assert_eq!(reveal, facet::Reveal::default(), "no reveal is stuck");
    // The keyboard is live.
    rig.keys("j");
    let (_, focused) = rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
    assert!(focused.is_some(), "focus still walks after the storm");

    let mut fresh = self::rig(rig.cx, Some(page_route("KindGlyph")), 1440.0, 900.0);
    assert_eq!(storm, fresh.said(), "settle equals fresh");
}

/// Every `(width, text %)` of the verification matrix.
const MATRIX: [(f32, u16); 8] = [
    (2560.0, 100),
    (1440.0, 100),
    (1100.0, 100),
    (760.0, 100),
    (480.0, 100),
    (1440.0, 200),
    (480.0, 200),
    (360.0, 200),
];

#[gpui::test]
fn the_hero_name_is_whole_at_every_width_and_text_size(cx: &mut TestAppContext) {
    let long = "RelationLabelWithAVeryLongCompoundIdentifierName";
    let mut rig = rig(cx, Some(page_route(long)), 1440.0, 900.0);
    let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
    for (width, percent) in MATRIX {
        rig.cx.simulate_resize(size(px(width), px(900.0)));
        rig.go(Intent::ZoomTo {
            display: display.clone(),
            percent,
        });
        let lines = rig.shell.read_with(rig.cx, |shell, cx| shell.hero_lines(cx));
        assert_eq!(lines.concat(), long, "at {width} px, {percent} %: {lines:?}");
        assert!(
            lines.iter().all(|line| !line.contains('…') && !line.ends_with('-')),
            "no ellipsis, no hyphen glyph: {lines:?}"
        );
    }
    // Narrow and big: the name really wrapped (it did not simply overflow).
    let lines = rig.shell.read_with(rig.cx, |shell, cx| shell.hero_lines(cx));
    assert!(lines.len() > 1, "{lines:?}");
}

#[gpui::test]
fn a_view_switch_replaces_the_entry_keeps_the_shared_mark_and_back_leaves(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    let back = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().back.len());
    let selector: &'static str = Box::leak(super::kit::shared_key(&symbol("RelationLabel")).into_boxed_str());
    rig.repaint();
    assert!(rig.cx.debug_bounds(selector).is_some(), "the page's hero gem carries the shared id");
    rig.keys("cmd-.");
    assert_eq!(rig.route(), view_route("RelationLabel", View::Code));
    assert!(rig.said().iter().any(|line| line.contains("pub enum RelationLabel {")), "the code view shows the source");
    rig.keys("g");
    assert_eq!(rig.route(), view_route("RelationLabel", View::Graph));
    rig.repaint();
    assert!(rig.cx.debug_bounds(selector).is_some(), "the graph's node carries the same id");
    let after = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().back.len());
    assert_eq!(after, back, "three views, no history pushed");
    rig.keys("g");
    assert_eq!(rig.route(), page_route("RelationLabel"), "G again: back to the page");
    rig.keys("cmd-[");
    assert!(matches!(rig.route(), Route::Orbit(_)), "Back leaves the declaration: {:?}", rig.route());
    // G with nothing selected: the whole world.
    rig.keys("g");
    assert_eq!(rig.route(), Route::World);
    assert!(rig.said().iter().any(|line| line == "The graph of everything is being drawn."));
}

#[gpui::test]
fn viewing_another_release_says_so_and_escape_returns_to_the_pin(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    let release = crate::navigation::ReleaseId::new("0.3.0").expect("release");
    rig.go(Intent::SetRelease(Some(release.clone())));
    assert_eq!(rig.route().at(), Some(&release));
    let here = rig.graph.store.read_with(rig.cx, |store, _| {
        super::thread::here(&store.snapshot(), store).path.to_string()
    });
    assert_eq!(here, "viewing 0.3.0 · you pin your checkout", "a local project pins its checkout");
    rig.keys("escape");
    assert_eq!(rig.route(), page_route("RelationLabel"), "Esc returns to the pinned release");
}

#[gpui::test]
fn the_zoom_keys_move_this_display_only_and_persist(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
    let scale = |rig: &mut Rig| rig.cx.update(|_, cx| facet::ActiveFacet::facet(cx).text_scale);
    assert!((scale(&mut rig) - 1.0).abs() < 1e-6);
    rig.keys("cmd-=");
    rig.keys("cmd-=");
    assert!((scale(&mut rig) - 1.25).abs() < 1e-6, "{}", scale(&mut rig));
    let zoom = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().settings().zoom.clone());
    assert_eq!(zoom.percent(&display), 125);
    assert_eq!(zoom.percent("another-display"), 100, "other displays keep their size");
    // Persisted per display.
    let snapshot = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot());
    let persisted = crate::model::PersistentState::project(&snapshot);
    assert_eq!(persisted.zoom.get(display.as_ref()), Some(&2));
    rig.keys("cmd--");
    assert!((scale(&mut rig) - 1.1).abs() < 1e-6);
    rig.keys("cmd-0");
    assert!((scale(&mut rig) - 1.0).abs() < 1e-6);
    // ⌘− no longer surfaces: the route stayed put.
    assert_eq!(rig.route(), page_route("RelationLabel"));
}

#[gpui::test]
fn a_replaced_page_leaves_instead_of_vanishing(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    rig.graph
        .root
        .update(rig.cx, |root, cx| root.queue(Intent::Navigate(page_route("KindGlyph")), cx));
    rig.cx.run_until_parked();
    rig.draw();
    let pages = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_pages(cx));
    assert_eq!(pages, 2, "the old page is still on screen, leaving");
    // A second change mid-flight: the page in between leaves too, nothing cuts.
    rig.cx.executor().advance_clock(Duration::from_millis(60));
    rig.graph
        .root
        .update(rig.cx, |root, cx| root.queue(Intent::SetView(View::Code), cx));
    rig.cx.run_until_parked();
    rig.draw();
    let pages = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_pages(cx));
    assert!(pages >= 2, "{pages} pages");
    rig.settle();
    let pages = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_pages(cx));
    assert_eq!(pages, 1, "once settled only the current page is drawn");
}
