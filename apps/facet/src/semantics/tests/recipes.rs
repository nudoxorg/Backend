//! Getting one / Calling it against the prototype: `recipes.golden` is
//! `recipes.js` run headless over the pinned `fixtures/recipes.json`
//! (`fixtures/recipes.mjs golden`): the producer table row by row, and each
//! target's routes, costs, code and section words.

use crate::graph::model::{NodeId, World};
use crate::semantics::recipes::{Recipes, Section, code};
use std::path::PathBuf;
use std::sync::OnceLock;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/semantics/tests/fixtures/recipes.json")
}

/// The pinned recipe world, with the full world's importance.
pub(super) fn world() -> &'static World {
    static WORLD: OnceLock<World> = OnceLock::new();
    WORLD.get_or_init(|| {
        let bytes = std::fs::read(fixture()).expect("recipes.json");
        let mut world = World::from_json(&bytes).expect("the fixture parses");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        #[allow(clippy::cast_possible_truncation)]
        let imp: Vec<f32> = json["imp"].as_array().expect("imp").iter().map(|v| v.as_f64().unwrap_or(0.0) as f32).collect();
        assert_eq!(imp.len(), world.len());
        world.importance = imp;
        world
    })
}

fn find(qualified: &str) -> NodeId {
    let w = world();
    let (place, name) = qualified.rsplit_once("::").expect("a path");
    (0..u32::try_from(w.len()).unwrap_or(0))
        .find(|&i| w.node(i).name.as_ref() == name && w.qual(i).as_ref() == place)
        .unwrap_or_else(|| panic!("no {qualified}"))
}

fn named_keys(w: &World, key: &str) -> String {
    let mut out = String::new();
    let mut rest = key;
    while let Some(at) = rest.find('#') {
        out.push_str(&rest[..=at]);
        let digits: String = rest[at + 1..].chars().take_while(char::is_ascii_digit).collect();
        match digits.parse::<u32>() {
            Ok(j) => out.push_str(&w.node(j).name),
            Err(_) => {}
        }
        rest = &rest[at + 1 + digits.len()..];
    }
    out.push_str(rest);
    out
}

