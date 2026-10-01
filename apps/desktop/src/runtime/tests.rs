//! Runtime race tests kept next to the actor/coordinator boundary.

#![allow(clippy::expect_used, clippy::panic)]

use super::actor::{EngineClient, EngineDto, EngineFault, EngineRequest};
use super::coordinator::{
    DesktopRuntime, RequestOutcome, RequestRefusalReason, RuntimeEvent,
};
use super::wait;
use crate::core::VersionedRoot;
use crate::model::AppSnapshot;
use crate::navigation::{Intent, RequestId};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

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

fn request_terminals(events: Vec<RuntimeEvent>) -> Vec<(RequestId, RequestOutcome)> {
    events
        .into_iter()
        .filter_map(|event| match event {
            RuntimeEvent::RequestCompleted { request, outcome } => Some((request, outcome)),
            _ => None,
        })
        .collect()
}

#[test]
fn superseding_a_root_refresh_emits_one_typed_terminal_for_the_old_request() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let actor = super::actor::EngineActor::start(
        GatedClient {
            entered: entered_tx,
            release: release_rx,
        },
        2,
    )
    .expect("actor thread");
    let mut runtime = DesktopRuntime::new(snapshot(), actor);
    // Dropped before `runtime` on unwinding, releasing any gate wait before
    // the runtime joins its actor thread.
    let mut gate = TestGate::new(release_tx, 3);
    let basis = runtime.snapshot().key();
    let blocker = RequestId::new(800);
    let first = RequestId::new(801);
    let second = RequestId::new(802);

    let _ = dispatch_object(&mut runtime, blocker, 800);
    assert_eq!(
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("blocker entered"),
        blocker
    );
    let _ = runtime.dispatch(Intent::RefreshRoot {
        basis,
        request: first,
    });
    let replacement = runtime.dispatch(Intent::RefreshRoot {
        basis,
        request: second,
    });
    assert!(!runtime.is_inflight(first));
    assert!(runtime.is_inflight(second));
    assert_eq!(
        request_terminals(replacement),
        [(first, RequestOutcome::Superseded)]
    );

    let mut old_terminals = 1;
    gate.release_all();
    let mut outcomes = Vec::new();
    wait::until("the blocker and replacement root reached terminal results", || {
        for (request, outcome) in request_terminals(runtime.poll()) {
            if request == first {
                old_terminals += 1;
            } else {
                outcomes.push((request, outcome));
            }
        }
        !runtime.is_inflight(blocker) && !runtime.is_inflight(second)
    });
    assert_eq!(
        old_terminals, 1,
        "a retired request has exactly one terminal event"
    );
    assert_eq!(
        outcomes,
        [
            (blocker, RequestOutcome::Succeeded),
            (second, RequestOutcome::Succeeded),
        ]
    );
}

struct FailingClient;

impl EngineClient for FailingClient {
    fn execute(&mut self, _request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        Err(EngineFault::Failed(crate::core::ErrorValue::new(
            crate::core::FaultCode::Transport,
            "owner unavailable",
        )))
    }
}

#[test]
fn actor_failure_is_a_failed_terminal_not_a_success_or_stuck_request() {
    let actor = super::actor::EngineActor::start(FailingClient, 2).expect("actor thread");
    let mut runtime = DesktopRuntime::new(snapshot(), actor);
    let request = RequestId::new(803);
    let basis = runtime.snapshot().key();
    let _ = runtime.dispatch(Intent::RefreshRoot { basis, request });

    let mut outcomes = Vec::new();
    wait::until("the failed root request retired", || {
        outcomes.extend(request_terminals(runtime.poll()).into_iter().map(|(_, outcome)| outcome));
        !runtime.is_inflight(request)
    });
    assert_eq!(outcomes, [RequestOutcome::Failed]);
}

