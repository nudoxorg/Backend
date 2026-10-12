//! Mounted regions renew their own visible observations at unchanged authority.

use super::{Fixture, PACKAGE, Rig, package, page, registry_dossier, rig_with_reads};
use crate::model::pages::{
    DeclRef, DocFragment, GapReason, IndexedPackage, Known, OutlinePosition, PackageRef, PageKey,
    PageValue, ReadFailure, Readiness, SearchQuery, SignatureText, SymbolRef,
};
use crate::navigation::{Coordinate, Intent, PackageLane, PackageRoute, Route, SymbolRoute, View};
use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use backend_library::DeclarationKind;
use gpui::{AppContext as _, TestAppContext};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const ERROR_TYPE: &str = "/fixture/present::errors.go:1::ErrorHandling";
const CONTINUE: &str = "/fixture/present::errors.go:2::ContinueOnError";

struct RecoveryReads(Arc<AtomicUsize>);
impl PageReader for RecoveryReads {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        if matches!(request, ReadRequest::Symbol(symbol) if *symbol == super::symbol("RelationLabel")) {
            let read = self.0.fetch_add(1, Ordering::SeqCst);
            if read < 2 {
                return Err(ReadFailure::Fault(crate::core::ErrorValue::new(
                    crate::core::FaultCode::Transport, "actual read fixture refused")));
            }
        }
        Fixture.read(request, context)
    }
}

#[gpui::test]
fn retained_native_read_retry_cannot_replace_a_new_read_and_fresh_failure_recovers(cx: &mut TestAppContext) {
    let reads = Arc::new(AtomicUsize::new(0));
    let client = Arc::clone(&reads);
    let mut rig = rig_with_reads(cx, Some(super::page_route("RelationLabel")), 1440.0, 900.0,
        ReadPool::start(1, move |_| RecoveryReads(Arc::clone(&client))).expect("pool"));
    let shell = rig.shell.clone();
    rig.cx.update(|window, cx| {
        window.replace_root(cx, |window, cx| gpui_component::Root::new(shell, window, cx).bordered(false));
        window.set_a11y_forced(true);
    });
    rig.go(Intent::SetMotion(crate::model::MotionPreference::Reduced));
    let symbol = super::symbol("RelationLabel");
    let key = PageKey::Symbol(symbol.clone());
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    let (target, bounds) = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx)).placed()
        .into_iter().find(|(target, _)| target.id.starts_with("retry-")).expect("actual failure recovery");
    let native = rig.cx.update(|window, _| window.a11y_tree().expect("failure AX").nodes.iter()
        .find(|(_, node)| node.role() == gpui::Role::Button && node.label() == Some("Try again")
            && node.supports_action(gpui::AccessibleAction::Click)).map(|(id, _)| *id).expect("native retry"));
    let frame = rig.cx.update(|window, _| window.a11y_frame_number());
    let before = rig.graph.store.read_with(rig.cx, |store, _| store.stats().submitted);
    let focused = rig.cx.update(|window, cx| {
        window.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
            position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left,
            click_count: 1, first_mouse: false,
        }), cx);
        let focused = window.focused(cx);
        // Another current caller starts a real pool read while this failure's
        // native dispatch tree and held press are still committed.
        rig.graph.store.update(cx, |store, cx| store.retry(key.clone(), cx));
        assert!(!target.action.admits(cx));
        target.action.run(window, cx);
        window.dispatch_event(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
            position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left, click_count: 1,
        }), cx);
        for key in ["enter", "space"] {
            let keystroke = gpui::Keystroke::parse(key).expect("retry key");
            window.dispatch_event(gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                keystroke: keystroke.clone(), is_held: false, prefer_character_input: false,
            }), cx);
            window.dispatch_event(gpui::PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke }), cx);
        }
        window.simulate_a11y_action(gpui::accesskit::ActionRequest {
            action: gpui::AccessibleAction::Click, target_tree: gpui::accesskit::TreeId::ROOT, target_node: native, data: None,
        }, cx);
        assert_eq!(rig.graph.store.read(cx).stats().submitted, before + 1);
        assert_eq!(window.focused(cx), focused);
        assert_eq!(window.a11y_frame_number(), frame);
        focused
    });
    assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.stats().submitted), before + 1);
    assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), focused);
    assert_eq!(rig.cx.update(|window, _| window.a11y_frame_number()), frame);
    rig.settle();
    assert_eq!(reads.load(Ordering::SeqCst), 2);
    rig.cx.update(|window, cx| target.action.run(window, cx));
    rig.cx.run_until_parked();
    assert_eq!(reads.load(Ordering::SeqCst), 2, "a new identical failure does not revive the old callback");
    let (_, bounds) = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx)).placed()
        .into_iter().find(|(target, _)| target.id.starts_with("retry-")).expect("new failure retry");
    rig.cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    rig.settle();
    assert_eq!(reads.load(Ordering::SeqCst), 3, "fresh native recovery reaches the real worker without Back");
    assert!(rig.graph.store.read_with(rig.cx, |store, _| store.symbol(&symbol).is_loaded()));
    rig.cx.update(|window, cx| target.action.run(window, cx));
    rig.cx.run_until_parked();
    assert_eq!(reads.load(Ordering::SeqCst), 3, "old failure cannot replace the successful current reading");
}

