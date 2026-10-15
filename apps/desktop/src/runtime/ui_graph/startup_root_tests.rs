//! Mounted owner handover controls. The Root DTO is a fixture, not a native
//! socket, certified publication, or proof that a busy owner releases in time.
#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use crate::core::{LocalProjectId, VersionedRoot};
use crate::model::ServiceMode;
use crate::runtime::actor::{
    CancellationToken, EngineActor, EngineClient, EngineDto, EngineFault, EngineRequest, ProjectDto,
};
use crate::runtime::owner::{OwnerGate, OwnerState};
use gpui::TestAppContext;
use std::sync::Mutex;

#[derive(Clone)]
struct RootCall {
    basis: VersionedRoot,
    cancel: CancellationToken,
}

struct RecordsRoots {
    gate: Option<OwnerGate>,
    calls: Arc<Mutex<Vec<RootCall>>>,
}

impl EngineClient for RecordsRoots {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        let EngineRequest::Root {
            request: id,
            basis,
            cancel,
            ..
        } = request
        else {
            panic!("the empty startup fixture only asks for its Root");
        };
        self.calls.lock().expect("root calls").push(RootCall {
            basis: *basis,
            cancel: cancel.clone(),
        });
        if let Some(gate) = &self.gate {
            gate.wait_cancelled(cancel)
                .map_err(|_| EngineFault::Cancelled)?;
        }
        if request.cancelled() {
            return Err(EngineFault::Cancelled);
        }
        Ok(EngineDto::Root {
            request: *id,
            basis: *basis,
            key: *basis,
            revision: basis.revision(),
            delta: None,
            project: Some(ProjectDto {
                id: LocalProjectId::new("/fixture/gui-single-root").expect("project identity"),
                label: Arc::from("Root reply admitted"),
                packages: Arc::from([]),
            }),
            catalog: None,
        })
    }
}

fn runtime(gate: Option<&OwnerGate>, calls: &Arc<Mutex<Vec<RootCall>>>) -> DesktopRuntime {
    let client = RecordsRoots {
        gate: gate.cloned(),
        calls: Arc::clone(calls),
    };
    DesktopRuntime::new(
        AppSnapshot::empty(VersionedRoot::unserved()),
        EngineActor::start(client, 4).expect("actor"),
    )
}

#[test]
fn only_an_ungated_runtime_queues_a_constructor_root() {
    let calls = Arc::default();
    let gate = OwnerGate::starting();
    let mut gated = runtime(Some(&gate), &calls);
    gated.observe_initial_root(Some(gate));
    assert!(UiRootEntity::new(gated, None).pending.is_empty());
    assert!(matches!(
        UiRootEntity::new(runtime(None, &calls), None)
            .pending
            .as_slice(),
        [QueuedIntent::Plain(Intent::RefreshRoot { .. })]
    ));
}

#[gpui::test]
fn an_owner_handover_runs_one_root_without_cancelling_a_constructor_read(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    // Exercise both an owner that answers after mount and one already ready
    // when the watcher is installed. Neither case needs a constructor Root.
    for already_ready in [false, true] {
        let calls = Arc::default();
        let gate = OwnerGate::starting();
        let key = VersionedRoot::synthetic(
            backend_library::view_state_root(&[("single".to_owned(), "startup".to_owned())]),
            1,
        );
        if already_ready {
            gate.publish(OwnerState::Ready {
                key,
                mode: ServiceMode::Embedded,
            });
        }
        let graph = cx.update(|cx| {
            UiEntityGraph::install_with_owner(
                cx,
                runtime(Some(&gate), &calls),
                None,
                None,
                Some(gate.clone()),
                None,
            )
        });
        cx.run_until_parked();
        if !already_ready {
            assert!(
                !graph.root.read_with(cx, |root, _| root.has_pending_work()),
                "no constructor Root waits at the gate to race owner admission"
            );
            assert!(
                graph
                    .root
                    .read_with(cx, |root, _| root.snapshot().key().is_unserved())
            );
            assert!(calls.lock().expect("root calls").is_empty());
            gate.publish(OwnerState::Ready {
                key,
                mode: ServiceMode::Embedded,
            });
        }
        super::super::wait::until("the owner Root reaches the mounted reducer", || {
            cx.run_until_parked();
            !calls.lock().expect("root calls").is_empty()
                && !graph.root.read_with(cx, |root, _| root.has_pending_work())
        });
        let calls = calls.lock().expect("root calls").clone();
        assert_eq!(
            calls.len(),
            1,
            "one authoritative Root, already_ready={already_ready}"
        );
        assert_eq!(
            calls[0].basis, key,
            "the Root uses the answering owner's authority"
        );
        assert!(
            !calls[0].cancel.is_cancelled(),
            "no rival startup read cancels this Root"
        );
        assert_eq!(
            graph.root.read_with(cx, |root, _| root.snapshot().key()),
            key
        );
        assert!(
            graph.root.read_with(cx, |root, _| {
                root.snapshot()
                    .project()
                    .loaded_value()
                    .is_some_and(|project| project.label.as_ref() == "Root reply admitted")
            }),
            "the Root reply changed the mounted snapshot beyond OwnerReady's basis"
        );
    }
}
