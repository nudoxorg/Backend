use super::icon_kind;
use crate::graph::{Kind as WorldKind, Module, Node, Package, World};
use crate::icons::Kind as IconKind;

#[test]
fn every_world_mark_keeps_its_kind_and_existing_layout_discriminant() {
    // Layout content keys include Kind as u8. New kinds append; these values
    // must not move when presentation projections gain another case.
    let cases = [
        (WorldKind::Struct, IconKind::Struct, 0),
        (WorldKind::Enum, IconKind::Enum, 1),
        (WorldKind::Union, IconKind::Union, 2),
        (WorldKind::Trait, IconKind::Trait, 3),
        (WorldKind::Type, IconKind::Type, 4),
        (WorldKind::Function, IconKind::Function, 5),
        (WorldKind::Method, IconKind::Method, 6),
        (WorldKind::Macro, IconKind::Macro, 7),
        (WorldKind::Constant, IconKind::Constant, 8),
        (WorldKind::Field, IconKind::Field, 9),
        (WorldKind::Variant, IconKind::Variant, 10),
        (WorldKind::Other, IconKind::Unknown, 11),
        (WorldKind::Variable, IconKind::Variable, 12),
    ];
    for (kind, mark, discriminant) in cases {
        assert_eq!(icon_kind(kind), mark, "{kind:?}");
        assert_eq!(kind as u8, discriminant, "{kind:?}");
    }
}

#[test]
fn identical_value_names_keep_distinct_layout_keys_without_borrowing_source() {
    let world = |kind| {
        World::new(
            vec![Package {
                name: "p".into(),
                version: "0".into(),
                yours: true,
                external: false,
                deps: vec![],
            }],
            vec![Module {
                pkg: 0,
                path: "".into(),
                file: "src/lib.rs".into(),
            }],
            vec![Node::new(kind, "signal", 0, 0)],
            vec![],
        )
        .expect("a valid one-value world")
    };
    let variable = world(WorldKind::Variable);
    let constant = world(WorldKind::Constant);
    assert_ne!(
        crate::graph::layout::key(&variable),
        crate::graph::layout::key(&constant)
    );
    assert_eq!(variable.node(0).declaration_word(), "variable");
    assert_eq!(constant.node(0).declaration_word(), "constant");
    assert_ne!(
        icon_kind(variable.node(0).kind),
        icon_kind(constant.node(0).kind)
    );
    assert!(variable.node(0).file.is_none());
    assert_eq!(
        variable.node(0).source_status,
        crate::graph::model::SourceStatus::NotCaptured
    );
    assert_eq!(
        variable.node(0).source_words(),
        "No source location captured for this declaration"
    );
}