/// Two ordinary pool faults can land between paints. Keeping the same native
/// focus through their replacement must not lend the old held key to the new
/// retry callback; no intermediate Pending paint may cancel it for this law.
#[gpui::test]
fn coalesced_read_failure_repaint_cannot_inherit_a_held_retry_key(cx: &mut TestAppContext) {
    for key_name in ["enter", "space"] {
        let reads = Arc::new(AtomicUsize::new(0));
        let client = Arc::clone(&reads);
        let mut rig = rig_with_reads(cx, Some(super::page_route("RelationLabel")), 1440.0, 900.0,
            ReadPool::start(1, move |_| RecoveryReads(Arc::clone(&client))).expect("pool"));
        let shell = rig.shell.clone();
        rig.cx.update(|window, cx| {
            window.replace_root(cx, |window, cx| gpui_component::Root::new(shell, window, cx).bordered(false));
            window.set_a11y_forced(true);
        });
        rig.go(Intent::SetMotion(crate::model::MotionPreference::Reduced));
        let symbol = super::symbol("RelationLabel");
        let key = PageKey::Symbol(symbol.clone());
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        let targets = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx));
        let (target, _) = targets.placed().into_iter().find(|(target, _)| target.id.starts_with("retry-"))
            .expect("actual first failure retry");
        let focus = rig.cx.update(|window, cx| {
            let focus = targets.native_handle(&target.id, cx);
            focus.focus(window, cx);
            focus
        });
        rig.settle();
        let before = rig.graph.store.read_with(rig.cx, |store, _| store.stats().submitted);
        let (armed_frame, focus_epoch, old_stamp) = rig.cx.update(|window, cx| {
            assert!(focus.is_focused(window) && window.is_focus_handle_mounted(&focus));
            window.dispatch_event(gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                keystroke: gpui::Keystroke::parse(key_name).expect("held retry key"),
                is_held: false, prefer_character_input: false,
            }), cx);
            let armed = window.a11y_frame_number();
            let stamp = rig.graph.store.read(cx).stamp(&key);
            rig.graph.store.update(cx, |store, cx| store.retry(key.clone(), cx));
            assert!(!target.action.admits(cx));
            // Hold the UI effect boundary while the real worker publishes its
            // terminal. Public drain is the ordinary landing implementation;
            // neither the Pending slot nor its missing button is painted.
            crate::runtime::wait::until("the second real failure is ready to land", || {
                rig.graph.store.read(cx).pool_activity().undelivered > 0
            });
            assert_eq!(rig.graph.store.update(cx, |store, cx| store.drain(cx)), 1);
            assert_eq!(reads.load(Ordering::SeqCst), 2);
            assert_ne!(rig.graph.store.read(cx).stamp(&key), stamp);
            assert!(!rig.graph.store.read(cx).is_loading(&key));
            assert_eq!(window.a11y_frame_number(), armed, "the worker completed without a Pending paint");
            assert!(focus.is_focused(window));
            (armed, window.focus_epoch(), stamp)
        });
        rig.settle();
        let replacement_frame = rig.cx.update(|window, cx| {
            let targets = rig.shell.read(cx).reader_targets(cx);
            assert_eq!(targets.native_handle(&target.id, cx), focus, "the replacement keeps the exact native handle");
            let (fresh, _) = targets.placed().into_iter().find(|(row, _)| row.id == target.id)
                .expect("same represented retry after the new real fault");
            assert!(fresh.action.admits(cx) && !target.action.admits(cx));
            assert_ne!(rig.graph.store.read(cx).stamp(&key), old_stamp);
            assert!(focus.is_focused(window) && window.is_focus_handle_mounted(&focus));
            assert_eq!(window.focus_epoch(), focus_epoch, "focus loss cannot cancel this pending key");
            assert!(window.a11y_frame_number() > armed_frame, "the new callback has actually committed");
            window.a11y_frame_number()
        });
        rig.cx.update(|window, cx| {
            window.dispatch_event(gpui::PlatformInput::KeyUp(gpui::KeyUpEvent {
                keystroke: gpui::Keystroke::parse(key_name).expect("old retry release"),
            }), cx);
            assert_eq!(window.a11y_frame_number(), replacement_frame);
            assert_eq!(window.focus_epoch(), focus_epoch);
            assert_eq!(rig.graph.store.read(cx).stats().submitted, before + 1,
                "the replacement failure must not inherit the old held {key_name}");
        });
        rig.cx.run_until_parked();
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        rig.cx.update(|window, cx| {
            let keystroke = gpui::Keystroke::parse(key_name).expect("fresh retry key");
            window.dispatch_event(gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                keystroke: keystroke.clone(), is_held: false, prefer_character_input: false,
            }), cx);
            window.dispatch_event(gpui::PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke }), cx);
        });
        rig.settle();
        assert_eq!(reads.load(Ordering::SeqCst), 3, "a fresh native retry reaches the actual worker");
        assert!(rig.graph.store.read_with(rig.cx, |store, _| store.symbol(&symbol).is_loaded()));
    }
}

