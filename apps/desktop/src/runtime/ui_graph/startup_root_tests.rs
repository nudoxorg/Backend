//! Mounted owner handover controls. The Root DTO is a fixture, not a native
//! socket, certified publication, or proof that a busy owner releases in time.

use super::*;
use crate::core::{ErrorValue, FaultCode, LocalProjectId, VersionedRoot};
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
            return Err(EngineFault::Failed(ErrorValue::new(
                FaultCode::Protocol,
                "the empty startup fixture only accepts its Root request",
            )));
        };
        self.calls
            .lock()
            .map_err(|_| {
                EngineFault::Failed(ErrorValue::new(
                    FaultCode::Protocol,
                    "startup root call ledger was poisoned",
                ))
            })?
            .push(RootCall {
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
                id: LocalProjectId::new("/fixture/gui-single-root").map_err(|error| {
                    EngineFault::Failed(ErrorValue::new(
                        FaultCode::Protocol,
                        format!("startup project identity: {error:?}"),
                    ))
                })?,
                label: Arc::from("Root reply admitted"),
                packages: Arc::from([]),
            }),
            catalog: None,
        })
    }
}

fn runtime(
    gate: Option<&OwnerGate>,
    calls: &Arc<Mutex<Vec<RootCall>>>,
) -> anyhow::Result<DesktopRuntime> {
    let client = RecordsRoots {
        gate: gate.cloned(),
        calls: Arc::clone(calls),
    };
    Ok(DesktopRuntime::new(
        AppSnapshot::empty(VersionedRoot::unserved()),
        EngineActor::start(client, 4)
            .map_err(|error| anyhow::anyhow!("startup actor: {error:?}"))?,
    ))
}

#[test]
fn only_an_ungated_runtime_queues_a_constructor_root() -> anyhow::Result<()> {
    let calls = Arc::default();
    let gate = OwnerGate::starting();
    let mut gated = runtime(Some(&gate), &calls)?;
    gated.observe_initial_root(Some(gate));
    assert!(UiRootEntity::new(gated, None).pending.is_empty());
    assert!(matches!(
        UiRootEntity::new(runtime(None, &calls)?, None)
            .pending
            .as_slice(),
        [QueuedIntent::Plain(Intent::RefreshRoot { .. })]
    ));
    Ok(())
}

#[gpui::test]
fn an_owner_handover_runs_one_root_without_cancelling_a_constructor_read(cx: &mut TestAppContext) {
    let result = exercise_owner_handover(cx);
    assert!(result.is_ok(), "mounted startup fixture failed: {result:?}");
}

fn exercise_owner_handover(cx: &mut TestAppContext) -> anyhow::Result<()> {
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
        let runtime = runtime(Some(&gate), &calls)?;
        let graph = cx.update(|cx| {
            UiEntityGraph::install_with_owner(cx, runtime, None, None, Some(gate.clone()), None)
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
            assert!(
                calls
                    .lock()
                    .map_err(|_| anyhow::anyhow!("startup root call ledger was poisoned"))?
                    .is_empty()
            );
            gate.publish(OwnerState::Ready {
                key,
                mode: ServiceMode::Embedded,
            });
        }
        super::super::wait::until("the owner Root reaches the mounted reducer", || {
            cx.run_until_parked();
            calls.lock().is_ok_and(|calls| !calls.is_empty())
                && !graph.root.read_with(cx, |root, _| root.has_pending_work())
        });
        let calls = calls
            .lock()
            .map_err(|_| anyhow::anyhow!("startup root call ledger was poisoned"))?
            .clone();
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
    Ok(())
}
