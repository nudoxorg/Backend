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
    // Public wire journey: discover the exact declaration seed, then request
    // candidate impacts with the durable owner-issued handles and cursor.
    let mut public_tone = request.clone();
    public_tone.role = StyloNameRole::CustomProperty;
    public_tone.name = "--tone".into();
    let public_seed = loop {
        let reply = invoke(adapter, daemon, backend_library::SurfaceCommand::StyloQuery { request: public_tone.clone() }, 1402)?;
        let backend_library::SurfaceReply::StyloQuery(page) = reply else { return Err("wrong CSS seed reply".into()); };
        if let Some(source) = page.hits.iter().find_map(|hit| match hit {
            StyloQueryHit::SourceSite { source, .. } => Some(source.clone()), _ => None,
        }) { break source; }
        match page.terminal { StyloQueryTerminal::More { continuation, .. } => public_tone.continuation = Some(continuation), _ => return Err("public declaration seed missing".into()) }
    };
    public_tone.continuation = None;
    public_tone.output_bytes = 147_456;
    let mut impact_request = backend_library::stylo_impact::StyloImpactRequest { seed: public_seed, query: public_tone, continuation: None };
    let mut public_candidates = 0;
    loop {
        let reply = invoke(adapter, daemon, backend_library::SurfaceCommand::StyloImpact { request: impact_request.clone() }, 1403)?;
        let backend_library::SurfaceReply::StyloImpact(page) = reply else { return Err("wrong impact reply".into()); };
        assert!(page.basis.gaps.contains(&StyloScopeGap::ProviderUnknown));
        public_candidates += page.rows.len();
        match page.terminal {
            backend_library::stylo_impact::StyloImpactTerminal::Complete => break,
            backend_library::stylo_impact::StyloImpactTerminal::More { continuation, .. } => impact_request.continuation = Some(continuation),
            other => return Err(format!("unexpected public impact terminal {other:?}").into()),
        }
    }
    assert_eq!(public_candidates, 2);
    let mut missing = request.clone();
    missing.scan_rows = 7;
    missing.scan_bytes = 229_376;
    for (role, name) in [
        (StyloNameRole::Class, "missing-class"),
        (StyloNameRole::CustomProperty, "--declared-nowhere"),
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
    // An ordinary declaration which uses var() is reported from the parser's own
    // references (the vendored Stylo exposes them); the page still declares that
    // declarations outside the walked rule kinds are not covered.
    let mut ordinary = request.clone();
    ordinary.role = StyloNameRole::CustomProperty;
    ordinary.name = "--ordinary-only".into();
    ordinary.continuation = None;
    let mut ordinary_uses = 0;
    loop {
        let page = query(&snapshot, &secret, &ordinary)?;
        assert!(
            page.basis
                .gaps
                .contains(&StyloScopeGap::OrdinaryVariableReferencesUnavailable)
        );
        ordinary_uses += page
            .hits
            .iter()
            .filter(|hit| matches!(hit, StyloQueryHit::NativeDependency { dependency, .. } if dependency.name == "ordinary-only"))
            .count();
        match page.terminal {
            StyloQueryTerminal::Complete => break,
            StyloQueryTerminal::More { continuation, .. } => ordinary.continuation = Some(continuation),
            other => return Err(format!("unexpected ordinary-use terminal {other:?}").into()),
        }
    }
    assert!(ordinary_uses >= 1, "a color: var(--ordinary-only) declaration is a native dependency");
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
    let mut definition_refs = Vec::new();
    loop {
        let page = query(&snapshot, &secret, &request)?;
        for hit in page.hits {
            match hit {
                StyloQueryHit::SourceSite { id, source, .. } => { definition_sites.push(id); definition_refs.push(source); },
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
    // Traverse only owner-selected native references. Equal names are candidates,
    // and a cursor from one authored declaration cannot be reused for another.
    request.continuation = None;
    request.output_bytes = 147_456;
    let mut impact_request = backend_library::stylo_impact::StyloImpactRequest {
        seed: definition_refs[0].clone(), query: request.clone(), continuation: None,
    };
    let mut candidates = 0;
    let mut checked_seed_binding = false;
    loop {
        let page = crate::builtin::commands::stylo_query::impact(&snapshot, &secret, &impact_request)?;
        assert!(page.basis.gaps.contains(&StyloScopeGap::ProviderUnknown));
        candidates += page.rows.len();
        match page.terminal {
            backend_library::stylo_impact::StyloImpactTerminal::Complete => break,
            backend_library::stylo_impact::StyloImpactTerminal::More { continuation, .. } => {
                if !checked_seed_binding {
                    let mut wrong = impact_request.clone();
                    wrong.seed = definition_refs[1].clone();
                    wrong.continuation = Some(continuation.clone());
                    assert!(crate::builtin::commands::stylo_query::impact(&snapshot, &secret, &wrong).is_err());
                    checked_seed_binding = true;
                }
                impact_request.continuation = Some(continuation);
            },
            other => return Err(format!("unexpected impact terminal {other:?}").into()),
        }
    }
    assert_eq!(candidates, 2);
    assert!(checked_seed_binding);

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
    let captured_at = std::time::Instant::now();
    let lease = adapter.capture_css_impact(
        daemon,
        backend_library::package_key(&label).to_bytes(),
        crate::builtin::commands::stylo_impact_capture::CaptureLimits {
            rows: 4096, scan_bytes: 4 * 1024 * 1024,
            retained_bytes: 4 * 1024 * 1024, source_bytes: 1024 * 1024, temporary_bytes: 4 * 1024 * 1024,
        },
        captured_at,
        || false,
    )?;
    let walked = crate::builtin::commands::stylo_impact_adapter::walk_lease(
        adapter.impact_retention.as_mut().ok_or("retention unavailable")?.get(lease, captured_at).map_err(|e| format!("{e:?}"))?,
        definition_sites[0],
        crate::builtin::commands::stylo_impact_walk::Limits { input_rows: 4096, input_bytes: 4 * 1024 * 1024, edges: 128, nodes: 128, depth: 8, rows: 128, witness_bytes: 16 * 1024, temporary_bytes: 4 * 1024 * 1024 },
        || false,
    ).map_err(|e| format!("{e:?}"))?;
    assert_eq!(walked.outcome.stop, crate::builtin::commands::stylo_impact_walk::Stop::Complete);
    assert_eq!(walked.outcome.rows.len(), 2);
    assert!(walked.outcome.rows.iter().all(|row| row.owner.is_some()));
    let host_key = adapter.impact_retention.as_mut().ok_or("retention unavailable")?.get(lease, captured_at).map_err(|e| format!("{e:?}"))?.sources.first().ok_or("captured source missing")?.key;
    assert_eq!(adapter.impact_retention.as_mut().ok_or("retention unavailable")?.get(lease, captured_at).map_err(|e| format!("{e:?}"))?.historical_source(&host_key), Some(bytes.as_bytes()));
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
    assert_eq!(adapter.impact_retention.as_mut().ok_or("retention unavailable")?.get(lease, captured_at).map_err(|e| format!("{e:?}"))?.historical_source(&host_key), Some(bytes.as_bytes()));
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
    let capture_freshness = |fixture: &mut AdapterFixture| -> Result<(crate::builtin::commands::impact_retention::LeaseId, crate::builtin::commands::impact_retention::Freshness, crate::builtin::commands::impact_retention::Worker), Box<dyn std::error::Error>> {
        let (adapter, daemon) = fixture.parts();
        let now = std::time::Instant::now();
        let lease = adapter.capture_css_impact(daemon, backend_library::package_key(&label).to_bytes(), crate::builtin::commands::stylo_impact_capture::CaptureLimits { rows: 4096, scan_bytes: 4 * 1024 * 1024, retained_bytes: 4 * 1024 * 1024, source_bytes: 1024 * 1024, temporary_bytes: 4 * 1024 * 1024 }, now, || false)?;
        let captured = adapter.impact_retention.as_mut().ok_or("retention unavailable")?.get(lease, now).map_err(|e| format!("{e:?}"))?;
        Ok((lease, captured.basis.freshness, captured.basis.worker))
    };
    let (initial_lease, initial_freshness, initial_worker) = capture_freshness(&mut fixture)?;
    assert_eq!(initial_freshness, crate::builtin::commands::impact_retention::Freshness::LatestObserved);
    assert!(matches!(initial_worker, crate::builtin::commands::impact_retention::Worker::Admitted(_)));
    // A budget that cannot read Attempt does not fabricate an absent legacy row.
    {
        let (adapter, daemon) = fixture.parts();
        let now = std::time::Instant::now();
        let tiny = adapter.capture_css_impact(daemon, backend_library::package_key(&label).to_bytes(), crate::builtin::commands::stylo_impact_capture::CaptureLimits { rows: 1, scan_bytes: 32 * 1024, retained_bytes: 1024 * 1024, source_bytes: 0, temporary_bytes: 1024 * 1024 }, now, || false)?;
        let lease = adapter.impact_retention.as_mut().ok_or("retention unavailable")?.get(tiny, now).map_err(|e| format!("{e:?}"))?;
        assert_eq!(lease.basis.freshness, crate::builtin::commands::impact_retention::Freshness::Unknown);
    }
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
    let (_, retained_freshness, _) = capture_freshness(&mut fixture)?;
    assert!(matches!(retained_freshness, crate::builtin::commands::impact_retention::Freshness::RetainedAfterUnavailable(_)));
    let (adapter, _) = fixture.parts();
    let old = adapter.impact_retention.as_mut().ok_or("retention unavailable")?.get(initial_lease, std::time::Instant::now()).map_err(|e| format!("{e:?}"))?;
    assert_eq!(old.basis.freshness, crate::builtin::commands::impact_retention::Freshness::LatestObserved);
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

#[test]
#[ignore = "requires NUDOX_TEST_STYLO_WORKER and native owner admission"]
fn retained_css_change_uses_exact_base_and_target_without_name_continuity()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::builtin::commands::{impact_retention::ChangeSide, stylo_impact_capture::CaptureLimits, stylo_impact_walk::Limits};
    let mut fixture = AdapterFixture::open(TempTree::new(), false);
    let (package, label) = fixture.add_target();
    let root = PathBuf::from(&label);
    let css = root.join("theme.css");
    let worker = PathBuf::from(std::env::var_os("NUDOX_TEST_STYLO_WORKER").ok_or("worker missing")?);
    let artifact = backend_compile::ToolchainArtifact::from_path(&worker, Vec::new())?;
    let permit = Arc::new(crate::builtin::stylo_worker::StyloWorkerPermit::from_admitted(worker, artifact, backend_frontend_stylo::support_fingerprint()).map_err(|e| format!("{e:?}"))?);
    let publish = |fixture: &mut AdapterFixture, ticket: u64| -> Result<(), Box<dyn std::error::Error>> {
        let (adapter, daemon) = fixture.parts();
        let mut work = capture_index_scan(daemon, package, &label, &root, None, ticket, CompileExecutionIntent::Interactive, false, Arc::new(AtomicBool::new(false)), adapter.compiler.python_interpreter_admission())?;
        work.stylo_worker = Some(Arc::clone(&permit));
        let scan = run_index_scan(work).map_err(|e| match e { IndexScanFailure::Cancelled => BuiltinModelError("cancelled".into()), IndexScanFailure::Refused(e) => e })?;
        let snapshot = daemon.engine().daemon().owner().snapshot();
        let PreparedSourceScan::Ready(selection) = prepare_source_scan(&snapshot, scan, &adapter.compiler, &mut adapter.semantic_authority, None, None, &mut || {})? else { return Err("unexpected capture".into()); };
        super::super::commit_builtin_intent(daemon, ticket + 1, &selection.intent.ok_or("missing intent")?)?;
        Ok(())
    };
    let invoke = |adapter: &mut CommandAdapter, daemon: &mut ProductDaemon, surface: backend_library::SurfaceCommand| -> Result<backend_library::SurfaceReply, Box<dyn std::error::Error>> {
        let command = backend_library::CommandDto::new(1800, backend_library::Command::Surface(surface));
        let body = backend_library::encode_command_body(&command)?;
        let response = adapter.execute(daemon, &body)?;
        let reply = backend_library::decode_reply_body(&response)?;
        backend_library::admit_reply(&command, &reply)?;
        match reply.reply { backend_library::CommandReply::Surface(reply) => Ok(reply), other => Err(format!("unexpected reply {other:?}").into()) }
    };
    let select = |fixture: &mut AdapterFixture, name: &str| -> Result<([u8;32], crate::builtin::commands::impact_retention::LeaseId, backend_library::impact_comparison::ImpactCaptureReply), Box<dyn std::error::Error>> {
        let (adapter, daemon) = fixture.parts();
        let snapshot = daemon.engine().daemon().owner().snapshot();
        let mut request = StyloQueryRequest { package: backend_library::PackageReference::parse(label.clone()).map_err(|e| e.to_string())?, role: StyloNameRole::CustomProperty, name: name.into(), page_rows: 128, output_bytes: 131_072, scan_rows: 4096, scan_bytes: 4 * 1024 * 1024, continuation: None };
        let seed = loop {
            let page = query(&snapshot, &[71;32], &request)?;
            if let Some(id) = page.hits.iter().find_map(|hit| match hit { StyloQueryHit::SourceSite { id, .. } => Some(*id), _ => None }) { break id; }
            match page.terminal { StyloQueryTerminal::More { continuation, .. } => request.continuation = Some(continuation), _ => return Err("seed missing".into()) }
        };
        let lease = adapter.capture_css_impact(daemon, backend_library::package_key(&label).to_bytes(), CaptureLimits { rows: 4096, scan_bytes: 4 * 1024 * 1024, retained_bytes: 4 * 1024 * 1024, source_bytes: 1024 * 1024, temporary_bytes: 4 * 1024 * 1024 }, std::time::Instant::now(), || false)?;
        request.continuation = None;
        let public_seed = loop {
            let response = invoke(adapter, daemon, backend_library::SurfaceCommand::StyloQuery { request: request.clone() })?;
            let backend_library::SurfaceReply::StyloQuery(page) = response else { return Err("wrong public CSS query".into()); };
            if let Some(source) = page.hits.iter().find_map(|hit| match hit { StyloQueryHit::SourceSite { source, .. } => Some(source.clone()), _ => None }) { break source; }
            match page.terminal { StyloQueryTerminal::More { continuation, .. } => request.continuation = Some(continuation), _ => return Err("public seed missing".into()) }
        };
        let capture = backend_library::impact_comparison::ImpactCaptureRequest { package: request.package.clone(), seed: Some(public_seed), scan_rows: 4096, scan_bytes: 4 * 1024 * 1024, retain_bytes: 4 * 1024 * 1024, source_bytes: 1024 * 1024, temporary_bytes: 4 * 1024 * 1024, time_ms: 10_000 };
        let reply = invoke(adapter, daemon, backend_library::SurfaceCommand::ImpactCapture { request: capture })?;
        let backend_library::SurfaceReply::ImpactCapture(public) = reply else { return Err("wrong public capture".into()); };
        Ok((seed, lease, public))
    };
    fs::write(&css, ".theme { --tone: red; --paint: var(--tone); --accent: var(--paint); }")?;
    publish(&mut fixture, 1700)?;
    let (old_seed, base, public_base) = select(&mut fixture, "--tone")?;
    fs::write(&css, ".theme { --hue: red; --paint: var(--hue); --accent: var(--paint); }")?;
    publish(&mut fixture, 1710)?;
    let (new_seed, target, public_target) = select(&mut fixture, "--hue")?;
    {
    let (adapter, daemon) = fixture.parts();
    {
    let store = adapter.impact_retention.as_mut().ok_or("retention unavailable")?;
    let limits = Limits { input_rows: 4096, input_bytes: 4 * 1024 * 1024, edges: 128, nodes: 128, depth: 8, rows: 128, witness_bytes: 16 * 1024, temporary_bytes: 4 * 1024 * 1024 };
    let now = std::time::Instant::now();
    let removed = crate::builtin::commands::stylo_impact_adapter::walk_change(store, base, target, ChangeSide::Base, old_seed, limits, now, || false).map_err(|e| format!("{e:?}"))?;
    assert_ne!(removed.base.native, removed.target.native);
    assert_eq!(removed.result.rows.len(), 2);
    // Native row enumeration coverage is distinct from CSS semantic coverage.
    assert!(matches!(removed.relation_coverage, crate::builtin::commands::impact_retention::Coverage::Complete | crate::builtin::commands::impact_retention::Coverage::Gapped));
    let added = crate::builtin::commands::stylo_impact_adapter::walk_change(store, base, target, ChangeSide::Target, new_seed, limits, now, || false).map_err(|e| format!("{e:?}"))?;
    assert_eq!(added.result.rows.len(), 2);
    assert!(crate::builtin::commands::stylo_impact_adapter::walk_change(store, base, target, ChangeSide::Target, old_seed, limits, now, || false).is_err());
    assert!(crate::builtin::commands::stylo_impact_adapter::walk_change(store, base, target, ChangeSide::Base, new_seed, limits, now, || false).is_err());
    let diff_limits = crate::builtin::commands::stylo_impact_diff::Limits {
        scan_rows: 8192, scan_bytes: 8 * 1024 * 1024, temporary_bytes: 8 * 1024 * 1024,
        diff: crate::builtin::commands::impact_diff::Limits { input_rows: 8192, compared_bytes: 8 * 1024 * 1024, comparisons: 8192, output_rows: 1024, output_bytes: 128 * 1024 },
    };
    let source_diff = crate::builtin::commands::stylo_impact_diff::compare(store, base, target, crate::builtin::commands::stylo_impact_diff::Domain::VerifiedSourceImages, diff_limits, now, || false).map_err(|e| format!("{e:?}"))?;
    assert!(source_diff.result.changes.iter().any(|change| change.side == crate::builtin::commands::impact_diff::Side::BothChanged && change.certainty == crate::builtin::commands::impact_diff::Certainty::Exact));
    let analysis = crate::builtin::commands::stylo_impact_diff::compare_and_walk(store, base, target, diff_limits, crate::builtin::commands::stylo_impact_diff::WalkBudget { seed_walks: 16, input_rows: 16_384, input_bytes: 16 * 1024 * 1024, edges: 512, nodes: 512, rows: 512, witness_bytes: 128 * 1024, path_bytes: 128 * 1024, depth: 8, temporary_bytes: 4 * 1024 * 1024 }, 0, 1024, now, || false).map_err(|e| format!("{e:?}"))?;
    assert!(analysis.impacts.iter().any(|impact| impact.change.id == old_seed && impact.change.side == crate::builtin::commands::impact_diff::Side::BaseOnly && impact.change.certainty == crate::builtin::commands::impact_diff::Certainty::Exact && impact.rows.len() == 2));
    assert!(analysis.impacts.iter().any(|impact| impact.change.id == new_seed && impact.change.side == crate::builtin::commands::impact_diff::Side::TargetOnly && impact.change.certainty == crate::builtin::commands::impact_diff::Certainty::Exact && impact.rows.len() == 2));
    store.release(base); store.release(target);
    }
    let historical = invoke(adapter, daemon, backend_library::SurfaceCommand::ImpactReadSource { request: backend_library::impact_comparison::ImpactReadSourceRequest { source: public_base.source.clone().ok_or("historical source missing")? } })?;
    assert!(matches!(historical, backend_library::SurfaceReply::ImpactReadSource(backend_library::impact_comparison::ImpactSourceReply::Available { ref text, .. }) if text == "--tone"));
    let mut comparison = backend_library::impact_comparison::ImpactCompareRequest { base: public_base.lease.clone(), target: public_target.lease.clone(), side: backend_library::impact_comparison::ImpactSide::Base, limits: backend_library::impact_comparison::ImpactLimits { input_rows: 4096, input_bytes: 4 * 1024 * 1024, edges: 128, nodes: 128, depth: 8, page_rows: 1, output_bytes: 131_072, witness_bytes: 16 * 1024, temporary_bytes: 4 * 1024 * 1024, time_ms: 10_000 }, continuation: None, cancel: false };
    let mut count = 0;
    loop {
        let response = invoke(adapter, daemon, backend_library::SurfaceCommand::ImpactCompare { request: comparison.clone() })?;
        let backend_library::SurfaceReply::ImpactCompare(page) = response else { return Err("wrong public comparison".into()); };
        assert!(page.relation_gapped && page.provider_unknown);
        assert_eq!(page.base_attempt_at_capture, backend_library::impact_comparison::ImpactAttemptAtCapture::LatestObserved);
        assert_eq!(page.target_attempt_at_capture, backend_library::impact_comparison::ImpactAttemptAtCapture::LatestObserved);
        count += page.rows.len();
        match page.terminal { backend_library::impact_comparison::ImpactTerminal::More { continuation } => comparison.continuation = Some(continuation), backend_library::impact_comparison::ImpactTerminal::Complete => break, other => return Err(format!("unexpected public terminal {other:?}").into()) }
    }
    assert_eq!(count, 2);
    }
    // Delete the only stylesheet, index the target, and capture it without
    // inventing a target seed. The base still traverses; target is unavailable.
    fs::remove_file(&css)?;
    publish(&mut fixture, 1720)?;
    let (adapter, daemon) = fixture.parts();
    let target_request = backend_library::impact_comparison::ImpactCaptureRequest { package: backend_library::PackageReference::parse(label.clone()).map_err(|e| e.to_string())?, seed: None, scan_rows: 4096, scan_bytes: 4 * 1024 * 1024, retain_bytes: 4 * 1024 * 1024, source_bytes: 1024 * 1024, temporary_bytes: 4 * 1024 * 1024, time_ms: 10_000 };
    let response = invoke(adapter, daemon, backend_library::SurfaceCommand::ImpactCapture { request: target_request })?;
    let backend_library::SurfaceReply::ImpactCapture(empty_target) = response else { return Err("wrong unseeded target reply".into()); };
    assert!(empty_target.seed.is_none() && empty_target.source.is_none());
    let limits = backend_library::impact_comparison::ImpactLimits { input_rows: 4096, input_bytes: 4 * 1024 * 1024, edges: 128, nodes: 128, depth: 8, page_rows: 128, output_bytes: 131_072, witness_bytes: 16 * 1024, temporary_bytes: 4 * 1024 * 1024, time_ms: 10_000 };
    let mut request = backend_library::impact_comparison::ImpactCompareRequest { base: public_base.lease.clone(), target: empty_target.lease.clone(), side: backend_library::impact_comparison::ImpactSide::Base, limits, continuation: None, cancel: false };
    let response = invoke(adapter, daemon, backend_library::SurfaceCommand::ImpactCompare { request: request.clone() })?;
    let backend_library::SurfaceReply::ImpactCompare(base_page) = response else { return Err("wrong deletion base reply".into()); };
    assert_eq!(base_page.rows.len(), 2);
    request.side = backend_library::impact_comparison::ImpactSide::Target;
    let response = invoke(adapter, daemon, backend_library::SurfaceCommand::ImpactCompare { request })?;
    let backend_library::SurfaceReply::ImpactCompare(target_page) = response else { return Err("wrong deletion target reply".into()); };
    assert!(matches!(target_page.terminal, backend_library::impact_comparison::ImpactTerminal::SeedUnavailable));
    assert!(target_page.rows.is_empty());
    let diff_request = backend_library::impact_comparison::ImpactDiffRequest {
        base: public_base.lease.clone(), target: empty_target.lease.clone(), domain: backend_library::impact_comparison::ImpactDiffDomain::NativeFacts,
        limits: backend_library::impact_comparison::ImpactDiffLimits { scan_rows: 8192, scan_bytes: 8 * 1024 * 1024, temporary_bytes: 8 * 1024 * 1024, compared_bytes: 8 * 1024 * 1024, comparisons: 8192, page_rows: 128, output_bytes: 2 * 1024 * 1024, seed_walks: 16, walk_input_rows: 16_384, walk_input_bytes: 16 * 1024 * 1024, walk_edges: 512, walk_nodes: 512, walk_depth: 8, walk_rows: 512, witness_bytes: 128 * 1024, walk_path_bytes: 128 * 1024, time_ms: 10_000 },
        continuation: None, cancel: false,
    };
    let response = invoke(adapter, daemon, backend_library::SurfaceCommand::ImpactDiff { request: diff_request })?;
    let backend_library::SurfaceReply::ImpactDiff(diff_page) = response else { return Err("wrong public diff".into()); };
    assert!(matches!(diff_page.terminal, backend_library::impact_comparison::ImpactDiffTerminal::Complete));
    let removed_seed = diff_page.rows.iter().find(|row| row.id == old_seed).ok_or("removed seed missing from diff")?;
    assert_eq!(removed_seed.side, backend_library::impact_comparison::ImpactDiffSide::BaseOnly);
    assert_eq!(removed_seed.certainty, if diff_page.target_capture_complete { backend_library::impact_comparison::ImpactDiffCertainty::ExactCapturedRow } else { backend_library::impact_comparison::ImpactDiffCertainty::CandidateMissing });
    assert_eq!(removed_seed.impacts.len(), 2);
    assert!(removed_seed.impacts.iter().all(|path| path.strength == backend_library::impact_comparison::ImpactStrength::Candidate));
    let historical = invoke(adapter, daemon, backend_library::SurfaceCommand::ImpactReadSource { request: backend_library::impact_comparison::ImpactReadSourceRequest { source: removed_seed.base_source.clone().ok_or("changed seed source handle missing")? } })?;
    assert!(matches!(historical, backend_library::SurfaceReply::ImpactReadSource(backend_library::impact_comparison::ImpactSourceReply::Available { ref text, .. }) if text == "--tone"));
    Ok(())
}