#[test]
fn a_closed_actor_refuses_the_request_and_emits_its_terminal_once() {
    let actor = super::actor::EngineActor::start(EchoClient, 2).expect("actor thread");
    actor.close_request_channels_for_test();
    let mut runtime = DesktopRuntime::new(snapshot(), actor);
    let request = RequestId::new(804);
    let basis = runtime.snapshot().key();

    let events = runtime.dispatch(Intent::RefreshRoot { basis, request });
    assert!(!runtime.is_inflight(request));
    assert_eq!(
        request_terminals(events),
        [(request, RequestOutcome::Refused(RequestRefusalReason::Closed))]
    );
    assert!(request_terminals(runtime.poll()).is_empty());
}

struct GatedClient {
    entered: Sender<RequestId>,
    release: Receiver<()>,
}

/// Releases a gated actor before its runtime is dropped, including when an
/// assertion unwinds before the test reaches its normal release point.
struct TestGate {
    release: Sender<()>,
    permits: usize,
    released: bool,
}

impl TestGate {
    fn new(release: Sender<()>, permits: usize) -> Self {
        Self {
            release,
            permits,
            released: false,
        }
    }

    fn release_all(&mut self) {
        if self.released {
            return;
        }
        for _ in 0..self.permits {
            let _ = self.release.send(());
        }
        self.released = true;
    }
}

impl Drop for TestGate {
    fn drop(&mut self) {
        self.release_all();
    }
}

impl EngineClient for GatedClient {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        self.entered
            .send(request.request())
            .map_err(|_| EngineFault::Cancelled)?;
        self.release.recv().map_err(|_| EngineFault::Cancelled)?;
        EchoClient.execute(request)
    }
}

fn dispatch_object(
    runtime: &mut DesktopRuntime,
    request: RequestId,
    object: u64,
) -> Vec<RuntimeEvent> {
    runtime.dispatch(Intent::RefreshObject {
        object: crate::model::ObjectId::test(object),
        delta: crate::model::DeltaId::test(object),
        basis: runtime.snapshot().key(),
        request,
    })
}

#[test]
fn a_full_actor_queue_refuses_new_work_without_leaking_an_inflight_request() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let actor = super::actor::EngineActor::start(
        GatedClient {
            entered: entered_tx,
            release: release_rx,
        },
        1,
    )
    .expect("actor thread");
    let mut runtime = DesktopRuntime::new(snapshot(), actor);
    // This guard runs before the runtime's actor join if an assertion fails.
    let mut gate = TestGate::new(release_tx, 2);
    let first = RequestId::new(805);
    let queued = RequestId::new(806);
    let refused = RequestId::new(807);

    let _ = dispatch_object(&mut runtime, first, 805);
    assert_eq!(
        entered_rx.recv_timeout(Duration::from_secs(2)).expect("first request entered"),
        first
    );
    let _ = dispatch_object(&mut runtime, queued, 806);
    let refusal = dispatch_object(&mut runtime, refused, 807);
    let refused_terminal = request_terminals(refusal);

    // Always release the worker before assertions so a failed test cannot
    // leave the actor join waiting on a fixture gate.
    gate.release_all();
    let mut admitted_outcomes = Vec::new();
    wait::until("the admitted queue entries reached terminal results", || {
        admitted_outcomes.extend(request_terminals(runtime.poll()));
        !runtime.is_inflight(first) && !runtime.is_inflight(queued)
    });

    assert_eq!(
        refused_terminal,
        [(refused, RequestOutcome::Refused(RequestRefusalReason::QueueFull))]
    );
    assert!(!runtime.is_inflight(refused));
    assert_eq!(
        admitted_outcomes,
        [
            (first, RequestOutcome::Succeeded),
            (queued, RequestOutcome::Succeeded),
        ]
    );
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
    // The actor wakes the owner when the result lands; the owner never spins.
    let mut wake = runtime.take_wake().expect("wake signal");
    wait::until("the root landed", || {
        if wake.try_take() {
            runtime.poll();
        }
        !runtime.is_inflight(request)
    });
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
    let stopped = runtime.dispatch(Intent::Stop);
    assert!(!runtime.is_inflight(request));
    assert_eq!(
        request_terminals(stopped),
        [(request, RequestOutcome::Cancelled)]
    );
    assert!(request_terminals(runtime.poll()).is_empty());
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
    wait::until("the object result was delivered", || !runtime.poll().is_empty());
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
    wait::until("the actor queued its one event", || actor.queued_events() == 1);
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
    wait::until("the first result was queued", || runtime.queued_results() == 1);
    assert!(runtime.is_inflight(first));
    let second = RequestId::new(21);
    runtime.dispatch(Intent::RefreshObject {
        object,
        delta: crate::model::DeltaId::test(21),
        basis,
        request: second,
    });
    // Replacing `first` retires it at once; its already-queued result then
    // drains as a stale rejection. `second` is only retired once the actor
    // has produced its own result, so poll until that happens instead of
    // stopping at the first non-empty drain, which is `first`'s leftover.
    assert!(!runtime.is_inflight(first));
    wait::until("the second request was retired by its own result", || {
        let _ = runtime.poll();
        !runtime.is_inflight(second)
    });
    assert!(!runtime.is_inflight(first));
    assert!(!runtime.is_inflight(second));
}

