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
    ReadFailure, Receiver, RecordSource, Relation, RelationKind, Rose, SearchPage, SearchRow, SearchContinuation,
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

/// A fresh index-backed open carries the actual declaration source line.
fn indexed_view_route(name: &str, view: View) -> Route {
    let Route::Symbol(mut route) = view_route(name, view) else { unreachable!() };
    route.line = Some(138);
    Route::Symbol(route)
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
    /// Keeps the window's handle alive for the test's length.
    pub _window: WindowHandle<Shell>,
    pub shell: Entity<Shell>,
    pub graph: UiEntityGraph,
    pub cx: &'static mut VisualTestContext,
}

/// Opens a real shell window at `route` (after an Orbit start, so the
/// thread has a bead behind), at `width`×`height`.
pub(crate) fn rig(cx: &mut TestAppContext, route: Option<Route>, width: f32, height: f32) -> Rig {
    rig_with_reads(cx, route, width, height, ReadPool::start(2, |_| Fixture).expect("pool"))
}

fn rig_with_reads(cx: &mut TestAppContext, route: Option<Route>, width: f32, height: f32, pool: ReadPool) -> Rig {
    cx.executor().allow_parking();
    cx.update(|cx| {
        gpui_component::init(cx);
        let _ = facet::fonts::install(cx);
        super::bodies::graph::install_test_fixture(cx);
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
        _window: window,
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
    pub(crate) fn draw(&mut self) {
        self.cx.update(|window, cx| window.draw(cx).clear(cx));
        self.cx.run_until_parked();
    }

    /// Redraws every view (cached ones included): the debug-bounds map is
    /// rebuilt only by views that actually paint.
    pub(crate) fn repaint(&mut self) {
        self.cx.update(|window, cx| {
            window.refresh();
            window.draw(cx).clear(cx);
        });
    }

    /// Lets reads land and motion finish, drawing as a platform would.
    pub(crate) fn settle(&mut self) {
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
            if frames == 0 && queued == 0 && running == 0
                && !self.graph.root.read_with(self.cx, |root, _| root.has_pending_work())
                && self.shell.read_with(self.cx, |shell, cx| shell.graph_ready(cx)) {
                self.draw();
                return;
            }
            assert!(Instant::now() < deadline, "the shell never settled");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Lets `ms` of virtual time pass and draws the frame the platform
    /// would draw then (motion mid-flight, reads not waited for).
    pub(crate) fn frame(&mut self, ms: u64) {
        self.cx.executor().advance_clock(Duration::from_millis(ms));
        self.cx.run_until_parked();
        self.cx.update(|window, cx| window.simulate_next_frame(cx));
        self.draw();
    }

    /// Queues an intent on the state owner and settles.
    pub(crate) fn go(&mut self, intent: Intent) {
        self.graph.root.update(self.cx, |root, cx| root.queue(intent, cx));
        self.settle();
    }

    pub(crate) fn counts(&mut self) -> super::RenderCounts {
        self.shell.read_with(self.cx, |shell, cx| shell.render_counts(cx))
    }

    pub(crate) fn said(&mut self) -> Vec<String> {
        self.shell
            .read_with(self.cx, |shell, cx| shell.reader_text(cx))
            .into_iter()
            .map(|text| text.to_string())
            .collect()
    }

    pub(crate) fn route(&mut self) -> Route {
        self.graph.store.read_with(self.cx, |store, _| store.snapshot().route().clone())
    }

    pub(crate) fn keys(&mut self, keys: &str) {
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

/// The travelling focus bevel is one continuous track per region, and a
/// bevel whose zone loses focus mid-flight still comes to rest. (The storm
/// saw `glow-h` jump 1.044 in 0 ms when two regions' bevels published one
/// key, and `glow-x` left live past its budget when Esc cleared focus while
/// it travelled.)
#[gpui::test]
fn the_focus_bevel_is_one_track_per_region_and_rests_when_focus_leaves(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    let _ = rig.cx.update(|_, cx| facet::probe::take(cx));
    // A hand on the keyboard: every key lands while the bevel still moves.
    for keys in ["j", "j", "j", "tab", "j", "shift-tab", "j", "escape"] {
        rig.cx.simulate_keystrokes(keys);
        rig.frame(16);
    }
    // Then frame by frame, as a display draws, for a second and a half.
    for _ in 0..90 {
        rig.frame(16);
    }
    let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
    let mut glows = std::collections::BTreeMap::<&str, Vec<&facet::probe::TrackSample>>::new();
    for track in ledger.tracks.iter().filter(|track| track.key.contains("glow-")) {
        glows.entry(track.key.as_str()).or_default().push(track);
    }
    assert!(glows.len() >= 8, "two regions' bevels each published x, y, w, h: {:?}", glows.keys());
    for (key, samples) in &glows {
        for pair in samples.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let dt = ((b.at_ms - a.at_ms) / 1000.0).max(0.0) as f32;
            let allowed = a.velocity.abs().max(b.velocity.abs()) * dt * 1.5 + 0.5;
            assert!(
                (b.value - a.value).abs() <= allowed,
                "{key} jumped {:.3} -> {:.3} in {:.0} ms (allowed {allowed:.3}): one track, two bevels",
                a.value,
                b.value,
                dt * 1000.0
            );
        }
        let last = samples.last().expect("a sample");
        assert!(!last.live, "{key} was left live at {} ms, {:.3} short of {:.3}", last.at_ms, last.target - last.value, last.target);
    }
}

/// The storm's shrunk seed 8 on `desktop-symbol`, act for act: Tab, a
/// step, then Esc while the bevel travels. Every bevel track ends at rest.
#[gpui::test]
fn storm_seed_8_leaves_no_bevel_mid_flight(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    let _ = rig.cx.update(|_, cx| facet::probe::take(cx));
    let frames = |rig: &mut Rig, ms: u64| {
        for _ in 0..ms / 16 {
            rig.frame(16);
        }
    };
    rig.cx.simulate_keystrokes("tab");
    frames(&mut rig, 500);
    rig.cx.simulate_keystrokes("down");
    frames(&mut rig, 110);
    for _ in 0..4 {
        rig.cx.simulate_keystrokes("escape");
        frames(&mut rig, 60);
    }
    frames(&mut rig, 1200);
    let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
    let mut last = std::collections::BTreeMap::<&str, &facet::probe::TrackSample>::new();
    for track in ledger.tracks.iter().filter(|track| track.key.contains("glow-")) {
        last.insert(track.key.as_str(), track);
    }
    assert!(!last.is_empty(), "the bevel moved");
    for (key, sample) in last {
        assert!(!sample.live, "{key} left live at {} ms, {:.3} short of {:.3}", sample.at_ms, sample.target - sample.value, sample.target);
    }
}

#[gpui::test]
fn the_hero_name_is_whole_at_every_width_and_text_size(cx: &mut TestAppContext) {
    // Read from the painted frame: the probe ledger holds every text box the
    // shell drew, its box width and the width its words need.
    let long = "RelationLabelWithAVeryLongCompoundIdentifierName";
    let mut rig = rig(cx, Some(page_route(long)), 1440.0, 900.0);
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
    let mut wrapped = false;
    let mut cut = false;
    for (width, percent) in MATRIX {
        rig.cx.simulate_resize(size(px(width), px(900.0)));
        rig.go(Intent::ZoomTo {
            display: display.clone(),
            percent,
        });
        rig.repaint();
        let ledger = rig.cx.update(|_, cx| facet::probe::take(cx));
        let lines = ledger
            .texts
            .iter()
            .filter(|text| text.key.contains("name:"))
            .collect::<Vec<_>>();
        let painted = lines.iter().map(|text| text.content.as_str()).collect::<String>();
        eprintln!(
            "{width:>6} px @ {percent:>3} %: {:?}",
            lines.iter().map(|text| text.content.as_str()).collect::<Vec<_>>()
        );
        assert_eq!(painted, long, "the painted name at {width} px, {percent} % is the full identifier");
        for line in &lines {
            assert!(
                !line.clipped_without_ellipsis(),
                "at {width} px, {percent} %: {:?} needs {} px in a {} px box",
                line.content,
                line.natural_width,
                line.bounds.width
            );
            assert!(!line.content.contains('…') && !line.content.ends_with('-'));
        }
        wrapped |= lines.len() > 1;
        // The status bar's address: its path may give way from the left,
        // the name never does.
        let address = ledger
            .texts
            .iter()
            .filter(|text| text.key.starts_with("address:"))
            .collect::<Vec<_>>();
        let said = address.iter().map(|text| text.content.as_str()).collect::<String>();
        eprintln!("{width:>6} px @ {percent:>3} %: address {:?}", address.iter().map(|text| text.content.as_str()).collect::<Vec<_>>());
        assert!(said.ends_with(long), "the address at {width} px, {percent} % ends in the whole name: {said:?}");
        for line in &address {
            assert!(
                !line.clipped_without_ellipsis() && line.overflow != facet::probe::TextOverflow::Ellipsis,
                "at {width} px, {percent} %: address line {:?} needs {} px in a {} px box",
                line.content,
                line.natural_width,
                line.bounds.width
            );
        }
        cut |= said.starts_with('…');
    }
    assert!(wrapped, "somewhere in the matrix the name had to wrap, and did");
    assert!(cut, "somewhere in the matrix the address path had to give way, and did");
}

#[gpui::test]
fn a_view_switch_replaces_the_entry_mounts_the_real_map_and_back_leaves(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    let back = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().back.len());
    let selector: &'static str = Box::leak(super::kit::shared_key(&symbol("RelationLabel")).into_boxed_str());
    rig.repaint();
    assert!(rig.cx.debug_bounds("shell-root").is_some(), "the root paints its debug selector");
    assert!(rig.cx.debug_bounds("reader-scroll").is_some(), "the reader paints its debug selector");
    assert!(rig.cx.debug_bounds(selector).is_some(), "the page's hero gem carries the shared id: {:?}", rig.said().first());
    rig.keys("cmd-.");
    assert_eq!(rig.route(), view_route("RelationLabel", View::Code));
    assert!(rig.said().iter().any(|line| line.contains("pub enum RelationLabel {")), "the code view shows the source");
    rig.keys("g");
    assert_eq!(rig.route(), view_route("RelationLabel", View::Graph));
    rig.repaint();
    assert!(rig.said().iter().any(|line| line.contains("Graph fixture")), "the graph is an explicitly marked fixture");
    let map_state = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_report(cx));
    assert!(map_state.contains("fixture 3 nodes"), "the mounted map reads the synthetic v1 input");
    assert!(map_state.contains("focus Some(0)") && map_state.contains("camera Some("),
        "retention starts from an actually focused and framed synthetic A: {map_state}");
    let after = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().back.len());
    assert_eq!(after, back, "three views, no history pushed");
    rig.keys("g");
    assert_eq!(rig.route(), page_route("RelationLabel"), "G again: back to the page");
    rig.keys("g");
    assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_report(cx)), map_state,
        "the same map entity, focus and camera survive a page visit");
    rig.keys("g");
    rig.keys("cmd-[");
    assert!(matches!(rig.route(), Route::Orbit(_)), "Back leaves the declaration: {:?}", rig.route());
    // G with nothing selected: the whole world.
    rig.keys("g");
    assert_eq!(rig.route(), Route::World);
    assert!(rig.said().iter().any(|line| line.contains("Graph fixture")));
    assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_report(cx)).contains("focus None"),
        "a new explicit world route releases the previous symbol and frames the world");
    rig.keys("g");
    assert!(matches!(rig.route(), Route::Orbit(_)), "G toggles an unfocused World back along its thread");
}

