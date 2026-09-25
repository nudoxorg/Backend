//! Runtime race tests kept next to the actor/coordinator boundary.

#![allow(clippy::expect_used, clippy::panic)]

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
    // The actor wakes the owner when the result lands; the owner never spins.
    let mut wake = runtime.take_wake().expect("wake signal");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while runtime.is_inflight(request) {
        if wake.try_take() {
            runtime.poll();
        } else {
            assert!(std::time::Instant::now() < deadline, "the root never landed");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
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
    // Replacing `first` retires it at once; its already-queued result then
    // drains as a stale rejection. `second` is only retired once the actor
    // has produced its own result, so poll until that happens instead of
    // stopping at the first non-empty drain, which is `first`'s leftover.
    assert!(!runtime.is_inflight(first));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while runtime.is_inflight(second) && std::time::Instant::now() < deadline {
        let _ = runtime.poll();
        std::thread::yield_now();
    }
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
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while (runtime.is_inflight(local) || runtime.is_inflight(root))
        && std::time::Instant::now() < deadline
    {
        runtime.poll();
        std::thread::yield_now();
    }
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
        RegistryEcosystem, RegistryNativeMetadata, RegistryPackageRecord,
        RegistryReleaseStanding,
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
    for _ in 0..10_000 {
        runtime.poll();
        if !runtime.is_inflight(request) {
            return;
        }
        std::thread::yield_now();
    }
    panic!("request {request:?} never landed");
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
