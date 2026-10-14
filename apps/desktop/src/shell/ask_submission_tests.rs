//! Genuine worker deliveries inside the native Enter/deferred-effect boundary.
//! The producer and query stay fixed; only the exact Search resource changes.

use super::SearchReadLease;
use crate::core::{VersionedRoot, admit_resource};
use crate::model::ServiceMode;
use crate::model::pages::{PageKey, PageValue, ReadFailure, SearchQuery};
use crate::navigation::{BrowseRoute, Intent, OrbitRoute, Route};
use crate::runtime::owner::OwnerGate;
use crate::runtime::reads::{PageReader, ReadContext, ReadPool, ReadRequest};
use crate::shell::tests::{Fixture, Rig, page_route, rig_with_engine_gate};
use gpui::{AppContext as _, Focusable as _, TestAppContext};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

/// One actual worker first returns Label and then Direction for the same query.
/// Its bytes remain fixture data, not real compiler/publication acceptance.
struct SwitchingSearch {
    deliveries: Arc<AtomicUsize>,
}

impl PageReader for SwitchingSearch {
    fn read(
        &mut self,
        request: &ReadRequest,
        context: &ReadContext<'_>,
    ) -> Result<PageValue, ReadFailure> {
        if let ReadRequest::Search(query) = request {
            let number = self.deliveries.fetch_add(1, Ordering::SeqCst);
            let name = if number == 0 {
                "RelationLabel"
            } else {
                "RelationDirection"
            };
            let addressed = SearchQuery::new(name, query.limit).expect("controlled fixture row");
            let PageValue::Search(mut page) =
                Fixture.read(&ReadRequest::Search(addressed), context)?
            else {
                unreachable!("Fixture serves Search");
            };
            page.query = query.text.clone();
            return Ok(PageValue::Search(page));
        }
        Fixture.read(request, context)
    }
}

fn mounted_search(cx: &mut TestAppContext) -> (Rig, Arc<AtomicUsize>) {
    let root = VersionedRoot::synthetic(
        backend_library::view_state_root(&[("shell".to_owned(), "tests".to_owned())]),
        4,
    );
    let gate = OwnerGate::ready(root, ServiceMode::Attached);
    let deliveries = Arc::new(AtomicUsize::new(0));
    let observed = deliveries.clone();
    let pool = ReadPool::start(1, move |_| SwitchingSearch {
        deliveries: observed.clone(),
    })
    .expect("actual search worker");
    let mut rig = rig_with_engine_gate(
        cx,
        Some(page_route("KindGlyph")),
        1440.0,
        900.0,
        pool,
        crate::shell::jump_tests::StableRoot,
        Some(gate),
    );
    rig.go(Intent::SetMotion(crate::model::MotionPreference::Reduced));
    super::tests::native_ask(&mut rig);
    rig.cx.simulate_input("Relation");
    rig.settle();
    assert_eq!(deliveries.load(Ordering::SeqCst), 1);
    let ask = rig.shell.read_with(&rig.cx, |shell, _| shell.ask_entity());
    assert_eq!(
        ask.read_with(&rig.cx, |ask, _| ask
            .painted_search
            .as_ref()
            .expect("rendered initial read")
            .rows[0]
            .route
            .clone()),
        Some(page_route("RelationLabel"))
    );
    (rig, deliveries)
}

/// Deliver the second genuine worker result while App/Window are borrowed.
/// No executor, render, or native event is run by this helper.
fn replace_search_without_paint(
    store: &gpui::Entity<crate::runtime::store::DataStore>,
    query: &SearchQuery,
    cx: &mut gpui::App,
) {
    store.update(cx, |store, cx| {
        store.retry(PageKey::Search(query.clone()), cx)
    });
    crate::runtime::wait::until(
        "actual replacement Search delivery inside native input batch",
        || {
            store.update(cx, |store, cx| {
                store.drain(cx);
            });
            let store = store.read(cx);
            let resource = store.search(query);
            admit_resource(&resource, store.snapshot().key(), store.owner_serving())
                .current_value()
                .is_some_and(|page| {
                    page.rows
                        .first()
                        .is_some_and(|row| row.decl.name.as_ref() == "RelationDirection")
                })
                && store.pool_activity().is_idle()
        },
    );
}

