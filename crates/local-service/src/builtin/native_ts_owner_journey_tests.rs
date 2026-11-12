//! Native TS owner listener, selected evidence, cold reopen, and edit journeys.
use crate::{EmbeddedLocalService, ProcessConfig};
use backend_client::Session;
use backend_library::{
    CommandReply, CompileExecutionIntent, IndexOperationKey, IndexOperationObservation,
    IndexOperationState, PackageReference, ProductText, SurfaceCommand, SurfaceReply,
};
use std::sync::{Arc, atomic::Ordering};
use std::{
    error::Error,
    fs, io,
    path::Path,
    thread,
    time::{Duration, Instant},
};

pub(super) fn start_owner(
    paths: &backend_runtime::WorkspacePaths,
) -> Result<EmbeddedLocalService, Box<dyn Error>> {
    start_owner_with_frame_budget(paths, None)
}

fn start_owner_with_frame_budget(
    paths: &backend_runtime::WorkspacePaths,
    budget: Option<usize>,
) -> Result<EmbeddedLocalService, Box<dyn Error>> {
    let mut config = ProcessConfig::parse([
        "--endpoint".to_owned(),
        paths.endpoint().to_string_lossy().into_owned(),
        "--workspace".to_owned(),
        paths.data().to_string_lossy().into_owned(),
        "--registry-offline".to_owned(),
        "--registry-discovery-offline".to_owned(),
        "--advisory-offline".to_owned(),
        "--forge-offline".to_owned(),
    ])?;
    config.profile = "builtin".to_owned();
    if let Some(budget) = budget { config.listener.limits.max_frames_per_connection = budget; }
    config.worker_endpoint = None;
    config.authority_secret = Some(paths.authority_secret().to_path_buf());
    Ok(EmbeddedLocalService::start(config)?)
}

#[test]
fn native_ts_index_polling_reconnects_without_resubmitting() -> Result<(), Box<dyn Error>> {
    if crate::test_support::run_test_in_child_with_env(
        module_path!(), "native_ts_index_polling_reconnects_without_resubmitting",
        &[(backend_client::CONNECTION_FRAME_BUDGET_ENV, "1")],
    )? {
        return Ok(());
    }
    if backend_client::connection_frame_budget() != 1 {
        return Err("run with BACKEND_LOCALD_MAX_FRAMES_PER_CONNECTION=1".into());
    }
    let mut builder = tempfile::Builder::new();
    builder.prefix("tsx-poll-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    let fixture = builder.tempdir_in("/tmp")?;
    let project = fixture.path().join("project");
    fs::create_dir(&project)?;
    fs::write(project.join("view.ts"), "export function greeting() { return 1; }\n")?;
    let paths = backend_runtime::WorkspacePaths::discover(Some(fixture.path().join("owner")), Some(fixture.path().join("state")), Some(fixture.path().join("owner.sock")))?;
    paths.initialize()?;
    let owner = start_owner_with_frame_budget(&paths, Some(1))?;
    let mut session = Session::connect(owner.endpoint())?;
    index(&mut session, &project, 0x53)?;
    let key = IndexOperationKey::from_bytes([0x53; 32]).map_err(|e| io::Error::other(e.to_string()))?;
    assert!(matches!(poll_index_status(&mut session, key)?, IndexOperationObservation::Known(known) if matches!(known.state, IndexOperationState::Published(_))));
    drop(session);
    let report = owner.close()?;
    assert!(report.connections >= 2, "one-frame budget must rotate connections");
    assert_eq!(report.failures, 0);
    Ok(())
}

fn index(session: &mut Session, project: &Path, byte: u8) -> Result<(), Box<dyn Error>> {
    let package = PackageReference::parse(project.to_string_lossy().into_owned())
        .map_err(|e| io::Error::other(e.to_string()))?;
    let key =
        IndexOperationKey::from_bytes([byte; 32]).map_err(|e| io::Error::other(e.to_string()))?;
    eprintln!("native owner: start index request");
    let mut status = session.start_index_operation(key, package, CompileExecutionIntent::Interactive)
        .map_err(|e| io::Error::other(format!("start index request: {e:?}")))?;
    eprintln!("native owner: index accepted; polling status");
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        let IndexOperationObservation::Known(known) = status else {
            return Err("unknown index operation".into());
        };
        match known.state {
            IndexOperationState::Published(_) => {
                if session.connection_frames_remaining() == Some(0) {
                    session.reconnect().map_err(|e| io::Error::other(format!("reconnect after index: {e:?}")))?;
                }
                eprintln!("native owner: index published");
                return Ok(());
            },
            IndexOperationState::Accepted | IndexOperationState::Active { .. } => {}
            state => return Err(io::Error::other(format!("index refused: {state:?}")).into()),
        }
        if Instant::now() >= deadline {
            return Err("index deadline".into());
        }
        thread::sleep(Duration::from_millis(30));
        status = poll_index_status(session, key)?;
    }
}

pub(super) fn poll_index_status(session: &mut Session, key: IndexOperationKey) -> Result<IndexOperationObservation, Box<dyn Error>> {
    if session.connection_frames_remaining() == Some(0) {
        session.reconnect().map_err(|e| io::Error::other(format!("reconnect index status: {e:?}")))?;
    }
    session.index_operation_status(key).map_err(|e| io::Error::other(format!("index status request: {e:?}")).into())
}

