//! The symbol page's anatomy through the real shell. A small world is
//! installed as the fixture; its declarations sit where the page fixture's
//! do (`glyph.rs:138`), so the join is the product's own
//! (`IdentityAdapter`, file + line + name), and every assertion reads what
//! was painted (the probe ledger), never the model it was built from.

use super::tests::{PACKAGE, Rig, page_route, rig};
use crate::model::pages::PackageRef;
use crate::navigation::Intent;
use crate::shell::bodies::graph::identity::IdentityAdapter;
use facet::graph::{Edge, Kind, Module, Node, Package, Rel, World};
use facet::probe::Ledger;
use gpui::{Modifiers, TestAppContext, point, px};
use std::collections::HashMap;
use std::sync::Arc;
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
    let root = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    rig.cx.update(|_, cx| {
        crate::runtime::indexed_world::install_test_projection(
            root,
            "single-package anatomy fixture",
            Arc::clone(&world),
            Arc::clone(&identities),
            None,
            cx,
        );
        crate::runtime::fixture_world::install(world, identities, files, cx);
        facet::probe::enable(cx);
    });
    rig.repaint();
}

pub(super) fn painted(rig: &mut Rig) -> Ledger {
    rig.repaint();
    rig.cx.update(|_, cx| facet::probe::take(cx))
}

/// A worker barrier holds the exact graph projection that anatomy must join.
/// It exercises the same bounded resolver as the hand and graph views.
struct ReleaseProjection(Arc<crate::runtime::indexed_world::TestProjectionGate>);
impl Drop for ReleaseProjection {
    fn drop(&mut self) { self.0.release(); }
}

#[gpui::test]
fn late_cross_package_outline_cannot_open_after_back_away_but_a_fresh_click_can(cx: &mut TestAppContext) {
    let gate = Arc::new(crate::runtime::indexed_world::TestProjectionGate::default());
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    let _release_on_unwind = ReleaseProjection(gate.clone());
    let world = world();
    let identities = Arc::new(IdentityAdapter::synthetic_exact_catalog(&world, vec![
        PackageRef::parse(PACKAGE).expect("present"),
        PackageRef::parse("/fixture/app").expect("app"),
    ]));
    let root = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot().key());
    rig.cx.update(|_, cx| {
        crate::runtime::indexed_world::install_test_projection(
            root,
            "late cross-package anatomy fixture",
            Arc::clone(&world),
            Arc::clone(&identities),
            Some(gate.clone()),
            cx,
        );
        crate::runtime::fixture_world::install(world, identities, HashMap::new(), cx);
        facet::probe::enable(cx);
    });
    rig.repaint();

    let open = || facet::anatomy::Open { target: facet::semantics::Target::Node(7) };
    rig.cx.update(|window, cx| window.dispatch_action(Box::new(open()), cx));
    let deadline = Instant::now() + Duration::from_secs(3);
    while !gate.entered() {
        rig.frame(16);
        assert!(Instant::now() < deadline, "cross-package graph projection never entered its worker");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(rig.route(), page_route("RelationLabel"), "no guessed route while the exact outline is cold");
    let notice = rig.graph.store.read_with(rig.cx, |store, _| store.notice().map(|notice| notice.message.to_string()));
    assert!(notice.as_deref().is_some_and(|message| message.contains("graph is still being read")), "the cold exact projection remains provisional: {notice:?}");

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
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        rig.cx.update(|window, cx| window.dispatch_action(Box::new(open()), cx));
        let notice = rig.graph.store.read_with(rig.cx, |store, _| store.notice().map(|notice| notice.message.to_string()));
        assert_eq!(rig.route(), page_route("RelationLabel"), "an unresolved anatomy link never guesses a route");
        if notice.as_deref() == Some("main isn't in the index") {
            break;
        }
        assert!(notice.as_deref().is_some_and(|message| message.contains("graph is still being read")), "the projection is either pending or has the exact miss: {notice:?}");
        assert!(Instant::now() < deadline, "the exact graph projection did not complete: {notice:?}");
        rig.frame(16);
        std::thread::sleep(Duration::from_millis(1));
    }
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

/// The fork's case names, in paint order.
fn case_names(ledger: &Ledger) -> Vec<String> {
    ledger
        .texts
        .iter()
        .filter(|text| text.key.starts_with("s6-case-") && text.key.ends_with("-name"))
        .map(|text| text.content.clone())
        .collect()
}

