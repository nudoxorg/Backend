//! The symbol page's anatomy through the real shell. A small world is
//! installed as the fixture; its declarations sit where the page fixture's
//! do (`glyph.rs:138`), so the join is the product's own
//! (`IdentityAdapter`, file + line + name), and every assertion reads what
//! was painted (the probe ledger), never the model it was built from.

use super::tests::{PACKAGE, Fixture, Rig, dossier, page_route, rig, rig_with_reads};
use crate::model::pages::{DeclRef, Known, OutlineNode, OutlineTree, PackageRef, PageValue, ReadFailure};
use crate::navigation::Intent;
use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use crate::shell::bodies::graph::identity::IdentityAdapter;
use backend_library::DeclarationKind;
use facet::graph::{Edge, Kind, Module, Node, Package, Rel, World};
use facet::probe::Ledger;
use gpui::{Modifiers, TestAppContext, point, px};
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// The caller's file: `print_labels` (lines 3–6) uses `RelationLabel`.
const GLYPH: &str = "//! Glyphs.\n\npub fn print_labels(out: &mut String) {\n    let label = RelationLabel::Related;\n    out.push_str(label.as_str());\n}\n";

/// `RelationLabel` (an enum of `Typed(SemanticLinkKind)` and `Related`,
/// so made of `SemanticLinkKind`), `relation_label(link: &Link) ->
/// RelationLabel`, and a caller of the enum.
pub(super) fn world() -> Arc<World> {
    let node = |kind, name: &str, line, parent: Option<u32>| {
        let mut node = Node::new(kind, name, 0, 0);
        node.line = line;
        node.parent = parent;
        node.vis = Some("pub".into());
        node
    };
    let label = node(Kind::Enum, "RelationLabel", 138, None);
    let mut typed = node(Kind::Variant, "Typed", 139, Some(0));
    typed.ty = Some("SemanticLinkKind".into());
    let related = node(Kind::Variant, "Related", 140, Some(0));
    let mut function = node(Kind::Function, "relation_label", 138, None);
    function.params = vec!["link: &Link".into()];
    function.ret = Some("RelationLabel".into());
    let mut caller = node(Kind::Function, "print_labels", 3, None);
    caller.end = Some(6);
    caller.file = Some("glyph.rs".into());
    // At the page fixture's line, so a held SemanticLinkKind joins too.
    let payload = node(Kind::Enum, "SemanticLinkKind", 138, None);
    // `RelationLabel::kind` reads its SemanticLinkKind (the hand's road
    // when RelationLabel itself is let go).
    let mut kind = node(Kind::Method, "kind", 142, Some(0));
    kind.recv = Some("&self".into());
    kind.ret = Some("SemanticLinkKind".into());
    kind.sig = Some("pub fn kind(&self) -> SemanticLinkKind".into());
    let error = node(Kind::Struct, "LabelError", 60, None);
    // Another of your packages uses it: that is what makes a door.
    let mut main = Node::new(Kind::Function, "main", 1, 1);
    main.line = 1;
    main.end = Some(4);
    main.file = Some("app.rs".into());
    Arc::new(
        World::new(
            vec![
                Package {
                    name: "present".into(),
                    version: "0.4.2".into(),
                    yours: true,
                    external: false,
                    deps: vec![],
                },
                Package {
                    name: "app".into(),
                    version: "0.1.0".into(),
                    yours: true,
                    external: false,
                    deps: vec![0],
                },
            ],
            vec![
                Module {
                    pkg: 0,
                    path: "glyph".into(),
                    file: "glyph.rs".into(),
                },
                Module {
                    pkg: 1,
                    path: String::new().into(),
                    file: "app.rs".into(),
                },
            ],
            vec![label, typed, related, function, caller, payload, error, main, kind],
            vec![
                Edge {
                    from: 4,
                    to: 0,
                    rel: Rel::USES,
                },
                // Its payload's type: the one-of rows say it.
                Edge {
                    from: 0,
                    to: 5,
                    rel: Rel::HAS,
                },
                // `relation_label` hands you one: the tour's door.
                Edge {
                    from: 3,
                    to: 0,
                    rel: Rel::GIVES,
                },
                Edge {
                    from: 7,
                    to: 3,
                    rel: Rel::CALLS,
                },
                Edge {
                    from: 7,
                    to: 0,
                    rel: Rel::USES,
                },
            ],
        )
        .expect("world"),
    )
}

pub(super) fn install(rig: &mut Rig) {
    let world = world();
    let identities = Arc::new(IdentityAdapter::synthetic(&world, PackageRef::parse(PACKAGE).expect("package")));
    let files = HashMap::from([("glyph.rs".to_owned(), Arc::<str>::from(GLYPH))]);
    rig.cx.update(|_, cx| {
        crate::runtime::fixture_world::install(world, identities, files, cx);
        facet::probe::enable(cx);
    });
    rig.repaint();
}

