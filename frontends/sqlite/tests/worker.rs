use backend_sql_contract::{Binding, Dialect, Request, Settings, Syntax};
use std::path::Path;
#[test]
fn sqlite_uses_shared_isolated_worker_frame() {
    let req = Request {
        expected_basis: None,
        version: backend_sql_contract::PROTOCOL_VERSION,
        source: "SELECT \"π\", \"π\" FROM [table];".as_bytes().to_vec(),
        settings: Settings::ProcessDefaults,
        context: None,
    };
    let got = backend_frontend_sqlite_experimental::analyze(
        Path::new(env!("CARGO_BIN_EXE_sqlite-worker")),
        &req,
    );
    assert_eq!(got.dialect, Dialect::Sqlite);
    assert_eq!(got.binding, Binding::MissingContext);
    assert!(matches!(got.syntax, Syntax::Parsed { statements: 1, .. }));
}

#[test]
fn proposed_catalog_worker_reports_native_unknown_column_and_epoch() {
    use backend_frontend_sqlite_experimental::{ProposedCatalog, ProposedTable};
    use backend_sql_contract::{DiagnosticKind, Extent, SemanticContext};
    let worker = Path::new(env!("CARGO_BIN_EXE_sqlite-worker"));
    let mut catalog = ProposedCatalog {
        epoch: 1,
        tables: vec![ProposedTable {
            name: "t".into(),
            columns: vec!["id".into(), "π".into()],
        }],
    };
    let payload = catalog.encode().unwrap();
    let mut req = Request {
        expected_basis: None,
        version: backend_sql_contract::PROTOCOL_VERSION,
        source: b"SELECT missing FROM t".to_vec(),
        settings: Settings::ProcessDefaults,
        context: Some(SemanticContext::Proposed {
            dialect: Dialect::Sqlite,
            payload,
        }),
    };
    let out = backend_frontend_sqlite_experimental::analyze(worker, &req);
    let Binding::ProposedAnalysis {
        context_sha256: first,
        diagnostics,
        ..
    } = out.binding
    else {
        panic!("missing proposed analysis")
    };
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].kind, DiagnosticKind::UnknownColumn);
    assert_eq!(diagnostics[0].extent, Some(Extent { start: 7, end: 14 }));
    catalog.epoch = 2;
    req.context = Some(SemanticContext::Proposed {
        dialect: Dialect::Sqlite,
        payload: catalog.encode().unwrap(),
    });
    let next = backend_frontend_sqlite_experimental::analyze(worker, &req);
    let Binding::ProposedAnalysis {
        context_sha256: second,
        ..
    } = next.binding
    else {
        panic!("missing second analysis")
    };
    assert_ne!(first, second);
    req.source = b"SELECT \"missing\" FROM t".to_vec();
    let quoted = backend_frontend_sqlite_experimental::analyze(worker, &req);
    let Binding::ProposedAnalysis { diagnostics, .. } = quoted.binding else {
        panic!("missing DQS analysis")
    };
    assert!(diagnostics.is_empty());
    req.source = b"CREATE TABLE q (id INT);".to_vec();
    let ddl = backend_frontend_sqlite_experimental::analyze(worker, &req);
    assert_eq!(ddl.binding, Binding::UnsupportedContext, "{ddl:?}");
    req.source = b"SELECT missing FROM t".to_vec();
    req.context = Some(SemanticContext::Proposed {
        dialect: Dialect::Sqlite,
        payload: b"{}".to_vec(),
    });
    let malformed = backend_frontend_sqlite_experimental::analyze(worker, &req);
    assert_eq!(malformed.binding, Binding::UnsupportedContext, "{malformed:?}");
}

#[test]
fn proposed_catalog_rejects_noncanonical_and_oversized_claims() {
    use backend_frontend_sqlite_experimental::{ProposedCatalog, ProposedTable};
    use backend_sql_contract::{MAX_CONTEXT_BYTES, SemanticContext};
    let worker = Path::new(env!("CARGO_BIN_EXE_sqlite-worker"));
    let unsorted = ProposedCatalog {
        epoch: 1,
        tables: vec![ProposedTable {
            name: "t".into(),
            columns: vec!["z".into(), "a".into()],
        }],
    };
    assert!(unsorted.encode().is_none());
    let req = Request {
        expected_basis: None,
        version: backend_sql_contract::PROTOCOL_VERSION,
        source: b"SELECT 1".to_vec(),
        settings: Settings::ProcessDefaults,
        context: Some(SemanticContext::Proposed {
            dialect: Dialect::Sqlite,
            payload: vec![b'x'; MAX_CONTEXT_BYTES + 1],
        }),
    };
    assert_eq!(
        backend_frontend_sqlite_experimental::analyze(worker, &req).syntax,
        Syntax::ResourceLimit
    );
}