#[gpui::test]
fn identical_owner_failure_retires_generic_native_retry_before_watcher_and_fresh_offline_retry_works(cx: &mut TestAppContext) {
    let gate = crate::runtime::owner::OwnerGate::starting();
    let mut rig = super::rig_with_engine_gate_at_root(cx, None, 1440.0, 900.0,
        ReadPool::start(1, |_| Fixture).expect("pool"), super::RootOnly, Some(gate.clone()), crate::core::VersionedRoot::unserved());
    let shell = rig.shell.clone();
    rig.cx.update(|window, cx| {
        window.replace_root(cx, |window, cx| gpui_component::Root::new(shell, window, cx).bordered(false));
        window.set_a11y_forced(true);
    });
    let failure = crate::runtime::owner::OwnerFault::Host("owner unavailable".into());
    gate.publish(crate::runtime::owner::OwnerState::Failed(failure.clone()));
    rig.draw(); rig.repaint();
    let (target, bounds) = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx)).placed()
        .into_iter().find(|(target, _)| target.id.starts_with("retry-")).expect("offline recovery");
    let frame = rig.cx.update(|window, _| window.a11y_frame_number());
    let native = rig.cx.update(|window, _| window.a11y_tree().expect("offline AX").nodes.iter()
        .find(|(_, node)| node.role() == gpui::Role::Button && node.label() == Some("Try again")
            && node.supports_action(gpui::AccessibleAction::Click)).map(|(id, _)| *id).expect("offline Retry Click"));
    let stamp = rig.graph.store.read_with(rig.cx, |store, _| store.stamp(&PageKey::Orbit));
    let retry = rig.graph.store.read_with(rig.cx, |store, _| store.current_owner_retry().expect("rendered failure token"));
    let focused = rig.cx.update(|window, cx| {
        window.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
            position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left,
            click_count: 1, first_mouse: false,
        }), cx);
        let focused = window.focused(cx);
        gate.publish(crate::runtime::owner::OwnerState::Failed(failure.clone()));
        assert_eq!(rig.graph.store.read(cx).stamp(&PageKey::Orbit), stamp, "identical words do not provide a stamp wake");
        assert_ne!(rig.graph.store.read(cx).current_owner_retry(), Some(retry.clone()));
        assert!(!target.action.admits(cx));
        target.action.run(window, cx);
        window.dispatch_event(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
            position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left, click_count: 1,
        }), cx);
        for key in ["enter", "space"] {
            let keystroke = gpui::Keystroke::parse(key).expect("offline retry key");
            window.dispatch_event(gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                keystroke: keystroke.clone(), is_held: false, prefer_character_input: false,
            }), cx);
            window.dispatch_event(gpui::PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke }), cx);
        }
        window.simulate_a11y_action(gpui::accesskit::ActionRequest {
            action: gpui::AccessibleAction::Click, target_tree: gpui::accesskit::TreeId::ROOT, target_node: native, data: None,
        }, cx);
        assert!(matches!(gate.state(), crate::runtime::owner::OwnerState::Failed(_)), "old failure cannot restart a newer failure");
        assert_eq!(window.focused(cx), focused);
        assert_eq!(window.a11y_frame_number(), frame);
        focused
    });
    assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), focused);
    assert_eq!(rig.cx.update(|window, _| window.a11y_frame_number()), frame);
    assert!(matches!(gate.state(), crate::runtime::owner::OwnerState::Failed(_)));
    rig.settle();
    let (_, bounds) = rig.shell.read_with(rig.cx, |shell, cx| shell.reader_targets(cx)).placed()
        .into_iter().find(|(target, _)| target.id.starts_with("retry-")).expect("renewed offline recovery");
    rig.cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    assert!(matches!(gate.state(), crate::runtime::owner::OwnerState::Starting), "fresh native recovery restarts the captured current failure without a Serving owner");
}

#[gpui::test]
fn a_new_native_failure_paint_cannot_submit_through_unobserved_starting_or_ready_owner(cx: &mut TestAppContext) {
    for returned in [false, true] {
        let at = crate::core::VersionedRoot::synthetic(backend_library::view_state_root(&[("shell".into(), "tests".into())]), 4);
        let gate = crate::runtime::owner::OwnerGate::ready(at, crate::model::ServiceMode::Attached);
        let reads = Arc::new(AtomicUsize::new(0));
        let client = Arc::clone(&reads);
        let mut rig = super::rig_with_engine_gate_at_root(cx, Some(super::page_route("RelationLabel")), 1440.0, 900.0,
            ReadPool::start(1, move |_| RecoveryReads(Arc::clone(&client))).expect("pool"), super::RootOnly, Some(gate.clone()), at);
        let shell = rig.shell.clone();
        rig.cx.update(|window, cx| {
            window.replace_root(cx, |window, cx| gpui_component::Root::new(shell, window, cx).bordered(false));
            window.set_a11y_forced(true);
        });
        rig.go(Intent::SetMotion(crate::model::MotionPreference::Reduced));
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        let key = PageKey::Symbol(super::symbol("RelationLabel"));
        rig.cx.update(|window, cx| {
            let submitted = rig.graph.store.read(cx).stats().submitted;
            let old_frame = window.a11y_frame_number();
            gate.publish(crate::runtime::owner::OwnerState::Starting);
            if returned { gate.publish(crate::runtime::owner::OwnerState::Ready { key: at, mode: crate::model::ServiceMode::Attached }); }
            assert!(rig.graph.store.read(cx).capture_page_retry(&key).is_none());
            // A real viewport change commits another native dispatch tree
            // while this effect still withholds the async owner watcher.
            window.resize(gpui::size(gpui::px(1000.0), gpui::px(900.0)));
            window.bounds_changed(cx);
            window.draw(cx).clear(cx);
            assert!(window.a11y_frame_number() > old_frame, "the law uses newly rendered handlers, not just the old tree");
            let (target, bounds) = rig.shell.read(cx).reader_targets(cx).placed().into_iter()
                .find(|(target, _)| target.id.starts_with("retry-")).expect("new failure recovery paint");
            assert!(!target.action.admits(cx), "absence is not a current Starting or Serving receipt");
            let native = window.a11y_tree().expect("new failure AX").nodes.iter()
                .find(|(_, node)| node.role() == gpui::Role::Button && node.label() == Some("Try again")
                    && node.supports_action(gpui::AccessibleAction::Click)).map(|(id, _)| *id).expect("new native retry");
            let focused = window.focused(cx);
            target.action.run(window, cx);
            window.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
                position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left,
                click_count: 1, first_mouse: false,
            }), cx);
            window.dispatch_event(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
                position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left, click_count: 1,
            }), cx);
            for key in ["enter", "space"] {
                let keystroke = gpui::Keystroke::parse(key).expect("unadmitted retry key");
                window.dispatch_event(gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                    keystroke: keystroke.clone(), is_held: false, prefer_character_input: false,
                }), cx);
                window.dispatch_event(gpui::PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke }), cx);
            }
            window.simulate_a11y_action(gpui::accesskit::ActionRequest {
                action: gpui::AccessibleAction::Click, target_tree: gpui::accesskit::TreeId::ROOT, target_node: native, data: None,
            }, cx);
            assert_eq!(window.focused(cx), focused, "unadmitted current paint cannot take native focus");
            assert_eq!(rig.graph.store.read(cx).stats().submitted, submitted, "no pool read may use a stale Serving phase");
        });
        rig.cx.run_until_parked();
    }
}