#[gpui::test]
fn graph_toggle_opens_current_b_instead_of_the_original_route_a(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(view_route("RelationLabel", View::Graph)), 1440.0, 900.0);
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(1, cx));
    rig.settle();
    rig.keys("g");
    assert_eq!(rig.route(), indexed_view_route("RelationDirection", View::Page), "G uses focused B's exact indexed declaration rather than route A");
    assert!(rig.said().iter().any(|line| line.as_str() == "RelationDirection"));
}

struct RetainedFailureFixture { fail: Arc<std::sync::atomic::AtomicBool> }
impl PageReader for RetainedFailureFixture {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        if matches!(request, ReadRequest::Search(query) if query.text.as_ref() == "RelationDirection")
            && self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(ReadFailure::Fault(crate::core::ErrorValue::new(crate::core::FaultCode::Transport, "new-root lookup failed")));
        }
        Fixture.read(request, context)
    }
}

#[gpui::test]
fn graph_failed_new_root_open_with_retained_old_results_settles_and_can_retry(cx: &mut TestAppContext) {
    let fail = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = fail.clone();
    let pool = ReadPool::start(1, move |_| RetainedFailureFixture { fail: flag.clone() }).expect("failure pool");
    let mut rig = rig_with_reads(cx, Some(view_route("RelationLabel", View::Graph)), 1440.0, 900.0, pool);
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(1, cx));
    rig.settle();
    rig.keys("g");
    assert_eq!(rig.route(), indexed_view_route("RelationDirection", View::Page), "R1 really indexed B successfully");
    let query = crate::model::pages::SearchQuery::new("RelationDirection", 200).expect("same guarded query");
    let old_root = rig.graph.store.read_with(rig.cx, |store, _| {
        assert!(store.search(&query).loaded_value().is_some());
        store.snapshot().key()
    });
    rig.go(Intent::Navigate(view_route("RelationLabel", View::Graph)));
    fail.store(true, std::sync::atomic::Ordering::SeqCst);
    rig.go(Intent::RefreshRoot { basis: old_root, request: crate::navigation::RequestId::from_authority(old_root, 500) });
    let new_root = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    assert_ne!(new_root, old_root, "the test really advances producer authority");
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(1, cx));
    rig.settle();
    rig.keys("g");
    assert_eq!(rig.route(), view_route("RelationLabel", View::Graph), "failed R2 cannot route retained R1 data");
    assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_ready(cx)), "stopped failure clears pending instead of hanging quiet");
    assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_report(cx)).contains("new-root lookup failed"));
    rig.graph.store.read_with(rig.cx, |store, _| {
        let retained = store.search(&query);
        assert!(retained.loaded_value().is_some(), "failure actually retains the old successful page");
        assert_eq!(retained.value_root(), Some(old_root));
        assert!(matches!(retained.terminal(), crate::core::ResourceTerminal::Fault(_)));
    });
    fail.store(false, std::sync::atomic::Ordering::SeqCst);
    rig.graph.store.update(rig.cx, |store, cx| store.retry(crate::model::pages::PageKey::Search(query), cx));
    rig.keys("g");
    assert_eq!(rig.route(), indexed_view_route("RelationDirection", View::Page), "a new successful retry resolves the current root");
}

