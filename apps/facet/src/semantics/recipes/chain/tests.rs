//! The connector on the semantic tests' pinned world
//! (`semantics/tests/fixtures/recipes.json`: present, serde_core and
//! serde_json whole): the roads and sentences a hand of serde_json's pieces
//! makes.

use super::{Connector, JoinHow, Role};
use crate::graph::model::{Kind, NodeId, World};
use crate::semantics::recipes::{PreparedRecipes, Recipes};
use std::collections::HashSet;
use std::sync::OnceLock;

/// The pinned world and its producer table, parsed once per test binary;
/// each test attaches its own caches.
fn world() -> (&'static World, Recipes) {
    static WORLD: OnceLock<(World, PreparedRecipes)> = OnceLock::new();
    let (world, prepared) = WORLD.get_or_init(|| {
        let world = World::from_json(include_bytes!("../../tests/fixtures/recipes.json")).expect("the pinned world");
        let prepared = Recipes::new(&world).into_prepared();
        (world, prepared)
    });
    (world, Recipes::from_prepared(prepared.clone()))
}

/// The one node named `name` of `kind` in package `package`, top-level
/// unless `owner` names its parent.
fn find(world: &World, package: &str, owner: Option<&str>, name: &str, kind: Kind) -> NodeId {
    let hits: Vec<NodeId> = (0..u32::try_from(world.len()).expect("size"))
        .filter(|&i| {
            let n = world.node(i);
            n.name.as_ref() == name
                && n.kind == kind
                && world.packages[n.pkg as usize].name.as_ref() == package
                && match owner {
                    Some(owner) => n.parent.is_some_and(|p| world.node(p).name.as_ref() == owner),
                    None => n.parent.is_none(),
                }
        })
        .collect();
    assert_eq!(hits.len(), 1, "{package} {owner:?} {name}: {hits:?}");
    hits[0]
}

/// A world of exactly the facts a trait test is about: `Value` implements
/// `Deserialize` (for every lifetime, as serde spells it), `DeserializeOwned`
/// is a trait whose supertrait is `Deserialize`, `Owned` implements
/// `DeserializeOwned` itself, `Value::as_table` gives a `Table`, and
/// `parse(text)` gives a `Value`.
fn traits_world() -> World {
    use crate::graph::model::{Impl, Module, Node, Package};
    let mut nodes = Vec::new();
    let mut push = |node: Node| {
        nodes.push(node);
        u32::try_from(nodes.len() - 1).expect("id")
    };
    let public = |mut node: Node| {
        node.vis = Some("pub".into());
        node
    };
    let mut deserialize = public(Node::new(Kind::Trait, "Deserialize", 0, 0));
    deserialize.sig = Some("trait Deserialize<'de>:Sized".into());
    let deserialize = push(deserialize);
    let mut owned_trait = public(Node::new(Kind::Trait, "DeserializeOwned", 0, 0));
    owned_trait.sig = Some("trait DeserializeOwned:for<'de> Deserialize<'de>".into());
    let owned_trait = push(owned_trait);
    let mut value = public(Node::new(Kind::Enum, "Value", 0, 0));
    value.impls = vec![Impl { trait_: Some(deserialize), line: 1, members: vec!["deserialize".into()], generic: true }];
    let value = push(value);
    let table = push(public(Node::new(Kind::Struct, "Table", 0, 0)));
    let mut owned = public(Node::new(Kind::Struct, "Owned", 0, 0));
    owned.impls = vec![Impl { trait_: Some(owned_trait), line: 1, members: Vec::new(), generic: false }];
    push(owned);
    let mut as_table = public(Node::new(Kind::Method, "as_table", 0, 0));
    as_table.parent = Some(value);
    as_table.recv = Some("&self".into());
    as_table.ret = Some("Table".into());
    as_table.sig = Some("pub fn as_table(&self) -> Table".into());
    push(as_table);
    let mut parse = public(Node::new(Kind::Function, "parse", 0, 0));
    parse.params = vec!["s: &str".into()];
    parse.ret = Some("Value".into());
    parse.sig = Some("pub fn parse(s: &str) -> Value".into());
    push(parse);
    let _ = table;
    World::new(
        vec![Package { name: "doc".into(), version: "1.0.0".into(), yours: true, external: false, deps: vec![] }],
        vec![Module { pkg: 0, path: "".into(), file: "lib.rs".into() }],
        nodes,
        vec![],
    )
    .expect("the traits world")
}