pub(super) fn painted(rig: &mut Rig) -> Ledger {
    rig.repaint();
    rig.cx.update(|_, cx| facet::probe::take(cx))
}

/// A worker barrier makes the package outline arrive only after the mounted
/// shell has navigated away. Releasing on unwind also keeps the pool finite.
#[derive(Default)]
struct PackageGate {
    state: Mutex<(bool, bool)>, // entered, released
    wake: Condvar,
}

impl PackageGate {
    fn hold(&self) {
        let mut state = self.state.lock().expect("package gate");
        state.0 = true;
        self.wake.notify_all();
        while !state.1 {
            let (next, timed) = self.wake.wait_timeout(state, Duration::from_secs(10)).expect("package barrier");
            state = next;
            assert!(!timed.timed_out() || state.1, "the test never released its package read");
        }
    }

    fn entered(&self) -> bool { self.state.lock().expect("package gate").0 }
    fn release(&self) {
        self.state.lock().expect("package gate").1 = true;
        self.wake.notify_all();
    }
}

struct ReleasePackage(Arc<PackageGate>);
impl Drop for ReleasePackage {
    fn drop(&mut self) { self.0.release(); }
}

struct CrossPackageFixture { gate: Arc<PackageGate> }
impl PageReader for CrossPackageFixture {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        if let ReadRequest::Package(package) = request
            && package.as_str() == "/fixture/app"
        {
            self.gate.hold();
            let mut page = dossier();
            page.package = package.clone();
            let decl = DeclRef::from_label("/fixture/app::app.rs:1::main", None, Some(DeclarationKind::Function), Some(("app.rs", 1))).expect("exact main");
            page.outline = Known::Known(OutlineTree {
                roots: Arc::from([OutlineNode { decl, children: Arc::from([]) }]),
                complete: true,
            });
            return Ok(PageValue::Package(page));
        }
        Fixture.read(request, context)
    }
}

struct MirroredRelations;
impl PageReader for MirroredRelations {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        let mut value = Fixture.read(request, context)?;
        if let PageValue::Symbol(page) = &mut value
            && page.identity.name.as_ref() == "KindGlyph"
        {
            page.rose.up = page.rose.down.clone();
        }
        Ok(value)
    }
}

#[gpui::test]
fn the_same_recorded_relation_in_two_directions_has_distinct_targets(cx: &mut TestAppContext) {
    let pool = ReadPool::start(2, |_| MirroredRelations).expect("mirrored relation pool");
    let mut rig = rig_with_reads(cx, Some(page_route("KindGlyph")), 1440.0, 900.0, pool);
    rig.cx.update(|_, cx| facet::probe::enable(cx));
    let ledger = painted(&mut rig);
    let target = super::tests::coordinate("Typed");
    let is = format!("rel-is-0-{target}");
    let made_of = format!("rel-made of-0-{target}");
    assert!(ledger.targets.iter().any(|entry| entry.key == is), "the 'is' occurrence is independently actionable");
    assert!(ledger.targets.iter().any(|entry| entry.key == made_of), "the 'made of' occurrence is independently actionable");
    assert_ne!(is, made_of);
}