/// The exact typed declaration is beyond the first broad name page.
struct TwoPageFixture { more: Arc<std::sync::atomic::AtomicUsize>, first_exact: bool }
impl PageReader for TwoPageFixture {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        let mut fixture = Fixture;
        let mut value = fixture.read(request, context)?;
        match request {
            ReadRequest::Search(query) if query.text.as_ref() == "RelationDirection" => {
                let PageValue::Search(page) = &mut value else { unreachable!() };
                let mut unrelated = page.rows[0].clone();
                if !self.first_exact {
                    unrelated.package = Some(Arc::from("/fixture/other"));
                    page.rows = (0..200).map(|rank| { let mut row = unrelated.clone(); row.rank = rank; row }).collect();
                }
                page.next = Some(SearchContinuation { cursor: backend_library::PageContinuation::from_cursor(backend_library::Cursor::new()), worker: 0 });
            }
            ReadRequest::SearchMore { query, .. } if query.text.as_ref() == "RelationDirection" => {
                self.more.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let PageValue::Search(page) = value else { unreachable!() };
                value = PageValue::SearchMore(page);
            }
            _ => {}
        }
        Ok(value)
    }
}

#[gpui::test]
fn graph_open_resolves_exact_identity_after_a_broad_search_continuation(cx: &mut TestAppContext) {
    let more = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = more.clone();
    let pool = ReadPool::start(1, move |_| TwoPageFixture { more: count.clone(), first_exact: false }).expect("two-page pool");
    let mut rig = rig_with_reads(cx, Some(view_route("RelationLabel", View::Graph)), 1440.0, 900.0, pool);
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(1, cx));
    rig.settle();
    rig.keys("g");
    assert_eq!(more.load(std::sync::atomic::Ordering::SeqCst), 1, "the route lookup consumes the real continuation");
    assert_eq!(rig.route(), indexed_view_route("RelationDirection", View::Page), "unrelated first-page name rows cannot hide exact typed B");
}

