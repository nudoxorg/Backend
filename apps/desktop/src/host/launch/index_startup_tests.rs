//! An expired page seed cannot prevent the real owner from accepting an index.
//! The compiler environment is deliberately closed and empty. This proves
//! authenticated first send, durable terminal refusal and Retry, not successful
//! semantic compilation or native window acceptance.
//! Add occurs while Starting, before any index claim exists. Saved-preflight
//! publication/recovery lineage is covered by the controlled writer regressions.
//! The cold assertion reloads desktop persisted state while this service stays
//! alive; it does not prove a service-journal stop/reopen or native cold restart.
use super::*;
use crate::model::{IndexOperationClaim, ProjectPhase};
use crate::navigation::Intent;
use backend_client::Session;
use backend_library::{IndexOperationObservation, IndexOperationState};
use backend_local_service::{
    ClosedLocalHostEnvironmentSnapshot, EmbeddedLocalService, ProcessConfig,
};
use gpui::TestAppContext;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Fixture {
    scratch: PathBuf,
    gate: OwnerGate,
    service: Option<EmbeddedLocalService>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.gate.close();
        drop(self.service.take());
        // Retain diagnostics on a failed oracle; successful tests clean up
        // only after the GPUI quit hook has drained the real writer.
    }
}

fn offline_config(paths: &WorkspacePaths) -> TestResult<ProcessConfig> {
    let text = |path: &std::path::Path| {
        path.to_str()
            .map(str::to_owned)
            .ok_or("fixture path must be UTF-8")
    };
    let mut config = ProcessConfig::parse([
        "--endpoint".to_owned(),
        text(paths.endpoint())?,
        "--workspace".to_owned(),
        text(paths.data())?,
        "--authority-secret-file".to_owned(),
        text(paths.authority_secret())?,
        "--profile".to_owned(),
        "builtin".to_owned(),
        "--registry-offline".to_owned(),
        "--registry-discovery-offline".to_owned(),
        "--advisory-offline".to_owned(),
        "--forge-offline".to_owned(),
    ])?;
    config.compiler_environment = Some(ClosedLocalHostEnvironmentSnapshot::from_paths([])?);
    Ok(config)
}

fn wait_for_terminal(
    graph: &UiEntityGraph,
    cx: &mut TestAppContext,
    project: &LocalProjectId,
    previous: Option<&IndexOperationClaim>,
) -> TestResult<IndexOperationClaim> {
    let mut terminal = None;
    crate::runtime::wait::until(
        "the real owner returned an exact terminal operation",
        || {
            // The owner works on real threads, while this context's status
            // cadence uses its virtual clock. Pump the same keyed read the
            // desktop schedules after an Accepted/Active receipt.
            cx.executor().advance_clock(Duration::from_millis(500));
            cx.run_until_parked();
            terminal = graph.root.read_with(cx, |root, _| {
                let snapshot = root.snapshot();
                let row = snapshot
                    .workspace()
                    .projects
                    .iter()
                    .find(|row| row.id == *project)?;
                let claim = row.operation.as_ref()?;
                if previous.is_some_and(|old| old.key == claim.key)
                    || row.request.is_some()
                    || row.phase != ProjectPhase::Failed
                    || !claim.has_terminal_observation()
                {
                    return None;
                }
                Some(claim.clone())
            });
            terminal.is_some()
        },
    );
    terminal.ok_or_else(|| "terminal operation must retain its checked receipt".into())
}

fn assert_actual_receipt(session: &mut Session, claim: &IndexOperationClaim) -> TestResult {
    let observed = session.index_operation_status(claim.key)?;
    assert_eq!(claim.observation.as_ref(), Some(&observed), "the UI holds the exact terminal receipt returned by the real socket");
    assert!(
        claim.admits_observation(&observed),
        "the socket receipt owns the exact saved package, key and execution digest"
    );
    assert!(
        matches!(observed, IndexOperationObservation::Known(status)
        if matches!(status.state, IndexOperationState::Failed { .. })),
        "an empty selected compiler environment must produce a real terminal refusal"
    );
    Ok(())
}