#[gpui::test]
fn late_cross_package_outline_cannot_open_after_back_away_but_a_fresh_click_can(cx: &mut TestAppContext) {
    let gate = Arc::new(PackageGate::default());
    let worker_gate = gate.clone();
    let pool = ReadPool::start(2, move |_| CrossPackageFixture { gate: worker_gate.clone() }).expect("package pool");
    let mut rig = rig_with_reads(cx, Some(page_route("RelationLabel")), 1440.0, 900.0, pool);
    let _release_on_unwind = ReleasePackage(gate.clone());
    let world = world();
    let identities = Arc::new(IdentityAdapter::synthetic_catalog(&world, vec![
        PackageRef::parse(PACKAGE).expect("present"),
        PackageRef::parse("/fixture/app").expect("app"),
    ]));
    rig.cx.update(|_, cx| {
        crate::runtime::fixture_world::install(world, identities, HashMap::new(), cx);
        facet::probe::enable(cx);
    });
    rig.repaint();

    let open = || facet::anatomy::Open { target: facet::semantics::Target::Node(7) };
    rig.cx.update(|window, cx| window.dispatch_action(Box::new(open()), cx));
    let deadline = Instant::now() + Duration::from_secs(3);
    while !gate.entered() {
        rig.frame(16);
        assert!(Instant::now() < deadline, "cross-package outline never entered its worker");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(rig.route(), page_route("RelationLabel"), "no guessed route while the exact outline is cold");

    let away = page_route("SemanticLinkKind");
    rig.graph.root.update(rig.cx, |root, cx| root.queue(Intent::Navigate(away.clone()), cx));
    let deadline = Instant::now() + Duration::from_secs(3);
    while rig.route() != away {
        rig.frame(16);
        assert!(Instant::now() < deadline, "the later navigation did not land");
    }
    gate.release();
    rig.settle();
    assert_eq!(rig.route(), away, "a late exact result cannot replace the newer page");
    assert!(!painted(&mut rig).texts.iter().any(|text| text.content.contains("Opening main")), "cancel clears the pending status");

    rig.go(Intent::Navigate(page_route("RelationLabel")));
    rig.cx.update(|window, cx| window.dispatch_action(Box::new(open()), cx));
    rig.settle();
    let exact = super::kit::symbol_view_route("/fixture/app", &crate::model::pages::SymbolRef::new("/fixture/app::app.rs:1::main").expect("main"), crate::navigation::View::Page, Some(1)).expect("route");
    assert_eq!(rig.route(), exact, "a fresh click can open only the complete cross-package outline match");
}

/// S2: a link whose package is not even admitted (the fixture's `app` here)
/// cannot be resolved to any page. The link does not move it; the Notice
/// says so instead of silently doing nothing (§15 ruling 1), and it clears
/// the moment the page navigates away.
#[gpui::test]
fn an_anatomy_link_the_index_lacks_speaks_through_the_notice_instead_of_moving(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    install(&mut rig); // single-package identity: `app`/`main` (node 7) is not admitted
    let open = || facet::anatomy::Open { target: facet::semantics::Target::Node(7) };
    rig.cx.update(|window, cx| window.dispatch_action(Box::new(open()), cx));
    rig.settle();
    assert_eq!(rig.route(), page_route("RelationLabel"), "an unresolvable link does not move the page");
    let message = rig.graph.store.read_with(rig.cx, |store, _| store.notice().map(|notice| notice.message.to_string()));
    assert_eq!(message.as_deref(), Some("main isn't in the index"));
    rig.go(Intent::Navigate(page_route("SemanticLinkKind")));
    assert!(rig.graph.store.read_with(rig.cx, |store, _| store.notice().is_none()), "a notice does not survive navigation");
}

/// The painted texts published under exactly `key`, in paint order.
fn at(ledger: &Ledger, key: &str) -> Vec<String> {
    ledger.texts.iter().filter(|text| text.key == key).map(|text| text.content.clone()).collect()
}

/// The fork's branch names, in paint order (`anatomy-shape-{n}-name`).
fn branch_names(ledger: &Ledger) -> Vec<String> {
    ledger
        .texts
        .iter()
        .filter(|text| text.key.starts_with("anatomy-shape-") && text.key.ends_with("-name") && !text.key.contains("-in-"))
        .map(|text| text.content.clone())
        .collect()
}

#[gpui::test]
fn an_enum_page_draws_one_of_with_each_variant_in_place_of_its_code(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    install(&mut rig);
    let ledger = painted(&mut rig);
    assert_eq!(at(&ledger, "anatomy-shape-heading"), ["one of"]);
    assert_eq!(branch_names(&ledger), ["Typed", "Related"], "one row per variant, in order");
    assert_eq!(at(&ledger, "anatomy-shape-0-ty"), ["SemanticLinkKind"], "Typed carries its payload");
    let said = rig.said();
    assert!(
        !said.iter().any(|line| line.starts_with("pub enum RelationLabel {")),
        "the anatomy replaces the declaration's code: {said:#?}"
    );
    assert!(
        !ledger.texts.iter().any(|text| text.key.contains("prism")),
        "the prism stays in the graph"
    );
    // Said once: the one-of rows already name what it is made of.
    assert!(!said.iter().any(|line| line == "made of"), "no made-of row under the fork: {said:#?}");
}

#[gpui::test]
fn relations_the_anatomy_does_not_say_stay_in_a_plain_list(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    install(&mut rig);
    let _ = painted(&mut rig);
    let said = rig.said();
    let used = said.iter().position(|line| line == "used by").unwrap_or_else(|| panic!("the used-by group is not covered by the anatomy: {said:#?}"));
    let caller = said.iter().position(|line| line == "print_labels").expect("pinned caller in the anatomy");
    assert!(caller < used, "the anatomy owns the featured caller: {said:#?}");
    assert_eq!(said.get(used + 1).map(String::as_str), Some("1 example from a real caller"), "the plain list stays summarized: {said:#?}");
}

#[gpui::test]
fn getting_one_shows_its_producer_without_repeating_the_maker_count(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    install(&mut rig);
    let ledger = painted(&mut rig);
    let said = rig.said();

    assert!(said.iter().any(|line| line == "comes from"), "Getting one exposes its producer row: {said:#?}");
    assert!(said.iter().any(|line| line == "relation_label"), "the exact pinned producer remains visible: {said:#?}");
    assert!(
        !ledger.texts.iter().any(|sample| sample.content.contains("in this world make one")),
        "the rendered recipe foot's maker count is replaced by the producer row: {:#?}",
        ledger.texts.iter().map(|sample| (&sample.key, &sample.content)).collect::<Vec<_>>(),
    );
}

#[gpui::test]
fn a_function_page_draws_its_pipe_from_inputs_to_output(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("relation_label")), 1440.0, 900.0);
    install(&mut rig);
    let ledger = painted(&mut rig);
    assert_eq!(at(&ledger, "anatomy-shape-in-0-name"), ["link"], "the input's name");
    assert_eq!(at(&ledger, "anatomy-shape-in-0"), ["Link"], "the input's type");
    assert_eq!(at(&ledger, "anatomy-shape-out"), ["RelationLabel"], "the output");
}

