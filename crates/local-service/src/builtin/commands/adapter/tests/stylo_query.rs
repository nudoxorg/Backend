//! Native owner-backed query and exact-source fixture.
use super::*;
use crate::builtin::BuiltinModelError;
use crate::builtin::commands::index::{
    IndexScanFailure, PreparedSourceScan, capture_index_scan, prepare_source_scan, run_index_scan,
};
use crate::builtin::commands::stylo_query::{query, read_site};
use backend_library::stylo_query::{
    StyloAttemptState, StyloHostTerminalSummary, StyloNameRole, StyloQueryHit, StyloQueryRequest,
    StyloQueryTerminal, StyloScopeGap, StyloSourceRead,
};

#[test]
fn absent_project_is_not_legacy_evidence() -> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = AdapterFixture::open(TempTree::new(), false);
    let (_, label) = fixture.add_target();
    let (_, daemon) = fixture.parts();
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let request = StyloQueryRequest {
        package: backend_library::PackageReference::parse(label)
            .map_err(|error| error.to_string())?,
        role: StyloNameRole::Class,
        name: "foo".into(),
        page_rows: 1,
        output_bytes: 131_072,
        scan_rows: 7,
        scan_bytes: 229_376,
        continuation: None,
    };
    let page = query(&snapshot, &[42; 32], &request)?;
    assert_eq!(page.basis.attempt, StyloAttemptState::ProjectNotSelected);
    assert_eq!(page.terminal, StyloQueryTerminal::Unavailable);
    Ok(())
}

