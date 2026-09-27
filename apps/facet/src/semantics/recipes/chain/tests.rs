//! The connector on the prototype's world (the 56 k-node snapshot the
//! graph and the page read): the roads and sentences a hand of toml's
//! pieces makes.

use super::{Connector, JoinHow, Role};
use crate::graph::model::{Kind, NodeId, World};
use crate::semantics::recipes::{PreparedRecipes, Recipes};
use std::collections::HashSet;
use std::sync::OnceLock;

/// The world and its producer table, parsed once per test binary; each
/// test attaches its own caches.
fn world() -> (&'static World, Recipes) {
    static WORLD: OnceLock<(World, PreparedRecipes)> = OnceLock::new();
    let (world, prepared) = WORLD.get_or_init(|| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Nudox-Design-System/v4/graph/world.json");
        let world = World::from_json(&std::fs::read(path).expect("world.json")).expect("world");
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

#[test]
fn a_value_becomes_a_table_in_one_step_as_table() {
    let (world, recipes) = world();
    let c = Connector::new(&recipes, world);
    let value = find(world, "toml", None, "Value", Kind::Enum);
    let table = find(world, "toml", None, "Table", Kind::Type);
    let chain = c.convert(value, table).expect("a road from Value to Table");
    assert_eq!(chain.steps(), 1);
    let join = c.join(&c.piece(value), &c.piece(table)).expect("Value feeds Table");
    assert_eq!(c.verbs(&join), ["as_table"]);
    assert!(matches!(join.how, JoinHow::Steps(_)));
}

#[test]
fn text_to_table_in_two_steps_through_value() {
    let (world, recipes) = world();
    let c = Connector::new(&recipes, world);
    let from_str = find(world, "toml", Some("Value"), "from_str", Kind::Method);
    let value = find(world, "toml", None, "Value", Kind::Enum);
    let table = find(world, "toml", None, "Table", Kind::Type);
    // Held as the design's example holds them (Value, Table, then
    // from_str): the hand arranges by what feeds what, then by held order.
    let hand = c.arrange(&[value, table, from_str]);
    assert_eq!(hand.roads.len(), 1, "{hand:#?}");
    let road = &hand.roads[0];
    let names: Vec<&str> = road.links.iter().map(|l| world.node(l.piece.node).name.as_ref()).collect();
    assert_eq!(names, ["from_str", "Value", "Table"]);
    assert_eq!(c.sentence(road, &HashSet::new()).text(), "from text to Table, in two steps");
}

#[test]
fn a_free_generic_from_str_needs_a_blanket_impl_the_world_does_not_record() {
    // toml::from_str gives "any DeserializeOwned". Value is a Deserialize
    // (for every lifetime), and serde's blanket impl makes that a
    // DeserializeOwned, but the world records no blanket impls: the join is
    // not assumed.
    let (world, recipes) = world();
    let c = Connector::new(&recipes, world);
    let from_str = find(world, "toml", None, "from_str", Kind::Function);
    let value = find(world, "toml", None, "Value", Kind::Enum);
    let piece = c.piece(from_str);
    assert_eq!(piece.role, Role::Call);
    assert!(c.traits_of(value).contains("Deserialize"));
    assert!(!c.traits_of(value).contains("DeserializeOwned"));
    assert_eq!(c.join(&piece, &c.piece(value)), None);
}

#[test]
fn what_joins_nothing_stands_apart_in_held_order() {
    let (world, recipes) = world();
    let c = Connector::new(&recipes, world);
    let value = find(world, "toml", None, "Value", Kind::Enum);
    let table = find(world, "toml", None, "Table", Kind::Type);
    let deserialize = find(world, "serde_core", None, "Deserialize", Kind::Trait);
    let hand = c.arrange(&[value, deserialize, table]);
    assert_eq!(hand.roads.len(), 1);
    // A held trait filters: Value is a Deserialize, so the road goes through it.
    assert_eq!(hand.roads[0].through, [deserialize]);
    assert_eq!(c.sentence(&hand.roads[0], &HashSet::new()).text(), "from Value to Table through Deserialize, in one step");
    assert!(hand.apart.is_empty());
}