#[gpui::test]
fn graph_open_rejects_a_duplicate_exact_identity_on_a_later_search_page(cx: &mut TestAppContext) {
    let more = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = more.clone();
    let pool = ReadPool::start(1, move |_| TwoPageFixture { more: count.clone(), first_exact: true }).expect("duplicate-page pool");
    let mut rig = rig_with_reads(cx, Some(view_route("RelationLabel", View::Graph)), 1440.0, 900.0, pool);
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(1, cx));
    rig.settle();
    rig.keys("g");
    assert_eq!(more.load(std::sync::atomic::Ordering::SeqCst), 1, "a partial page cannot prove uniqueness");
    assert_eq!(rig.route(), view_route("RelationLabel", View::Graph));
    assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_report(cx)).contains("several exact matches"));
    assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_ready(cx)), "ambiguity settles the native pending state");
}

#[gpui::test]
fn graph_titlebar_page_and_code_open_visible_b_instead_of_route_a(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(view_route("RelationLabel", View::Graph)), 1440.0, 900.0);
    for (id, view) in [("view-page", View::Page), ("view-code", View::Code)] {
        rig.go(Intent::Navigate(view_route("RelationLabel", View::Graph)));
        rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(1, cx));
        rig.settle();
        let bounds = rig.shell.read_with(rig.cx, |shell, cx| shell.titlebar_target_bounds(id, cx)).expect("mounted view button");
        rig.cx.simulate_click(bounds.center(), Modifiers::default());
        rig.settle();
        let Route::Symbol(route) = rig.route() else { panic!("must open a declaration") };
        assert_eq!(route.id.as_str(), coordinate("RelationDirection"));
        assert_eq!(route.view, view);
    }
}