fn js_round(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// The summary `recipes.mjs` writes, rebuilt from the Rust port, as JSON.
fn summary(r: &Recipes, i: NodeId) -> serde_json::Value {
    let w = world();
    let section = r.getting_one(w, i).or_else(|| r.calling_it(w, i)).map(|s: Section| s.words(w)).unwrap_or_default();
    if matches!(w.node(i).kind, crate::graph::Kind::Function | crate::graph::Kind::Method) {
        return match r.call_route(w, i) {
            Some(route) => serde_json::json!({ "call": code(r, w, &route.tree), "section": section }),
            None => serde_json::Value::Null,
        };
    }
    let routes = r.routes(w, i, 3);
    serde_json::json!({
        "makers": routes.makers,
        "picks": routes.picks,
        "section": section,
        "routes": routes.routes.iter().map(|route| serde_json::json!({
            "cost": js_round(route.cost),
            "code": code(r, w, &route.tree),
            "alts": route.alts.iter().map(|k| named_keys(w, k)).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

fn recipes() -> &'static Recipes {
    // The run cache is a RefCell, so each test thread keeps its own.
    thread_local! {
        static RECIPES: &'static Recipes = Box::leak(Box::new(Recipes::new(world())));
    }
    RECIPES.with(|r| *r)
}

#[test]
fn the_producer_table_matches_the_prototype_row_by_row() {
    let w = world();
    let r = recipes();
    let want: Vec<&str> = include_str!("recipes.golden").lines().filter(|l| l.starts_with("table|")).collect();
    let got: Vec<String> = r
        .table()
        .iter()
        .map(|e| {
            format!(
                "table|{}|{}|{}|{}|{}|{}{}",
                e.node,
                w.node(e.node).name,
                e.how.text(),
                e.ins.join(";"),
                e.out,
                u8::from(e.fails),
                u8::from(e.maybe)
            )
        })
        .collect();
    let diffs: Vec<String> =
        want.iter().zip(&got).filter(|(a, b)| **a != b.as_str()).map(|(a, b)| format!("want {a}\n got  {b}")).collect();
    assert!(want.len() > 500, "{} rows", want.len());
    assert!(diffs.is_empty() && want.len() == got.len(), "{} rows want, {} got; {} differ:\n{}", want.len(), got.len(), diffs.len(), diffs.iter().take(20).cloned().collect::<Vec<_>>().join("\n"));
}

#[test]
fn every_target_and_call_matches_the_prototype_entry_for_entry() {
    let r = recipes();
    let mut checked = 0;
    for line in include_str!("recipes.golden").lines().filter(|l| l.starts_with("target|")) {
        let mut parts = line.splitn(3, '|');
        let (_, q, json) = (parts.next(), parts.next().unwrap_or_default(), parts.next().unwrap_or_default());
        let want: serde_json::Value = serde_json::from_str(json).expect("golden json");
        let got = summary(r, find(q));
        assert_eq!(got, want, "{q}\n got  {got}\n want {want}");
        checked += 1;
    }
    assert_eq!(checked, 6);
}

#[test]
fn the_lead_s_content_truth_reads_as_promised() {
    let w = world();
    let r = recipes();
    let codes = |q: &str| r.routes(w, find(q), 3).routes.iter().map(|x| code(r, w, &x.tree)).collect::<Vec<_>>();
    assert_eq!(codes("serde_json::de::Deserializer"), ["Deserializer::new(read)", "Deserializer::from_reader(reader)", "Deserializer::from_slice(bytes)"]);
    assert_eq!(codes("desktop::model::local_package::manifest::Manifest"), ["Manifest::default()", "read_manifest(path)"]);
    let value = r.routes(w, find("serde_json::value::Value"), 3);
    assert_eq!(code(r, w, &value.routes[0].tree), "Value::default()");
    assert_eq!(code(r, w, &value.routes[1].tree), "Value::from(f)");
    assert_eq!(value.routes[1].alts.len(), 8);
    assert_eq!(value.picks, 6);
    assert_eq!(
        codes("present::page::Page"),
        ["let source = Source::Absent { fault: shape(what) };\nPage::new(Identity::parse(label), DeclarationKind::Module, source)"]
    );
}

/// A hand-built world: `pair(a: Foo, b: Foo) -> Pair`, where a `Foo` comes
/// from `Foo::new(Bar::parse(s))`.
fn shared_world() -> World {
    use crate::graph::model::{Kind, Module, Node, Package};
    let package = Package { name: "demo".into(), version: "0.1.0".into(), yours: false, external: false, deps: Vec::new() };
    let module = Module { pkg: 0, path: "".into(), file: "src/lib.rs".into() };
    let item = |kind: Kind, name: &str| {
        let mut n = Node::new(kind, name.to_owned(), 0, 0);
        n.vis = Some("pub".into());
        n.file = Some("src/lib.rs".into());
        n.non_exhaustive = true;
        n
    };
    let callable = |name: &str, parent: Option<NodeId>, params: &[&str], ret: &str| {
        let mut n = item(Kind::Function, name);
        n.non_exhaustive = false;
        n.parent = parent;
        n.params = params.iter().map(|p| (*p).into()).collect();
        n.ret = Some(ret.to_owned().into());
        n
    };
    let nodes = vec![
        item(Kind::Struct, "Bar"),
        item(Kind::Struct, "Foo"),
        item(Kind::Struct, "Pair"),
        callable("parse", Some(0), &["s: &str"], "Bar"),
        callable("new", Some(1), &["bar: Bar"], "Self"),
        callable("pair", None, &["a: Foo", "b: Foo"], "Pair"),
    ];
    World::new(vec![package], vec![module], nodes, Vec::new()).expect("a world")
}

#[test]
fn shared_sub_steps_are_bound_once() {
    let w = shared_world();
    let r = Recipes::new(&w);
    let routes = r.routes(&w, 2, 3);
    assert_eq!(routes.routes.len(), 1);
    assert_eq!(code(&r, &w, &routes.routes[0].tree), "let foo = Foo::new(Bar::parse(s));\npair(foo, foo)");
    // Calling it: the same tree rooted at `pair`.
    let section = r.calling_it(&w, 5).expect("pair needs a Foo");
    assert_eq!(section.words(&w), "Calling it from text s parse Bar new Foo pair + Foo b every argument, from plain values · ⌥ for code");
}

#[test]
fn equal_costs_rank_by_importance_then_table_order() {
    use crate::graph::model::{Kind, Module, Node, Package};
    let package = Package { name: "demo".into(), version: "0.1.0".into(), yours: false, external: false, deps: Vec::new() };
    let module = Module { pkg: 0, path: "".into(), file: "src/lib.rs".into() };
    let make = |kind: Kind, name: &str, ret: Option<&str>| {
        let mut n = Node::new(kind, name.to_owned(), 0, 0);
        n.vis = Some("pub".into());
        n.file = Some("src/lib.rs".into());
        n.non_exhaustive = kind == Kind::Struct;
        n.ret = ret.map(|r| r.to_owned().into());
        n
    };
    let nodes = vec![
        make(Kind::Struct, "Pair", None),
        make(Kind::Function, "first", Some("Pair")),
        make(Kind::Function, "second", Some("Pair")),
        make(Kind::Function, "third", Some("Pair")),
    ];
    let mut w = World::new(vec![package], vec![module], nodes, Vec::new()).expect("a world");
    // Three makers of equal cost: the most important first, then table order.
    w.importance = vec![1.0, 0.2, 0.9, 0.2];
    let r = Recipes::new(&w);
    let order: Vec<String> = r.routes(&w, 0, 3).routes.iter().map(|x| code(&r, &w, &x.tree)).collect();
    assert_eq!(order, ["second()", "first()", "third()"]);
}

#[test]
fn the_page_rail_draws_what_its_words_say() {
    use crate::semantics::types::Piece;
    let w = world();
    let r = recipes();
    let view = r.getting_one(w, find("present::page::Page")).expect("a section").view(w);
    let text = |p: &[Piece]| p.iter().map(Piece::text).collect::<String>();
    let rail = &view.rails[0];
    assert_eq!(text(&rail.lead), "from text what");
    let verbs: Vec<&str> = rail.steps.iter().map(|s| s.verb.as_ref()).collect();
    assert_eq!(verbs, ["shape", "Source::Absent", "new"]);
    let stations: Vec<String> = rail.steps.iter().filter_map(|s| s.station.as_deref().map(text)).collect();
    assert_eq!(stations, ["Fault", "Source"]);
    let riders: Vec<String> = rail.steps[2].riders.iter().map(|x| text(x)).collect();
    assert_eq!(riders, ["+ Identity identity", "+ a DeclarationKind kind"]);
    assert!(rail.code.starts_with("let source = Source::Absent"));
    assert_eq!(view.foot.as_deref(), Some("4 ways in this world make one · ⌥ for code"));
    // Every station and rider name is a link into the world.
    assert!(rail.steps[0].station.as_ref().is_some_and(|s| matches!(s[0], Piece::Name { .. })));
}