#[test]
fn local_package_read_runs_off_the_producer_lane_and_survives_a_root_advance() -> Result<(), String>
{
    let folder = std::env::temp_dir().join(format!(
        "nudox-runtime-local-package-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos())
    ));
    std::fs::create_dir_all(&folder).map_err(|error| error.to_string())?;
    std::fs::write(
        folder.join("Cargo.toml"),
        "[package]\nname = \"runtime-fixture\"\nversion = \"3.1.4\"\n[dependencies]\nserde = \"1\"\n",
    )
    .map_err(|error| error.to_string())?;
    let project =
        crate::core::LocalProjectId::from_path(&folder).map_err(|error| error.to_string())?;
    let actor = super::actor::EngineActor::start_with_loader(
        EchoClient,
        2,
        crate::model::LocalPackageLoader::without_cargo(),
    )
    .map_err(|error| error.to_string())?;
    let mut runtime = DesktopRuntime::new(snapshot(), actor);
    let basis = runtime.snapshot().key();
    let local = RequestId::new(30);
    runtime.dispatch(Intent::RefreshLocalPackage {
        project: project.clone(),
        basis,
        request: local,
    });
    // A root advance while the read is in flight must not discard it: local
    // manifest facts are not producer-root state.
    let root = RequestId::new(31);
    runtime.dispatch(Intent::RefreshRoot {
        basis,
        request: root,
    });
    wait::until("the local package read and the root both landed", || {
        runtime.poll();
        !(runtime.is_inflight(local) || runtime.is_inflight(root))
    });
    let _removed = std::fs::remove_dir_all(&folder);
    let snapshot = runtime.snapshot();
    assert!(
        snapshot.key().generation() > basis.generation(),
        "root advanced"
    );
    let package = snapshot
        .local_package()
        .loaded_value()
        .ok_or("local package was not admitted after the root advanced")?;
    assert_eq!(package.project, project);
    assert_eq!(package.name.as_ref(), "runtime-fixture");
    assert_eq!(package.version.as_deref(), Some("3.1.4"));
    assert_eq!(package.dependencies.len(), 1);
    Ok(())
}

/// A registry release fixture shaped exactly like the producer's DTO.
pub(super) fn registry_record(name: &str, version: &str) -> backend_library::RegistryPackageRecord {
    use backend_library::{
        AdvisoryPackageDto, PackageReference, ProductText, RegistryDownloadCount,
        RegistryEcosystem, RegistryNativeMetadata, RegistryPackageRecord, RegistryReleaseStanding,
    };
    let native_metadata = RegistryNativeMetadata::unavailable(RegistryEcosystem::Cargo, "fixture");
    RegistryPackageRecord {
        coordinate: PackageReference::parse(format!("pkg:cargo/{name}@{version}"))
            .expect("fixture package"),
        ecosystem: RegistryEcosystem::Cargo,
        name: ProductText::new(name).expect("fixture name"),
        version: ProductText::new(version).expect("fixture version"),
        bytes: 1_024,
        standing: RegistryReleaseStanding::Available,
        downloads: RegistryDownloadCount::Exact(42),
        facts_version: [0; 32],
        authority: None,
        native_metadata_version: native_metadata
            .identity()
            .expect("fixture metadata identity"),
        native_metadata,
        forge_sources: Box::new([]),
        advisory: AdvisoryPackageDto::unknown(),
    }
}