fn resource_interleaving(cx: &mut TestAppContext, completion_before_key_down: bool) {
    for activation in ["enter", "secondary-enter"] {
        let (mut rig, deliveries) = mounted_search(cx);
        let original = rig.route();
        let ask = rig.shell.read_with(&rig.cx, |shell, _| shell.ask_entity());
        let (input, query) = ask.read_with(&rig.cx, |ask, _| {
            (
                ask.input.clone(),
                ask.draft.query().cloned().expect("current query"),
            )
        });
        let store = rig.graph.store.clone();
        rig.cx.update(|window, cx| {
            assert!(input.read(cx).focus_handle(cx).is_focused(window));
            let root = store.read(cx).snapshot().key();
            let owner = store
                .read(cx)
                .current_owner_attachment()
                .expect("serving owner");
            let revision = ask.read(cx).revision.get();
            let visit = rig.graph.root.read(cx).graph_view_generation();
            let key = PageKey::Search(query.clone());
            let stamp = store.read(cx).stamp(&key);
            let read = ask
                .read(cx)
                .painted_search
                .as_ref()
                .expect("old drawn result")
                .read
                .clone();
            assert!(read.admits(&ask.read(cx).links, &query, cx));
            let mut frame = window.a11y_frame_number();
            let mut renders = ask.read(cx).renders();
            if completion_before_key_down {
                replace_search_without_paint(&store, &query, cx);
                assert_eq!(window.a11y_frame_number(), frame);
                assert_eq!(
                    ask.read(cx).renders(),
                    renders,
                    "delivery alone has not redrawn the cached plate"
                );
            }
            let native_key = gpui::Keystroke::parse(activation).expect("native Enter");
            window.dispatch_event(
                gpui::PlatformInput::KeyDown(gpui::KeyDownEvent {
                    keystroke: native_key.clone(),
                    is_held: false,
                    prefer_character_input: false,
                }),
                cx,
            );
            assert_eq!(
                store.read(cx).snapshot().route(),
                &original,
                "submission is genuinely deferred"
            );
            if !completion_before_key_down {
                // Snapshot after the native event permits its ordinary work;
                // the resource completion itself must not paint newer rows.
                frame = window.a11y_frame_number();
                renders = ask.read(cx).renders();
                replace_search_without_paint(&store, &query, cx);
                assert_eq!(window.a11y_frame_number(), frame);
                assert_eq!(ask.read(cx).renders(), renders);
            }
            assert_ne!(store.read(cx).stamp(&key), stamp);
            assert_eq!(store.read(cx).snapshot().key(), root);
            assert_eq!(store.read(cx).current_owner_attachment(), Some(owner));
            assert_eq!(rig.graph.root.read(cx).graph_view_generation(), visit);
            assert_eq!(ask.read(cx).revision.get(), revision);
            assert_eq!(input.read(cx).value().as_ref(), "Relation");
            assert!(
                !read.admits(&ask.read(cx).links, &query, cx),
                "only exact Search evidence has changed"
            );
            window.dispatch_event(
                gpui::PlatformInput::KeyUp(gpui::KeyUpEvent {
                    keystroke: native_key,
                }),
                cx,
            );
        });
        rig.cx.run_until_parked();
        assert_eq!(
            deliveries.load(Ordering::SeqCst),
            2,
            "the second result came from the actual reader"
        );
        assert_eq!(
            rig.route(),
            original,
            "old native Enter cannot consume unpainted newer results"
        );
        assert!(ask.read_with(&rig.cx, |ask, _| ask.refusal.is_some()));
        assert_eq!(
            input.read_with(&rig.cx, |input, _| input.value().to_string()),
            "Relation"
        );
        assert!(
            rig.cx
                .update(|window, cx| input.read(cx).focus_handle(cx).is_focused(window)),
            "refused deferred input retains the live query editor focus"
        );
        rig.settle();
        ask.read_with(&rig.cx, |ask, cx| {
            let current = ask
                .painted_search
                .as_ref()
                .expect("new result is now actually drawn");
            assert!(current.read.admits(&ask.links, &query, cx));
            assert_eq!(current.rows[0].route, Some(page_route("RelationDirection")));
            assert!(SearchReadLease::capture(&ask.links, &query, cx).is_some());
        });
        rig.keys(activation);
        let destination = if activation == "enter" {
            page_route("RelationDirection")
        } else {
            Route::Orbit(OrbitRoute::Browse(BrowseRoute::Find(query)))
        };
        assert_eq!(
            rig.route(),
            destination,
            "fresh native input uses the current drawn result"
        );
    }
}

#[gpui::test]
fn native_enter_rejects_same_query_completion_after_key_down_before_deferred_delivery(
    cx: &mut TestAppContext,
) {
    resource_interleaving(cx, false);
}

#[gpui::test]
fn native_enter_rejects_current_search_that_completed_before_key_down_without_paint(
    cx: &mut TestAppContext,
) {
    resource_interleaving(cx, true);
}