#[test]
fn checked_sqlite_worker_rejects_wrong_source_and_config() {
    use backend_sql_contract::{
        Cancellation, CheckedError, CheckedLaunch, ProcessEnvironment, ProcessLimits,
        ToolchainArtifact, VerifiedSourceText,
    };
    use std::time::Duration;
    let host = b"SELECT id FROM t;";
    let verified = VerifiedSourceText::capture(host).unwrap();
    let input = verified.content();
    let source = verified.extent(0..host.len()).unwrap().source();
    let worker = Path::new(env!("CARGO_BIN_EXE_sqlite-worker"));
    let artifact = ToolchainArtifact::from_path(worker, vec![]).unwrap();
    let cwd = std::env::current_dir().unwrap();
    let limits = ProcessLimits::new(
        8 * 1024 * 1024,
        1024 * 1024,
        Duration::from_secs(10),
        9 * 1024 * 1024,
    )
    .unwrap();
    let (cancel, _) = Cancellation::new();
    let launch = || CheckedLaunch {
        program: worker,
        workspace: &cwd,
        artifact: artifact.clone(),
        environment: ProcessEnvironment::new(vec![]).unwrap(),
        limits,
        cancellation: &cancel,
    };
    let mut req = Request {
        version: backend_sql_contract::PROTOCOL_VERSION,
        expected_basis: Some(backend_frontend_sqlite_experimental::observed_basis()),
        source: host.to_vec(),
        settings: Settings::ProcessDefaults,
        context: None,
    };
    assert!(matches!(
        &req.expected_basis.as_ref().unwrap().settings,
        backend_sql_contract::EffectiveSettings::Sqlite {
            version: i32::MAX,
            cflags: 0,
            analyzer_dqs_fallback: true
        }
    ));
    let result =
        backend_frontend_sqlite_experimental::analyze_checked(&req, host, input, source, launch())
            .unwrap();
    assert_eq!(result.response().binding, Binding::MissingContext);
    let mut invalid_request = req.clone();
    invalid_request.source = vec![0xff];
    assert!(matches!(
        backend_frontend_sqlite_experimental::analyze_checked(
            &invalid_request,
            &[0xff],
            input,
            source,
            launch()
        ),
        Err(CheckedError::Source(
            backend_sql_contract::SourceVerificationError::NonUtf8
        ))
    ));
    let oversized = vec![b'x'; backend_sql_contract::MAX_SOURCE_BYTES + 1];
    let mut oversized_request = req.clone();
    oversized_request.source = oversized.clone();
    assert!(matches!(
        backend_frontend_sqlite_experimental::analyze_checked(
            &oversized_request,
            &oversized,
            input,
            source,
            launch()
        ),
        Err(CheckedError::InputLimit)
    ));

    let missing_host = b"SELECT missing FROM t;";
    let missing = VerifiedSourceText::capture(missing_host).unwrap();
    let catalog = backend_frontend_sqlite_experimental::ProposedCatalog {
        epoch: 7,
        tables: vec![backend_frontend_sqlite_experimental::ProposedTable {
            name: "t".into(),
            columns: vec!["id".into()],
        }],
    };
    let context_request = Request {
        version: backend_sql_contract::PROTOCOL_VERSION,
        expected_basis: Some(backend_frontend_sqlite_experimental::observed_basis()),
        source: missing_host.to_vec(),
        settings: Settings::ProcessDefaults,
        context: Some(backend_sql_contract::SemanticContext::Proposed {
            dialect: Dialect::Sqlite,
            payload: catalog.encode().unwrap(),
        }),
    };
    let checked = backend_frontend_sqlite_experimental::analyze_checked(
        &context_request,
        missing_host,
        missing.content(),
        missing.extent(0..missing_host.len()).unwrap().source(),
        launch(),
    )
    .unwrap();
    assert!(
        matches!(&checked.response().binding, Binding::ProposedAnalysis { diagnostics, .. } if diagnostics.len() == 1)
    );
    assert!(
        checked
            .extents()
            .iter()
            .any(|extent| extent.text() == "missing")
    );
    assert!(matches!(
        backend_frontend_sqlite_experimental::analyze_checked(
            &req,
            b"SELECT ix FROM t;",
            input,
            source,
            launch()
        ),
        Err(CheckedError::Source(_))
    ));
    if let backend_sql_contract::EffectiveSettings::Sqlite {
        analyzer_dqs_fallback,
        ..
    } = &mut req.expected_basis.as_mut().unwrap().settings
    {
        *analyzer_dqs_fallback = false;
    } else {
        panic!("wrong engine settings")
    }
    assert!(matches!(
        backend_frontend_sqlite_experimental::analyze_checked(&req, host, input, source, launch()),
        Err(CheckedError::UnsupportedSettings)
    ));
}