/// Refuses two actual pool reads of the exact Find query, then returns its
/// prepared empty result through the ordinary Browse landing path.
struct FindRecoveryReads(Arc<AtomicUsize>);
impl PageReader for FindRecoveryReads {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        let ReadRequest::Browse(crate::model::browse::BrowseKey::Find(query)) = request else {
            return Fixture.read(request, context);
        };
        assert_eq!(query.text.as_ref(), "RelationLabel", "the law counts the actual represented query");
        if self.0.fetch_add(1, Ordering::SeqCst) < 2 {
            return Err(ReadFailure::Fault(crate::core::ErrorValue::new(
                crate::core::FaultCode::Transport, "actual Find read fixture refused")));
        }
        let answers = Known::Known(super::SearchPage {
            query: Arc::clone(&query.text), rows: Arc::from([]),
            coverage: backend_present::CoverageLine::new(&[], Some(0)), next: None,
        });
        let package_coverage = Known::Known(());
        let prepared = Arc::new(crate::runtime::browse_views::prepare_find(
            query.text.as_ref(), &answers, &[], &package_coverage,
        ));
        Ok(PageValue::Browse(crate::model::browse::BrowseValue::Find(Arc::new(
            crate::model::browse::FindModel { answers, packages: Arc::from([]), package_coverage, prepared },
        ))))
    }
}

fn find_retry_native(rig: &mut Rig) -> (gpui::accesskit::NodeId, gpui::Bounds<gpui::Pixels>) {
    rig.cx.update(|window, _| {
        let native = window.a11y_tree().expect("Find failure AX").nodes.iter()
            .find(|(_, node)| node.role() == gpui::Role::Button && node.label() == Some("Retry current Find query")
                && node.supports_action(gpui::AccessibleAction::Click)).map(|(id, _)| *id).expect("native Find recovery");
        (native, window.a11y_node_bounds(native).expect("actual mounted native Retry bounds"))
    })
}

fn status_retry_native(rig: &mut Rig) -> (gpui::accesskit::NodeId, gpui::Bounds<gpui::Pixels>) {
    rig.cx.update(|window, _| {
        let tree: serde_json::Value = serde_json::from_str(&window.debug_a11y_tree_json().expect("status AX")).expect("native tree");
        let nodes: Vec<_> = tree["nodes"].as_object().expect("native nodes").values().filter(|node|
            node["element_id"].as_str().is_some_and(|id| id.starts_with("Name(\"status-retry"))
                && node["aria"]["role"] == "Button" && node["aria"]["label"] == "Try again"
                && node["aria"]["on_action"].as_array().is_some_and(|actions| actions.iter().any(|action| action == "Click"))
        ).collect();
        assert_eq!(nodes.len(), 1, "one exact native Status retry receiver");
        let native = gpui::accesskit::NodeId(nodes[0]["accesskit_id"].as_str().expect("native ID").parse().expect("node ID"));
        (native, window.a11y_node_bounds(native).expect("native Status retry bounds"))
    })
}

