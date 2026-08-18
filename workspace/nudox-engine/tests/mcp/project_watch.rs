use std::{path::PathBuf, time::Duration};

use nudox_engine::{
    ClientCommand, Engine, EngineConfig, JobEvent, ProducerLanguage, ProjectEvent, SyncEvent,
};
use nudox_ir::change::{EcosystemId, PackageLineageId, PackageName};

async fn next_event(rx: &flume::Receiver<ProjectEvent>) -> ProjectEvent {
    tokio::time::timeout(Duration::from_secs(3), rx.recv_async())
        .await
        .expect("project watcher must emit within three seconds")
        .expect("project watcher must remain connected")
}

#[tokio::test]
async fn project_resolution_discovers_manifests_and_watches_changes() {
    let root = tempfile::tempdir().expect("temporary project root");
    let manifest = root.path().join("Cargo.toml");
    std::fs::write(
        &manifest,
        "[package]\nname = \"probe\"\nversion = \"0.1.0\"\n",
    )
    .expect("write manifest");

    let engine = Engine::start_with_fixtures(EngineConfig::default());
    let (stream, rx) = engine.resolve_project(root.path().to_owned());

    assert!(matches!(
        next_event(&rx).await,
        ProjectEvent::Discovered { path, language }
            if path == manifest && language == ProducerLanguage::Rust
    ));
    assert!(matches!(next_event(&rx).await, ProjectEvent::Done { .. }));

    std::fs::write(
        &manifest,
        "[package]\nname = \"probe\"\nversion = \"0.2.0\"\n",
    )
    .expect("edit manifest");
    assert!(matches!(
        next_event(&rx).await,
        ProjectEvent::Changed { path } if path == manifest
    ));

    std::fs::remove_file(&manifest).expect("remove manifest");
    assert!(matches!(
        next_event(&rx).await,
        ProjectEvent::Removed { path } if path == manifest
    ));

    drop(stream);
    drop(engine);
}

#[tokio::test]
async fn project_resolution_reports_invalid_roots_without_claiming_success() {
    let engine = Engine::start_with_fixtures(EngineConfig::default());
    let root = tempfile::tempdir().expect("temporary parent");
    let missing = root.path().join("missing");
    let (_stream, rx) = engine.resolve_project(missing.clone());

    assert!(
        matches!(next_event(&rx).await, ProjectEvent::Error { message }
        if message.contains("not a directory"))
    );
    assert!(matches!(next_event(&rx).await, ProjectEvent::Done { .. }));
    drop(engine);
}

#[tokio::test]
async fn jobs_are_live_and_close_when_cancelled() {
    let engine = Engine::start_with_fixtures(EngineConfig::default());
    let (stream, rx) = engine.jobs();

    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(3), rx.recv_async())
            .await
            .expect("job snapshot must arrive")
            .expect("job stream must remain connected"),
        JobEvent::Idle
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), rx.recv_async())
            .await
            .is_err(),
        "job stream must stay live after its snapshot"
    );

    drop(stream);
    assert!(
        tokio::time::timeout(Duration::from_secs(3), rx.recv_async())
            .await
            .expect("cancellation must close the job stream")
            .is_err(),
        "job receiver must close after cancellation"
    );
    drop(engine);
}

#[tokio::test]
async fn open_package_stays_live_until_cancelled() {
    let engine = Engine::start_with_fixtures(EngineConfig::default());
    let package = PackageLineageId::new(EcosystemId::new("fixture"), PackageName::new("rich"));
    let (stream, rx) = engine.open_package(package, nudox_engine::wire::Gen(7));

    assert!(
        tokio::time::timeout(Duration::from_millis(100), rx.recv_async())
            .await
            .is_err(),
        "open package must not report an invented immediate completion"
    );

    drop(stream);
    assert!(
        tokio::time::timeout(Duration::from_secs(3), rx.recv_async())
            .await
            .expect("cancellation must close the package stream")
            .is_err(),
        "package receiver must close after cancellation"
    );
    drop(engine);
}