#[gpui::test]
fn graph_titlebar_unindexed_b_and_unfocused_world_stay_honest(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(view_route("RelationLabel", View::Graph)), 1440.0, 900.0);
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(2, cx));
    rig.settle();
    for id in ["view-page", "view-code"] {
        let bounds = rig.shell.read_with(rig.cx, |shell, cx| shell.titlebar_target_bounds(id, cx)).expect("mounted view button");
        rig.cx.simulate_click(bounds.center(), Modifiers::default());
        rig.settle();
        assert_eq!(rig.route(), view_route("RelationLabel", View::Graph));
        assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_report(cx)).contains("no exact match"));
    }
    rig.go(Intent::Navigate(Route::World));
    let bounds = rig.shell.read_with(rig.cx, |shell, cx| shell.titlebar_target_bounds("view-page", cx)).expect("world page button");
    rig.cx.simulate_click(bounds.center(), Modifiers::default());
    rig.settle();
    assert_eq!(rig.route(), Route::World);
    assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_report(cx)).contains("Choose a graph symbol"));
}

#[gpui::test]
fn direct_graph_view_intents_resolve_the_native_current_selection(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(view_route("RelationLabel", View::Graph)), 1440.0, 900.0);
    for view in [View::Page, View::Code] {
        rig.go(Intent::Navigate(view_route("RelationLabel", View::Graph)));
        rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(1, cx));
        rig.settle();
        rig.go(Intent::SetView(view));
        let Route::Symbol(route) = rig.route() else { panic!("native view intent must resolve B") };
        assert_eq!(route.id.as_str(), coordinate("RelationDirection"));
        assert_eq!(route.view, view);
    }
}

#[gpui::test]
fn native_graph_handoff_uses_the_scaled_translated_canvas_and_rejects_absent_sources(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(view_route("RelationLabel", View::Graph)), 1440.0, 900.0);
    rig.cx.update(|_, cx| cx.set_global(super::bodies::graph::TestCanvasLayer { scale: 0.75, x: 35.0, y: -28.0 }));
    rig.repaint();
    let graph = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("actual mounted graph");
    assert!(!rig.shell.read_with(rig.cx, |shell, cx| shell.graph_gem_morphing(cx)), "this proves the resting canvas path rather than the interrupted-ghost fallback");
    let raw = graph.read_with(rig.cx, |graph, _| graph.node_bounds(0)).expect("visible core");
    let (anchor, painted, parent) = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_canvas_geometry(0, cx));
    assert_eq!(parent.scale.width, 0.75, "actual GPUI parent layer was prepainted");
    assert_ne!(painted, Some(raw), "native scale/translation changes the real source rectangle");
    assert_eq!(painted, Some(parent.apply_bounds(raw)), "the shared ledger records actual composited window bounds");
    assert_eq!(anchor, painted, "the routed hero starts at actual painted source geometry");
    rig.cx.update(|_, cx| cx.set_global(super::bodies::graph::TestCanvasLayer { scale: 0.0, x: 35.0, y: -28.0 }));
    rig.repaint();
    assert!(graph.read_with(rig.cx, |graph, _| graph.node_bounds(0)).is_some(), "the logical node exists while its native layer is collapsed");
    let (anchor, painted, _) = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_canvas_geometry(0, cx));
    assert!(anchor.is_none() && painted.is_none(), "a collapsed painted source cannot revive the previous nonempty seed");
    rig.cx.update(|_, cx| cx.set_global(super::bodies::graph::TestCanvasLayer { scale: 0.75, x: 35.0, y: -28.0 }));
    rig.repaint();
    let camera = graph.read_with(rig.cx, |graph, _| graph.camera().expect("camera"));
    graph.update(rig.cx, |graph, cx| graph.fly_to(facet::motion::Camera { x: camera.x + 1_000_000.0, ..camera }, cx));
    rig.settle();
    assert!(graph.read_with(rig.cx, |graph, _| graph.node_bounds(0)).is_none(), "the source really left the current viewport");
    let (anchor, painted, _) = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_canvas_geometry(0, cx));
    assert!(anchor.is_none() && painted.is_none(), "a disappeared source never reuses the previous transformed endpoint");
}

struct MissingSymbolFixture { fail: Arc<std::sync::atomic::AtomicBool> }
impl PageReader for MissingSymbolFixture {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        if matches!(request, ReadRequest::Symbol(id) if id == &symbol("RelationLabel")) && self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(ReadFailure::Fault(crate::core::ErrorValue::new(crate::core::FaultCode::Transport, "current declaration disappeared")));
        }
        Fixture.read(request, context)
    }
}

