//! How often the shell asks the owner for its root. A root read is the full
//! bootstrap (the owner's packages and catalog), so it must follow a new
//! certified publication one for one and stop when the owner is quiet: a
//! read that begets another read would show as a root bootstrap every few
//! seconds on an idle library (PERF-01).

#![allow(clippy::expect_used, clippy::panic)]

use super::tests::rig_with_engine_gate_at_root;
use crate::core::VersionedRoot;
use crate::model::ServiceMode;
use crate::navigation::Route;
use crate::runtime::actor::{EngineClient, EngineDto, EngineFault, EngineRequest};
use crate::runtime::owner::{OwnerGate, PublicationAdmission};
use crate::runtime::reads::ReadPool;
use gpui::TestAppContext;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Answers a root read with the root it was asked at, and counts the reads.
struct CountingRoot(Arc<AtomicUsize>);

impl EngineClient for CountingRoot {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        let EngineRequest::Root { request, basis, .. } = request else {
            return Err(EngineFault::Cancelled);
        };
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(EngineDto::Root {
            request: *request,
            basis: *basis,
            key: *basis,
            revision: basis.revision(),
            delta: None,
            project: None,
            catalog: None,
        })
    }
}

#[gpui::test]
fn a_certified_publication_costs_one_root_read_and_a_quiet_owner_costs_none(cx: &mut TestAppContext) {
    let view = crate::runtime::owner::publication_tests::view();
    let cursor = backend_library::Cursor::for_view_root_at(&view, 0);
    let root = VersionedRoot::from_revision(4, cursor, 0);
    let gate = OwnerGate::ready(root, ServiceMode::Attached);
    let reads = Arc::new(AtomicUsize::new(0));
    let mut rig = rig_with_engine_gate_at_root(
        cx,
        Some(Route::World),
        1440.0,
        900.0,
        ReadPool::start(2, |_| super::tests::Fixture).expect("controlled page pool"),
        CountingRoot(Arc::clone(&reads)),
        Some(gate.clone()),
        root,
    );
    let attachment = gate.ready_epoch().expect("the serving attachment");
    assert_eq!(gate.publish_view(attachment, Arc::clone(&view), cursor), PublicationAdmission::Admitted);
    rig.settle();

    // Quiet: virtual time passing with no new publication asks for nothing.
    let rested = reads.load(Ordering::SeqCst);
    for _ in 0..40 {
        rig.cx.executor().advance_clock(Duration::from_secs(1));
        rig.settle();
    }
    assert_eq!(reads.load(Ordering::SeqCst), rested, "a quiet owner is not asked for its root again");

    // Each new certified revision is read once, however long the shell then rests.
    for sequence in 1..=4u64 {
        let before = reads.load(Ordering::SeqCst);
        let next = backend_library::Cursor::for_view_root_at(&view, sequence);
        assert_eq!(gate.publish_view(attachment, Arc::clone(&view), next), PublicationAdmission::Admitted);
        rig.settle();
        for _ in 0..10 {
            rig.cx.executor().advance_clock(Duration::from_secs(1));
            rig.settle();
        }
        let spent = reads.load(Ordering::SeqCst) - before;
        assert_eq!(spent, 1, "revision {sequence} cost {spent} root reads");
    }
}