/// `Value::from(map)` (a whole conversion) beats `map.get(key)` (a maybe
/// that needs a key you do not hold).
#[test]
fn a_map_becomes_a_value_in_one_step_from() {
    let (world, recipes) = world();
    let c = Connector::new(&recipes, world);
    let value = find(world, "serde_json", None, "Value", Kind::Enum);
    let map = find(world, "serde_json", None, "Map", Kind::Struct);
    let chain = c.convert(map, value).expect("a road from Map to Value");
    assert_eq!(chain.steps(), 1);
    let join = c.join(&c.piece(map), &c.piece(value)).expect("Map feeds Value");
    assert_eq!(c.verbs(&join), ["from"]);
    assert!(matches!(join.how, JoinHow::Steps(_)));
}

#[test]
fn from_str_feeds_the_value_it_makes_directly() {
    let (world, recipes) = world();
    let c = Connector::new(&recipes, world);
    let from_str = find(world, "serde_json", Some("Value"), "from_str", Kind::Method);
    let value = find(world, "serde_json", None, "Value", Kind::Enum);
    // Held Value first: the hand reads in flow order.
    let hand = c.arrange(&[value, from_str]);
    assert_eq!(hand.roads.len(), 1, "{hand:#?}");
    let road = &hand.roads[0];
    let names: Vec<&str> = road.links.iter().map(|l| world.node(l.piece.node).name.as_ref()).collect();
    assert_eq!(names, ["from_str", "Value"]);
    assert!(matches!(road.links[1].join.as_ref().map(|j| &j.how), Some(JoinHow::Direct)));
    assert_eq!(c.sentence(road, &HashSet::new()).text(), "from text to Value, in one step");
}

#[test]
fn text_to_table_in_two_steps_through_value() {
    let world = traits_world();
    let recipes = Recipes::new(&world);
    let c = Connector::new(&recipes, &world);
    let (value, table, parse) = (2, 3, 6);
    // Held Value, Table, then parse: arranged by what feeds what.
    let hand = c.arrange(&[value, table, parse]);
    assert_eq!(hand.roads.len(), 1, "{hand:#?}");
    let road = &hand.roads[0];
    let names: Vec<&str> = road.links.iter().map(|l| world.node(l.piece.node).name.as_ref()).collect();
    assert_eq!(names, ["parse", "Value", "Table"]);
    assert_eq!(c.sentence(road, &HashSet::new()).text(), "from text to Table, in two steps");
}

#[test]
fn any_deserialize_owned_needs_a_blanket_impl_the_world_does_not_record() {
    // A generic `from_str` gives "any DeserializeOwned". Value implements
    // Deserialize for every lifetime, and serde's blanket impl makes that a
    // DeserializeOwned, but a world records no blanket impls: the join is
    // not assumed. A type that implements DeserializeOwned itself joins.
    let world = traits_world();
    let recipes = Recipes::new(&world);
    let c = Connector::new(&recipes, &world);
    let generic = super::Piece {
        node: 0,
        role: Role::Call,
        q: None,
        ins: vec!["text".into()],
        out: Some("any DeserializeOwned".into()),
        fails: true,
        maybe: false,
    };
    let (value, owned) = (2, 4);
    assert!(c.traits_of(value).contains("Deserialize"));
    assert!(!c.traits_of(value).contains("DeserializeOwned"));
    assert_eq!(c.join(&generic, &c.piece(value)), None);
    let join = c.join(&generic, &c.piece(owned)).expect("Owned is a DeserializeOwned");
    assert!(matches!(join.how, JoinHow::As));
    // Implementing a trait is being its supertraits too.
    assert!(c.traits_of(owned).contains("Deserialize"));
}

#[test]
fn a_held_trait_filters_the_road_it_goes_through() {
    let world = traits_world();
    let recipes = Recipes::new(&world);
    let c = Connector::new(&recipes, &world);
    let (deserialize, value, table) = (0, 2, 3);
    let hand = c.arrange(&[value, deserialize, table]);
    assert_eq!(hand.roads.len(), 1, "{hand:#?}");
    assert_eq!(hand.roads[0].through, [deserialize]);
    assert_eq!(c.sentence(&hand.roads[0], &HashSet::new()).text(), "from Value to Table through Deserialize, in one step");
    assert!(hand.apart.is_empty());
}

#[test]
fn what_joins_nothing_stands_apart_in_held_order() {
    let world = traits_world();
    let recipes = Recipes::new(&world);
    let c = Connector::new(&recipes, &world);
    let (owned_trait, table, owned) = (1, 3, 4);
    let hand = c.arrange(&[owned, table, owned_trait]);
    assert!(hand.roads.is_empty(), "{hand:#?}");
    let apart: Vec<NodeId> = hand.apart.iter().map(|p| p.node).collect();
    assert_eq!(apart, [owned, table, owned_trait]);
}