#[gpui::test]
fn new_root_without_an_indexed_join_clears_the_previous_painted_graph_ghost(cx: &mut TestAppContext) {
    let fail = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = fail.clone();
    let pool = ReadPool::start(1, move |_| MissingSymbolFixture { fail: flag.clone() }).expect("read pool");
    let mut rig = rig_with_reads(cx, Some(view_route("RelationLabel", View::Graph)), 1440.0, 900.0, pool);
    assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_canvas_geometry(0, cx).1).is_some(), "there really is a previous indexed paint");
    let old_root = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    fail.store(true, std::sync::atomic::Ordering::SeqCst);
    rig.go(Intent::RefreshRoot { basis: old_root, request: crate::navigation::RequestId::from_authority(old_root, 501) });
    rig.graph.store.read_with(rig.cx, |store, _| {
        assert_ne!(store.snapshot().key(), old_root);
        let resource = store.symbol(&symbol("RelationLabel"));
        assert!(resource.loaded_value().is_some(), "new-root failure actually retains its prior page");
        assert_eq!(resource.value_root(), Some(old_root));
        assert!(matches!(resource.terminal(), crate::core::ResourceTerminal::Fault(_)));
    });
    assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_canvas_geometry(0, cx).1).is_none(), "retained stale page cannot keep its old painted morph endpoint alive");
}

#[gpui::test]
fn graph_focus_display_tracks_b_without_rewriting_history_or_guessing_unindexed_rows(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(view_route("RelationLabel", View::Graph)), 1440.0, 900.0);
    let back = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().session().back.len());
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(1, cx));
    rig.settle();
    rig.graph.store.read_with(rig.cx, |store, cx| {
        let snapshot = store.snapshot();
        let focus = store.graph_focus().expect("current typed B selection");
        assert_eq!(focus.node, 1);
        assert_eq!(focus.indexed, Some((package(), symbol("RelationDirection"))), "only the exact typed complete-outline match highlights B");
        let here = super::thread::here(&snapshot, store);
        assert_eq!(here.name.to_string(), "RelationDirection");
        assert!(here.path.contains("graph fixture"));
        let (lines, _) = super::status::display_lines(&snapshot, Some(focus), px(1440.0), cx);
        assert_eq!(lines, ["Graph fixture · synthetic-present-v1::glyph::RelationDirection"]);
        assert_eq!(snapshot.session().back.len(), back);
        assert_eq!(snapshot.route(), &view_route("RelationLabel", View::Graph), "selection is not navigation");
        assert!(super::thread::address_parts(&snapshot).full().contains("RelationLabel/graph"), "the copyable address remains the real graph visit");
    });
    assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.shelf_current_symbols(cx)), [symbol("RelationDirection")]);
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(2, cx));
    rig.settle();
    rig.graph.store.read_with(rig.cx, |store, _| {
        let focus = store.graph_focus().expect("unindexed selection still has fixture semantics");
        assert_eq!(focus.name.as_ref(), "Unindexed");
        assert!(focus.indexed.is_none(), "a same-name or previous indexed row cannot become its identity");
    });
    assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.shelf_current_symbols(cx)).is_empty(), "unknown B does not leave old A selected");
    rig.go(Intent::Navigate(page_route("RelationLabel")));
    rig.graph.store.read_with(rig.cx, |store, _| {
        assert!(store.graph_focus().is_none(), "the hidden graph cannot rename the page capsule");
        assert_eq!(super::thread::here(&store.snapshot(), store).name.to_string(), "RelationLabel");
    });
}

#[gpui::test]
fn graph_camera_flights_do_not_publish_semantic_selection_events(cx: &mut TestAppContext) {
    struct SelectionEvents(usize);
    let mut rig = rig(cx, Some(view_route("RelationLabel", View::Graph)), 1440.0, 900.0);
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(1, cx));
    rig.settle();
    let store = rig.graph.store.clone();
    let events = rig.cx.update(|_, cx| cx.new(|cx| {
        cx.subscribe(&store, |events: &mut SelectionEvents, _, event: &crate::runtime::store::StoreEvent, _| {
            if event.is_branch(crate::runtime::store::Branch::GraphFocus) { events.0 += 1; }
        }).detach();
        SelectionEvents(0)
    }));
    let graph = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("real mounted graph");
    let before = graph.read_with(rig.cx, |graph, _| graph.camera().expect("laid out camera"));
    graph.update(rig.cx, |graph, cx| graph.fly_to(facet::motion::Camera { x: before.x + 200.0, y: before.y + 70.0, w: before.w * 1.2 }, cx));
    rig.frame(16);
    assert_eq!(events.read_with(rig.cx, |events, _| events.0), 0);
    rig.settle();
    assert_ne!(graph.read_with(rig.cx, |graph, _| graph.camera().expect("camera after actual flight")), before);
    assert_eq!(events.read_with(rig.cx, |events, _| events.0), 0, "every real camera frame stays below the semantic notification boundary");
}

#[gpui::test]
fn immediate_page_to_graph_acquires_its_actual_visible_hero(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(view_route("RelationLabel", View::Graph)), 1440.0, 900.0);
    rig.go(Intent::Navigate(page_route("RelationLabel")));
    rig.repaint();
    rig.graph.root.update(rig.cx, |root, cx| root.queue(Intent::SetView(View::Graph), cx));
    rig.frame(16);
    assert_eq!(rig.route(), view_route("RelationLabel", View::Graph));
    assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_gem_morphing(cx)), "the immediately visible Page A supplies the native canvas endpoint");
    rig.settle();
    assert!(!rig.shell.read_with(rig.cx, |shell, cx| shell.graph_gem_morphing(cx)));
}