#[cfg(unix)]
#[test]
fn checked_supervisor_enforces_output_deadline_and_environment() {
    use backend_sql_contract::{
        Cancellation, CheckedError, CheckedLaunch, ProcessEnvironment, ProcessLimits,
        ProcessTerminal, ToolchainArtifact, VerifiedSourceText,
    };
    use std::{os::unix::fs::PermissionsExt, time::Duration};
    let host = b"SELECT 1;";
    let source_text = VerifiedSourceText::capture(host).unwrap();
    let input = source_text.content();
    let source = source_text.extent(0..host.len()).unwrap().source();
    let cwd = std::env::current_dir().unwrap();
    let worker = Path::new(env!("CARGO_BIN_EXE_sqlite-worker"));
    let artifact = ToolchainArtifact::from_path(worker, vec![]).unwrap();
    let (cancel, _) = Cancellation::new();
    let req = Request {
        version: backend_sql_contract::PROTOCOL_VERSION,
        expected_basis: Some(backend_frontend_sqlite_experimental::observed_basis()),
        source: host.to_vec(),
        settings: Settings::ProcessDefaults,
        context: None,
    };
    let output_limit = ProcessLimits::new(1, 65536, Duration::from_secs(10), 65537)
        .unwrap()
        .with_input_bytes_limit(1024 * 1024)
        .unwrap();
    let launch = CheckedLaunch {
        program: worker,
        workspace: &cwd,
        artifact: artifact.clone(),
        environment: ProcessEnvironment::new(vec![]).unwrap(),
        limits: output_limit,
        cancellation: &cancel,
    };
    assert!(matches!(
        backend_frontend_sqlite_experimental::analyze_checked(&req, host, input, source, launch),
        Err(CheckedError::Process(
            backend_sql_contract::ProcessError::OutputLimit
        )) | Err(CheckedError::Terminal(ProcessTerminal::OutputLimit))
    ));
    let limits = ProcessLimits::new(65536, 65536, Duration::from_secs(5), 131072).unwrap();
    let polluted = CheckedLaunch {
        program: worker,
        workspace: &cwd,
        artifact,
        environment: ProcessEnvironment::new(vec![("PGOPTIONS".into(), "ignored".into())]).unwrap(),
        limits,
        cancellation: &cancel,
    };
    assert!(matches!(
        backend_frontend_sqlite_experimental::analyze_checked(&req, host, input, source, polluted),
        Err(CheckedError::UnsupportedSettings)
    ));
    let script = std::env::temp_dir().join(format!("nudox-sql-deadline-{}", std::process::id()));
    std::fs::write(&script, "#!/bin/sh\nexec /bin/sleep 5\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let fake_artifact = ToolchainArtifact::from_path(&script, vec![]).unwrap();
    let deadline = ProcessLimits::new(65536, 65536, Duration::from_millis(100), 131072).unwrap();
    let launch = CheckedLaunch {
        program: &script,
        workspace: &cwd,
        artifact: fake_artifact,
        environment: ProcessEnvironment::new(vec![]).unwrap(),
        limits: deadline,
        cancellation: &cancel,
    };
    let result =
        backend_frontend_sqlite_experimental::analyze_checked(&req, host, input, source, launch);
    std::fs::remove_file(script).unwrap();
    assert!(matches!(
        result,
        Err(CheckedError::Process(
            backend_sql_contract::ProcessError::Deadline
        )) | Err(CheckedError::Terminal(ProcessTerminal::Deadline))
    ));
}
