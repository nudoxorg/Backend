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
    config.worker_endpoint = None;
    config.authority_secret = Some(paths.authority_secret().to_path_buf());
    Ok(EmbeddedLocalService::start(config)?)
}

fn index(session: &mut Session, project: &Path, byte: u8) -> Result<(), Box<dyn Error>> {
    let package = PackageReference::parse(project.to_string_lossy().into_owned())
        .map_err(|e| io::Error::other(e.to_string()))?;
    let key =
        IndexOperationKey::from_bytes([byte; 32]).map_err(|e| io::Error::other(e.to_string()))?;
    let mut status =
        session.start_index_operation(key, package, CompileExecutionIntent::Interactive)?;
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        let IndexOperationObservation::Known(known) = status else {
            return Err("unknown index operation".into());
        };
        match known.state {
            IndexOperationState::Published(_) => return Ok(()),
            IndexOperationState::Accepted | IndexOperationState::Active { .. } => {}
            state => return Err(io::Error::other(format!("index refused: {state:?}")).into()),
        }
        if Instant::now() >= deadline {
            return Err("index deadline".into());
        }
        thread::sleep(Duration::from_millis(30));
        status = session.index_operation_status(key)?;
    }
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
#[ignore = "native owner journey; run this test alone with the test-only frontend failpoint"]
fn native_ts_listener_cold_reopen_and_edit_without_frontends() -> Result<(), Box<dyn Error>> {
    native_ts_listener_journey(true, false)
}

#[test]
#[ignore = "native owner journey uninstrumented control"]
fn native_ts_listener_cold_reopen_and_edit_control() -> Result<(), Box<dyn Error>> {
    native_ts_listener_journey(false, false)
}

#[test]
#[ignore = "native owner reference journey with test-only frontend failpoint"]
fn native_ts_listener_references_without_structural_frontends() -> Result<(), Box<dyn Error>> {
    native_ts_listener_journey(true, true)
}

#[test]
#[ignore = "native owner reference journey uninstrumented control"]
fn native_ts_listener_references_uninstrumented_control() -> Result<(), Box<dyn Error>> {
    native_ts_listener_journey(false, true)
}

#[test]
#[ignore = "native source range, retained selection and stale-source guard"]
fn native_ts_owner_explicit_range_retained_selection_and_stale_refusal()
-> Result<(), Box<dyn Error>> {
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
    let before = "export function Greeting() { return 'before'; }\n";
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
    index(&mut session, &project, 0x51)?;
    let (coordinate, document) =
        select_native_document(&mut session, &authority, "Greeting", "view.ts")?;
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
    let CommandReply::Document(ranged) = session
        .declaration_source_range(&selector, None, request)?
        .reply
    else {
        return Err("range page document".into());
    };
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
    let CommandReply::Document(next) = session
        .declaration_source_range(&selector, None, continuation)?
        .reply
    else {
        return Err("range continuation document".into());
    };
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
    let SurfaceReply::References { coverage, .. } =
        session.surface(SurfaceCommand::References { target })?
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
    assert_eq!(owner.close()?.failures, 0);
    let owner = start_owner(&paths)?;
    let mut session = Session::connect(owner.endpoint())?;
    let CommandReply::Document(cold) = session
        .declaration(&selector, DeclarationRead::Source)?
        .reply
    else {
        return Err("cold retained selection reply".into());
    };
    assert_eq!(cold.selection.as_ref(), Some(&original));
    cold.source_policy
        .verify_owner(&authority, &original)
        .map_err(|error| io::Error::other(format!("{error:?}")))?;
    let CommandReply::Document(cold_range) = session
        .declaration_source_range(&selector, None, continuation)?
        .reply
    else {
        return Err("cold continuation reply".into());
    };
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
        "export function Greeting() { return 'after'; }\n",
    )?;
    if let Ok(reply) = session.declaration_source_range(&selector, None, continuation) {
        let CommandReply::Document(stale) = reply.reply else {
            return Err("stale range reply".into());
        };
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
    assert_eq!(owner.close()?.failures, 0);
    Ok(())
}

#[test]
#[ignore = "native source-qualified Read plus same-length stale host edit"]
fn native_ts_read_owner_policy_and_same_length_stale_refusal() -> Result<(), Box<dyn Error>> {
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
#[ignore = "native TS readiness after index with test-only frontend construction failpoint"]
fn native_ts_health_without_structural_frontends() -> Result<(), Box<dyn Error>> {
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
            "{\"compilerOptions\":{\"strict\":true},\"include\":[\"*.ts\"],\"exclude\":[\"view.ts\"]}",
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
#[ignore = "native same-profile deletion, selected NSE retirement and cold reopen"]
fn native_ts_deleted_file_retires_evidence_and_old_handle() -> Result<(), Box<dyn Error>> {
    native_ts_retirement_journey(false, false)
}

#[test]
#[ignore = "native same-profile config exclusion, selected NSE retirement and cold reopen"]
fn native_ts_excluded_file_retires_evidence_and_old_handle() -> Result<(), Box<dyn Error>> {
    native_ts_retirement_journey(true, false)
}

#[test]
#[ignore = "native exclusion search diagnostic; run alone with frontend failpoint"]
fn native_ts_exclusion_kept_symbol_remains_searchable() -> Result<(), Box<dyn Error>> {
    native_ts_retirement_journey(true, true)
}

#[test]
#[ignore = "last native TypeScript source deletion and cold owner retirement"]
fn native_ts_last_file_deletion_retires_selected_evidence() -> Result<(), Box<dyn Error>> {
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