#[gpui::test]
fn retained_pinned_card_actions_reveal_or_open_the_cards_exact_node(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(view_route("RelationLabel", View::Graph)), 1440.0, 900.0);
    let graph = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_entity(cx)).expect("mounted retained graph");
    rig.go(Intent::SetView(View::Page));
    rig.cx.update(|window, cx| graph.update(cx, |graph, cx| graph.act_on_peek(1, facet::graph::peek::Action::Focus, window, cx)));
    rig.settle();
    assert_eq!(rig.route(), view_route("RelationLabel", View::Graph), "Focus reveals the original retained graph visit");
    assert_eq!(graph.read_with(rig.cx, |graph, _| graph.focused()), Some(1), "the pinned card's B overrides its graph route A");
    rig.go(Intent::Navigate(page_route("RelationLabel")));
    rig.cx.update(|window, cx| graph.update(cx, |graph, cx| graph.act_on_peek(1, facet::graph::peek::Action::Open, window, cx)));
    rig.settle();
    assert_eq!(rig.route(), indexed_view_route("RelationDirection", View::Page), "Open from a retained card resolves B while its graph is hidden");
    rig.cx.update(|window, cx| graph.update(cx, |graph, cx| graph.act_on_peek(2, facet::graph::peek::Action::Open, window, cx)));
    rig.settle();
    assert_eq!(rig.route(), indexed_view_route("RelationDirection", View::Page), "an unavailable pinned symbol does not route arbitrary A or B");
    assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_report(cx)).contains("no exact match"));
    rig.graph.store.read_with(rig.cx, |store, cx| {
        let notice = store.graph_notice().expect("hidden pinned failure has visible current-page feedback");
        let (lines, _) = super::status::feedback_lines(&store.snapshot(), None, Some(notice), px(1440.0), cx);
        assert!(lines.join("").contains("no exact match"));
    });
    rig.go(Intent::Navigate(page_route("RelationLabel")));
    assert!(rig.graph.store.read_with(rig.cx, |store, _| store.graph_notice().is_none()), "the previous page's failed card intent does not follow another route");
}

#[gpui::test]
fn code_to_graph_never_reuses_a_recent_hidden_page_hero(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(view_route("RelationLabel", View::Code)), 1440.0, 900.0);
    // A still-recent measurement from a page that is no longer visible.
    // This is deliberately inside the endpoint TTL, so expiry cannot mask
    // a mistaken acquisition from the wrong immediately preceding view.
    let key = super::kit::shared_id(&symbol("RelationLabel"));
    rig.cx.update(|window, cx| facet::motion::shared::remember(key,
        gpui::Bounds::new(point(px(900.0), px(700.0)), size(px(64.0), px(64.0))), window, cx));
    rig.graph.root.update(rig.cx, |root, cx| root.queue(Intent::SetView(View::Graph), cx));
    let deadline = Instant::now() + Duration::from_secs(3);
    rig.frame(16);
    while !rig.shell.read_with(rig.cx, |shell, cx| shell.graph_ready(cx)) {
        rig.frame(16);
        assert!(Instant::now() < deadline, "the tiny synthetic map must mount");
        std::thread::sleep(Duration::from_millis(1));
    }
    rig.frame(16);
    assert_eq!(rig.route(), view_route("RelationLabel", View::Graph));
    assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_report(cx)).contains("focus Some(0)"), "the graph actually focuses synthetic A before testing its painted source");
    assert!(!rig.shell.read_with(rig.cx, |shell, cx| shell.graph_gem_morphing(cx)),
        "the canvas never morphs from a hidden page's old measurement after Code");
}

#[gpui::test]
fn graph_view_intents_survive_settings_but_reject_competing_content_bursts(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(view_route("RelationLabel", View::Graph)), 1440.0, 900.0);
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(1, cx));
    rig.settle();
    let display = rig.shell.read_with(rig.cx, |shell, _| shell.display_key());
    rig.graph.root.update(rig.cx, |root, cx| {
        root.queue(Intent::SetView(View::Page), cx);
        root.queue(Intent::ZoomTo { display, percent: 125 }, cx);
    });
    rig.settle();
    assert_eq!(rig.route(), indexed_view_route("RelationDirection", View::Page), "unrelated settings do not cancel the user's current symbol open");
    rig.go(Intent::Navigate(view_route("RelationLabel", View::Graph)));
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(1, cx));
    rig.settle();
    rig.graph.root.update(rig.cx, |root, cx| {
        root.queue(Intent::SetView(View::Page), cx);
        root.queue(Intent::SetView(View::Code), cx);
    });
    rig.settle();
    assert_eq!(rig.route(), indexed_view_route("RelationDirection", View::Code), "only the latest competing graph destination can open");
    rig.go(Intent::Navigate(view_route("RelationLabel", View::Graph)));
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(1, cx));
    rig.settle();
    rig.graph.root.update(rig.cx, |root, cx| {
        root.queue(Intent::SetView(View::Page), cx);
        root.queue(Intent::Navigate(page_route("RelationLabel")), cx);
        root.queue(Intent::Navigate(view_route("RelationLabel", View::Graph)), cx);
    });
    rig.settle();
    assert_eq!(rig.route(), view_route("RelationLabel", View::Graph), "returning to the same route cannot revive an earlier open intent");
}