#[test]
#[ignore = "requires NUDOX_TEST_STYLO_WORKER and native owner admission"]
fn repeated_native_sites_and_checked_source() -> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = AdapterFixture::open(TempTree::new(), false);
    let (package, label) = fixture.add_target();
    let css = Path::new(&label).join("site.css");
    let bytes = "\u{feff}/* 🙂 */\r\n.foo.foo { --tone: red; --tone: blue; --paint: var(--tone, var(--other)) var(--tone) env(tone); } .foo { color: red } .foo { color: var(--ordinary-only) } .\\66 oo { color: red } @media (min-width: 1px) { .inside { color: red } }";
    fs::write(&css, bytes)?;
    let worker =
        PathBuf::from(std::env::var_os("NUDOX_TEST_STYLO_WORKER").ok_or("worker fixture missing")?);
    let artifact = backend_compile::ToolchainArtifact::from_path(&worker, Vec::new())?;
    let permit = Arc::new(
        crate::builtin::stylo_worker::StyloWorkerPermit::from_admitted(
            worker,
            artifact,
            backend_frontend_stylo::support_fingerprint(),
        )
        .map_err(|e| format!("{e:?}"))?,
    );
    let (adapter, daemon) = fixture.parts();
    let mut work = capture_index_scan(
        daemon,
        package,
        &label,
        Path::new(&label),
        None,
        1200,
        CompileExecutionIntent::Interactive,
        false,
        Arc::new(AtomicBool::new(false)),
        adapter.compiler.python_interpreter_admission(),
    )?;
    work.stylo_worker = Some(permit);
    let scan = run_index_scan(work).map_err(|e| match e {
        IndexScanFailure::Cancelled => BuiltinModelError("cancelled".into()),
        IndexScanFailure::Refused(e) => e,
    })?;
    let base = daemon.engine().daemon().owner().snapshot();
    let PreparedSourceScan::Ready(selection) = prepare_source_scan(
        &base,
        scan,
        &adapter.compiler,
        &mut adapter.semantic_authority,
        None,
        None,
        &mut || {},
    )?
    else {
        return Err("unexpected compiler capture".into());
    };
    let intent = selection.intent.ok_or("no native publication")?;
    super::super::commit_builtin_intent(daemon, 1201, &intent)?;
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let secret = [42; 32];
    let mut request = StyloQueryRequest {
        package: backend_library::PackageReference::parse(label.clone())
            .map_err(|error| error.to_string())?,
        role: StyloNameRole::Class,
        name: "foo".into(),
        page_rows: 1,
        output_bytes: 131_072,
        scan_rows: 64,
        scan_bytes: 4_194_304,
        continuation: None,
    };
    // Exercise the actual SurfaceCommand wire boundary and the durable owner
    // authority, not a test-supplied query secret.
    let invoke = |adapter: &mut CommandAdapter,
                  daemon: &mut ProductDaemon,
                  surface: backend_library::SurfaceCommand,
                  id: u64|
     -> Result<backend_library::SurfaceReply, Box<dyn std::error::Error>> {
        let request =
            backend_library::CommandDto::new(id, backend_library::Command::Surface(surface));
        let body = backend_library::encode_command_body(&request)?;
        let bytes = adapter.execute(daemon, &body)?;
        let reply = backend_library::decode_reply_body(&bytes)?;
        backend_library::admit_reply(&request, &reply)?;
        match reply.reply {
            backend_library::CommandReply::Surface(value) => Ok(value),
            other => Err(format!("unexpected public reply {other:?}").into()),
        }
    };
    let mut public_request = request.clone();
    let public_source = loop {
        let reply = invoke(
            adapter,
            daemon,
            backend_library::SurfaceCommand::StyloQuery {
                request: public_request.clone(),
            },
            1400,
        )?;
        let backend_library::SurfaceReply::StyloQuery(page) = reply else {
            return Err("wrong CSS query reply".into());
        };
        if let Some(source) = page.hits.iter().find_map(|hit| match hit {
            StyloQueryHit::SourceSite { source, .. } => Some(source.clone()),
            _ => None,
        }) {
            break source;
        }
        match page.terminal {
            StyloQueryTerminal::More { continuation, .. } => {
                public_request.continuation = Some(continuation)
            }
            _ => return Err("public CSS site missing".into()),
        }
    };
    let source_reply = invoke(
        adapter,
        daemon,
        backend_library::SurfaceCommand::StyloReadSite {
            reference: public_source,
        },
        1401,
    )?;
    assert!(matches!(
        source_reply,
        backend_library::SurfaceReply::StyloReadSite(StyloSourceRead::Available { line_1: 2, .. })
    ));
    let mut missing = request.clone();
    missing.scan_rows = 7;
    missing.scan_bytes = 229_376;
    for (role, name) in [
        (StyloNameRole::Class, "missing-class"),
        (StyloNameRole::CustomProperty, "--ordinary-only"),
    ] {
        missing.role = role;
        missing.name = name.into();
        missing.continuation = None;
        let mut seen_cursors = Vec::new();
        loop {
            let page = query(&snapshot, &secret, &missing)?;
            assert!(page.hits.is_empty());
            assert_eq!(
                page.basis.host_terminal_summary,
                StyloHostTerminalSummary::Unknown
            );
            assert!(
                page.basis
                    .gaps
                    .contains(&StyloScopeGap::OrdinaryVariableReferencesUnavailable)
            );
            assert!(
                page.basis
                    .gaps
                    .contains(&StyloScopeGap::AuthoredReferenceSitesUnavailable)
            );
            assert!(
                page.basis
                    .gaps
                    .contains(&StyloScopeGap::NativeDeclarationNameDecodeUnavailable)
            );
            match page.terminal {
                StyloQueryTerminal::Complete => break,
                StyloQueryTerminal::More { continuation, .. } => {
                    assert!(
                        !seen_cursors.contains(&continuation),
                        "bounded scan must advance"
                    );
                    seen_cursors.push(continuation.clone());
                    assert!(seen_cursors.len() < 512, "bounded scan must terminate");
                    missing.continuation = Some(continuation);
                }
                other => return Err(format!("unexpected missing-name terminal {other:?}").into()),
            }
        }
        assert!(
            !seen_cursors.is_empty(),
            "fixture must exercise continuation"
        );
    }
    let mut nested = request.clone();
    nested.name = "inside".into();
    let mut nested_selectors = 0;
    loop {
        let page = query(&snapshot, &secret, &nested)?;
        assert!(
            page.basis
                .gaps
                .contains(&StyloScopeGap::AncestorRuleContextNotReturned)
        );
        nested_selectors += page
            .hits
            .iter()
            .filter(|hit| matches!(hit, StyloQueryHit::NativeSelector { .. }))
            .count();
        match page.terminal {
            StyloQueryTerminal::Complete => break,
            StyloQueryTerminal::More { continuation, .. } => {
                nested.continuation = Some(continuation)
            }
            other => return Err(format!("unexpected nested terminal {other:?}").into()),
        }
    }
    assert_eq!(nested_selectors, 1);
    let mut sites = Vec::new();
    let mut selectors = 0;
    loop {
        let page = query(&snapshot, &secret, &request)?;
        assert!(
            page.basis
                .gaps
                .contains(&StyloScopeGap::OrdinaryVariableReferencesUnavailable)
        );
        assert!(
            page.basis
                .gaps
                .contains(&StyloScopeGap::AuthoredReferenceSitesUnavailable)
        );
        for hit in page.hits {
            match hit {
                StyloQueryHit::SourceSite { id, source, .. } => sites.push((id, source)),
                StyloQueryHit::NativeSelector { .. } => selectors += 1,
                _ => {}
            }
        }
        match page.terminal {
            StyloQueryTerminal::Complete => break,
            StyloQueryTerminal::More { continuation, .. } => {
                request.continuation = Some(continuation)
            }
            other => return Err(format!("unexpected terminal {other:?}").into()),
        }
    }
    assert_eq!(sites.len(), 5);
    assert_eq!(selectors, 4);
    let mut keys = sites.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), 5);
    request.role = StyloNameRole::CustomProperty;
    request.name = "--tone".into();
    request.continuation = None;
    let mut declarations = 0;
    let mut dependencies = 0;
    let mut reference_paths = Vec::new();
    let mut definition_sites = Vec::new();
    loop {
        let page = query(&snapshot, &secret, &request)?;
        for hit in page.hits {
            match hit {
                StyloQueryHit::SourceSite { id, .. } => definition_sites.push(id),
                StyloQueryHit::NativeDeclaration { .. } => declarations += 1,
                StyloQueryHit::NativeDependency {
                    dependency,
                    declaration,
                    ..
                } => {
                    assert_eq!(dependency.kind, "var");
                    assert_eq!(dependency.name, "tone");
                    assert_eq!(declaration.property, "--paint");
                    reference_paths.push(dependency.reference_path);
                    dependencies += 1;
                }
                _ => {}
            }
        }
        match page.terminal {
            StyloQueryTerminal::Complete => break,
            StyloQueryTerminal::More { continuation, .. } => {
                request.continuation = Some(continuation)
            }
            other => return Err(format!("unexpected terminal {other:?}").into()),
        }
    }
    definition_sites.sort();
    definition_sites.dedup();
    assert_eq!(definition_sites.len(), 2);
    assert_eq!(declarations, 1);
    assert_eq!(dependencies, 2);
    reference_paths.sort();
    reference_paths.dedup();
    assert_eq!(reference_paths.len(), 2);
    let mut raw_sites = Vec::new();
    for (_, reference) in &sites {
        match read_site(&snapshot, &secret, reference)? {
            StyloSourceRead::Available {
                served_root,
                text,
                line_1,
                ..
            } => {
                assert_eq!(served_root, label);
                assert_eq!(line_1, 2);
                raw_sites.push(text);
            }
            other => return Err(format!("unexpected source outcome {other:?}").into()),
        }
    }
    assert!(raw_sites.iter().any(|text| text == "foo"));
    assert!(raw_sites.iter().any(|text| text == "\\66 oo"));
    fs::write(&css, bytes.replace("red", "tan"))?;
    assert_eq!(
        read_site(&snapshot, &secret, &sites[0].1)?,
        StyloSourceRead::SourceChanged
    );
    fs::remove_file(&css)?;
    assert_eq!(
        read_site(&snapshot, &secret, &sites[0].1)?,
        StyloSourceRead::BytesNotRetained
    );
    Ok(())
}