/// Resolve the actual returned native Function row, then prove its selected
/// source path using the authenticated owner claim. Native-only rows may use
/// semantic-identity coordinates instead of path-shaped labels.
pub(super) fn select_native_document(
    session: &mut Session,
    authority: &[u8; 32],
    name: &str,
    expected_path: &str,
) -> Result<(String, backend_library::Document), Box<dyn Error>> {
    let CommandReply::Names(names) = session.names(name, 100)?.reply else {
        return Err("names reply".into());
    };
    let suffix = format!("::{name}");
    let candidates = names
        .root
        .rows()
        .iter()
        .filter(|row| {
            row.kind == Some(backend_library::DeclarationKind::Function)
                && row.label.ends_with(&suffix)
        })
        .filter_map(|row| match row.id {
            backend_library::RowId::Symbol(symbol) => Some((row.label.clone(), symbol, row.basis)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let names_basis = names.root.basis();
    let mut admitted = Vec::new();
    let mut refusals = Vec::new();
    for (coordinate, row_symbol, row_basis) in &candidates {
        let label = coordinate.chars().take(200).collect::<String>();
        let reply = match session.source(coordinate) {
            Ok(reply) => reply,
            Err(error) => {
                if refusals.len() < 20 {
                    refusals.push((
                        label,
                        format!("source request: {error:?}")
                            .chars()
                            .take(240)
                            .collect::<String>(),
                    ));
                }
                continue;
            }
        };
        let CommandReply::Document(document) = reply.reply else {
            if refusals.len() < 20 {
                refusals.push((label, "source reply is not Document".to_owned()));
            }
            continue;
        };
        let Some(selection) = document.selection.as_ref() else {
            if refusals.len() < 20 {
                refusals.push((label, "no selected source identity".to_owned()));
            }
            continue;
        };
        if selection.coordinate() != coordinate
            || selection.symbol() != *row_symbol
            || selection.symbol() != document.symbol
            || selection.source() != *row_basis
            || selection.source() != names_basis
            || document.source_basis() != Some(selection.source())
        {
            if refusals.len() < 20 {
                refusals.push((
                    label,
                    "selected coordinate/symbol/basis mismatch".to_owned(),
                ));
            }
            continue;
        }
        if let Err(error) = document.source_policy.verify_owner(authority, selection) {
            if refusals.len() < 20 {
                refusals.push((
                    label,
                    format!("owner policy: {error:?}")
                        .chars()
                        .take(240)
                        .collect::<String>(),
                ));
            }
            continue;
        }
        let policy = serde_json::to_value(&document.source_policy)?;
        if policy["state"] == "captured_observed"
            && policy["data"]["statement"]["source_path"] == expected_path
        {
            admitted.push((coordinate.clone(), document));
        } else if refusals.len() < 20 {
            refusals.push((
                label,
                format!(
                    "policy state={} source_path={}",
                    policy["state"], policy["data"]["statement"]["source_path"]
                )
                .chars()
                .take(240)
                .collect::<String>(),
            ));
        }
    }
    if admitted.len() != 1 {
        let rows = names
            .root
            .rows()
            .iter()
            .take(20)
            .map(|row| {
                (
                    row.label.chars().take(200).collect::<String>(),
                    row.kind,
                    format!("{:?}", row.source)
                        .chars()
                        .take(200)
                        .collect::<String>(),
                )
            })
            .collect::<Vec<_>>();
        return Err(io::Error::other(format!("expected one source-proved {name} in {expected_path}; admitted={} refusals={refusals:?} rows={rows:?} coverage={:?}", admitted.len(), names.root.coverage())).into());
    }
    Ok(admitted.remove(0))
}

fn query(
    session: &mut Session,
    authority: &[u8; 32],
    expected: &str,
    consumer: &str,
    require_references: bool,
) -> Result<serde_json::Value, Box<dyn Error>> {
    let (coordinate, document) = select_native_document(session, authority, "Greeting", "view.ts")?;
    assert!(
        document
            .excerpt
            .text()
            .is_some_and(|text| text.contains(expected))
    );
    let selection = document
        .selection
        .as_ref()
        .ok_or("selected native document")?;
    document
        .source_policy
        .verify_owner(authority, selection)
        .map_err(|error| io::Error::other(format!("{error:?}")))?;
    let policy = serde_json::to_value(&document.source_policy)?;
    assert_eq!(policy["state"], "captured_observed");
    assert_eq!(policy["data"]["statement"]["policy_stability"], "unknown");
    if require_references {
        let target =
            ProductText::new(coordinate.clone()).map_err(|e| io::Error::other(e.to_string()))?;
        let SurfaceReply::References { references, .. } =
            session.surface(SurfaceCommand::References { target })?
        else {
            return Err("references reply".into());
        };
        assert!(references.iter().any(|item| {
            item.evidence.source.as_ref().is_some_and(|span| {
                span.file.as_str().ends_with("consumer.ts")
                    && consumer.get(span.start as usize..span.end as usize) == Some("Greeting")
            })
        }));
    }
    Ok(policy["data"]["statement"]["source_fact"].clone())
}

pub(super) struct FrontendFailpoint;
impl Drop for FrontendFailpoint {
    fn drop(&mut self) {
        super::ingest::TEST_REJECT_STRUCTURAL_FRONTENDS.store(false, Ordering::Release);
    }
}

pub(super) fn selected_evidence_root(
    workspace: &Path,
    project: &Path,
    expected: &[(&str, backend_semantic::vocabulary::TypeScriptSource)],
) -> Result<[u8; 32], Box<dyn Error>> {
    let profile = super::profile_descriptor(super::BuiltinProfile::Product)?;
    let dispatcher = super::builtin_dispatcher(
        Some(super::ECHO_AUTHORITY_SECRET),
        Arc::clone(&profile),
        60_000,
    )?;
    let registry = super::product_relation_registry()?;
    let daemon = crate::Locald::open_with_dispatcher_and_registry(
        workspace,
        super::BuiltinModel,
        super::genesis()?,
        dispatcher,
        backend_engine::DaemonConfig::default(),
        registry,
    )?;
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let sources = super::read_indexed_sources(&snapshot)?;
    let canonical = project.canonicalize()?;
    let key = backend_engine::PackageKey::from_value(canonical.to_str().ok_or("project path")?)
        .to_bytes();
    let selected = sources.projects.get(&key).ok_or("selected Project row")?;
    assert!(
        selected.files.is_empty(),
        "TS-native Project must select no structural File rows"
    );
    assert!(!sources.files.iter().any(|(_, row)| matches!(row, backend_engine::ProductSourceRecord::File { project, .. } if *project == key)));
    let evidence = backend_engine::builtin::native_source_evidence_relation(&snapshot)?
        .ok_or("selected native evidence")?;
    for &(path, source_profile) in expected {
        let profile = backend_semantic::vocabulary::LanguageProfile::TypeScript(source_profile);
        let expected_bytes = fs::read(project.join(path))?;
        let row_key = backend_engine::builtin::native_source_evidence_key(key, profile, path)
            .map_err(io::Error::other)?;
        let row = evidence
            .lookup(&row_key)?
            .ok_or("selected native source evidence row")?;
        let source_id =
            backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                &expected_bytes,
            );
        assert_eq!(row.source_digest, *source_id.as_ref());
        assert_eq!(row.byte_length as usize, expected_bytes.len());
        assert_eq!(row.path, path);
    }
    Ok(*evidence.root().as_bytes())
}

fn assert_legacy_coordinate_routes(
    session: &mut Session,
    project: &Path,
    expected: backend_library::SymbolKey,
) -> Result<(), Box<dyn Error>> {
    let coordinate = format!("{}::view.ts:3::Greeting", project.display());
    for reply in [session.document(&coordinate)?, session.source(&coordinate)?] {
        let CommandReply::Document(document) = reply.reply else { return Err("legacy document reply".into()) };
        assert_eq!(document.symbol, expected);
        assert_eq!(document.selection.as_ref().ok_or("legacy selection")?.coordinate(), coordinate);
    }
    for reply in [session.graph(&coordinate)?, session.related(&coordinate)?] {
        let CommandReply::Graph(graph) = reply.reply else { return Err("legacy graph reply".into()) };
        assert!(graph.root.row_ref(backend_library::RowId::Symbol(expected)).is_some());
    }
    let locator = ProductText::new(coordinate.clone()).map_err(|e| io::Error::other(e.to_string()))?;
    let SurfaceReply::Read(records) = session.surface(SurfaceCommand::Read { locators: vec![locator.clone()].into_boxed_slice() })? else { return Err("legacy read reply".into()) };
    assert_eq!(records[0].stable_id, Some(expected.to_bytes()));
    assert_eq!(records[0].label.as_str(), coordinate);
    let SurfaceReply::References { target, .. } = session.surface(SurfaceCommand::References { target: locator })? else { return Err("legacy references reply".into()) };
    assert_eq!(target.as_str(), coordinate);
    let wrong = format!("{}::view.ts:4::Greeting", project.display());
    assert!(session.source(&wrong).is_err());
    Ok(())
}

fn native_ts_listener_journey(
    reject_frontends: bool,
    require_references: bool,
) -> Result<(), Box<dyn Error>> {
    let _guard = FrontendFailpoint;
    super::ingest::TEST_REJECT_STRUCTURAL_FRONTENDS.store(reject_frontends, Ordering::Release);
    let mut builder = tempfile::Builder::new();
    builder.prefix("tsx-nse-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    let fixture = builder.tempdir_in("/tmp")?;
    let project = fixture.path().join("project");
    fs::create_dir(&project)?;
    fs::write(
        project.join("tsconfig.json"),
        "{\"compilerOptions\":{\"strict\":true},\"include\":[\"*.ts\"]}",
    )?;
    let before = "// é\r\n// 漢\r\nexport function Greeting() {\r\n  return 'before';\r\n}\r\n";
    let after = "// é\r\n// 漢\r\nexport function Greeting() {\r\n  return 'after';\r\n}\r\n";
    let consumer = "import { Greeting } from './view';\nGreeting();\n";
    fs::write(project.join("view.ts"), before)?;
    fs::write(project.join("consumer.ts"), consumer)?;
    let paths = backend_runtime::WorkspacePaths::discover(
        Some(fixture.path().join("owner")),
        Some(fixture.path().join("state")),
        Some(fixture.path().join("owner.sock")),
    )?;
    paths.initialize()?;
    let authority = backend_engine::read_authority_secret(paths.authority_secret())?;
    let owner = start_owner(&paths)?;
    let mut session = Session::connect(owner.endpoint())?;
    index(&mut session, &project, 0x41)?;
    let (_, observed) = select_native_document(&mut session, &authority, "Greeting", "view.ts")?;
    let site = observed.location.captured().ok_or("observed native site")?;
    assert_eq!(site.path(), "view.ts");
    assert_eq!(site.start_line(), 3);
    assert_legacy_coordinate_routes(&mut session, &project, observed.symbol)?;
    assert!(observed.excerpt.text().is_some_and(|text| text.contains("return 'before'")));
    let original = match query(
        &mut session,
        &authority,
        "before",
        consumer,
        require_references,
    ) {
        Ok(original) => original,
        Err(error) => {
            drop(session);
            let close = owner.close();
            let retained = fixture.keep();
            eprintln!(
                "native TS diagnostic: private failed fixture retained at {}; owner close={close:?}",
                retained.display()
            );
            return Err(error);
        }
    };
    drop(session);
    assert_eq!(owner.close()?.failures, 0);
    let first_root = selected_evidence_root(
        paths.data(),
        &project,
        &[
            (
                "view.ts",
                backend_semantic::vocabulary::TypeScriptSource::TypeScript,
            ),
            (
                "consumer.ts",
                backend_semantic::vocabulary::TypeScriptSource::TypeScript,
            ),
        ],
    )?;
    let owner = start_owner(&paths)?;
    let mut session = Session::connect(owner.endpoint())?;
    let (_, cold_document) = select_native_document(&mut session, &authority, "Greeting", "view.ts")?;
    assert_legacy_coordinate_routes(&mut session, &project, cold_document.symbol)?;
    let cold = query(
        &mut session,
        &authority,
        "before",
        consumer,
        require_references,
    )?;
    assert_eq!(cold, original);
    index(&mut session, &project, 0x43)?;
    assert_eq!(
        query(
            &mut session,
            &authority,
            "before",
            consumer,
            require_references
        )?,
        original
    );
    drop(session);
    assert_eq!(owner.close()?.failures, 0);
    assert_eq!(
        selected_evidence_root(
            paths.data(),
            &project,
            &[
                (
                    "view.ts",
                    backend_semantic::vocabulary::TypeScriptSource::TypeScript
                ),
                (
                    "consumer.ts",
                    backend_semantic::vocabulary::TypeScriptSource::TypeScript
                )
            ]
        )?,
        first_root,
        "identical reindex must retain the selected native evidence root"
    );
    let owner = start_owner(&paths)?;
    let mut session = Session::connect(owner.endpoint())?;
    fs::write(project.join("view.ts"), after)?;
    let stale_coordinate = format!("{}::view.ts:3::Greeting", project.display());
    assert!(session.source(&stale_coordinate).is_err());
    assert!(session.document(&stale_coordinate).is_err());
    let stale_locator = ProductText::new(stale_coordinate).map_err(|e| io::Error::other(e.to_string()))?;
    assert!(session.surface(SurfaceCommand::Read { locators: vec![stale_locator].into_boxed_slice() }).is_err());
    index(&mut session, &project, 0x42)?;
    let edited = query(
        &mut session,
        &authority,
        "after",
        consumer,
        require_references,
    )?;
    assert_ne!(edited, original);
    drop(session);
    assert_eq!(owner.close()?.failures, 0);
    assert_ne!(
        selected_evidence_root(
            paths.data(),
            &project,
            &[
                (
                    "view.ts",
                    backend_semantic::vocabulary::TypeScriptSource::TypeScript
                ),
                (
                    "consumer.ts",
                    backend_semantic::vocabulary::TypeScriptSource::TypeScript
                )
            ]
        )?,
        first_root
    );
    Ok(())
}

#[test]
fn native_ts_listener_cold_reopen_and_edit_without_frontends() -> Result<(), Box<dyn Error>> {
    if crate::test_support::run_test_in_child(
        module_path!(),
        "native_ts_listener_cold_reopen_and_edit_without_frontends",
    )? {
        return Ok(());
    }
    native_ts_listener_journey(true, false)
}

#[test]
fn native_ts_listener_cold_reopen_and_edit_control() -> Result<(), Box<dyn Error>> {
    if crate::test_support::run_test_in_child(
        module_path!(),
        "native_ts_listener_cold_reopen_and_edit_control",
    )? {
        return Ok(());
    }
    native_ts_listener_journey(false, false)
}

#[test]
fn native_ts_listener_references_without_structural_frontends() -> Result<(), Box<dyn Error>> {
    if crate::test_support::run_test_in_child(
        module_path!(),
        "native_ts_listener_references_without_structural_frontends",
    )? {
        return Ok(());
    }
    native_ts_listener_journey(true, true)
}

#[test]
fn native_ts_listener_references_uninstrumented_control() -> Result<(), Box<dyn Error>> {
    if crate::test_support::run_test_in_child(
        module_path!(),
        "native_ts_listener_references_uninstrumented_control",
    )? {
        return Ok(());
    }
    native_ts_listener_journey(false, true)
}

#[test]
fn native_ts_owner_explicit_range_retained_selection_and_stale_refusal()
-> Result<(), Box<dyn Error>> {
    if crate::test_support::run_test_in_child(
        module_path!(),
        "native_ts_owner_explicit_range_retained_selection_and_stale_refusal",
    )? {
        return Ok(());
    }
    use backend_library::{
        DeclarationRead, DeclarationSelector, ReferenceCoverage, SelectedSymbolId,
        SourceAvailability, SourceRangeRequest, SourceRangeScope,
    };
    let _guard = FrontendFailpoint;
    super::ingest::TEST_REJECT_STRUCTURAL_FRONTENDS.store(true, Ordering::Release);
    let mut builder = tempfile::Builder::new();
    builder.prefix("tsx-range-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    let fixture = builder.tempdir_in("/tmp")?;
    let project = fixture.path().join("project");
    fs::create_dir(&project)?;
    fs::write(
        project.join("tsconfig.json"),
        "{\"compilerOptions\":{\"strict\":true},\"include\":[\"*.ts\"]}",
    )?;
    let before = "export async function Greeting(): Promise<string> { return 'before'; }\n";
    let expected_signature = "export async function Greeting(): Promise<string>";
    let consumer = "import { Greeting } from './view';\nGreeting();\n";
    fs::write(project.join("view.ts"), before)?;
    fs::write(project.join("consumer.ts"), consumer)?;
    let paths = backend_runtime::WorkspacePaths::discover(
        Some(fixture.path().join("owner")),
        Some(fixture.path().join("state")),
        Some(fixture.path().join("owner.sock")),
    )?;
    paths.initialize()?;
    let authority = backend_engine::read_authority_secret(paths.authority_secret())?;
    eprintln!("native owner: start warm owner");
    let owner = start_owner(&paths).map_err(|e| io::Error::other(format!("start warm owner: {e:?}")))?;
    let mut session = Session::connect(owner.endpoint()).map_err(|e| io::Error::other(format!("connect warm owner: {e:?}")))?;
    if let Err(error) = index(&mut session, &project, 0x51) {
        let running = owner.is_running();
        drop(session);
        let disposition = owner.close();
        return Err(io::Error::other(format!("warm index: {error:?}; owner running: {running}; owner close: {disposition:?}")).into());
    }
    eprintln!("native owner: select warm document");
    let (coordinate, document) =
        select_native_document(&mut session, &authority, "Greeting", "view.ts")
            .map_err(|e| io::Error::other(format!("select warm document: {e:?}")))?;
    assert_eq!(document.signature.as_deref(), Some(expected_signature));
    eprintln!("native owner: warm document");
    let CommandReply::Document(shown) = session.document(&coordinate).map_err(|e| io::Error::other(format!("warm document: {e:?}")))?.reply else {
        return Err("document reply".into());
    };
    assert_eq!(shown.signature.as_deref(), Some(expected_signature));
    let original = document
        .selection
        .clone()
        .ok_or("range selected document")?;
    document
        .source_policy
        .verify_owner(&authority, &original)
        .map_err(|error| io::Error::other(format!("{error:?}")))?;
    let selected_id =
        SelectedSymbolId::new(*original.symbol().as_bytes()).ok_or("selected symbol ID")?;
    let selector = DeclarationSelector::SelectedAt {
        symbol: selected_id,
        coordinate: original.coordinate().to_owned(),
    };
    let request = SourceRangeRequest {
        start: 0,
        limit: 12,
        source_identity: None,
    };
    eprintln!("native owner: first source range");
    let CommandReply::Document(ranged) = session
        .declaration_source_range(&selector, None, request).map_err(|e| io::Error::other(format!("first source range: {e:?}")))?
        .reply
    else {
        return Err("range page document".into());
    };
    assert_eq!(ranged.signature.as_deref(), Some(expected_signature));
    let page = ranged.source_range.as_ref().ok_or("range primary page")?;
    assert_eq!(page.scope, SourceRangeScope::NativePrimaryDeclaration);
    assert_eq!(page.start, 0);
    assert_eq!(
        before.get(page.owner_start as usize..(page.owner_start + page.end) as usize),
        Some(page.text.as_str())
    );
    assert!(page.end < page.total, "fixture must exercise continuation");
    assert_eq!(page.source_policy, ranged.source_policy);
    page.source_policy
        .verify_owner(
            &authority,
            ranged.selection.as_ref().ok_or("range selection")?,
        )
        .map_err(|error| io::Error::other(format!("{error:?}")))?;
    let continuation = SourceRangeRequest {
        start: page.end,
        limit: 12,
        source_identity: Some(page.source_identity),
    };
    eprintln!("native owner: source continuation");
    let CommandReply::Document(next) = session
        .declaration_source_range(&selector, None, continuation).map_err(|e| io::Error::other(format!("source continuation: {e:?}")))?
        .reply
    else {
        return Err("range continuation document".into());
    };
    assert_eq!(next.signature.as_deref(), Some(expected_signature));
    let next_page = next
        .source_range
        .as_ref()
        .ok_or("range continuation page")?;
    assert_eq!(next_page.start, continuation.start);
    assert_eq!(next_page.source_identity, page.source_identity);
    assert_eq!(next_page.source_policy, next.source_policy);
    next_page
        .source_policy
        .verify_owner(
            &authority,
            next.selection.as_ref().ok_or("continuation selection")?,
        )
        .map_err(|error| io::Error::other(format!("{error:?}")))?;
    let target =
        ProductText::new(coordinate.clone()).map_err(|e| io::Error::other(e.to_string()))?;
    eprintln!("native owner: references");
    let SurfaceReply::References { coverage, .. } =
        session.surface(SurfaceCommand::References { target }).map_err(|e| io::Error::other(format!("references: {e:?}")))?
    else {
        return Err("references coverage reply".into());
    };
    assert!(
        matches!(
            coverage,
            ReferenceCoverage::Partial { .. } | ReferenceCoverage::Unknown { .. }
        ),
        "unavailable native reference coverage cannot claim completeness"
    );
    drop(session);
    eprintln!("native owner: close warm owner");
    assert_eq!(owner.close()?.failures, 0);
    eprintln!("native owner: start cold owner");
    let owner = start_owner(&paths).map_err(|e| io::Error::other(format!("start cold owner: {e:?}")))?;
    let mut session = Session::connect(owner.endpoint()).map_err(|e| io::Error::other(format!("connect cold owner: {e:?}")))?;
    eprintln!("native owner: cold source");
    let CommandReply::Document(cold) = session
        .declaration(&selector, DeclarationRead::Source).map_err(|e| io::Error::other(format!("cold source: {e:?}")))?
        .reply
    else {
        return Err("cold retained selection reply".into());
    };
    assert_eq!(cold.signature.as_deref(), Some(expected_signature));
    assert_eq!(cold.selection.as_ref(), Some(&original));
    cold.source_policy
        .verify_owner(&authority, &original)
        .map_err(|error| io::Error::other(format!("{error:?}")))?;
    eprintln!("native owner: cold continuation");
    let CommandReply::Document(cold_range) = session
        .declaration_source_range(&selector, None, continuation).map_err(|e| io::Error::other(format!("cold continuation: {e:?}")))?
        .reply
    else {
        return Err("cold continuation reply".into());
    };
    assert_eq!(cold_range.signature.as_deref(), Some(expected_signature));
    assert_eq!(
        cold_range
            .source_range
            .as_ref()
            .ok_or("cold range page")?
            .source_identity,
        page.source_identity
    );
    fs::write(
        project.join("view.ts"),
        "export async function Greeting(): Promise<string> { return 'afters'; }\n",
    )?;
    eprintln!("native owner: stale continuation");
    if let Ok(reply) = session.declaration_source_range(&selector, None, continuation) {
        let CommandReply::Document(stale) = reply.reply else {
            return Err("stale range reply".into());
        };
        assert_ne!(stale.signature.as_deref(), Some(expected_signature));
        assert!(
            stale.source_range.is_none(),
            "changed host bytes must not yield a source page"
        );
        assert!(matches!(
            stale.location,
            SourceAvailability::StaleFile { .. }
                | SourceAvailability::NotHydrated
                | SourceAvailability::NotCaptured
                | SourceAvailability::Unconfigured
        ));
        assert!(
            stale.source_policy.is_unknown(),
            "stale bytes cannot retain captured-current policy"
        );
    }
    drop(session);
    eprintln!("native owner: close cold owner");
    assert_eq!(owner.close()?.failures, 0);
    Ok(())
}

#[test]
fn native_ts_read_owner_policy_and_same_length_stale_refusal() -> Result<(), Box<dyn Error>> {
    if crate::test_support::run_test_in_child(
        module_path!(),
        "native_ts_read_owner_policy_and_same_length_stale_refusal",
    )? {
        return Ok(());
    }
    let _guard = FrontendFailpoint;
    super::ingest::TEST_REJECT_STRUCTURAL_FRONTENDS.store(true, Ordering::Release);
    let mut builder = tempfile::Builder::new();
    builder.prefix("tsx-read-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    let fixture = builder.tempdir_in("/tmp")?;
    let project = fixture.path().join("project");
    fs::create_dir(&project)?;
    fs::write(
        project.join("tsconfig.json"),
        "{\"compilerOptions\":{\"strict\":true},\"include\":[\"*.ts\"]}",
    )?;
    let before = "export function Greeting() { return 'before'; }\n";
    let after = "export function Greeting() { return 'afters'; }\n";
    assert_eq!(before.len(), after.len());
    fs::write(project.join("view.ts"), before)?;
    let paths = backend_runtime::WorkspacePaths::discover(
        Some(fixture.path().join("owner")),
        Some(fixture.path().join("state")),
        Some(fixture.path().join("owner.sock")),
    )?;
    paths.initialize()?;
    let authority = backend_engine::read_authority_secret(paths.authority_secret())?;
    let owner = start_owner(&paths)?;
    let mut session = Session::connect(owner.endpoint())?;
    index(&mut session, &project, 0x71)?;
    let CommandReply::Names(names) = session.names("Greeting", 100)?.reply else {
        return Err("Read names reply".into());
    };
    assert!(
        names
            .root
            .coverage()
            .contains(&backend_library::Coverage::Complete),
        "generic Names traversal retains its completed status"
    );
    assert!(
        names
            .root
            .coverage()
            .contains(&backend_library::Coverage::Gapped {
                lane: backend_library::Lane::Semantic
            }),
        "observed native Names must retain its semantic source-scope gap"
    );
    let (coordinate, document) =
        select_native_document(&mut session, &authority, "Greeting", "view.ts")?;
    let selection = document
        .selection
        .as_ref()
        .ok_or("Read selected native document")?;
    let target =
        ProductText::new(coordinate.clone()).map_err(|e| io::Error::other(e.to_string()))?;
    let request = || SurfaceCommand::Read {
        locators: vec![target.clone()].into_boxed_slice(),
    };
    let SurfaceReply::Read(records) = session.surface(request())? else {
        return Err("Read reply".into());
    };
    let [record] = records.as_ref() else {
        return Err("Read result count".into());
    };
    record
        .source_policy
        .verify_owner(&authority, selection)
        .map_err(|e| io::Error::other(format!("{e:?}")))?;
    assert!(
        record.extent.is_some(),
        "fresh source Read must be captured, not signature-only"
    );
    assert!(
        record
            .signature
            .as_ref()
            .is_some_and(|text| text.as_str().contains("before"))
    );
    let policy = serde_json::to_value(&record.source_policy)?;
    assert_eq!(policy["data"]["statement"]["policy_stability"], "unknown");
    fs::write(project.join("view.ts"), after)?;
    if let Ok(reply) = session.surface(request()) {
        let SurfaceReply::Read(stale) = reply else {
            return Err("stale Read shape".into());
        };
        let [record] = stale.as_ref() else {
            return Err("stale Read result count".into());
        };
        assert!(
            record.source_policy.is_unknown(),
            "changed host bytes cannot retain a current captured claim"
        );
        assert!(
            record.extent.is_none(),
            "changed host bytes cannot return a captured source extent"
        );
    }
    drop(session);
    assert_eq!(owner.close()?.failures, 0);
    Ok(())
}

#[test]
fn native_ts_health_without_structural_frontends() -> Result<(), Box<dyn Error>> {
    if crate::test_support::run_test_in_child(
        module_path!(),
        "native_ts_health_without_structural_frontends",
    )? {
        return Ok(());
    }
    let _guard = FrontendFailpoint;
    super::ingest::TEST_REJECT_STRUCTURAL_FRONTENDS.store(true, Ordering::Release);
    let mut builder = tempfile::Builder::new();
    builder.prefix("tsx-health-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    let fixture = builder.tempdir_in("/tmp")?;
    let project = fixture.path().join("project");
    fs::create_dir(&project)?;
    fs::write(
        project.join("tsconfig.json"),
        "{\"compilerOptions\":{\"strict\":true},\"include\":[\"*.ts\"]}",
    )?;
    fs::write(
        project.join("view.ts"),
        "export const answer: number = 42;\n",
    )?;
    let paths = backend_runtime::WorkspacePaths::discover(
        Some(fixture.path().join("owner")),
        Some(fixture.path().join("state")),
        Some(fixture.path().join("owner.sock")),
    )?;
    paths.initialize()?;
    let owner = start_owner(&paths)?;
    let mut session = Session::connect(owner.endpoint())?;
    let initial = session.health()?;
    index(&mut session, &project, 0x61)?;
    let after = session.health()?;
    let select = |report: &backend_library::HealthReport, native: bool| {
        report
            .capabilities()
            .as_slice()
            .iter()
            .copied()
            .filter(|row| match row.family() {
                backend_library::CapabilityFamily::LanguageOracle { profile, .. } if native => {
                    matches!(
                        profile,
                        backend_semantic::vocabulary::LanguageProfile::TypeScript(_)
                    )
                }
                backend_library::CapabilityFamily::StructuralFrontend { .. } if !native => true,
                _ => false,
            })
            .collect::<Vec<_>>()
    };
    let initial_native = select(&initial, true);
    assert_eq!(
        initial_native.len(),
        4,
        "TS and TSX type-check and semantic-index capability rows must be retained"
    );
    assert!(
        initial_native.iter().all(|row| row.manifest().is_some()
            && row.lifecycle() == backend_library::CapabilityLifecycle::Active
            && matches!(
                row.authority(),
                Some(backend_library::CapabilityAuthority::Compiler {
                    toolchain: backend_semantic::vocabulary::NativeTool::TypeScriptCompiler,
                    ..
                })
            )),
        "linked native TypeScript must retain its configured active authority without claiming a fresh readiness probe: {initial_native:?}"
    );
    assert_eq!(
        select(&after, true),
        initial_native,
        "indexing must preserve native compiler capability"
    );
    assert_eq!(
        select(&after, false),
        select(&initial, false),
        "uninitialized structural registry must not rewrite its baseline"
    );
    drop(session);
    assert_eq!(owner.close()?.failures, 0);
    Ok(())
}

fn assert_retired_native_evidence(
    workspace: &Path,
    project: &Path,
    retired: &str,
) -> Result<(), Box<dyn Error>> {
    let profile = super::profile_descriptor(super::BuiltinProfile::Product)?;
    let dispatcher = super::builtin_dispatcher(
        Some(super::ECHO_AUTHORITY_SECRET),
        Arc::clone(&profile),
        60_000,
    )?;
    let registry = super::product_relation_registry()?;
    let daemon = crate::Locald::open_with_dispatcher_and_registry(
        workspace,
        super::BuiltinModel,
        super::genesis()?,
        dispatcher,
        backend_engine::DaemonConfig::default(),
        registry,
    )?;
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let sources = super::read_indexed_sources(&snapshot)?;
    let canonical = project.canonicalize()?;
    let project_key =
        backend_engine::PackageKey::from_value(canonical.to_str().ok_or("project path")?)
            .to_bytes();
    let project_row = sources
        .projects
        .get(&project_key)
        .ok_or("selected Project")?;
    assert!(
        project_row.files.is_empty(),
        "native Project must retain no structural File inventory"
    );
    let evidence = backend_engine::builtin::native_source_evidence_relation(&snapshot)?
        .ok_or("selected native evidence")?;
    let profile = backend_semantic::vocabulary::LanguageProfile::TypeScript(
        backend_semantic::vocabulary::TypeScriptSource::TypeScript,
    );
    let old_key =
        backend_engine::builtin::native_source_evidence_key(project_key, profile, retired)
            .map_err(io::Error::other)?;
    assert!(
        evidence.lookup(&old_key)?.is_none(),
        "omitted path must have no selected NSE row"
    );
    let kept_key =
        backend_engine::builtin::native_source_evidence_key(project_key, profile, "other.ts")
            .map_err(io::Error::other)?;
    let kept = evidence
        .lookup(&kept_key)?
        .ok_or("retained native source evidence")?;
    let bytes = fs::read(project.join("other.ts"))?;
    let identity =
        backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
            &bytes,
        );
    assert_eq!(kept.source_digest, *identity.as_ref());
    assert_eq!(kept.byte_length as usize, bytes.len());
    Ok(())
}

fn assert_retired_handle_unavailable(
    session: &mut Session,
    selector: &backend_library::DeclarationSelector,
) -> Result<(), Box<dyn Error>> {
    match session.declaration(selector, backend_library::DeclarationRead::Source) {
        Ok(reply) => {
            let CommandReply::Document(document) = reply.reply else {
                return Err("retired handle reply shape".into());
            };
            assert!(
                document.source_policy.is_unknown(),
                "retired source cannot retain a captured policy claim"
            );
            assert!(
                document.excerpt.text().is_none(),
                "retired source cannot serve old captured bytes"
            );
            assert!(
                document.source_range.is_none(),
                "retired source cannot provide a current source page"
            );
        }
        Err(backend_client::ClientError::CommandFailed(
            backend_library::CommandFailure::NotFound
            | backend_library::CommandFailure::CursorMismatch,
        ))
        | Err(
            backend_client::ClientError::StaleSelection | backend_client::ClientError::StaleCursor,
        ) => {}
        Err(backend_client::ClientError::CommandFailed(
            backend_library::CommandFailure::InvalidQuery(detail),
        )) if detail.contains("selected source freshness could not be verified") => {}
        Err(error) => {
            return Err(
                io::Error::other(format!("unexpected retired-handle refusal: {error:?}")).into(),
            );
        }
    }
    Ok(())
}

fn native_ts_retirement_journey(exclude: bool, check_search: bool) -> Result<(), Box<dyn Error>> {
    let _guard = FrontendFailpoint;
    struct SearchDiagnosticGuard;
    impl Drop for SearchDiagnosticGuard {
        fn drop(&mut self) {
            crate::builtin::query::TEST_REPORT_UNJOINED_SEARCH_ROWS.store(false, Ordering::Release);
        }
    }
    let _search_guard = SearchDiagnosticGuard;
    crate::builtin::query::TEST_REPORT_UNJOINED_SEARCH_ROWS.store(check_search, Ordering::Release);
    super::ingest::TEST_REJECT_STRUCTURAL_FRONTENDS.store(true, Ordering::Release);
    let mut builder = tempfile::Builder::new();
    builder.prefix("tsx-retire-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(fs::Permissions::from_mode(0o700));
    }
    let fixture = builder.tempdir_in("/tmp")?;
    let project = fixture.path().join("project");
    fs::create_dir(&project)?;
    fs::write(
        project.join("tsconfig.json"),
        "{\"compilerOptions\":{\"strict\":true},\"include\":[\"*.ts\",\"*.tsx\"]}",
    )?;
    fs::write(
        project.join("view.ts"),
        "export function Retired() { return 'old'; }\n",
    )?;
    fs::write(
        project.join("other.ts"),
        "export function Kept() { return 'current'; }\n",
    )?;
    fs::write(project.join("component.tsx"), "export function Widget() { return 'widget'; }\n")?;
    let paths = backend_runtime::WorkspacePaths::discover(
        Some(fixture.path().join("owner")),
        Some(fixture.path().join("state")),
        Some(fixture.path().join("owner.sock")),
    )?;
    paths.initialize()?;
    let authority = backend_engine::read_authority_secret(paths.authority_secret())?;
    let owner = start_owner(&paths)?;
    let mut session = Session::connect(owner.endpoint())?;
    index(&mut session, &project, 0x91)?;
    let (old_coordinate, old_document) =
        select_native_document(&mut session, &authority, "Retired", "view.ts")?;
    let (_, kept_document) = select_native_document(&mut session, &authority, "Kept", "other.ts")?;
    if check_search {
        assert_native_search_and_query(&mut session, &kept_document)?;
        let (_, widget) = select_native_document(&mut session, &authority, "Widget", "component.tsx")?;
        let selected = widget.selection.as_ref().ok_or("selected TSX document")?;
        let CommandReply::Search(search) = session.search("Widget", 100)?.reply else { return Err("TSX search reply".into()) };
        assert!(search.root.rows().iter().any(|row| row.id == backend_library::RowId::Symbol(selected.symbol())));
    }
    assert!(
        old_document
            .excerpt
            .text()
            .is_some_and(|text| text.contains("old"))
    );
    assert!(
        kept_document
            .excerpt
            .text()
            .is_some_and(|text| text.contains("current"))
    );
    let old_selection = old_document
        .selection
        .as_ref()
        .ok_or("retired original selection")?;
    let old_symbol = backend_library::SelectedSymbolId::new(*old_selection.symbol().as_bytes())
        .ok_or("retired selected ID")?;
    let selector = backend_library::DeclarationSelector::SelectedAt {
        symbol: old_symbol,
        coordinate: old_coordinate,
    };
    if exclude {
        // The source bytes remain on disk; only the authoritative project
        // configuration omits them from the fresh same-profile Program.
        fs::write(
            project.join("tsconfig.json"),
            "{\"compilerOptions\":{\"strict\":true},\"include\":[\"*.ts\",\"*.tsx\"],\"exclude\":[\"view.ts\"]}",
        )?;
    } else {
        fs::remove_file(project.join("view.ts"))?;
    }
    index(&mut session, &project, 0x92)?;
    assert_retired_handle_unavailable(&mut session, &selector)?;
    let CommandReply::Names(retired_names) = session.names("Retired", 100)?.reply else {
        return Err("retired Names reply".into());
    };
    assert!(
        !retired_names
            .root
            .rows()
            .iter()
            .any(|row| row.id == backend_library::RowId::Symbol(old_selection.symbol())),
        "retired native declaration must leave selected Names"
    );
    let CommandReply::Names(names) = session.names("Kept", 100)?.reply else {
        return Err("retained Names reply".into());
    };
    assert!(
        names
            .root
            .coverage()
            .contains(&backend_library::Coverage::Gapped {
                lane: backend_library::Lane::Semantic
            }),
        "retirement does not establish complete semantic inventory"
    );
    let (_, kept) = select_native_document(&mut session, &authority, "Kept", "other.ts")?;
    if check_search {
        let CommandReply::Search(search) = session.search("Kept", 100)?.reply else {
            return Err("retained search reply".into());
        };
        let selection = kept.selection.as_ref().ok_or("retained selection")?;
        eprintln!("retirement search diagnostic: kept names={:?}; retired names={:?}; search={:?}",
            names.root.rows().iter().take(20).collect::<Vec<_>>(),
            retired_names.root.rows().iter().take(20).collect::<Vec<_>>(),
            search.root.rows().iter().take(20).collect::<Vec<_>>());
        assert!(search.root.rows().iter().any(|row| row.id == backend_library::RowId::Symbol(selection.symbol())),
            "search must include exact kept selected symbol after exclusion");
    }
    assert!(
        kept.excerpt
            .text()
            .is_some_and(|text| text.contains("current"))
    );
    drop(session);
    assert_eq!(owner.close()?.failures, 0);
    assert_retired_native_evidence(paths.data(), &project, "view.ts")?;
    let owner = start_owner(&paths)?;
    let mut cold = Session::connect(owner.endpoint())?;
    assert_retired_handle_unavailable(&mut cold, &selector)?;
    let (_, kept) = select_native_document(&mut cold, &authority, "Kept", "other.ts")?;
    if check_search {
        assert_native_search_and_query(&mut cold, &kept)?;
    }
    assert!(
        kept.excerpt
            .text()
            .is_some_and(|text| text.contains("current"))
    );
    drop(cold);
    assert_eq!(owner.close()?.failures, 0);
    assert_retired_native_evidence(paths.data(), &project, "view.ts")?;
    Ok(())
}

fn assert_native_search_and_query(
    session: &mut Session,
    document: &backend_library::Document,
) -> Result<(), Box<dyn Error>> {
    let selection = document.selection.as_ref().ok_or("selected native document")?;
    let CommandReply::Search(search) = session.search("Kept", 100)?.reply else {
        return Err("native search reply".into());
    };
    assert!(search.root.rows().iter().any(|row| row.id == backend_library::RowId::Symbol(selection.symbol())));
    let page = session.graph_query(
        "{ Declaration { name @filter(op: \"=\", value: [\"$name\"]) name @output state @output } }".to_owned(),
        std::collections::BTreeMap::from([("name".to_owned(), backend_library::GraphValue::String("Kept".to_owned()))]),
        100, None, false,
    )?;
    let rows = format!("{:?}", page.rows);
    assert!(rows.contains("Kept") && rows.contains("compiler_observed"), "{rows}");
    Ok(())
}

#[test]
fn native_ts_deleted_file_retires_evidence_and_old_handle() -> Result<(), Box<dyn Error>> {
    if crate::test_support::run_test_in_child(
        module_path!(),
        "native_ts_deleted_file_retires_evidence_and_old_handle",
    )? {
        return Ok(());
    }
    native_ts_retirement_journey(false, false)
}

#[test]
fn native_ts_excluded_file_retires_evidence_and_old_handle() -> Result<(), Box<dyn Error>> {
    if crate::test_support::run_test_in_child(
        module_path!(),
        "native_ts_excluded_file_retires_evidence_and_old_handle",
    )? {
        return Ok(());
    }
    native_ts_retirement_journey(true, false)
}

#[test]
fn native_ts_exclusion_kept_symbol_remains_searchable() -> Result<(), Box<dyn Error>> {
    if crate::test_support::run_test_in_child(
        module_path!(),
        "native_ts_exclusion_kept_symbol_remains_searchable",
    )? {
        return Ok(());
    }
    native_ts_retirement_journey(true, true)
}

#[test]
fn native_ts_last_file_deletion_retires_selected_evidence() -> Result<(), Box<dyn Error>> {
    if crate::test_support::run_test_in_child(
        module_path!(),
        "native_ts_last_file_deletion_retires_selected_evidence",
    )? {
        return Ok(());
    }
    let _guard = FrontendFailpoint;
    super::ingest::TEST_REJECT_STRUCTURAL_FRONTENDS.store(true, Ordering::Release);
    let fixture = tempfile::Builder::new().prefix("tsx-last-retire-").tempdir_in("/tmp")?;
    let project = fixture.path().join("project");
    fs::create_dir(&project)?;
    fs::write(project.join("tsconfig.json"), "{\"compilerOptions\":{\"strict\":true},\"include\":[\"*.ts\"]}")?;
    fs::write(project.join("view.ts"), "export function Retired() { return 'old'; }\n")?;
    let paths = backend_runtime::WorkspacePaths::discover(Some(fixture.path().join("owner")), Some(fixture.path().join("state")), Some(fixture.path().join("owner.sock")))?;
    paths.initialize()?;
    let authority = backend_engine::read_authority_secret(paths.authority_secret())?;
    let owner = start_owner(&paths)?;
    let mut session = Session::connect(owner.endpoint())?;
    index(&mut session, &project, 0xa1)?;
    let (coordinate, document) = select_native_document(&mut session, &authority, "Retired", "view.ts")?;
    let selection = document.selection.as_ref().ok_or("last source selection")?;
    let symbol = backend_library::SelectedSymbolId::new(*selection.symbol().as_bytes()).ok_or("last selected ID")?;
    let selector = backend_library::DeclarationSelector::SelectedAt { symbol, coordinate };
    fs::remove_file(project.join("view.ts"))?;
    index(&mut session, &project, 0xa2)?;
    assert_retired_handle_unavailable(&mut session, &selector)?;
    let CommandReply::Names(names) = session.names("Retired", 100)?.reply else { return Err("retired Names reply".into()) };
    assert!(!names.root.rows().iter().any(|row| row.id == backend_library::RowId::Symbol(selection.symbol())));
    assert!(names.root.coverage().contains(&backend_library::Coverage::Gapped { lane: backend_library::Lane::Semantic }));
    drop(session);
    assert_eq!(owner.close()?.failures, 0);
    let profile = super::profile_descriptor(super::BuiltinProfile::Product)?;
    let dispatcher = super::builtin_dispatcher(Some(super::ECHO_AUTHORITY_SECRET), Arc::clone(&profile), 60_000)?;
    let daemon = crate::Locald::open_with_dispatcher_and_registry(paths.data(), super::BuiltinModel, super::genesis()?, dispatcher, backend_engine::DaemonConfig::default(), super::product_relation_registry()?)?;
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let project_key = backend_engine::PackageKey::from_value(project.canonicalize()?.to_str().ok_or("project path")?).to_bytes();
    let sources = super::read_indexed_sources(&snapshot)?;
    assert!(sources.projects.get(&project_key).ok_or("selected project")?.files.is_empty());
    if let Some(evidence) = backend_engine::builtin::native_source_evidence_relation(&snapshot)? {
        let profile = backend_semantic::vocabulary::LanguageProfile::TypeScript(backend_semantic::vocabulary::TypeScriptSource::TypeScript);
        let key = backend_engine::builtin::native_source_evidence_key(project_key, profile, "view.ts").map_err(io::Error::other)?;
        assert!(evidence.lookup(&key)?.is_none(), "last source NSE row retired");
    }
    drop(snapshot); drop(daemon);
    let owner = start_owner(&paths)?;
    let mut cold = Session::connect(owner.endpoint())?;
    assert_retired_handle_unavailable(&mut cold, &selector)?;
    drop(cold);
    assert_eq!(owner.close()?.failures, 0);
    Ok(())
}

/// This is deliberately a real owner/index/cold-reopen fixture.  If the
/// compiler emits an image for the over-width input, the fixture must fail:
/// it has not exercised the member-only case.
#[test]
fn native_ts_omitted_program_member_cold_replay() -> Result<(), Box<dyn Error>> {
    omitted_program_member_cold_replay(false)
}
#[test]
fn native_ts_mixed_profile_program_member_cold_replay() -> Result<(), Box<dyn Error>> {
    omitted_program_member_cold_replay(true)
}
fn omitted_program_member_cold_replay(mixed: bool) -> Result<(), Box<dyn Error>> {
    let test_name = if mixed { "native_ts_mixed_profile_program_member_cold_replay" } else { "native_ts_omitted_program_member_cold_replay" };
    if crate::test_support::run_test_in_child(
        module_path!(), test_name,
    )? {
        return Ok(());
    }
    fn index_member_fixture(session: &mut Session, project: &Path) -> Result<(), Box<dyn Error>> {
        let package = PackageReference::parse(project.to_string_lossy().into_owned()).map_err(|e| io::Error::other(e.to_string()))?;
        let key = IndexOperationKey::from_bytes([0x91; 32]).map_err(|e| io::Error::other(e.to_string()))?;
        let mut status = session.start_index_operation(key, package, CompileExecutionIntent::Interactive)?;
        let deadline = Instant::now() + Duration::from_secs(600);
        loop {
            let IndexOperationObservation::Known(known) = status else { return Err("unknown index operation".into()) };
            match known.state {
                IndexOperationState::Published(_) => {
                    if session.connection_frames_remaining() == Some(0) { session.reconnect()?; }
                    return Ok(());
                }
                IndexOperationState::PartiallyPublished { refused_profiles, .. } => {
                    if refused_profiles.is_empty() { return Err("partial publication missing refused profiles".into()); }
                    eprintln!("member fixture partial profiles: {refused_profiles:?}");
                    if session.connection_frames_remaining() == Some(0) { session.reconnect()?; }
                    return Ok(());
                }
                IndexOperationState::Accepted | IndexOperationState::Active { .. } => {}
                state => return Err(io::Error::other(format!("index refused: {state:?}")).into()),
            }
            if Instant::now() >= deadline { return Err("index deadline".into()); }
            thread::sleep(Duration::from_millis(30));
            status = poll_index_status(session, key)?;
        }
    }
    use backend_engine::builtin::{native_source_evidence_key, native_source_evidence_relation, NativeSourceEvidenceKind};
    use backend_semantic::vocabulary::{LanguageProfile, TypeScriptSource};

    struct IntentCaptureGuard;
    impl Drop for IntentCaptureGuard {
        fn drop(&mut self) {
            super::profile::TEST_CAPTURE_NATIVE_INTENT.store(false, Ordering::Release);
            super::ingest::TEST_CAPTURE_NATIVE_PROGRAMS.store(false, Ordering::Release);
            if let Ok(mut slot) = super::profile::TEST_NATIVE_INTENT.lock() { *slot = None; }
        }
    }
    let _intent_guard = IntentCaptureGuard;
    *super::profile::TEST_NATIVE_INTENT.lock().map_err(|_| "intent lock")? = None;
    super::profile::TEST_CAPTURE_NATIVE_INTENT.store(true, Ordering::Release);
    super::ingest::TEST_NATIVE_PROGRAMS.lock().map_err(|_| "program lock")?.clear();
    super::ingest::TEST_CAPTURE_NATIVE_PROGRAMS.store(true, Ordering::Release);

    let _guard = FrontendFailpoint;
    super::ingest::TEST_REJECT_STRUCTURAL_FRONTENDS.store(true, Ordering::Release);
    let fixture = tempfile::Builder::new().prefix("nse-member-").tempdir_in("/tmp")?.keep();
    eprintln!("retained native member fixture: {}", fixture.display());
    let project = fixture.as_path().join("project");
    fs::create_dir_all(project.join("src/app"))?;
    fs::create_dir_all(project.join("src/components"))?;
    fs::create_dir_all(project.join("src/types"))?;
    // A parser-valid tuple over the native lowering width limit produces a
    // source-local gap without inventing a Program member or compiler fact.
    let config = if mixed { r#"{"compilerOptions":{"noLib":true,"types":[],"module":"ESNext","moduleResolution":"bundler","jsx":"preserve"},"files":["src/app/layout.tsx","src/app/page.tsx","src/components/NavBar.tsx","src/types/react-node.tsx","src/types/shared.ts","src/types/valid.ts"]}"# } else { r#"{"compilerOptions":{"noLib":true,"types":[],"module":"ESNext","moduleResolution":"bundler","jsx":"preserve"},"files":["src/app/layout.tsx","src/app/page.tsx","src/components/NavBar.tsx","src/types/react-node.tsx"]}"# };
    fs::write(project.join("tsconfig.json"), config)?;
    let member = format!("export declare const wide: [{}];\n", std::iter::repeat_n("unknown", 256).collect::<Vec<_>>().join(", "));
    let image = "export function NavBar() { return \"nav\"; }\n";
    let changed_member = member.replacen("unknown", "unknowm", 1);
    let changed_image = "export function NavBar() { return \"nax\"; }\n";
    fs::write(project.join("src/types/react-node.tsx"), "export type ReactNode = { readonly children?: ReactNode };\n")?;
    let shared = format!("export declare const shared: [{}];\n", std::iter::repeat_n("unknown", 256).collect::<Vec<_>>().join(", "));
    if mixed {
        fs::write(project.join("src/types/shared.ts"), &shared)?;
        fs::write(project.join("src/types/valid.ts"), "export const valid = 1;\n")?;
    }
    let page = if mixed { "import { NavBar } from \"../components/NavBar\";\nimport type { shared } from \"../types/shared\";\nimport { valid } from \"../types/valid\";\nexport function Page() { return NavBar(); }\n" } else { "import { NavBar } from \"../components/NavBar\";\nexport function Page() { return NavBar(); }\n" };
    fs::write(project.join("src/app/page.tsx"), page)?;
    assert_eq!(member.len(), changed_member.len());
    assert_eq!(image.len(), changed_image.len());
    fs::write(project.join("src/app/layout.tsx"), &member)?;
    fs::write(project.join("src/components/NavBar.tsx"), image)?;
    let paths = backend_runtime::WorkspacePaths::discover(
        Some(fixture.as_path().join("owner")), Some(fixture.as_path().join("state")),
        Some(fixture.as_path().join("owner.sock")),
    )?;
    paths.initialize()?;
    let authority = backend_engine::read_authority_secret(paths.authority_secret())?;
    let owner = start_owner(&paths)?;
    let mut session = Session::connect(owner.endpoint())?;
    index_member_fixture(&mut session, &project)?;
    super::profile::test_native_intent_budget_control()?;
    let (coordinate, document) = select_native_document(&mut session, &authority, "NavBar", "src/components/NavBar.tsx")?;
    let warm_policy = serde_json::to_value(&document.source_policy)?;
    assert_eq!(warm_policy["data"]["statement"]["policy_stability"], "unknown");
    let membership: [u8; 32] = serde_json::from_value(warm_policy["data"]["statement"]["program_membership"].clone())?;
    let binding: [u8; 32] = serde_json::from_value(warm_policy["data"]["statement"]["publication_binding"].clone())?;
    {
        let observed = super::ingest::TEST_NATIVE_PROGRAMS.lock().map_err(|_| "program lock")?;
        let selected = observed.iter().find(|entry| entry.membership == membership && entry.binding == binding).ok_or("selected Program was not observed at admitted image join")?;
        let digest = backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(member.as_bytes());
        assert_eq!(selected.sources.iter().filter(|(path, _, id, len)| path.ends_with("src/app/layout.tsx") && id == digest.as_ref() && *len as usize == member.len()).count(), 1);
        if mixed {
            let digest = backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(shared.as_bytes());
            assert_eq!(selected.sources.iter().filter(|(path, _, id, len)| path.ends_with("src/types/shared.ts") && id == digest.as_ref() && *len as usize == shared.len()).count(), 1, "TSX selected Program must include exact imported TS source");
        }
    }
    drop(session);
    assert_eq!(owner.close()?.failures, 0);

    // Reopen the durable relation separately from the listener. A Member row
    // is created by the native capture from a real Program mapping, not by
    // this test; it grants no image or declaration authority.
    let profile = super::profile_descriptor(super::BuiltinProfile::Product)?;
    let dispatcher = super::builtin_dispatcher(Some(super::ECHO_AUTHORITY_SECRET), profile, 60_000)?;
    let mut daemon = crate::Locald::open_with_dispatcher_and_registry(
        paths.data(), super::BuiltinModel, super::genesis()?, dispatcher,
        backend_engine::DaemonConfig::default(), super::product_relation_registry()?,
    )?;
    let mut selected_authority = super::semantic_authority::SemanticAuthority::open(paths.data())?;
    selected_authority.reconcile_workspace(&mut daemon)?;
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let project_key = backend_engine::PackageKey::from_value(project.canonicalize()?.to_str().ok_or("project path")?).to_bytes();
    let relation = native_source_evidence_relation(&snapshot)?.ok_or("missing selected NSE")?;
    let ts = LanguageProfile::TypeScript(TypeScriptSource::Tsx);
    let member_row = relation.lookup(&native_source_evidence_key(project_key, ts, "src/app/layout.tsx")?)?.ok_or("missing ProgramMember")?;
    let image_row = relation.lookup(&native_source_evidence_key(project_key, ts, "src/components/NavBar.tsx")?)?.ok_or("missing Image")?;
    assert_eq!(member_row.kind, NativeSourceEvidenceKind::ProgramMember, "fixture did not produce an omitted image");
    assert_eq!(image_row.kind, NativeSourceEvidenceKind::Image);
    let member_id = backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(member.as_bytes());
    let image_id = backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(image.as_bytes());
    assert_eq!(member_row.source_digest.as_slice(), member_id.as_ref());
    assert_eq!(image_row.source_digest.as_slice(), image_id.as_ref());
    assert!(image_row.verify_host(image.as_bytes(), image_id).is_ok());
    assert!(image_row.verify_host(changed_image.as_bytes(), image_id).is_err());
    super::view_build::native_evidence_lookup_budget_control(
        &relation, backend_engine::PackageKey::from_value(project.canonicalize()?.to_str().ok_or("project path")?), ts,
        &[("src/app/layout.tsx", backend_semantic::ir::SourceIdentity::from_bytes(member.as_bytes()).ok_or("member source identity")?),
          ("src/components/NavBar.tsx", backend_semantic::ir::SourceIdentity::from_bytes(image.as_bytes()).ok_or("image source identity")?)],
    )?;
    // Use the actual selected key and capture, then replay against the real
    // persisted compiler history. The altered rows are inputs to production
    // replay admission only; they do not replace the workspace relation.
    let captures = backend_engine::builtin::semantic_capture_relation(&snapshot)?.ok_or("missing captures")?;
    let capture_page = captures.page(None, backend_engine::MAX_SNAPSHOT_PAGE_ROWS)?;
    assert!(capture_page.next().is_none(), "fixture capture page must be complete");
    let (selected_key, selected_capture) = capture_page.entries().iter()
        .find(|(key, capture)| key.package_key().to_bytes() == project_key
            && key.profile() == ts && key.is_selected()
            && matches!(capture.outcome(), backend_engine::builtin::ProductSemanticCaptureOutcome::Published { .. }))
        .ok_or("published selected TS capture")?;
    let publications = snapshot.relation::<super::BuiltinSemanticRelation>()?;
    let backend_engine::builtin::ProductSemanticCaptureOutcome::Published { coverage, claim } = selected_capture.outcome() else { return Err("capture not published".into()) };
    assert!(matches!(coverage, backend_engine::builtin::SemanticPublicationCoverage::Partial(_)), "omitted native source requires partial semantic coverage");
    assert_eq!(publications.lookup(selected_key)?, Some(backend_engine::builtin::ProductSemanticPublicationRecord::Published { coverage, claim }));
    {
        use backend_semantic::ir::{SourceCoverageGapCause, SourceCoverageRow};
        let observed = selected_authority.selected_source_coverage(selected_key, claim)?
            .ok_or("selected compiler coverage absent")?;
        // Coverage is per selected profile: TS files have their own capture.
        assert_eq!((observed.source_count(), observed.completed()), (4, 3));
        let catalog = observed.catalog().ok_or("selected compiler gap catalog absent")?;
        assert_eq!(catalog.rows().iter().filter(|row| matches!(row,
            SourceCoverageRow::Gap { path, source, byte_len, cause, .. }
                if path == "src/app/layout.tsx" && source.as_slice() == member_id.as_ref()
                    && *byte_len as usize == member.len()
                    && *cause == SourceCoverageGapCause::SourceNotRepresentable
        )).count(), 1, "selected compiler must record this exact omitted source and cause");
    }
    let change = super::profile::BuiltinCaptureChange {
        cargo_target: None, key: selected_key.clone(), expected: Some(selected_capture.clone()),
        capture: selected_capture.capture(), outcome: selected_capture.outcome(), compiler_failure: None,
    };
    let mut changes = vec![change.clone()];
    if mixed {
        let ts_profile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
        let (key, capture) = capture_page.entries().iter().find(|(key, capture)| key.package_key().to_bytes() == project_key && key.profile() == ts_profile && key.is_selected() && matches!(capture.outcome(), backend_engine::builtin::ProductSemanticCaptureOutcome::Published { .. })).ok_or("published selected TS capture")?;
        changes.push(super::profile::BuiltinCaptureChange { cargo_target: None, key: key.clone(), expected: Some(capture.clone()), capture: capture.capture(), outcome: capture.outcome(), compiler_failure: None });
    }
    let store = backend_engine::FileStore::open(paths.data().join("objects"), 8 * 1024 * 1024)
        .map_err(|error| io::Error::other(format!("open selected object store: {error:?}")))?;
    let page = relation.page(None, backend_engine::MAX_SNAPSHOT_PAGE_ROWS)?;
    assert!(page.next().is_none(), "fixture evidence page must be complete");
    let rows = page.entries().iter().map(|(_, row)| row.clone())
        .filter(|row| row.project == project_key && (row.profile == ts || (mixed && row.profile == LanguageProfile::TypeScript(TypeScriptSource::TypeScript)))).collect::<Vec<_>>();
    assert!(rows.iter().any(|row| row == &member_row));
    assert!(rows.iter().any(|row| row == &image_row));
    let refs = rows.iter().collect::<Vec<_>>();
    if mixed {
        let shared_row = rows.iter().find(|row| row.path == "src/types/shared.ts" && row.profile == LanguageProfile::TypeScript(TypeScriptSource::TypeScript)).ok_or("missing cross-profile TS ProgramMember")?;
        assert_eq!(shared_row.kind, NativeSourceEvidenceKind::ProgramMember);
    }
    super::semantic_authority::admit_replayed_native_evidence(&store, &changes, &refs, &[])?;
    if mixed {
        let mut altered = rows.clone();
        altered.iter_mut().find(|row| row.path == "src/types/shared.ts").ok_or("shared row")?.source_digest[0] ^= 1;
        let altered_refs = altered.iter().collect::<Vec<_>>();
        let error = super::semantic_authority::admit_replayed_native_evidence(&store, &changes, &altered_refs, &[]).err().ok_or("altered cross-profile source unexpectedly admitted")?;
        assert_eq!(error.0, "native replay Program source differs from evidence");
    }
    for (path, kind_only, expected) in [
        ("src/app/layout.tsx", false, "native replay Program source differs from evidence"),
        ("src/components/NavBar.tsx", false, "native replay Program source differs from evidence"),
        ("src/app/layout.tsx", true, "native replay source lacks its claimed Program evidence"),
        ("src/components/NavBar.tsx", true, "native replay Program member unexpectedly has an image"),
    ] {
        let mut altered = rows.clone();
        let row = altered.iter_mut().find(|row| row.path == path).ok_or("tamper target")?;
        if kind_only {
            row.kind = if row.admits_image() { NativeSourceEvidenceKind::ProgramMember } else { NativeSourceEvidenceKind::Image };
        } else {
            row.source_digest[0] ^= 1;
        }
        let refs = altered.iter().collect::<Vec<_>>();
        let error = super::semantic_authority::admit_replayed_native_evidence(&store, &changes, &refs, &[]).err().ok_or("tampered replay unexpectedly admitted")?;
        assert_eq!(error.0, expected, "tamper path {path}");
    }
    drop(relation);
    drop(snapshot);
    drop(selected_authority);
    drop(daemon);

    let owner = start_owner(&paths)?;
    let mut session = Session::connect(owner.endpoint())?;
    let (cold_coordinate, cold) = select_native_document(&mut session, &authority, "NavBar", "src/components/NavBar.tsx")?;
    assert_eq!(coordinate, cold_coordinate);
    assert_eq!(document.symbol, cold.symbol);
    let cold_policy = serde_json::to_value(&cold.source_policy)?;
    assert_eq!(cold_policy["data"]["statement"]["policy_stability"], "unknown");
    assert_eq!(cold_policy["data"]["statement"]["program_membership"], warm_policy["data"]["statement"]["program_membership"]);
    assert_eq!(cold_policy["data"]["statement"]["publication_binding"], warm_policy["data"]["statement"]["publication_binding"]);
    fs::write(project.join("src/components/NavBar.tsx"), changed_image)?;
    let request = SurfaceCommand::Read { locators: vec![ProductText::new(cold_coordinate).map_err(|e| io::Error::other(e.to_string()))?].into_boxed_slice() };
    match session.surface(request) {
        Ok(SurfaceReply::Read(records)) => {
            let [record] = records.as_ref() else { return Err("stale Read result count".into()) };
            assert!(record.source_policy.is_unknown());
            assert!(record.extent.is_none());
        }
        Ok(_) => return Err("unexpected stale Read reply".into()),
        Err(backend_client::ClientError::CommandFailed(
            backend_library::CommandFailure::InvalidQuery(detail),
        )) if detail.contains("indexed source changed on disk; reindex and retry") => {}
        Err(error) => return Err(error.into()),
    }
    fs::write(project.join("src/app/layout.tsx"), changed_member)?;
    let changed_member_id = backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(&fs::read(project.join("src/app/layout.tsx"))?);
    assert_ne!(member_row.source_digest.as_slice(), changed_member_id.as_ref());
    drop(session);
    assert_eq!(owner.close()?.failures, 0);
    Ok(())
}

#[test]
fn native_ts_large_crlf_line_map_source_show_read_and_cold() -> Result<(), Box<dyn Error>> {
    if crate::test_support::run_test_in_child(module_path!(), "native_ts_large_crlf_line_map_source_show_read_and_cold")? { return Ok(()); }
    use backend_engine::builtin::{native_source_evidence_key, native_source_evidence_relation, ProductSemanticCaptureOutcome};
    use backend_semantic::vocabulary::{LanguageProfile, TypeScriptSource};
    let _guard = FrontendFailpoint;
    super::ingest::TEST_REJECT_STRUCTURAL_FRONTENDS.store(true, Ordering::Release);
    let fixture = tempfile::Builder::new().prefix("native-lines-").tempdir_in("/tmp")?;
    let project = fixture.path().join("project");
    fs::create_dir(&project)?;
    fs::write(project.join("tsconfig.json"), r#"{"compilerOptions":{"strict":true},"include":["*.ts"]}"#)?;
    let mut source = " ".repeat(30_000);
    source.push_str(&"\r\n".repeat(20_000));
    source.push_str("export function Greeting() { return 'before'; }\r\n");
    fs::write(project.join("view.ts"), &source)?;
    for (path, text) in [("bare.ts", "// one\rexport function Bare() { return 1; }"), ("sep.ts", "// one\u{2028}export function Sep() { return 2; }"), ("para.ts", "// one\u{2029}export function Para() { return 3; }")] {
        fs::write(project.join(path), text)?;
    }
    let paths = backend_runtime::WorkspacePaths::discover(Some(fixture.path().join("owner")), Some(fixture.path().join("state")), Some(fixture.path().join("owner.sock")))?;
    paths.initialize()?;
    let authority_secret = backend_engine::read_authority_secret(paths.authority_secret())?;
    let owner = start_owner(&paths)?;
    let mut session = Session::connect(owner.endpoint())?;
    index(&mut session, &project, 0x72)?;
    let (coordinate, document) = select_native_document(&mut session, &authority_secret, "Greeting", "view.ts")?;
    assert_eq!(document.location.captured().ok_or("warm source location")?.start_line(), 20_001);
    let CommandReply::Names(warm_names) = session.names("Greeting", 100)?.reply else { return Err("warm Names reply".into()) };
    let warm_row = warm_names.root.rows().iter().find(|row| row.label.as_str() == coordinate).ok_or("warm native Names row")?;
    assert_eq!(warm_row.source.captured().ok_or("warm Names location")?.start_line(), 20_001);
    let CommandReply::Document(shown) = session.document(&coordinate)?.reply else { return Err("Show reply".into()) };
    assert_eq!(shown.location.captured().ok_or("warm Show location")?.start_line(), 20_001);
    for (name, path) in [("Bare", "bare.ts"), ("Sep", "sep.ts"), ("Para", "para.ts")] {
        let (_, unsupported) = select_native_document(&mut session, &authority_secret, name, path)?;
        assert!(unsupported.location.captured().is_none(), "unsupported terminator must not claim a line location");
    }
    let request = || -> Result<SurfaceCommand, io::Error> { Ok(SurfaceCommand::Read { locators: vec![ProductText::new(coordinate.clone()).map_err(|e| io::Error::other(e.to_string()))?].into_boxed_slice() }) };
    let SurfaceReply::Read(records) = session.surface(request()?)? else { return Err("Read reply".into()) };
    let [record] = records.as_ref() else { return Err("warm Read result count".into()) };
    assert!(record.extent.is_some(), "fresh native Read extent");
    assert!(record.signature.as_ref().is_some_and(|text| text.as_str().contains("Greeting") && text.as_str().contains("before")));
    drop(session);
    assert_eq!(owner.close()?.failures, 0);

    let profile = super::profile_descriptor(super::BuiltinProfile::Product)?;
    let dispatcher = super::builtin_dispatcher(Some(super::ECHO_AUTHORITY_SECRET), profile, 60_000)?;
    let mut daemon = crate::Locald::open_with_dispatcher_and_registry(paths.data(), super::BuiltinModel, super::genesis()?, dispatcher, backend_engine::DaemonConfig::default(), super::product_relation_registry()?)?;
    let mut authority = super::semantic_authority::SemanticAuthority::open(paths.data())?;
    authority.reconcile_workspace(&mut daemon)?;
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let project_key = backend_engine::PackageKey::from_value(project.canonicalize()?.to_str().ok_or("project path")?).to_bytes();
    let ts = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
    let relation = native_source_evidence_relation(&snapshot)?.ok_or("native evidence")?;
    let row = relation.lookup(&native_source_evidence_key(project_key, ts, "view.ts").map_err(io::Error::other)?)?.ok_or("native image row")?;
    assert!(row.line_starts.is_none(), "large line index must not be inline");
    let captures = backend_engine::builtin::semantic_capture_relation(&snapshot)?.ok_or("capture relation")?;
    let page = captures.page(None, backend_engine::MAX_SNAPSHOT_PAGE_ROWS)?;
    assert!(page.next().is_none());
    let (key, capture) = page.entries().iter().find(|(key, capture)| key.package_key().to_bytes() == project_key && key.profile() == ts && key.is_selected() && matches!(capture.outcome(), ProductSemanticCaptureOutcome::Published { .. })).ok_or("selected TS capture")?;
    let ProductSemanticCaptureOutcome::Published { claim, .. } = capture.outcome() else { return Err("publication claim".into()) };
    let mut budget = backend_extension_turso::SelectedNativeHeaderBudget::new(64 * 1024 * 1024, 10_000_000);
    let witness = authority.selected_native_lines(key, claim, &mut budget, &|| false)?.ok_or("selected compiler line map")?;
    let view = backend_semantic::native_source_lines::NativeSourceLineAggregateView::parse(witness.as_bytes(), &mut |_| Ok::<(), ()>(())).map_err(|_| "line map decode")?;
    for path in ["bare.ts", "sep.ts", "para.ts"] {
        assert!(view.find_file(path, &mut |_| Ok::<(), ()>(())).map_err(|_| "unsupported line lookup")?.is_none());
    }
    let file = view.find_file("view.ts", &mut |_| Ok::<(), ()>(())).map_err(|_| "line lookup")?.ok_or("line map file")?;
    assert!(file.matches_source(source.as_bytes(), &mut |_| Ok::<(), ()>(())).map_err(|_| "line verification")?);
    assert_eq!(file.line_count(), 20_002);
    assert_eq!(file.line_start(20_000), Some(u32::try_from(source.find("export function Greeting").ok_or("declaration offset")?)?));
    drop(snapshot);
    drop(authority);
    drop(daemon);

    let cold_owner = start_owner(&paths)?;
    let mut cold = Session::connect(cold_owner.endpoint())?;
    let (_, cold_source) = select_native_document(&mut cold, &authority_secret, "Greeting", "view.ts")?;
    assert_eq!(cold_source.location.captured().ok_or("cold source location")?.start_line(), 20_001);
    let CommandReply::Names(cold_names) = cold.names("Greeting", 100)?.reply else { return Err("cold Names reply".into()) };
    let cold_row = cold_names.root.rows().iter().find(|row| row.label.as_str() == coordinate).ok_or("cold native Names row")?;
    assert_eq!(cold_row.source.captured().ok_or("cold Names location")?.start_line(), 20_001);
    let CommandReply::Document(cold_show) = cold.document(&coordinate)?.reply else { return Err("cold Show reply".into()) };
    assert_eq!(cold_show.location.captured().ok_or("cold Show location")?.start_line(), 20_001);
    let SurfaceReply::Read(cold_records) = cold.surface(request()?)? else { return Err("cold Read reply".into()) };
    let [cold_record] = cold_records.as_ref() else { return Err("cold Read result count".into()) };
    assert!(cold_record.extent.is_some());
    assert!(cold_record.signature.as_ref().is_some_and(|text| text.as_str().contains("Greeting") && text.as_str().contains("before")));
    let changed = source.replacen("before", "afters", 1);
    assert_eq!(source.len(), changed.len());
    fs::write(project.join("view.ts"), changed)?;
    match cold.surface(request()?) {
        Ok(SurfaceReply::Read(stale)) => {
            let [record] = stale.as_ref() else { return Err("stale Read result count".into()) };
            assert!(record.extent.is_none() && record.source_policy.is_unknown());
            assert!(record.signature.as_ref().is_none_or(|text| !text.as_str().contains("before") && !text.as_str().contains("afters")));
        }
        Ok(_) => return Err("unexpected stale Read reply".into()),
        Err(backend_client::ClientError::CommandFailed(backend_library::CommandFailure::InvalidQuery(detail)))
            if detail.contains("indexed source changed on disk; reindex and retry") => {},
        Err(error) => return Err(error.into()),
    }
    drop(cold);
    assert_eq!(cold_owner.close()?.failures, 0);
    Ok(())
}