/// Answers the root with a three-package Orbit catalog and a package
/// surface with that one package — the exact replies a package visit sees.
struct CatalogClient;

impl EngineClient for CatalogClient {
    fn execute(&mut self, request: &EngineRequest) -> Result<EngineDto, EngineFault> {
        match request {
            EngineRequest::Root { request, basis, .. } => {
                let (key, revision) = next_key(*basis);
                let catalog = ["alpha", "beta", "gamma"]
                    .iter()
                    .filter_map(|name| {
                        super::client::package_summary(&registry_record(name, "1.0.0"))
                    })
                    .collect::<Vec<_>>();
                Ok(EngineDto::Root {
                    request: *request,
                    basis: *basis,
                    key,
                    revision,
                    delta: None,
                    project: None,
                    catalog: Some(catalog.into()),
                })
            }
            EngineRequest::Surface {
                request,
                basis,
                command,
                ..
            } => Ok(EngineDto::Surface {
                request: *request,
                basis: *basis,
                command: command.clone(),
                reply: match command {
                    backend_library::SurfaceCommand::PackageVersions { .. } => {
                        backend_library::SurfaceReply::PackageVersions(Box::new([
                            registry_record("beta", "0.9.0"),
                            registry_record("beta", "1.0.0"),
                        ]))
                    }
                    _ => backend_library::SurfaceReply::Package(Box::new([registry_record(
                        "beta", "1.0.0",
                    )])),
                },
            }),
            other => EchoClient.execute(other),
        }
    }
}

fn poll_until(runtime: &mut DesktopRuntime, request: RequestId) {
    wait::until(format!("request {request:?} landed"), || {
        runtime.poll();
        !runtime.is_inflight(request)
    });
}

fn catalog_names(runtime: &DesktopRuntime) -> Vec<String> {
    runtime
        .snapshot()
        .catalog()
        .loaded_value()
        .map(|catalog| {
            catalog
                .packages
                .iter()
                .map(|package| package.name.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// P0 regression: visiting a package (its Overview lane issues `package`,
/// its Releases lane `package-versions`) must never replace the Orbit
/// catalog with that one package's rows.
#[test]
fn visiting_a_package_never_overwrites_the_orbit_catalog() {
    let actor = super::actor::EngineActor::start(CatalogClient, 4).expect("actor thread");
    let mut runtime = DesktopRuntime::new(snapshot(), actor);
    let request = runtime.allocate_request();
    let basis = runtime.snapshot().key();
    runtime.dispatch(Intent::RefreshRoot { basis, request });
    poll_until(&mut runtime, request);
    assert_eq!(catalog_names(&runtime), ["alpha", "beta", "gamma"]);

    let package = backend_library::PackageReference::parse("pkg:cargo/beta@1.0.0")
        .expect("package reference");
    for command in [
        backend_library::SurfaceCommand::Package {
            package: package.clone(),
        },
        backend_library::SurfaceCommand::PackageVersions { package },
    ] {
        let request = runtime.allocate_request();
        let basis = runtime.snapshot().key();
        runtime.dispatch(Intent::RefreshSurface {
            command,
            basis,
            request,
        });
        poll_until(&mut runtime, request);
        assert_eq!(
            catalog_names(&runtime),
            ["alpha", "beta", "gamma"],
            "a package surface reply replaced the Orbit catalog"
        );
    }
}