#[test]
#[ignore = "requires NUDOX_TEST_STYLO_WORKER and native owner admission"]
fn cursor_roots_and_retained_source_follow_served_selection()
-> Result<(), Box<dyn std::error::Error>> {
    let mut fixture = AdapterFixture::open(TempTree::new(), false);
    let (package, label) = fixture.add_target();
    let root = PathBuf::from(&label);
    let css = root.join("site.css");
    let worker =
        PathBuf::from(std::env::var_os("NUDOX_TEST_STYLO_WORKER").ok_or("worker missing")?);
    let artifact = backend_compile::ToolchainArtifact::from_path(&worker, Vec::new())?;
    let permit = Arc::new(
        crate::builtin::stylo_worker::StyloWorkerPermit::from_admitted(
            worker,
            artifact,
            backend_frontend_stylo::support_fingerprint(),
        )
        .map_err(|e| format!("{e:?}"))?,
    );
    let run = |fixture: &mut AdapterFixture,
               scan_root: &Path,
               permit: Option<Arc<crate::builtin::stylo_worker::StyloWorkerPermit>>,
               ticket: u64|
     -> Result<backend_engine::WorkspaceSnapshot, Box<dyn std::error::Error>> {
        let (adapter, daemon) = fixture.parts();
        let mut work = capture_index_scan(
            daemon,
            package,
            &label,
            scan_root,
            None,
            ticket,
            CompileExecutionIntent::Interactive,
            false,
            Arc::new(AtomicBool::new(false)),
            adapter.compiler.python_interpreter_admission(),
        )?;
        work.stylo_worker = permit;
        let scan = run_index_scan(work).map_err(|e| match e {
            IndexScanFailure::Cancelled => BuiltinModelError("cancelled".into()),
            IndexScanFailure::Refused(e) => e,
        })?;
        let base = daemon.engine().daemon().owner().snapshot();
        let PreparedSourceScan::Ready(selection) = prepare_source_scan(
            &base,
            scan,
            &adapter.compiler,
            &mut adapter.semantic_authority,
            None,
            None,
            &mut || {},
        )?
        else {
            return Err("unexpected compiler capture".into());
        };
        if let Some(intent) = selection.intent {
            super::super::commit_builtin_intent(daemon, ticket + 1, &intent)?;
        }
        Ok(daemon.engine().daemon().owner().snapshot())
    };
    let secret = [51; 32];
    // Regenerate only real native input until its final physical row is an
    // observed class fact. Project-local hashes vary with the temporary path.
    let mut selected = None;
    for trial in 0..8u64 {
        let marker_list = (0..128)
            .map(|i| format!(".terminal{trial}_{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let css_text =
            format!(".foo {{ color: red }} .foo {{ color: blue }} {marker_list} {{ color: red }}");
        fs::write(&css, css_text)?;
        let snapshot = run(
            &mut fixture,
            &root,
            Some(Arc::clone(&permit)),
            1300 + trial * 100,
        )?;
        let relation = backend_engine::builtin::stylo_analysis_relation(&snapshot)?
            .ok_or("missing relation")?;
        let mut after = None;
        let mut before_last = None;
        let mut last = None;
        loop {
            let page = relation.page(after.as_ref(), 1)?;
            let Some((key, value)) = page.entries().first() else {
                break;
            };
            before_last = after;
            after = Some(*key);
            last = Some((*key, value.clone()));
        }
        let (last_key, last_row) = last.ok_or("empty admitted relation")?;
        assert_eq!(relation.page(before_last.as_ref(), 1)?.entries().len(), 1);
        assert!(relation.page(Some(&last_key), 1)?.entries().is_empty());
        let name = match last_row {
            backend_engine::builtin::StyloAnalysisRecord::Fact {
                fact: backend_engine::builtin::StyloProjectedRow::SourceOccurrence(site),
                ..
            } if site.role == backend_semantic::stylo_analysis::StyloSourceRole::ClassName => {
                Some(site.decoded)
            }
            backend_engine::builtin::StyloAnalysisRecord::Fact {
                fact: backend_engine::builtin::StyloProjectedRow::Selector(selector),
                ..
            } => selector
                .components
                .into_iter()
                .find(|component| {
                    component.kind
                        == backend_semantic::stylo_analysis::StyloSelectorComponentKind::Class
                })
                .map(|component| component.name),
            _ => None,
        };
        if let Some(name) = name {
            selected = Some((snapshot, last_key, name));
            break;
        }
    }
    let (first, last_key, terminal_name) =
        selected.ok_or("could not construct exact-final class fixture within 8 attempts")?;
    let mut terminal_request = StyloQueryRequest {
        package: backend_library::PackageReference::parse(label.clone())
            .map_err(|e| e.to_string())?,
        role: StyloNameRole::Class,
        name: terminal_name,
        page_rows: 1,
        output_bytes: 131_072,
        scan_rows: 4096,
        scan_bytes: 4_194_304,
        continuation: None,
    };
    loop {
        let page = query(&first, &secret, &terminal_request)?;
        assert!(serde_json::to_vec(&page)?.len() <= terminal_request.output_bytes as usize);
        let contains_last = page.hits.iter().any(|hit| matches!(hit, StyloQueryHit::SourceSite { id, .. } | StyloQueryHit::NativeSelector { id, .. } if *id == last_key));
        if contains_last {
            assert_eq!(page.terminal, StyloQueryTerminal::Complete);
            break;
        }
        match page.terminal {
            StyloQueryTerminal::More { continuation, .. } => {
                terminal_request.continuation = Some(continuation)
            }
            other => return Err(format!("final class not reached: {other:?}").into()),
        }
    }
    let mut request = StyloQueryRequest {
        package: backend_library::PackageReference::parse(label.clone())
            .map_err(|e| e.to_string())?,
        role: StyloNameRole::Class,
        name: "foo".into(),
        page_rows: 1,
        output_bytes: 131_072,
        scan_rows: 64,
        scan_bytes: 4_194_304,
        continuation: None,
    };
    let mut cursor = None;
    let mut source = None;
    loop {
        let page = query(&first, &secret, &request)?;
        for hit in page.hits {
            if let StyloQueryHit::SourceSite { source: site, .. } = hit {
                source = Some(site);
            }
        }
        match page.terminal {
            StyloQueryTerminal::More { continuation, .. } => {
                if cursor.is_none() {
                    cursor = Some(continuation.clone());
                }
                request.continuation = Some(continuation);
            }
            StyloQueryTerminal::Complete => break,
            other => return Err(format!("unexpected initial terminal {other:?}").into()),
        }
    }
    let cursor = cursor.ok_or("fixture did not page")?;
    let source = source.ok_or("fixture lacks a source site")?;
    let no_op = run(&mut fixture, &root, Some(Arc::clone(&permit)), 1310)?;
    let first_root = backend_engine::builtin::stylo_analysis_relation(&first)?
        .ok_or("missing root")?
        .root();
    let no_op_root = backend_engine::builtin::stylo_analysis_relation(&no_op)?
        .ok_or("missing root")?
        .root();
    assert_eq!(first.root(), no_op.root());
    assert_eq!(first_root, no_op_root);
    request.continuation = Some(cursor.clone());
    query(&no_op, &secret, &request)?;
    let other_root = root.parent().ok_or("parent missing")?.join("retargeted");
    fs::create_dir_all(&other_root)?;
    fs::write(other_root.join("site.css"), ".other { color: red }")?;
    let retained = run(&mut fixture, &other_root, None, 1320)?;
    request.continuation = None;
    let mut retained_source = None;
    loop {
        let page = query(&retained, &secret, &request)?;
        assert!(matches!(
            page.basis.attempt,
            StyloAttemptState::RetainedAfterUnavailable { .. }
        ));
        for hit in page.hits {
            if let StyloQueryHit::SourceSite { source: site, .. } = hit {
                retained_source = Some(site);
            }
        }
        match page.terminal {
            StyloQueryTerminal::More { continuation, .. } => {
                request.continuation = Some(continuation)
            }
            StyloQueryTerminal::Complete => break,
            other => return Err(format!("unexpected retained terminal {other:?}").into()),
        }
    }
    let retained_source = retained_source.ok_or("retained site missing")?;
    assert!(
        matches!(read_site(&retained, &secret, &retained_source)?, StyloSourceRead::Available { served_root, .. } if served_root == label)
    );
    fs::remove_file(&css)?;
    assert_eq!(
        read_site(&retained, &secret, &retained_source)?,
        StyloSourceRead::BytesNotRetained
    );
    fs::write(&css, ".bar { color: red } .bar { color: blue }")?;
    let changed = run(&mut fixture, &root, Some(permit), 1330)?;
    assert_eq!(first.root(), changed.root());
    assert_ne!(
        first_root,
        backend_engine::builtin::stylo_analysis_relation(&changed)?
            .ok_or("missing root")?
            .root()
    );
    request.continuation = Some(cursor);
    assert!(query(&changed, &secret, &request).is_err());
    assert_eq!(
        read_site(&changed, &secret, &source)?,
        StyloSourceRead::Expired
    );
    Ok(())
}
