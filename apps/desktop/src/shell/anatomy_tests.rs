//! The symbol page's anatomy through the real shell. A small world is
//! installed as the fixture; its declarations sit where the page fixture's
//! do (`glyph.rs:138`), so the join is the product's own
//! (`IdentityAdapter`, file + line + name), and every assertion reads what
//! was painted (the probe ledger), never the model it was built from.

use super::tests::{PACKAGE, Rig, page_route, rig};
use crate::model::pages::PackageRef;
use crate::shell::bodies::graph::identity::IdentityAdapter;
use facet::graph::{Edge, Kind, Module, Node, Package, Rel, World};
use facet::probe::Ledger;
use gpui::{Modifiers, TestAppContext, point, px};
use std::collections::HashMap;
use std::sync::Arc;

/// The caller's file: `print_labels` (lines 3–6) uses `RelationLabel`.
const GLYPH: &str = "//! Glyphs.\n\npub fn print_labels(out: &mut String) {\n    let label = RelationLabel::Related;\n    out.push_str(label.as_str());\n}\n";

/// `RelationLabel` (an enum of `Typed(SemanticLinkKind)` and `Related`,
/// so made of `SemanticLinkKind`), `relation_label(link: &Link) ->
/// RelationLabel`, and a caller of the enum.
fn world() -> Arc<World> {
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
    let payload = node(Kind::Enum, "SemanticLinkKind", 40, None);
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
            vec![label, typed, related, function, caller, payload, error, main],
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
    assert_eq!(said.get(used + 1).map(String::as_str), Some("print_labels"), "{said:#?}");
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
        said.iter().any(|line| line == "pub enum KindGlyph {\n    Typed(SemanticLinkKind),\n    Related,\n}"),
        "the declaration's code, from the index: {said:#?}"
    );
    assert!(said.iter().any(|line| line == "Made of"), "{said:#?}");
}

fn package_route() -> crate::navigation::Route {
    crate::navigation::Route::Package(crate::navigation::PackageRoute {
        project: None,
        package: crate::core::PackageId::new(PACKAGE).expect("package"),
        lane: crate::navigation::PackageLane::Overview,
        selected: None,
        at: None,
    })
}

/// Start here: the package's reading path, each stop with its role, in the
/// order a newcomer meets them; the modules follow under their own name.
#[gpui::test]
fn the_package_page_starts_here_with_its_reading_path(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(package_route()), 1440.0, 900.0);
    install(&mut rig);
    let _ = painted(&mut rig);
    let said = rig.said();
    let from = said.iter().position(|line| line == "Start here").unwrap_or_else(|| panic!("no Start here: {said:#?}"));
    assert_eq!(
        said[from + 1..from + 9],
        [
            "relation_label", "start here",
            "RelationLabel", "what you hold",
            "SemanticLinkKind", "inside it",
            "LabelError", "when it fails",
        ],
        "{said:#?}"
    );
    assert_eq!(said.get(from + 9).map(String::as_str), Some("Modules"), "{said:#?}");
}

/// A stop is a door to its page (joined through the package outline).
#[gpui::test]
fn a_start_here_stop_opens_its_page(cx: &mut TestAppContext) {
    let mut rig = rig(cx, Some(package_route()), 1440.0, 900.0);
    install(&mut rig);
    let ledger = painted(&mut rig);
    let first = ledger.targets.iter().find(|target| target.key == "tour-0").expect("the first stop").bounds.clone();
    rig.cx.simulate_click(point(px(first.x + first.width / 2.0), px(first.y + first.height / 2.0)), Modifiers::default());
    rig.settle();
    assert_eq!(rig.route(), page_route("relation_label"));
}