/// Keep the control's actual implicit Touch focus while a second identical
/// failure lands through the pool before Pending can paint. A new callback
/// must not inherit the previous receipt's pending native key press.
#[gpui::test]
fn coalesced_find_and_status_failures_cannot_inherit_held_retry_keys(cx: &mut TestAppContext) {
    for find in [true, false] {
        for key_name in ["enter", "space"] {
            let reads = Arc::new(AtomicUsize::new(0));
            let client = Arc::clone(&reads);
            let (route, key, pool) = if find {
                let query = SearchQuery::new("RelationLabel", 200).expect("exact query");
                (Route::Orbit(crate::navigation::OrbitRoute::Browse(crate::navigation::BrowseRoute::Find(query.clone()))),
                    PageKey::Browse(crate::model::browse::BrowseKey::Find(query)),
                    ReadPool::start(1, move |_| FindRecoveryReads(Arc::clone(&client))).expect("Find pool"))
            } else {
                (super::page_route("RelationLabel"), PageKey::Symbol(super::symbol("RelationLabel")),
                    ReadPool::start(1, move |_| RecoveryReads(Arc::clone(&client))).expect("symbol pool"))
            };
            let mut rig = rig_with_reads(cx, Some(route.clone()), 1440.0, 900.0, pool);
            let shell = rig.shell.clone();
            rig.cx.update(|window, cx| {
                window.replace_root(cx, |window, cx| gpui_component::Root::new(shell, window, cx).bordered(false));
                window.set_a11y_forced(true);
            });
            rig.go(Intent::SetMotion(crate::model::MotionPreference::Reduced));
            if !find {
                rig.graph.store.update(rig.cx, |store, cx| {
                    let snapshot = store.snapshot();
                    store.set_notice(Some(crate::runtime::graph_focus::Notice {
                        visit: snapshot.route().clone(), root: snapshot.key(),
                        message: "The represented symbol read failed.".into(), retry: Some(key.clone()),
                    }), cx);
                });
                rig.settle();
            }
            assert_eq!(reads.load(Ordering::SeqCst), 1);
            let (native, _) = if find { find_retry_native(&mut rig) } else { status_retry_native(&mut rig) };
            // Use the real native node's focus mapping, not a supplied test
            // handle that could hide a replacement of implicit Touch state.
            let focus = rig.cx.update(|window, cx| {
                window.simulate_a11y_action(gpui::accesskit::ActionRequest {
                    action: gpui::AccessibleAction::Focus, target_tree: gpui::accesskit::TreeId::ROOT,
                    target_node: native, data: None,
                }, cx);
                window.focused(cx).expect("actual native recovery focus")
            });
            rig.settle();
            rig.cx.update(|window, _| {
                let tree: serde_json::Value = serde_json::from_str(&window.debug_a11y_tree_json().expect("focused recovery AX")).expect("native tree");
                let focused = tree["accesskit_focus"].as_str().expect("native focus node");
                let receiver = &tree["nodes"][focused];
                assert_eq!(receiver["aria"]["role"], "Button", "native focus belongs to recovery, not the query field");
                assert_eq!(receiver["aria"]["label"], if find { "Retry current Find query" } else { "Try again" });
                if !find {
                    assert!(receiver["element_id"].as_str().is_some_and(|id| id.starts_with("Name(\"status-retry")),
                        "native Status recovery, not the duplicate body retry, owns this key");
                }
            });
            let before = rig.graph.store.read_with(rig.cx, |store, _| store.stats().submitted);
            let notice = rig.graph.store.read_with(rig.cx, |store, _| store.notice().cloned());
            let scope = rig.shell.read_with(rig.cx, |shell, _| shell.local_activation_scope());
            let (armed, epoch, stamp) = rig.cx.update(|window, cx| {
                assert!(focus.is_focused(window) && window.is_focus_handle_mounted(&focus));
                window.dispatch_event(gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                    keystroke: gpui::Keystroke::parse(key_name).expect("held retry key"),
                    is_held: false, prefer_character_input: false,
                }), cx);
                let frame = window.a11y_frame_number();
                let stamp = rig.graph.store.read(cx).stamp(&key);
                rig.graph.store.update(cx, |store, cx| store.retry(key.clone(), cx));
                crate::runtime::wait::until("the replacement recovery fault is ready", ||
                    rig.graph.store.read(cx).pool_activity().undelivered > 0);
                assert_eq!(rig.graph.store.update(cx, |store, cx| store.drain(cx)), 1);
                assert_eq!(reads.load(Ordering::SeqCst), 2);
                assert_ne!(rig.graph.store.read(cx).stamp(&key), stamp);
                assert_eq!(window.a11y_frame_number(), frame, "neither Pending nor the new failure painted inside this effect");
                (frame, window.focus_epoch(), stamp)
            });
            rig.settle();
            let (fresh_native, _) = if find { find_retry_native(&mut rig) } else { status_retry_native(&mut rig) };
            let frame = rig.cx.update(|window, cx| {
                assert_eq!(window.focused(cx), Some(focus.clone()), "the replacement retains the exact implicit Touch focus handle");
                assert!(window.is_focus_handle_mounted(&focus));
                assert_eq!(window.focus_epoch(), epoch);
                assert_eq!(rig.shell.read(cx).local_activation_scope(), scope, "resource-only movement does not retire the host scope");
                assert_ne!(rig.graph.store.read(cx).stamp(&key), stamp);
                assert_eq!(rig.graph.store.read(cx).notice().cloned(), notice);
                // Keyboard activation follows the persistent focus handle and
                // focus epoch. The ordinary pressed motion may change bounds.
                let tree: serde_json::Value = serde_json::from_str(&window.debug_a11y_tree_json().expect("replacement recovery AX")).expect("native tree");
                let native_focus = gpui::accesskit::NodeId(tree["accesskit_focus"].as_str().expect("replacement focus node").parse().expect("native focus ID"));
                assert_eq!(native_focus, fresh_native, "the replacement recovery receiver still owns native focus");
                assert!(window.a11y_frame_number() > armed, "the new receipt's control really committed while the key was held");
                window.a11y_frame_number()
            });
            rig.cx.update(|window, cx| {
                window.dispatch_event(gpui::PlatformInput::KeyUp(gpui::KeyUpEvent {
                    keystroke: gpui::Keystroke::parse(key_name).expect("old release"),
                }), cx);
                assert_eq!(window.a11y_frame_number(), frame);
                assert_eq!(window.focus_epoch(), epoch);
                assert_eq!(rig.graph.store.read(cx).stats().submitted, before + 1,
                    "new Find/Status recovery cannot adopt the earlier receipt's held {key_name}");
            });
            rig.cx.run_until_parked();
            assert_eq!(reads.load(Ordering::SeqCst), 2);
            assert_eq!(rig.route(), route);
            rig.cx.update(|window, cx| {
                let key = gpui::Keystroke::parse(key_name).expect("fresh retry");
                window.dispatch_event(gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                    keystroke: key.clone(), is_held: false, prefer_character_input: false,
                }), cx);
                window.dispatch_event(gpui::PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke: key }), cx);
            });
            rig.settle();
            assert_eq!(reads.load(Ordering::SeqCst), 3, "fresh native recovery reaches the actual worker");
            assert_eq!(rig.route(), route);
            assert!(match &key {
                PageKey::Browse(key) => rig.graph.store.read_with(rig.cx, |store, _| store.pages().browse(key).loaded_value().is_some()),
                PageKey::Symbol(symbol) => rig.graph.store.read_with(rig.cx, |store, _| store.symbol(symbol).is_loaded()),
                _ => unreachable!("only the exact represented recovery pages"),
            });
        }
    }
}

