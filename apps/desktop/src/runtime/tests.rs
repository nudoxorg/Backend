//! Runtime race tests kept next to the actor/coordinator boundary.

use super::actor::{EngineClient, EngineDto, EngineFault, EngineRequest};
use super::coordinator::{DesktopRuntime, RuntimeEvent};
use crate::core::VersionedRoot;
use crate::model::AppSnapshot;
use crate::navigation::{Intent, RequestId};

struct EchoClient;

fn next_key(basis: VersionedRoot) -> (VersionedRoot, backend_library::Cursor) {
    let revision = backend_library::Cursor::at(basis.root(), basis.generation().saturating_add(1));
    (
        VersionedRoot::from_revision(basis.producer_epoch(), revision, basis.observation()),
        revision,
    )
}

impl EngineClient for EchoClient {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        match request {
            EngineRequest::Root { request, basis, .. } => {
                let (key, revision) = next_key(*basis);
                Ok(EngineDto::Root {
                    request: *request,
                    basis: *basis,
                    key,
                    revision,
                    delta: None,
                    project: None,
                    catalog: None,
                })
            }
            EngineRequest::Object {
                request,
                basis,
                object,
                delta,
                ..
            } => Ok(EngineDto::Object {
                request: *request,
                basis: *basis,
                object: *object,
                delta: *delta,
            }),
            EngineRequest::Surface {
                request,
                basis,
                command,
                ..
            } => Ok(EngineDto::Surface {
                request: *request,
                basis: *basis,
                command: command.clone(),
                reply: backend_library::SurfaceReply::Explored(Box::new([])),
            }),
            EngineRequest::IndexProject {
                request,
                project,
                basis,
                ..
            } => {
                let (key, revision) = next_key(*basis);
                Ok(EngineDto::Index {
                    request: *request,
                    basis: *basis,
                    key,
                    revision,
                    delta: None,
                    project: project.clone(),
                    project_state: None,
                    catalog: None,
                    files_indexed: None,
                })
            }
        }
    }
}

fn snapshot() -> AppSnapshot {
    AppSnapshot::empty(VersionedRoot::synthetic(
        backend_library::view_state_root(&[("root".to_owned(), "runtime".to_owned())]),
        7,
    ))
}

#[test]
fn latest_root_supersedes_older_refreshes_without_ui_waiting() {
    let actor = super::actor::EngineActor::start(EchoClient, 2).expect("actor thread");
    let mut runtime = DesktopRuntime::new(snapshot(), actor);
    let request = RequestId::new(1);
    let basis = runtime.snapshot().key();
    let events = runtime.dispatch(Intent::RefreshRoot { basis, request });
    assert!(
        events
            .iter()
            .any(|event| matches!(event, RuntimeEvent::SnapshotChanged(_)))
    );
    assert!(runtime.is_inflight(request));
    for _ in 0..100 {
        if !runtime.poll().is_empty() {
            break;
        }
        std::thread::yield_now();
    }
    assert!(!runtime.is_inflight(request));
}

#[test]
fn stop_cancels_owned_requests_without_waiting_for_the_actor() {
    let actor = super::actor::EngineActor::start(EchoClient, 2).expect("actor thread");
    let mut runtime = DesktopRuntime::new(snapshot(), actor);
    let request = RequestId::new(2);
    let basis = runtime.snapshot().key();
    runtime.dispatch(Intent::RefreshRoot { basis, request });
    assert!(runtime.is_inflight(request));
    runtime.dispatch(Intent::Stop);
    assert!(!runtime.is_inflight(request));
}

#[test]
fn object_result_records_its_delta_without_advancing_the_root() {
    let actor = super::actor::EngineActor::start(EchoClient, 2).expect("actor thread");
    let mut runtime = DesktopRuntime::new(snapshot(), actor);
    let request = RequestId::new(3);
    let basis = runtime.snapshot().key();
    let delta = crate::model::DeltaId::test(11);
    runtime.dispatch(Intent::RefreshObject {
        object: crate::model::ObjectId::test(4),
        delta,
        basis,
        request,
    });
    for _ in 0..100 {
        if !runtime.poll().is_empty() {
            break;
        }
        std::thread::yield_now();
    }
    assert_eq!(runtime.snapshot().key(), basis);
    assert_eq!(runtime.snapshot().delta(), Some(delta));
}

#[test]
fn result_delivery_stays_bounded_while_the_ui_is_not_polling() {
    let basis = snapshot().key();
    let actor = super::actor::EngineActor::start(EchoClient, 1).expect("actor thread");
    for value in 0..4 {
        let token = super::actor::CancellationToken::new();
        let request = EngineRequest::Object {
            request: RequestId::new(10 + value),
            object: crate::model::ObjectId::test(value),
            delta: crate::model::DeltaId::test(value),
            basis,
            cancel: token,
        };
        let _ = actor.try_submit(request);
    }
    for _ in 0..100 {
        if actor.queued_events() == 1 {
            break;
        }
        std::thread::yield_now();
    }
    assert!(actor.queued_events() <= 1);
    drop(actor);
}

#[test]
fn explicit_shutdown_closes_both_bounded_channels_and_joins() {
    let actor = super::actor::EngineActor::start(EchoClient, 1).expect("actor thread");
    actor.shutdown();
}

#[test]
fn result_coalescing_retires_the_replaced_request() {
    let actor = super::actor::EngineActor::start(EchoClient, 1).expect("actor thread");
    let mut runtime = DesktopRuntime::new(snapshot(), actor);
    let basis = runtime.snapshot().key();
    let object = crate::model::ObjectId::test(20);
    let first = RequestId::new(20);
    runtime.dispatch(Intent::RefreshObject {
        object,
        delta: crate::model::DeltaId::test(20),
        basis,
        request: first,
    });
    for _ in 0..100 {
        if runtime.queued_results() == 1 {
            break;
        }
        std::thread::yield_now();
    }
    assert!(runtime.is_inflight(first));
    let second = RequestId::new(21);
    runtime.dispatch(Intent::RefreshObject {
        object,
        delta: crate::model::DeltaId::test(21),
        basis,
        request: second,
    });
    for _ in 0..100 {
        if !runtime.poll().is_empty() {
            break;
        }
        std::thread::yield_now();
    }
    assert!(!runtime.is_inflight(first));
    assert!(!runtime.is_inflight(second));
}
