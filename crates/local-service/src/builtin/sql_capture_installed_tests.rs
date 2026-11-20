//! Boundary tests: no parser output is simulated as native SQL evidence.
use super::*;
use std::sync::atomic::AtomicBool;

#[cfg(target_os = "linux")]
fn fixture(root: &Path) -> (SqliteWorkerPermit, PathBuf) {
    use std::os::unix::fs::PermissionsExt as _;
    let worker = root.join("backend-sqlite-worker");
    let library = root.join("libgcc_s.so.1");
    std::fs::write(&worker, b"not executed by these boundary tests").unwrap();
    std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(&library, b"captured dependency bytes").unwrap();
    let identity = backend_compile::ExecutableIdentity::from_path(&library).unwrap();
    let artifact = ToolchainArtifact::from_path(&worker, vec![identity.digest()]).unwrap();
    let basis = backend_sql_contract::ProducerBasis {
        engine: Engine::Syntaqlite4e4de76,
        native_revision: "4e4de76312429fb2235ae093ab2125914320d5bd".into(),
        recipe: "syntaqlite-sqlite-fmt-analysis-dqs-default-v1".into(),
        lock_sha256: [2; 32],
        support_sha256: Some([3; 32]),
        settings: backend_sql_contract::EffectiveSettings::Sqlite {
            version: i32::MAX,
            cflags: 0,
            analyzer_dqs_fallback: true,
        },
    };
    let wrong = ToolchainArtifact::from_path(&worker, vec![]).unwrap();
    assert!(
        SqliteWorkerPermit::from_admitted_with_linux_library(
            worker.clone(),
            wrong,
            basis.clone(),
            library.clone(),
            identity.clone(),
        )
        .is_err()
    );
    let permit = SqliteWorkerPermit::from_admitted_with_linux_library(
        worker,
        artifact,
        basis,
        library.clone(),
        identity,
    )
    .unwrap();
    (permit, library)
}

#[cfg(target_os = "linux")]
#[test]
fn installed_sqlite_revalidates_private_dependency_before_every_host_launch() {
    let root = tempfile::tempdir().unwrap();
    let (permit, library) = fixture(root.path());
    let source = b"SELECT 1";
    let request = Request {
        version: backend_sql_contract::PROTOCOL_VERSION,
        expected_basis: Some(permit.basis.clone()),
        source: source.to_vec(),
        settings: Settings::ProcessDefaults,
        context: None,
    };
    let cancellation = AtomicBool::new(false);
    let launch = || CheckedLaunch {
        program: &permit.program,
        workspace: root.path(),
        artifact: permit.artifact.clone(),
        environment: ProcessEnvironment::new(vec![]).unwrap(),
        limits: ProcessLimits::new(1024 * 1024, 1024, Duration::from_secs(3), 1024 * 1024).unwrap(),
        cancellation: &cancellation,
    };
    std::fs::write(&library, b"dependency changed after installed selection").unwrap();
    let result = permit.analyze(
        &request,
        source,
        backend_compile::typed_of::<backend_compile::InputContentSchema>(source),
        backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
            source,
        ),
        launch(),
    );
    assert!(matches!(
        result,
        Err(backend_sql_contract::CheckedError::Process(
            backend_compile::ProcessError::ExecutableDrift
        ))
    ));
    cancellation.store(true, std::sync::atomic::Ordering::Release);
    let result = permit.analyze(
        &request,
        source,
        backend_compile::typed_of::<backend_compile::InputContentSchema>(source),
        backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
            source,
        ),
        launch(),
    );
    assert!(matches!(
        result,
        Err(backend_sql_contract::CheckedError::Cancelled)
    ));
}