#[gpui::test]
fn retained_native_find_retry_cannot_adopt_a_later_same_query_read_and_fresh_failure_recovers(cx: &mut TestAppContext) {
    let reads = Arc::new(AtomicUsize::new(0));
    let client = Arc::clone(&reads);
    let query = SearchQuery::new("RelationLabel", 200).expect("query");
    let key = crate::model::browse::BrowseKey::Find(query.clone());
    let route = Route::Orbit(crate::navigation::OrbitRoute::Browse(crate::navigation::BrowseRoute::Find(query)));
    let mut rig = rig_with_reads(cx, Some(route.clone()), 1440.0, 900.0,
        ReadPool::start(1, move |_| FindRecoveryReads(Arc::clone(&client))).expect("pool"));
    let shell = rig.shell.clone();
    rig.cx.update(|window, cx| {
        window.replace_root(cx, |window, cx| gpui_component::Root::new(shell, window, cx).bordered(false));
        window.set_a11y_forced(true);
    });
    rig.go(Intent::SetMotion(crate::model::MotionPreference::Reduced));
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    let (native, bounds) = find_retry_native(&mut rig);
    let frame = rig.cx.update(|window, _| window.a11y_frame_number());
    let before = rig.graph.store.read_with(rig.cx, |store, _| store.stats().submitted);
    let focused = rig.cx.update(|window, cx| {
        let before_press = window.focused(cx);
        window.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
            position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left,
            click_count: 1, first_mouse: false,
        }), cx);
        let focused = window.focused(cx);
        assert!(focused.is_some(), "the current native recovery receives the held press");
        assert_ne!(focused, before_press, "current Retry accepts native focus from the query field");
        rig.graph.store.update(cx, |store, cx| store.retry(PageKey::Browse(key.clone()), cx));
        window.dispatch_event(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
            position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left, click_count: 1,
        }), cx);
        for key in ["enter", "space"] {
            let keystroke = gpui::Keystroke::parse(key).expect("Find Retry key");
            window.dispatch_event(gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                keystroke: keystroke.clone(), is_held: false, prefer_character_input: false,
            }), cx);
            window.dispatch_event(gpui::PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke }), cx);
        }
        window.simulate_a11y_action(gpui::accesskit::ActionRequest {
            action: gpui::AccessibleAction::Click, target_tree: gpui::accesskit::TreeId::ROOT, target_node: native, data: None,
        }, cx);
        assert_eq!(rig.graph.store.read(cx).stats().submitted, before + 1, "old native input cannot replace the newer pool read");
        assert_eq!(window.focused(cx), focused);
        let query_focus = before_press.expect("mounted native Find query focus");
        query_focus.focus(window, cx);
        window.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
            position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left,
            click_count: 1, first_mouse: false,
        }), cx);
        window.dispatch_event(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
            position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left, click_count: 1,
        }), cx);
        let focused = window.focused(cx);
        assert_eq!(focused, Some(query_focus), "a newly attempted stale press cannot steal native query focus");
        assert_eq!(window.a11y_frame_number(), frame, "the admission runs against retained native handlers before watcher redraw");
        focused
    });
    assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.stats().submitted), before + 1);
    assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), focused);
    assert_eq!(rig.cx.update(|window, _| window.a11y_frame_number()), frame);
    rig.settle();
    assert_eq!(reads.load(Ordering::SeqCst), 2);
    assert_eq!(rig.route(), route, "identical failure keeps the exact query visit mounted");
    let (_, bounds) = find_retry_native(&mut rig);
    rig.cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    rig.settle();
    assert_eq!(reads.load(Ordering::SeqCst), 3, "fresh Find recovery reaches the real worker without Back");
    assert_eq!(rig.route(), route);
    assert!(rig.graph.store.read_with(rig.cx, |store, _| store.pages().browse(&key).loaded_value().is_some()));
}

#[gpui::test]
fn identical_owner_failure_retires_native_find_retry_before_watcher_and_fresh_offline_retry_works(cx: &mut TestAppContext) {
    let at = crate::core::VersionedRoot::synthetic(backend_library::view_state_root(&[("shell".into(), "tests".into())]), 4);
    let gate = crate::runtime::owner::OwnerGate::ready(at, crate::model::ServiceMode::Attached);
    let reads = Arc::new(AtomicUsize::new(0));
    let client = Arc::clone(&reads);
    let query = SearchQuery::new("RelationLabel", 200).expect("query");
    let key = PageKey::Browse(crate::model::browse::BrowseKey::Find(query.clone()));
    let route = Route::Orbit(crate::navigation::OrbitRoute::Browse(crate::navigation::BrowseRoute::Find(query)));
    let mut rig = super::rig_with_engine_gate_at_root(cx, Some(route.clone()), 1440.0, 900.0,
        ReadPool::start(1, move |_| FindRecoveryReads(Arc::clone(&client))).expect("pool"), super::RootOnly, Some(gate.clone()), at);
    let shell = rig.shell.clone();
    rig.cx.update(|window, cx| {
        window.replace_root(cx, |window, cx| gpui_component::Root::new(shell, window, cx).bordered(false));
        window.set_a11y_forced(true);
    });
    rig.go(Intent::SetMotion(crate::model::MotionPreference::Reduced));
    let failure = crate::runtime::owner::OwnerFault::Host("Find owner unavailable".into());
    gate.publish(crate::runtime::owner::OwnerState::Failed(failure.clone()));
    rig.settle();
    let (native, bounds) = find_retry_native(&mut rig);
    let stamp = rig.graph.store.read_with(rig.cx, |store, _| store.stamp(&key));
    let retry = rig.graph.store.read_with(rig.cx, |store, _| store.current_owner_retry().expect("painted failure token"));
    let submitted = rig.graph.store.read_with(rig.cx, |store, _| store.stats().submitted);
    let frame = rig.cx.update(|window, _| window.a11y_frame_number());
    let focused = rig.cx.update(|window, cx| {
        let before_press = window.focused(cx);
        window.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
            position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left,
            click_count: 1, first_mouse: false,
        }), cx);
        let focused = window.focused(cx);
        assert_ne!(focused, before_press, "current offline Retry accepts native focus from the query field");
        gate.publish(crate::runtime::owner::OwnerState::Failed(failure.clone()));
        assert_eq!(rig.graph.store.read(cx).stamp(&key), stamp, "same query and identical fault words cannot stand in for retry identity");
        assert_ne!(rig.graph.store.read(cx).current_owner_retry(), Some(retry.clone()));
        window.dispatch_event(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
            position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left, click_count: 1,
        }), cx);
        for key in ["enter", "space"] {
            let keystroke = gpui::Keystroke::parse(key).expect("offline Find Retry key");
            window.dispatch_event(gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                keystroke: keystroke.clone(), is_held: false, prefer_character_input: false,
            }), cx);
            window.dispatch_event(gpui::PlatformInput::KeyUp(gpui::KeyUpEvent { keystroke }), cx);
        }
        window.simulate_a11y_action(gpui::accesskit::ActionRequest {
            action: gpui::AccessibleAction::Click, target_tree: gpui::accesskit::TreeId::ROOT, target_node: native, data: None,
        }, cx);
        assert!(matches!(gate.state(), crate::runtime::owner::OwnerState::Failed(_)), "old native callback cannot adopt a newer identical owner failure");
        assert_eq!(rig.graph.store.read(cx).stats().submitted, submitted);
        assert_eq!(window.focused(cx), focused);
        let query_focus = before_press.expect("mounted native Find query focus");
        query_focus.focus(window, cx);
        window.dispatch_event(gpui::PlatformInput::MouseDown(gpui::MouseDownEvent {
            position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left,
            click_count: 1, first_mouse: false,
        }), cx);
        window.dispatch_event(gpui::PlatformInput::MouseUp(gpui::MouseUpEvent {
            position: bounds.center(), modifiers: gpui::Modifiers::none(), button: gpui::MouseButton::Left, click_count: 1,
        }), cx);
        let focused = window.focused(cx);
        assert_eq!(focused, Some(query_focus), "a newly attempted stale press cannot steal native query focus");
        assert_eq!(window.a11y_frame_number(), frame);
        focused
    });
    assert_eq!(rig.cx.update(|window, cx| window.focused(cx)), focused);
    assert_eq!(rig.cx.update(|window, _| window.a11y_frame_number()), frame);
    assert!(matches!(gate.state(), crate::runtime::owner::OwnerState::Failed(_)));
    rig.settle();
    let (_, bounds) = find_retry_native(&mut rig);
    rig.cx.simulate_click(bounds.center(), gpui::Modifiers::none());
    assert!(matches!(gate.state(), crate::runtime::owner::OwnerState::Starting), "fresh exact offline recovery restarts locally without a Serving owner");
    assert_eq!(rig.graph.store.read_with(rig.cx, |store, _| store.stats().submitted), submitted, "offline restart does not force a read through an unadmitted owner");
    assert_eq!(rig.route(), route);
}