#[gpui::test]
fn the_usage_lens_shows_the_statements_callers_write(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    install(&mut rig);
    let ledger = painted(&mut rig);
    let usage = ledger.targets.iter().find(|target| target.key == "lens-Usage").expect("the Usage tab").bounds.clone();
    rig.cx.simulate_click(
        point(px(usage.x + usage.width / 2.0), px(usage.y + usage.height / 2.0)),
        Modifiers::default(),
    );
    rig.settle();
    let ledger = painted(&mut rig);
    assert_eq!(at(&ledger, "anatomy-in-use-caller-0"), ["print_labels"], "captioned with its caller");
    assert_eq!(
        at(&ledger, "anatomy-in-use-code-0-0"),
        ["let label = RelationLabel::Related;"],
        "the statement itself, not only where it is"
    );
}

#[gpui::test]
fn a_declaration_the_world_does_not_know_keeps_its_indexed_body(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("KindGlyph")), 1440.0, 900.0);
    install(&mut rig);
    let ledger = painted(&mut rig);
    assert!(!ledger.texts.iter().any(|text| text.key.starts_with("anatomy")), "no anatomy without a node");
    let said = rig.said();
    assert!(
        !said.iter().any(|line| line.starts_with("pub enum KindGlyph {")),
        "the indexed fallback also reserves raw declarations for Code: {said:#?}"
    );
    assert!(said.iter().any(|line| line == "One of"), "the recorded enum parts remain readable: {said:#?}");
    assert!(said.iter().any(|line| line == "Typed" || line.starts_with("Typed · ")), "the recorded variant and its captured payload are retained: {said:#?}");
    assert!(said.iter().any(|line| line == "Related"), "the recorded variant is retained: {said:#?}");
}

pub(super) fn package_route() -> crate::navigation::Route {
    crate::navigation::Route::Package(crate::navigation::PackageRoute {
        project: None,
        package: crate::core::PackageId::new(PACKAGE).expect("package"),
        lane: crate::navigation::PackageLane::Overview,
        selected: None,
        at: None,
    })
}

/// The package body begins with the producer's recorded outline, in its
/// recorded root order. It does not rank fixture declarations as advice.
#[gpui::test]
fn the_package_page_shows_its_recorded_outline(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(package_route()), 1440.0, 900.0);
    install(&mut rig);
    let _ = painted(&mut rig);
    let said = rig.said();
    let from = said.iter().position(|line| line == "Recorded outline").unwrap_or_else(|| panic!("no recorded outline: {said:#?}"));
    assert_eq!(said.get(from + 1).map(String::as_str), Some("Choose a row to inspect the declarations recorded beneath it."));
    let roots = said[from + 2..].iter().filter(|line| ["identity", "glyph", "outline"].contains(&line.as_str())).map(String::as_str).collect::<Vec<_>>();
    assert_eq!(roots, ["identity", "glyph", "outline"], "the recorded root order is preserved: {said:#?}");
    assert!(!said.iter().any(|line| line == "Start here" || line == "what you hold"), "no fixture-ranked tour: {said:#?}");
}

/// A recorded outline row is a door to its exact indexed coordinate.
#[gpui::test]
fn a_recorded_outline_row_opens_its_page(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(package_route()), 1440.0, 900.0);
    install(&mut rig);
    let ledger = painted(&mut rig);
    let key = format!("pkg-{}", super::tests::coordinate("glyph"));
    let glyph = ledger.targets.iter().find(|target| target.key == key).expect("recorded glyph root").bounds.clone();
    rig.cx.simulate_click(point(px(glyph.x + glyph.width / 2.0), px(glyph.y + glyph.height / 2.0)), Modifiers::default());
    rig.settle();
    assert_eq!(rig.route(), page_route("glyph"));
}