#[gpui::test]
fn every_graph_keyboard_page_or_code_command_uses_current_b(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(view_route("RelationLabel", View::Graph)), 1440.0, 900.0);
    for (key, view) in [("g", View::Page), ("cmd-3", View::Page), ("cmd-4", View::Code), ("cmd-.", View::Code), ("s", View::Code)] {
        rig.go(Intent::Navigate(view_route("RelationLabel", View::Graph)));
        rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(1, cx));
        rig.settle();
        rig.keys(key);
        let Route::Symbol(route) = rig.route() else { panic!("{key} must open focused B") };
        assert_eq!(route.id.as_str(), coordinate("RelationDirection"), "{key} uses visible focus");
        assert_eq!(route.view, view, "{key} keeps its requested destination");
    }
}

#[gpui::test]
fn graph_toggle_reports_an_unavailable_current_b_without_opening_route_a(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(view_route("RelationLabel", View::Graph)), 1440.0, 900.0);
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(2, cx));
    rig.settle();
    rig.keys("g");
    assert_eq!(rig.route(), view_route("RelationLabel", View::Graph), "an unavailable B cannot silently open A");
    assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_report(cx)).contains("no exact match"),
        "the actual focused node's indexed lookup reports its honest failure");
}

#[gpui::test]
fn back_from_an_opened_page_restores_the_same_world_camera_and_focus(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(Route::World), 1440.0, 900.0);
    rig.shell.update(rig.cx, |shell, cx| shell.focus_graph_node(1, cx));
    rig.settle();
    let before = rig.shell.read_with(rig.cx, |shell, cx| shell.graph_report(cx));
    rig.keys("g");
    assert_eq!(rig.route(), indexed_view_route("RelationDirection", View::Page));
    rig.keys("cmd-[");
    assert_eq!(rig.route(), Route::World);
    assert_eq!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_report(cx)), before,
        "the original World visit's map entity, focus and camera survive Back from its page");
    rig.keys("cmd-[");
    assert!(matches!(rig.route(), Route::Orbit(_)));
    rig.keys("g");
    assert_eq!(rig.route(), Route::World);
    assert!(rig.shell.read_with(rig.cx, |shell, cx| shell.graph_report(cx)).contains("focus None"),
        "a new explicit World command clears focus even though the retained route value is identical");
}

#[gpui::test]
fn leaving_graph_with_find_open_restores_the_page_keyboard_owner(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(view_route("RelationLabel", View::Graph)), 1440.0, 900.0);
    rig.cx.simulate_keystrokes("/");
    rig.frame(16);
    let shell = rig.shell.clone();
    let find_owner = rig.cx.update(|window, cx| {
        assert_eq!(shell.read(cx).graph_find_state(window, cx), (true, true), "find actually opened and owns native focus");
        window.focused(cx).expect("find focus owner")
    });
    rig.go(Intent::SetView(View::Page));
    let page_owner = rig.cx.update(|window, cx| {
        assert_eq!(shell.read(cx).graph_find_state(window, cx), (false, false), "suspend closes the retained find field");
        window.focused(cx).expect("page focus owner")
    });
    assert_ne!(find_owner, page_owner, "the hidden graph input releases the native keyboard owner");
    let before = rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
    rig.keys("j");
    let after = rig.shell.read_with(rig.cx, |shell, cx| shell.focus_state(cx));
    assert!(after.1.is_some() && after != before, "J changes the visible page target after find suspension");
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
    assert_eq!(here, "viewing 0.3.0 · yours is the working copy", "a workspace crate is read from its working copy");
    // The index holds only the working copy of a workspace crate: the page
    // says so once, and never calls a real declaration's address malformed.
    let said = rig.said();
    assert!(
        said.iter().any(|line| line == "Release 0.3.0 is not in this index; only your working copy is. Esc returns to it."),
        "{said:#?}"
    );
    assert!(!said.iter().any(|line| line.contains("not a declaration")), "{said:#?}");
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
