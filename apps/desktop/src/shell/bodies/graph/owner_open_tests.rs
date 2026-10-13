//! Delayed Peek ownership through the ordinary GraphView → Map → search path.
//! The projection, read lane and owner gate are explicit synthetic controls;
//! these tests do not establish acceptance by a real native/index owner.

use super::*;
use crate::runtime::{
    owner::{OwnerGate, OwnerState},
    reads::{PageReader, ReadContext, ReadFailure, ReadPool, ReadRequest},
};
use crate::model::{ServiceMode, pages::PageValue};
use crate::shell::tests::{Fixture, Rig, RootOnly, indexed_view_route, rig_with_engine_gate};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[derive(Default)]
struct SearchBarrier {
    released: AtomicBool,
    started: AtomicUsize,
}

struct ReleaseSearch(Arc<SearchBarrier>);
impl Drop for ReleaseSearch {
    fn drop(&mut self) { self.0.released.store(true, Ordering::Release); }
}

struct DelayedSearch(Arc<SearchBarrier>);
impl PageReader for DelayedSearch {
    fn read(&mut self, request: &ReadRequest, context: &ReadContext<'_>) -> Result<PageValue, ReadFailure> {
        if matches!(request, ReadRequest::Search(query) if query.text.as_ref() == "RelationLabel") {
            self.0.started.fetch_add(1, Ordering::AcqRel);
            let deadline = Instant::now() + Duration::from_secs(10);
            while !self.0.released.load(Ordering::Acquire) {
                if context.cancel.is_cancelled() { return Err(ReadFailure::Cancelled); }
                assert!(Instant::now() < deadline, "delayed Peek search was never released");
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        Fixture.read(request, context)
    }
}

fn rig(cx: &mut gpui::TestAppContext) -> (Rig, OwnerGate, Arc<SearchBarrier>, ReleaseSearch) {
    let root = VersionedRoot::synthetic(backend_library::view_state_root(&[
        ("shell".to_owned(), "tests".to_owned())]), 4);
    let owner = OwnerGate::ready(root, ServiceMode::Attached);
    let barrier = Arc::new(SearchBarrier::default());
    let reads = barrier.clone();
    let pool = ReadPool::start(2, move |_| DelayedSearch(reads.clone())).expect("synthetic read lane");
    let mut rig = rig_with_engine_gate(cx, Some(Route::World), 1440.0, 900.0, pool, RootOnly, Some(owner.clone()));
    crate::shell::anatomy_tests::install(&mut rig);
    rig.settle();
    let release = ReleaseSearch(barrier.clone());
    (rig, owner, barrier, release)
}

fn map(rig: &mut Rig) -> Entity<Map> {
    rig.shell.read_with(rig.cx, |shell, cx|
        shell.reader_entity().read(cx).graph_map()).expect("the actual mounted Map")
}

fn begin_unfocused_peek(rig: &mut Rig, barrier: &SearchBarrier) -> (Entity<Map>, OwnerAttachment, SearchQuery) {
    let map = map(rig);
    let graph = map.read_with(rig.cx, |map, _| map.graph.clone()).expect("mounted graph scene");
    assert!(graph.read_with(rig.cx, |graph, _| graph.focused()).is_none());
    let owner = rig.graph.store.read_with(rig.cx, |store, _|
        store.current_owner_attachment()).expect("original serving attachment");
    rig.cx.update(|window, cx| graph.update(cx, |graph, cx|
        graph.act_on_peek(0, facet::graph::peek::Action::Open, window, cx)));
    rig.cx.run_until_parked();
    crate::runtime::wait::until("the activated Peek's search is blocked", ||
        barrier.started.load(Ordering::Acquire) > 0);
    let query = map.read_with(rig.cx, |map, _| {
        let request = map.pending.as_ref().expect("delayed Peek is pending");
        assert_eq!(request.origin, OpenOrigin::Peek);
        assert_eq!(request.focus_at_open, None);
        assert_eq!(request.attachment, owner);
        request.query.clone()
    });
    assert_eq!(rig.route(), Route::World);
    (map, owner, query)
}

#[gpui::test]
fn delayed_unfocused_peek_cannot_borrow_a_same_root_replacement_owner(cx: &mut gpui::TestAppContext) {
    let (mut rig, gate, barrier, _release) = rig(cx);
    let (map, former, query) = begin_unfocused_peek(&mut rig, &barrier);
    let before = rig.graph.store.read_with(rig.cx, |store, _| store.snapshot());
    gate.publish(OwnerState::Starting);
    gate.publish(OwnerState::Ready { key: before.key(), mode: ServiceMode::Attached });
    // No UI watcher or new scene is required to refuse the old attachment.
    rig.cx.update(|window, cx| map.update(cx, |map, cx| map.resolve_open(window, cx)));
    assert!(map.read_with(rig.cx, |map, _| map.pending.is_none()));
    rig.cx.run_until_parked();
    assert!(!rig.graph.store.read_with(rig.cx, |store, _| store.admits_owner_attachment(&former)));
    // Valid rows under the new same-root owner must not finish the old open.
    // This also exercises the Resource event after a replacement scene mounts.
    barrier.released.store(true, Ordering::Release);
    rig.graph.store.update(rig.cx, |store, cx| { store.ensure(PageKey::Search(query.clone()), cx); });
    rig.settle();
    rig.graph.store.read_with(rig.cx, |store, _| {
        assert!(store.snapshot().key().same_authority(before.key()));
        assert_eq!(store.snapshot().session().reading.current.id, before.session().reading.current.id);
        assert!(store.search(&query).loaded_value().is_some(), "replacement search rows actually landed");
    });
    assert!(map.read_with(rig.cx, |map, cx| map.graph.as_ref()
        .and_then(|graph| graph.read(cx).focused())).is_none());
    assert_eq!(rig.route(), Route::World, "late Peek cannot navigate through a replacement owner");
}

#[gpui::test]
fn delayed_unfocused_peek_still_opens_when_its_original_owner_serves_the_reply(cx: &mut gpui::TestAppContext) {
    let (mut rig, _gate, barrier, _release) = rig(cx);
    let (_, original, _) = begin_unfocused_peek(&mut rig, &barrier);
    assert!(rig.graph.store.read_with(rig.cx, |store, _| store.admits_owner_attachment(&original)));
    barrier.released.store(true, Ordering::Release);
    rig.settle();
    assert_eq!(rig.route(), indexed_view_route("RelationLabel", View::Page),
        "the original owner can resolve the exact Peek target with no graph selection");
}