#[derive(Default)]
struct Observations {
    published: AtomicBool,
    reads: Mutex<BTreeMap<PageKey, usize>>,
}

impl Observations {
    fn count(&self, key: &PageKey) -> usize {
        self.reads.lock().expect("read counts").get(key).copied().unwrap_or(0)
    }
}

struct PublicationFixture(Arc<Observations>);

impl PageReader for PublicationFixture {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        let key = match request {
            ReadRequest::Orbit => PageKey::Orbit,
            ReadRequest::Package(package) => PageKey::Package(package.clone()),
            ReadRequest::Symbol(symbol) => PageKey::Symbol(symbol.clone()),
            _ => return Fixture.read(request, context),
        };
        *self.0.reads.lock().expect("read counts").entry(key).or_default() += 1;
        let published = self.0.published.load(Ordering::Acquire);
        match request {
            ReadRequest::Orbit => {
                let PageValue::Orbit(mut model) = Fixture.read(request, context)? else {
                    unreachable!("fixture Orbit family")
                };
                model.indexed = Known::Known(Arc::from([IndexedPackage {
                    package: package(),
                    name: Arc::from(if published { "Published library" } else { "Earlier library" }),
                    readiness: Readiness::Ready,
                    verified_registry_release: None,
                }]));
                Ok(PageValue::Orbit(model))
            }
            ReadRequest::Package(package) if !package.is_local() => {
                Ok(PageValue::Package(registry_dossier(package)))
            }
            ReadRequest::Symbol(symbol) if [ERROR_TYPE, CONTINUE].contains(&symbol.as_str()) => {
                let is_type = symbol.as_str() == ERROR_TYPE;
                let kind = if is_type { DeclarationKind::Type } else { DeclarationKind::Constant };
                let mut value = page(symbol.identity().name());
                value.identity = DeclRef::from_label(symbol.as_str(), None, Some(kind), None).expect("Go declaration");
                value.members = Known::unknown(GapReason::NoSemanticPublication, "companion fixture");
                value.signature = Known::Known(SignatureText {
                    text: Arc::from(if is_type { "type ErrorHandling int" } else { "const ContinueOnError ErrorHandling = iota" }),
                    tokens: Arc::from([]),
                    name_link_coverage: crate::model::pages::NameLinkCoverage::Unavailable,
                });
                value.docs = Arc::from([DocFragment::Text(Arc::from(if is_type {
                    "Controls error handling."
                } else if published {
                    "After publication."
                } else {
                    "Before publication."
                }))]);
                if is_type {
                    value.outline = Known::Known(OutlinePosition {
                        ancestors: Arc::from([]),
                        siblings: Arc::from([
                            value.identity.clone(),
                            DeclRef::from_label(CONTINUE, None, Some(DeclarationKind::Constant), None).expect("Go constant"),
                        ]),
                        index: Some(0),
                    });
                }
                Ok(PageValue::Symbol(value))
            }
            _ => Fixture.read(request, context),
        }
    }
}

fn open(cx: &mut TestAppContext, route: Route) -> (Rig, Arc<Observations>) {
    let observed = Arc::new(Observations::default());
    let reads = Arc::clone(&observed);
    let pool = ReadPool::start(2, move |_| PublicationFixture(Arc::clone(&reads))).expect("publication pool");
    (rig_with_reads(cx, Some(route), 1440.0, 900.0, pool), observed)
}

fn package_route() -> Route {
    Route::Package(PackageRoute {
        cargo: None,
        project: None,
        package: crate::core::PackageId::new(PACKAGE).expect("package route"),
        lane: PackageLane::Overview,
        selected: None,
        at: None,
    })
}

fn go_route() -> Route {
    Route::Symbol(SymbolRoute {
        project: None,
        package: crate::core::PackageId::new(PACKAGE).expect("Go package"),
        id: Coordinate::new(ERROR_TYPE).expect("Go coordinate"),
        at: None,
        view: View::Page,
        line: None,
        selected: None,
    })
}

// Companion summaries are wrapped text painted by the anatomy component;
// the Reader's `said` list only records the host's own synchronous words.
fn companion_is_painted(rig: &mut Rig, words: &str) -> bool {
    rig.repaint();
    rig.cx.update(|window, _| {
        window.painted_texts().iter().any(|text| text.text.to_string().contains(words))
    })
}