#[tokio::test]
async fn sync_command_schedules_work_and_reports_a_result() {
    let root = tempfile::tempdir().expect("temporary project root");
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname = \"sync_probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("manifest");
    std::fs::create_dir(root.path().join("src")).expect("src");
    std::fs::write(
        root.path().join("src/lib.rs"),
        "pub fn answer() -> u32 { 42 }\n",
    )
    .expect("source");

    let engine = Engine::start_with_fixtures(EngineConfig::default());
    let (_sync_stream, sync_rx) = engine.sync();
    let (_job_stream, job_rx) = engine.jobs();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(3), job_rx.recv_async())
            .await
            .expect("job snapshot")
            .expect("job stream"),
        JobEvent::Idle
    ));
    engine
        .command(ClientCommand::Sync {
            root: root.path().to_owned(),
        })
        .expect("sync command is accepted");

    let started = tokio::time::timeout(Duration::from_secs(3), sync_rx.recv_async())
        .await
        .expect("sync start")
        .expect("sync stream");
    let job_id = match started {
        SyncEvent::Started { job_id, .. } => job_id,
        other => panic!("expected sync start, got {other:?}"),
    };
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(3), job_rx.recv_async())
            .await
            .expect("job start")
            .expect("job stream"),
        JobEvent::Started { job_id: id, .. } if id == job_id
    ));

    let terminal = loop {
        match tokio::time::timeout(Duration::from_secs(60), sync_rx.recv_async())
            .await
            .expect("sync lifecycle")
            .expect("sync stream")
        {
            SyncEvent::Progress { job_id: id, .. } if id == job_id => continue,
            event @ (SyncEvent::Succeeded { .. }
            | SyncEvent::Failed { .. }
            | SyncEvent::Cancelled { .. }) => break event,
            other => panic!("unexpected sync event: {other:?}"),
        }
    };
    assert!(matches!(terminal, SyncEvent::Succeeded { job_id: id, loaded: 1 } if id == job_id));
}

#[tokio::test]
async fn sync_failure_is_typed_and_cancellation_is_a_command_effect() {
    let engine = Engine::start_with_fixtures(EngineConfig::default());
    let (_sync_stream, sync_rx) = engine.sync();
    let (_job_stream, job_rx) = engine.jobs();
    assert!(matches!(
        job_rx.recv_async().await.expect("job snapshot"),
        JobEvent::Idle
    ));
    engine
        .command(ClientCommand::Sync {
            root: PathBuf::from("/definitely/missing/nudox-sync-root"),
        })
        .expect("failed sync is still scheduled");
    let job_id = match sync_rx.recv_async().await.expect("sync start") {
        SyncEvent::Started { job_id, .. } => job_id,
        other => panic!("expected start, got {other:?}"),
    };
    assert!(matches!(
        sync_rx.recv_async().await.expect("sync failure"),
        SyncEvent::Failed { job_id: id, .. } if id == job_id
    ));
    assert!(matches!(
        job_rx.recv_async().await.expect("job start"),
        JobEvent::Started { job_id: id, .. } if id == job_id
    ));
    assert!(matches!(
        job_rx.recv_async().await.expect("job failure"),
        JobEvent::Failed { job_id: id, .. } if id == job_id
    ));

    let cancel_root = tempfile::tempdir().expect("cancellation root");
    let (_cancel_sync, cancel_rx) = engine.sync();
    let (_cancel_jobs, cancel_jobs_rx) = engine.jobs();
    assert!(matches!(
        cancel_jobs_rx.recv_async().await.expect("cancel snapshot"),
        JobEvent::Idle
    ));
    engine
        .command(ClientCommand::Sync {
            root: cancel_root.path().to_owned(),
        })
        .expect("cancellable sync is scheduled");
    let cancel_id = match cancel_rx.recv_async().await.expect("cancel start") {
        SyncEvent::Started { job_id, .. } => job_id,
        other => panic!("expected cancel start, got {other:?}"),
    };
    engine
        .command(ClientCommand::CancelJob { job_id: cancel_id })
        .expect("cancel command is accepted");
    assert!(matches!(
        cancel_rx.recv_async().await.expect("sync cancellation"),
        SyncEvent::Cancelled { job_id } if job_id == cancel_id
    ));
    assert!(matches!(
        cancel_jobs_rx.recv_async().await.expect("job cancellation"),
        JobEvent::Started { job_id, .. } if job_id == cancel_id
    ));
    assert!(matches!(
        cancel_jobs_rx.recv_async().await.expect("job cancellation"),
        JobEvent::Cancelled { job_id } if job_id == cancel_id
    ));

    // A command with a real target has an observable effect; the placeholder
    // command remains rejected rather than pretending to have run.
    assert!(engine.command(ClientCommand::Noop).is_err());
}