#[test]
fn late_launch_snapshot_still_dispatches_a_real_index_and_retry() -> TestResult {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let scratch =
        crate::host::scratch_base().join(format!("nx-idx-{}-{nonce}", std::process::id()));
    let host = scratch.join("host");
    let data = scratch.join("data");
    let app = scratch.join("app");
    for directory in [&scratch, &host, &data, &app] {
        crate::host::private_dir(directory)?;
    }
    std::fs::write(
        app.join("package.json"),
        r#"{"name":"first-send-fixture","version":"1.0.0","private":true}"#,
    )?;
    std::fs::write(app.join("index.js"), "export const firstSend = 1;\n")?;
    let project = LocalProjectId::from_path(&app)?;
    let native = project.native_wire()?;
    let paths = WorkspacePaths::discover(
        Some(host),
        Some(data.clone()),
        Some(scratch.with_extension("sock")),
    )?;
    if paths.authority_secret().parent() != Some(paths.data()) {
        return Err(
            "this fixture cannot use an ambient authority credential outside its private workspace"
                .into(),
        );
    }
    paths.initialize()?;
    let mut cx = TestAppContext::single();
    cx.executor().allow_parking();
    let service = EmbeddedLocalService::start(offline_config(&paths)?)?;
    let mut proof = Session::connect(paths.endpoint())?;
    let revision = proof.revision()?;
    let key = VersionedRoot::from_revision(1, revision.cursor(), 0);
    let mut boot = prepare_window(Ok(paths));
    let gate = boot.gate.clone();
    let mut fixture = Fixture {
        scratch,
        gate: gate.clone(),
        service: Some(service),
    };
    let (late, seed) = mpsc::channel();
    boot.keep = Some(SnapshotRead {
        file: SnapshotFile::in_data(&data),
        seed,
    });
    let keep = boot.keep.take().ok_or("late launch snapshot")?.joined();
    assert!(keep.seed.is_none());
    assert!(
        late.send(None).is_err(),
        "the expired seed cannot become a later authority or overwrite the route"
    );
    let before = boot.snapshot.session().clone();
    let persistence = boot.persistence.clone().ok_or("real startup persistence")?;
    let actor = EngineActor::start(boot.client, 4)?;
    let graph = cx.update(|cx| {
        UiEntityGraph::install_with_bootstrap(
            cx,
            DesktopRuntime::new(boot.snapshot, actor),
            boot.persistence,
            None,
            Some(gate.clone()),
            Some(keep),
            boot.binding,
        )
    });
    graph.root.update(&mut cx, |root, cx| {
        root.dispatch(
            Intent::AddProject {
                project: project.clone(),
            },
            cx,
        )
    });
    gate.publish(OwnerState::Ready {
        key,
        mode: crate::model::ServiceMode::Embedded,
    });
    let first = wait_for_terminal(&graph, &mut cx, &project, None)?;
    assert_actual_receipt(&mut proof, &first)?;
    crate::runtime::wait::until("the complete first terminal claim is synchronized before Retry", || {
        persistence.load().is_ok_and(|state| state.shelf.iter().any(|row|
            row.local_path == project.as_str() && row.native_path.as_ref() == Some(&native)
                && row.phase == crate::model::PersistedProjectPhase::Failed
                && row.operation.as_ref() == Some(&first)))
    });
    graph.root.read_with(&cx, |root, _| {
        let snapshot = root.snapshot();
        assert_eq!(snapshot.route(), &before.route);
        assert_eq!(snapshot.session().back, before.back);
        assert_eq!(snapshot.session().forward, before.forward);
    });
    graph.root.update(&mut cx, |root, cx| {
        root.dispatch(Intent::RetryIndex(project.clone()), cx)
    });
    let second = wait_for_terminal(&graph, &mut cx, &project, Some(&first))?;
    assert_ne!(
        first.key, second.key,
        "explicit Retry owns a fresh durable attempt"
    );
    assert_actual_receipt(&mut proof, &second)?;
    crate::runtime::wait::until(
        "the real writer synchronizes the exact retry receipt",
        || {
            persistence.load().is_ok_and(|state| {
                state.shelf.iter().any(|row| {
                    row.local_path == project.as_str() && row.native_path.as_ref() == Some(&native)
                        && row.phase == crate::model::PersistedProjectPhase::Failed
                        && row.operation.as_ref() == Some(&second)
                })
            })
        },
    );
    gate.close();
    cx.quit();
    let disk = persistence.load()?;
    let row = disk
        .shelf
        .iter()
        .find(|row| row.local_path == project.as_str() && row.native_path.as_ref() == Some(&native))
        .ok_or("quit must synchronize the exact retry receipt")?;
    assert_eq!(row.phase, crate::model::PersistedProjectPhase::Failed);
    assert_eq!(row.operation.as_ref(), Some(&second));
    let cold = persistence.cold_workspace(&disk);
    let restored = cold.projects.iter().find(|row| row.id == project).ok_or("desktop persisted-state reload folder")?;
    assert_eq!(restored.phase, ProjectPhase::Failed);
    assert_eq!(restored.request, None, "a cold terminal receipt carries no ephemeral transport request");
    assert_eq!(restored.id.native_wire()?, native);
    assert_eq!(restored.operation.as_ref(), Some(&second));
    drop(graph);
    drop(cx);
    drop(proof);
    fixture
        .service
        .take()
        .ok_or("live fixture owner")?
        .close()?;
    std::fs::remove_dir_all(&fixture.scratch)?;
    Ok(())
}