#[gpui::test]
fn an_enum_page_draws_its_fork_in_place_of_its_code(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("RelationLabel")), 1440.0, 900.0);
    install(&mut rig);
    let ledger = painted(&mut rig);
    assert_eq!(at(&ledger, "s6-shape-head-count"), ["one of 2"]);
    assert_eq!(case_names(&ledger), ["Typed", "Related"], "one row per variant, in order");
    assert_eq!(at(&ledger, "s6-case-0-holds-0-word"), ["SemanticLinkKind"], "Typed holds its payload, in words");
    assert_eq!(at(&ledger, "s6-case-1-nothing"), ["nothing inside"], "Related holds nothing");
    // The methods, by what they do with it.
    assert_eq!(at(&ledger, "s6-group-0-head"), ["Reads it"]);
    assert_eq!(at(&ledger, "s6-group-0-method-0-name"), ["as_str"]);
    assert_eq!(at(&ledger, "s6-group-0-method-1-name"), ["is_typed"]);
    let said = rig.said();
    assert!(!said.iter().any(|line| line.starts_with("pub enum RelationLabel {")), "the fork replaces the declaration's code: {said:#?}");
    assert!(!ledger.texts.iter().any(|text| text.key.contains("prism")), "the prism stays in the graph");
    for gone in ["Reference", "Relations", "Usage", "History", "made of", "is", "from", "to"] {
        assert!(!said.iter().any(|line| line == gone), "`{gone}` (a tab or the relation list) is still said: {said:#?}");
    }
}

#[gpui::test]
fn a_declaration_the_world_does_not_know_draws_from_the_index_alone(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(page_route("KindGlyph")), 1440.0, 900.0);
    install(&mut rig);
    let ledger = painted(&mut rig);
    assert!(!ledger.texts.iter().any(|text| text.key.starts_with("anatomy")), "no world anatomy without a node");
    assert_eq!(case_names(&ledger), ["Typed", "Related"], "the index's variants are the fork");
    let said = rig.said();
    assert!(!said.iter().any(|line| line.starts_with("pub enum KindGlyph {")), "raw declarations stay in Code: {said:#?}");
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

/// The package page is the producer's recorded outline: its names, in the
/// recorded order, drawn as the territory (one region per module) that opens
/// into one card per name. It does not rank fixture declarations as advice.
#[gpui::test]
fn the_package_page_shows_its_recorded_outline(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(package_route()), 1440.0, 900.0);
    install(&mut rig);
    let ledger = painted(&mut rig);
    // The fixture files every declaration under `glyph.rs`, so the recorded
    // outline is one module of its six names.
    let regions: Vec<&str> = ledger.texts.iter().filter(|text| text.key.contains("shingles-region-")).map(|text| text.content.as_str()).collect();
    assert_eq!(regions, ["glyph"], "the recorded outline's module is a region of the territory");
    let region = ledger.texts.iter().find(|text| text.key.contains("shingles-region-glyph")).expect("the glyph region").bounds.clone();
    rig.cx.simulate_click(point(px(region.x + region.width / 2.0), px(region.y + region.height / 2.0)), Modifiers::default());
    rig.settle();
    let ledger = painted(&mut rig);
    let names: Vec<&str> = ledger.texts.iter().filter(|text| text.key.contains("module-card-") && text.key.ends_with("-name")).map(|text| text.content.as_str()).collect();
    assert_eq!(names, ["Identity", "RelationLabel", "RelationDirection", "KindGlyph", "relation_label", "Outline"], "the recorded order is preserved");
    let said = rig.said();
    let painted_words: Vec<&str> = ledger.texts.iter().map(|text| text.content.as_str()).collect();
    assert!(!said.iter().any(|line| line == "Start here" || line == "what you hold") && !painted_words.iter().any(|w| *w == "Start here" || *w == "what you hold"), "no fixture-ranked tour: {said:#?}");
}

/// A recorded name's card is a door to its exact indexed coordinate.
#[gpui::test]
fn a_recorded_outline_row_opens_its_page(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(package_route()), 1440.0, 900.0);
    install(&mut rig);
    let ledger = painted(&mut rig);
    let region = ledger.texts.iter().find(|text| text.key.contains("shingles-region-glyph")).expect("the glyph region").bounds.clone();
    rig.cx.simulate_click(point(px(region.x + region.width / 2.0), px(region.y + region.height / 2.0)), Modifiers::default());
    rig.settle();
    let ledger = painted(&mut rig);
    let key = format!("pkg-card-{}", super::tests::coordinate("RelationLabel"));
    let card = ledger.targets.iter().find(|target| target.key == key).expect("the recorded RelationLabel card").bounds.clone();
    rig.cx.simulate_click(point(px(card.x + card.width / 2.0), px(card.y + card.height / 2.0)), Modifiers::default());
    rig.settle();
    assert_eq!(rig.route(), page_route("RelationLabel"));
}