#[gpui::test]
fn mounted_header_publication_renews_native_actions_at_the_same_authority(cx: &mut TestAppContext) {
    let (mut rig, observed) = open(cx, go_route());
    rig.cx.update(|window, _| window.set_a11y_forced(true));
    rig.go(Intent::SetMotion(crate::model::MotionPreference::Reduced));
    let before = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot());
    let titlebar_renders = rig.counts().titlebar;
    let frame = rig.cx.update(|window, _| window.a11y_frame_number());
    let key = PageKey::Symbol(SymbolRef::new(ERROR_TYPE).expect("the header's exact Go declaration"));
    let reads = observed.count(&key);
    observed.published.store(true, Ordering::Release);
    rig.graph.store.update(rig.cx, |store, cx| {
        store.packages_published(&BTreeSet::from([package()]), cx);
    });
    // Only drain ordinary effects while the actual fixture read worker lands.
    // Explicit drawing/refreshing would conceal a missing cached-header wake.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        rig.cx.run_until_parked();
        let current = rig.graph.store.read_with(rig.cx, |store, _| {
            store.pool_activity().is_idle() && !store.observation_revoked(&key)
        });
        if current { break; }
        assert!(std::time::Instant::now() < deadline, "publication must finish its controlled read");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert_eq!(observed.count(&key), reads + 1, "one publication renews the header's declaration exactly once");
    assert!(rig.counts().titlebar > titlebar_renders);
    rig.graph.store.read_with(rig.cx, |store, _| {
        assert!(before.key().same_authority(store.snapshot().key()));
        assert_eq!(before.route(), store.snapshot().route());
        assert_eq!(before.session().back, store.snapshot().session().back);
    });
    rig.cx.update(|window, _| {
        assert!(window.a11y_frame_number() > frame);
        let tree = window.a11y_tree().expect("publication's committed header tree");
        assert!(tree.nodes.iter().any(|(_, node)| node.label() == Some("Show Code view")
            && node.supports_action(gpui::AccessibleAction::Click)));
    });
    let bounds = rig.shell.read_with(rig.cx, |shell, cx| shell.titlebar_target_bounds("view-code", cx))
        .expect("publication's freshly mounted Code action");
    rig.cx.simulate_click(bounds.center(), gpui::Modifiers::default());
    rig.settle();
    assert!(matches!(rig.route(), Route::Symbol(route) if route.id.as_str() == ERROR_TYPE && route.view == View::Code),
        "the renewed native control opens its exact declaration");
}

#[gpui::test]
fn mounted_shelf_refreshes_its_unfocused_orbit_and_leaves_hidden_packages_lazy(cx: &mut TestAppContext) {
    let (mut rig, observed) = open(cx, package_route());
    let hidden = PackageRef::parse("pkg:cargo/hidden@1.0.0").expect("hidden release");
    let hidden_key = PageKey::Package(hidden.clone());
    rig.graph.store.update(rig.cx, |store, cx| { store.ensure(hidden_key.clone(), cx); });
    rig.settle();
    let before = rig.graph.store.read_with(rig.cx, |store, _| {
        assert!(!store.focused().contains(&PageKey::Orbit));
        assert!(!store.focused().contains(&hidden_key));
        store.snapshot()
    });
    let shelf_renders = rig.counts().shelf;
    let orbit_reads = observed.count(&PageKey::Orbit);
    let hidden_reads = observed.count(&hidden_key);
    observed.published.store(true, Ordering::Release);
    rig.graph.store.update(rig.cx, |store, cx| {
        store.packages_published(&BTreeSet::from([hidden]), cx);
    });
    rig.settle();
    assert_eq!(observed.count(&PageKey::Orbit), orbit_reads + 1, "the mounted Shelf renews its own key once");
    assert_eq!(observed.count(&hidden_key), hidden_reads, "resident hidden pages cost no eager read");
    assert!(rig.counts().shelf > shelf_renders, "the real Shelf observed the renewed data");
    rig.graph.store.read_with(rig.cx, |store, _| {
        let now = store.snapshot();
        assert!(before.key().same_authority(now.key()));
        assert_eq!(before.route(), now.route());
        assert_eq!(before.session().back, now.session().back);
        assert_eq!(before.session().forward, now.session().forward);
        assert!(store.observation_revoked(&hidden_key));
        assert!(!store.observation_revoked(&PageKey::Orbit));
        assert_eq!(store.orbit().loaded_value().expect("fresh Orbit").indexed.known()
            .expect("indexed packages").first().expect("indexed package").name.as_ref(), "Published library");
    });
    rig.graph.store.update(rig.cx, |store, cx| { store.ensure(PageKey::Orbit, cx); });
    rig.settle();
    assert_eq!(observed.count(&PageKey::Orbit), orbit_reads + 1, "the repaired observation remains current");
}

#[gpui::test]
fn mounted_go_companion_refreshes_without_navigation_and_stays_lazy_after_leaving(cx: &mut TestAppContext) {
    let (mut rig, observed) = open(cx, go_route());
    let companion = PageKey::Symbol(SymbolRef::new(CONTINUE).expect("companion key"));
    let before = rig.graph.store.read_with(rig.cx, |store, _| {
        assert!(!store.focused().contains(&companion));
        store.snapshot()
    });
    let companion_reads = observed.count(&companion);
    assert!(companion_reads > 0, "the mounted Go page requested its actual companion");
    assert!(companion_is_painted(&mut rig, "Before publication"));
    observed.published.store(true, Ordering::Release);
    rig.graph.store.update(rig.cx, |store, cx| {
        store.packages_published(&BTreeSet::from([package()]), cx);
    });
    rig.settle();
    assert_eq!(observed.count(&companion), companion_reads + 1, "the companion renews once at the same route and authority");
    assert!(companion_is_painted(&mut rig, "After publication"), "fresh companion words reach the mounted Reader");
    rig.graph.store.read_with(rig.cx, |store, _| {
        assert!(before.key().same_authority(store.snapshot().key()));
        assert_eq!(before.route(), store.snapshot().route());
        assert!(!store.observation_revoked(&companion));
    });
    rig.go(Intent::Navigate(package_route()));
    let settled_reads = observed.count(&companion);
    rig.graph.store.update(rig.cx, |store, cx| {
        store.packages_published(&BTreeSet::from([package()]), cx);
    });
    rig.settle();
    assert_eq!(observed.count(&companion), settled_reads, "an old subscription cannot eagerly renew a hidden companion");
    assert!(rig.graph.store.read_with(rig.cx, |store, _| store.observation_revoked(&companion)));
    rig.go(Intent::Navigate(go_route()));
    assert_eq!(observed.count(&companion), settled_reads + 1, "visiting the same companion list renews its revoked observations");
}
