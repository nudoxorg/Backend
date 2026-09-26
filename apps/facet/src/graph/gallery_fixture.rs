//! Small pinned world for interaction value assertions. Never derive expected
//! values from the mutable workspace extraction or from the renderer itself.
use super::super::model::{Edge, Kind, Module, Node, Package, Rel, World};

pub(super) fn world() -> World {
    let packages = vec![
        Package {
            name: "backend-present".into(),
            version: "0.1.0".into(),
            yours: true,
            external: false,
            deps: vec![1],
        },
        Package {
            name: "serde_json".into(),
            version: "1.0.0".into(),
            yours: false,
            external: true,
            deps: vec![],
        },
    ];
    let modules = vec![
        Module {
            pkg: 0,
            path: "glyph".into(),
            file: "src/glyph.rs".into(),
        },
        Module {
            pkg: 0,
            path: "page".into(),
            file: "src/page.rs".into(),
        },
        Module {
            pkg: 1,
            path: "de".into(),
            file: "src/de.rs".into(),
        },
    ];
    let mut nodes = vec![
        Node::new(Kind::Struct, "RelationLabel", 0, 0),    // 0
        Node::new(Kind::Field, "text", 0, 0).member_of(0), // 1
        Node::new(Kind::Method, "new", 0, 0).member_of(0), // 2
        Node::new(Kind::Struct, "RelationGroup", 0, 1),    // 3
        Node::new(Kind::Function, "render", 0, 1),         // 4
        Node::new(Kind::Function, "from_str", 1, 2),       // 5
        Node::new(Kind::Trait, "Visitor", 1, 2),           // 6
        Node::new(Kind::Enum, "Error", 1, 2),              // 7
        Node::new(Kind::Struct, "Value", 1, 2),            // 8
        Node::new(Kind::Function, "parse", 0, 1),          // 9
        Node::new(Kind::Struct, "Invocation", 0, 1),       // 10
        Node::new(Kind::Struct, "Grammar", 0, 1),          // 11
        Node::new(Kind::Method, "grammar", 0, 1).member_of(10), // 12
        Node::new(Kind::Method, "aliases", 0, 1).member_of(11), // 13
    ];
    for node in &mut nodes {
        node.vis = Some("pub".into());
        node.doc = Some(format!("Pinned documentation for {}.", node.name).into());
    }
    nodes[2].ret = Some("RelationLabel".into());
    nodes[5].params = vec!["text: &str".into()];
    nodes[5].ret = Some("Result<Value, Error>".into());
    nodes[9].params = vec!["text: &str".into()];
    nodes[9].ret = Some("Result<RelationGroup, Error>".into());
    nodes[10].non_exhaustive = true;
    nodes[11].non_exhaustive = true;
    nodes[12].recv = Some("reads".into());
    nodes[12].ret = Some("Option<Grammar>".into());
    nodes[13].recv = Some("reads".into());
    nodes[13].ret = Some("Vec<String>".into());
    let edges = vec![
        Edge {
            from: 12,
            to: 11,
            rel: Rel::GIVES,
        },
        Edge {
            from: 2,
            to: 0,
            rel: Rel::GIVES,
        },
        Edge {
            from: 3,
            to: 0,
            rel: Rel::TYPE,
        },
        Edge {
            from: 4,
            to: 0,
            rel: Rel::TAKES,
        },
        Edge {
            from: 2,
            to: 5,
            rel: Rel::CALLS,
        },
        Edge {
            from: 5,
            to: 6,
            rel: Rel::USES,
        },
        Edge {
            from: 5,
            to: 7,
            rel: Rel::GIVES,
        },
        Edge {
            from: 5,
            to: 8,
            rel: Rel::GIVES,
        },
        Edge {
            from: 9,
            to: 5,
            rel: Rel::CALLS,
        },
        Edge {
            from: 9,
            to: 3,
            rel: Rel::GIVES,
        },
        Edge {
            from: 9,
            to: 7,
            rel: Rel::GIVES,
        },
    ];
    World::new(packages, modules, nodes, edges)
        .unwrap_or_else(|error| panic!("invalid pinned graph world: {error}"))
}
