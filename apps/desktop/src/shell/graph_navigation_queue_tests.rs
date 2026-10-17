//! The resolved graph destination remains guarded until Navigate is reduced.
//! These use the existing row-mapper/native-window fixture, not a substitute
//! graph or a hand-constructed queued request. Native execution is required.

#![allow(clippy::expect_used, clippy::panic)]

use crate::model::AppSnapshot;
use crate::navigation::{Intent, OrbitRoute, Route, View};
use crate::runtime::owner::{OwnerGate, OwnerState};
use crate::runtime::ui_graph::{GraphDestination, UiRootEntity};
use crate::shell::graph_tests::canary_native_rig;
use crate::shell::tests::Rig;
use gpui::{App, Entity, TestAppContext};
use std::collections::BTreeSet;
use std::sync::Arc;

fn selected(cx: &mut TestAppContext) -> (Rig, OwnerGate) {
    let (mut rig, gate) = canary_native_rig(cx, 1440.0, 1.0, facet::tokens::Appearance::Abyss);
    rig.shell
        .update(rig.cx, |shell, cx| shell.focus_graph_node(1, cx));
    rig.settle();
    rig.graph.store.read_with(rig.cx, |store, _| {
        let focus = store.graph_focus().expect("selected native node");
        assert_eq!(focus.node, 1);
        assert!(
            focus.indexed.is_some(),
            "the fixture row already resolved this exact graph node"
        );
    });
    (rig, gate)
}

/// Call the real Map::open_current path while holding the outer UI update.
/// The selected fixture has an exact row mapping, so no search is pending.
/// Invalidation happens only after the actual navigation has been queued.
fn queue_then(
    rig: &mut Rig,
    view: View,
    invalidate: impl FnOnce(&Entity<UiRootEntity>, &mut App),
) -> Arc<AppSnapshot> {
    let shell = rig.shell.clone();
    let root = rig.graph.root.clone();
    let target = match view {
        View::Page => GraphDestination::Page,
        View::Code => GraphDestination::Code,
        View::Graph => unreachable!("only declaration destinations"),
    };
    rig.cx.update(|window, cx| {
        shell.update(cx, |shell, cx| {
            shell.reader.update(cx, |reader, cx| {
                reader.open_graph_current(target, window, cx)
            });
        });
        assert_eq!(
            root.read(cx).queued_graph_navigations(),
            1,
            "the real resolved-node open reached the guarded deferred boundary"
        );
        assert_eq!(
            root.read(cx).snapshot().route(),
            &Route::World,
            "queueing has not reduced Navigate inside this UI turn"
        );
        invalidate(&root, cx);
        root.read(cx).snapshot()
    })
}

fn unchanged(rig: &mut Rig, expected: &AppSnapshot) {
    rig.settle();
    rig.graph.root.read_with(rig.cx, |root, _| {
        let current = root.snapshot();
        assert_eq!(
            current.route(),
            expected.route(),
            "stale resolved navigation cannot win later"
        );
        assert_eq!(current.session().back, expected.session().back);
        assert_eq!(current.session().forward, expected.session().forward);
        assert_eq!(root.queued_graph_navigations(), 0);
    });
}

#[gpui::test]
fn resolved_graph_navigation_keeps_current_page_code_and_return_history(cx: &mut TestAppContext) {
    for view in [View::Page, View::Code] {
        let (mut rig, _) = selected(cx);
        let (package, symbol) = rig.graph.store.read_with(rig.cx, |store, _| {
            store
                .graph_focus()
                .expect("focus")
                .indexed
                .clone()
                .expect("exact row")
        });
        queue_then(&mut rig, view, |_, _| {});
        rig.settle();
        let Route::Symbol(route) = rig.route() else {
            panic!("current selected destination must open");
        };
        assert_eq!(route.id.as_str(), symbol.as_str());
        assert_eq!(route.package.as_str(), package.as_str());
        assert_eq!(route.view, view);
        assert_eq!(
            rig.graph.root.read_with(rig.cx, |root, _| root
                .snapshot()
                .session()
                .back
                .last()
                .cloned()),
            Some(Route::World)
        );
        rig.go(Intent::Back);
        assert_eq!(rig.route(), Route::World);
        assert_eq!(
            rig.shell.read_with(rig.cx, |shell, cx| shell
                .graph_entity(cx)
                .expect("retained graph")
                .read(cx)
                .focused()),
            Some(1),
            "the existing reading capture still restores the exact selected node"
        );
    }
}

#[gpui::test]
fn resolved_graph_navigation_refuses_same_root_owner_replacement_before_flush(
    cx: &mut TestAppContext,
) {
    for view in [View::Page, View::Code] {
        let (mut rig, gate) = selected(cx);
        let basis = rig
            .graph
            .store
            .read_with(rig.cx, |store, _| store.snapshot().key());
        let before = queue_then(&mut rig, view, |_, _| {
            gate.publish(OwnerState::Starting);
            gate.publish(OwnerState::Ready {
                key: basis,
                mode: crate::model::ServiceMode::Attached,
            });
        });
        unchanged(&mut rig, &before);
    }
}

#[gpui::test]
fn resolved_graph_navigation_refuses_package_publication_before_flush(cx: &mut TestAppContext) {
    for view in [View::Page, View::Code] {
        let (mut rig, _) = selected(cx);
        let package = rig.graph.store.read_with(rig.cx, |store, _| {
            store
                .graph_focus()
                .expect("focus")
                .indexed
                .as_ref()
                .expect("exact row")
                .0
                .clone()
        });
        let basis = rig
            .graph
            .root
            .read_with(rig.cx, |root, _| root.snapshot().key());
        let before = queue_then(&mut rig, view, |root, cx| {
            root.update(cx, |root, cx| {
                root.packages_published(&BTreeSet::from([package]), cx)
            });
        });
        assert!(
            before.key().same_authority(basis),
            "publication retirement must not depend on a changed root"
        );
        unchanged(&mut rig, &before);
    }
}

#[gpui::test]
fn resolved_graph_navigation_refuses_departed_and_returned_route_before_flush(
    cx: &mut TestAppContext,
) {
    for return_to_world in [false, true] {
        let (mut rig, _) = selected(cx);
        let before = queue_then(&mut rig, View::Page, |root, cx| {
            root.update(cx, |root, cx| {
                root.dispatch(Intent::Navigate(Route::Orbit(OrbitRoute::Home)), cx);
                if return_to_world {
                    root.dispatch(Intent::Navigate(Route::World), cx);
                }
            });
        });
        unchanged(&mut rig, &before);
    }
}

#[gpui::test]
fn graph_publication_does_not_retire_an_unrelated_add_dialog_visit(cx: &mut TestAppContext) {
    let (mut rig, _) = selected(cx);
    let package = rig.graph.store.read_with(rig.cx, |store, _| {
        store
            .graph_focus()
            .expect("focus")
            .indexed
            .as_ref()
            .expect("exact row")
            .0
            .clone()
    });
    rig.go(Intent::OpenAddProject);
    let root = rig.graph.root.clone();
    rig.cx.update(|_, cx| {
        let owner = root.entity_id();
        let before = root
            .read(cx)
            .add_project_visit(owner, cx)
            .expect("current Add dialog");
        root.update(cx, |root, cx| {
            root.packages_published(&BTreeSet::from([package]), cx)
        });
        assert_eq!(
            root.read(cx).add_project_visit(owner, cx),
            Some(before),
            "graph retirement must preserve the independent local dialog's existing admission"
        );
    });
}
