//! Native compiler captures exercised through the actual selected References command.
use super::*;
use crate::builtin::{
    BuiltinIntent, BuiltinSemanticChange, BuiltinSourceChange, BuiltinViewAdmission,
    generation_residence::SemanticGenerationResidence, semantic_authority::SemanticAuthority,
};
use backend_engine::application::{
    LocalCompilerHost, LocalHostDiscovery, LocalHostEnvironment, LocalHostVariable,
    OwnedPackageSource, OwnedPackageSourceSet,
};
use backend_engine::builtin::{ProductSemanticCaptureOutcome, SemanticSourceCapture};
use backend_library::interface::{CorrelationId, GenerateTarget, PackageCompileRequest};
use backend_semantic::vocabulary::{LanguageProfile, Stage, TypeScriptSource};
use std::{
    ffi::OsString,
    fs,
    num::NonZeroU32,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Env(PathBuf);
impl LocalHostEnvironment for Env {
    fn value(&self, key: LocalHostVariable) -> Option<OsString> {
        (key == LocalHostVariable::NudoxDataRoot).then(|| self.0.clone().into_os_string())
    }
    fn search_path(&self) -> Option<OsString> {
        Some(OsString::new())
    }
}
fn owner(path: &Path) -> crate::builtin::ProductDaemon {
    let profile =
        crate::builtin::profile_descriptor(crate::builtin::BuiltinProfile::Product).unwrap();
    let dispatcher = crate::builtin::builtin_dispatcher(Some([0x79; 32]), profile, 60_000).unwrap();
    crate::Locald::open_with_dispatcher_and_registry(
        path,
        crate::builtin::BuiltinModel,
        crate::builtin::genesis().unwrap(),
        dispatcher,
        backend_engine::DaemonConfig::default(),
        crate::builtin::product_relation_registry().unwrap(),
    )
    .unwrap()
}

#[test]
fn actual_selected_native_references_preserve_partial_and_unknown()
-> Result<(), Box<dyn std::error::Error>> {
    for private in [false, true] {
        run(private, false)?;
    }
    Ok(())
}
#[test]
fn complete_native_graph_rejects_changed_source() -> Result<(), Box<dyn std::error::Error>> {
    run(false, true)
}
fn run(private: bool, complete: bool) -> Result<(), Box<dyn std::error::Error>> {
    let root = Root(std::env::temp_dir().join(format!(
        "nudox-selected-native-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    )));
    let project = root.0.join("project");
    fs::create_dir_all(project.join("ui"))?;
    let button = if private {
        "function Button() { return 1; }\n"
    } else {
        "export function Button() { return 1; }\n"
    };
    let barrel = "export { Button as Action } from './button';\n";
    let unused = "export function Unused() { return 0; }\n";
    let page = "// λ\nimport { Action as Confirm } from './ui';\nexport function Page() { return <><Confirm /><Confirm /></>; }\n";
    let other = "export function Button() { return 2; }\n";
    let other_page = "import { Button as Confirm } from './ui/other';\nexport function OtherPage() { return <Confirm />; }\n";
    let excluded = "export const excluded = 1;\n";
    let mut files = vec![
        ("other.tsx", other_page),
        ("page.tsx", page),
        ("skip.ts", excluded),
        ("skip.tsx", excluded),
        ("ui/button.ts", button),
        ("ui/index.ts", barrel),
        ("ui/other.ts", other),
        ("ui/unused.ts", unused),
    ];
    if complete {
        files.retain(|(path, _)| !path.starts_with("skip."));
    }
    for &(path, text) in &files {
        fs::write(project.join(path), text)?;
    }
    fs::write(
        project.join("tsconfig.json"),
        r#"{"compilerOptions":{"noLib":true,"types":[],"module":"ESNext","moduleResolution":"bundler","jsx":"preserve"},"files":["ui/button.ts","ui/index.ts","ui/other.ts","ui/unused.ts","page.tsx","other.tsx"]}"#,
    )?;
    let client = LocalCompilerHost::new(
        Env(root.0.join("compiler")),
        LocalHostDiscovery::ExplicitOnly,
    )
    .open()?;
    let ts = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
    let tsx = LanguageProfile::TypeScript(TypeScriptSource::Tsx);
    let coordinate = PackageUrl::parse("pkg:npm/fixture@1.0.0".to_owned())
        .map_err(|e| format!("coordinate {e:?}"))?;
    let request = |profile| {
        PackageCompileRequest::new(
            GenerateTarget {
                correlation: CorrelationId(991),
                profile,
                stage: Stage::LowerIr,
            },
            coordinate.clone(),
        )
        .map_err(|e| format!("profile {e:?}"))
    };
    let primary_ts = files.iter().copied().filter(|(path, _)| path.ends_with(".ts"))
        .map(|(p, s)| OwnedPackageSource::new(p, s))
        .collect::<Result<Vec<_>, _>>()?
        .into_boxed_slice();
    let primary_tsx = files.iter().copied().filter(|(path, _)| path.ends_with(".tsx"))
        .map(|(p, s)| OwnedPackageSource::new(p, s))
        .collect::<Result<Vec<_>, _>>()?
        .into_boxed_slice();
    let staged_ts = client.compile_package_sources_staged(OwnedPackageSourceSet::new(
        request(ts)?,
        project.clone(),
        primary_ts,
    )?)?;
    let staged_tsx = client.compile_package_sources_staged(OwnedPackageSourceSet::new(
        request(tsx)?,
        project.clone(),
        primary_tsx,
    )?)?;
    assert_eq!(staged_tsx.artifacts().len(), 2);
    assert_eq!(staged_tsx.coverage_gaps().len(), usize::from(!complete));
    if !complete { assert_eq!(staged_tsx.coverage_gaps()[0].relative_path(), "skip.tsx"); }
    assert_eq!(staged_ts.artifacts().len(), 4);
    assert_eq!(staged_ts.coverage_gaps().len(), usize::from(!complete));
    if !complete { assert_eq!(staged_ts.coverage_gaps()[0].relative_path(), "skip.ts"); }
    let key_ts = publication_key(ts, "pkg:npm/fixture@1.0.0")?;
    let key_tsx = publication_key(tsx, "pkg:npm/fixture@1.0.0")?;
    let package = key_ts.package_key();
    let workspace = root.0.join("workspace");
    let mut daemon = owner(&workspace);
    let mut authority = SemanticAuthority::open(&workspace)?;
    let version = *blake3::hash(
        files
            .iter()
            .flat_map(|(p, s)| [p.as_bytes(), s.as_bytes()].concat())
            .collect::<Vec<_>>()
            .as_slice(),
    )
    .as_bytes();
    let mut source_changes = Vec::new();
    let mut ids = Vec::new();
    for &(path, text) in &files {
        let file = ProductSourceRecord::file_with_retention(
            package.to_bytes(),
            path,
            backend_engine::SourceLanguage::TypeScript,
            *blake3::hash(text.as_bytes()).as_bytes(),
            [3; 32],
            Vec::<backend_compile::SourceDeclaration>::new(),
            backend_engine::DeclarationRetention::Unavailable(
                backend_library::SourceUnavailableReason::Unparsed,
            ),
        )?
        .with_source_identity(ContentId::<SourceFactDomain>::from_canonical_bytes(
            text.as_bytes(),
        ))?;
        let id = backend_engine::product_source_file_key(package.to_bytes(), path);
        ids.push(id);
        source_changes.push(BuiltinSourceChange {
            key: id,
            after: Some(file),
        });
    }
    ids.sort_unstable();
    source_changes.push(BuiltinSourceChange {
        key: package.to_bytes(),
        after: Some(ProductSourceRecord::project(
            "fixture",
            version,
            ids.clone(),
        )?),
    });
    let mut semantic_changes = Vec::new();
    let mut capture_changes = Vec::new();
    let mut selected = Vec::new();
    for (key, staged, count, coverage) in [
        (
            &key_ts,
            &staged_ts,
            if complete { 4_u64 } else { 5_u64 },
            if complete { SemanticPublicationCoverage::Complete } else { SemanticPublicationCoverage::Partial(PartialSemanticCoverage::new(
                NonZeroU32::new(4).unwrap(),
                NonZeroU32::new(5).unwrap(),
            )?) },
        ),
        (
            &key_tsx,
            &staged_tsx,
            if complete { 2_u64 } else { 3_u64 },
            if complete { SemanticPublicationCoverage::Complete } else { SemanticPublicationCoverage::Partial(PartialSemanticCoverage::new(
                NonZeroU32::new(2).unwrap(),
                NonZeroU32::new(3).unwrap(),
            )?) },
        ),
    ] {
        let digest = *staged.input_witness().input_root();
        let observation = authority.observe(key, digest, count)?;
        let capture =
            SemanticSourceCapture::new(None, version, digest, observation.sequence(), count)?;
        let attempt = authority.begin_candidate_attempt(key, &observation)?;
        let claim = authority
            .publish_staged(key, attempt, staged, |_| Ok(()))?
            .0;
        let record = ProductSemanticPublicationRecord::Published { coverage, claim };
        semantic_changes.push(BuiltinSemanticChange {
            key: key.for_generation_bytes(*claim.binding().identity.as_ref())?,
            after: Some(record.clone()),
        });
        semantic_changes.push(BuiltinSemanticChange {
            key: key.clone(),
            after: Some(record),
        });
        capture_changes.push(crate::builtin::BuiltinCaptureChange {
            cargo_target: None,
            key: key.clone(),
            expected: None,
            capture,
            outcome: ProductSemanticCaptureOutcome::Published { coverage, claim },
            compiler_failure: None,
        });
        selected.push((key.clone(), claim));
    }
    let pending = capture_changes
        .iter()
        .cloned()
        .map(|mut change| {
            change.outcome = ProductSemanticCaptureOutcome::Pending { prior: None };
            change
        })
        .collect::<Vec<_>>();
    let source_intent =
        BuiltinIntent::index_with_capture(package, "fixture", source_changes, Vec::new(), pending)?;
    crate::builtin::commands::commit_builtin_intent(&mut daemon, 910, &source_intent)?;
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let capture_relation = backend_engine::builtin::semantic_capture_relation(&snapshot)?
        .ok_or("pending capture relation missing")?;
    for change in &mut capture_changes {
        change.expected = Some(
            capture_relation
                .lookup(&change.key)?
                .ok_or("pending capture missing")?,
        );
    }
    let intent = BuiltinIntent::index_with_capture(
        package,
        "fixture",
        Vec::new(),
        semantic_changes,
        capture_changes,
    )?;
    authority.commit_product_selection_changes(selected, Vec::new(), || {
        crate::builtin::commands::commit_builtin_intent(&mut daemon, 911, &intent)
    })?;
    let snapshot = daemon.engine().daemon().owner().snapshot();
    let (base, _) = crate::builtin::initial_view_for_workspace(&snapshot)?;
    let mut rows = vec![backend_engine::Row::new(
        backend_engine::RowId::Package(package),
        base.basis(),
        "fixture",
    )];
    let mut names = BTreeMap::new();
    let mut page_symbol = None;
    for staged in [&staged_ts, &staged_tsx] {
        for ordinal in 0..staged.artifacts().len() {
            let image = backend_semantic::ir::SemanticImageView::reopen(
                staged
                    .semantic_output_object(ordinal)
                    .ok_or("image missing")?
                    .bytes(),
            )?;
            let path = view_build::compiled_source_path(&image)?;
            for entity in DocumentationSession::new(&image).publication_entities() {
                let entity = entity?;
                let Some(name) = entity
                    .name
                    .named_bytes()
                    .and_then(|name| std::str::from_utf8(name).ok())
                else {
                    continue;
                };
                let identity = entity.entity.version.identity();
                let symbol = view_build::semantic_symbol(package, identity);
                let label = view_build::semantic_coordinate("fixture", identity, name);
                if name == "Page" && path == "page.tsx" {
                    page_symbol = Some(symbol);
                }
                if name == "Button" && path == "ui/other.ts" {
                    names.insert("OtherButton".to_owned(), label.clone());
                } else if (name == "Button" && path == "ui/button.ts") || name == "Unused" {
                    names.insert(name.to_owned(), label.clone());
                }
                rows.push(backend_engine::Row::in_package(
                    backend_engine::RowId::Symbol(symbol),
                    base.basis(),
                    package,
                    label,
                ));
            }
        }
    }
    let view = backend_engine::ViewRoot::new_checked(
        base.recipe(),
        base.basis(),
        base.frontier(),
        rows,
        vec![backend_engine::ViewCoverage::Partial {
            lane: backend_engine::Lane::Semantic,
            completed: 1,
            total: 2,
        }],
        crate::builtin::builtin_view_capability_for_workspace(&snapshot)?,
    )
    .map_err(|error| format!("native fixture view: {error:?}"))?;
    let cursor = backend_engine::Cursor::for_view_root(&view);
    let admission = BuiltinViewAdmission {
        workspace_root: snapshot.root(),
        source_root: view.basis().root,
    };
    daemon
        .engine_mut()
        .daemon_mut()
        .publish_view(view, cursor, &admission, None)?;
    let mut generations = SemanticGenerationResidence::default();
    authority.install_image_loader(&mut generations);
    let mut image_rows = view_build::ImageRowResidence::default();
    let mut structural_calls = view_build::StructuralCallResidence::default();
    let graph_query = backend_engine::GraphNeighborhoodQuery::new(
        page_symbol.ok_or("Page missing")?,
        daemon.engine().daemon().library().revision_root(),
    );
    let graph = super::super::execute_semantic_graph(
        &mut structural_calls, &daemon, &client, &mut generations, &mut image_rows,
        graph_query, true,
    )?.ok_or("native graph missing")?;
    eprintln!("native graph private={private}: relations={:?} rich={:?}", graph.graph_relations, graph.rich_graph.as_ref().map(|g| &g.availability));
    assert!(matches!(graph.rich_graph.as_ref().map(|g| &g.availability),
        Some(backend_engine::GraphAvailability::Unavailable(_))),
        "partial input and uncertified graph joins must not claim complete availability");
    assert!(graph.graph_relations.as_ref().is_some_and(|relations| relations.iter().any(|relation| {
        relation.from == backend_engine::RowId::Symbol(page_symbol.unwrap())
            && relation.relation == backend_engine::SemanticLinkKind::Reads
            && relation.confidence == Some(backend_engine::SemanticConfidence::Compiler)
    })), "current native Page reads must survive with their compiler confidence");
    for (name, expected) in [("Button", 2), ("OtherButton", 0), ("Unused", 0)] {
        let target =
            backend_engine::ProductText::new(names.get(name).ok_or("target absent")?.clone())
                .map_err(|error| format!("target address: {error:?}"))?;
        let reply = execute_references(
            &mut structural_calls,
            &daemon,
            &client,
            &mut generations,
            &mut image_rows,
            &target,
        )?;
        let backend_engine::SurfaceReply::References {
            references,
            coverage,
            ..
        } = reply
        else {
            return Err("wrong reply".into());
        };
        eprintln!(
            "selected private={private} target={name} coverage={coverage:?} refs={references:?}"
        );
        let page_refs = references
            .iter()
            .filter(|record| {
                record.relation == backend_engine::SemanticLinkKind::Reads
                    && record.evidence.confidence == backend_engine::SemanticConfidence::Compiler
                    && record.evidence.source.as_ref().is_some_and(|span| {
                        span.file.as_str() == "page.tsx"
                            && page.get(span.start as usize..span.end as usize) == Some("Confirm")
                    })
            })
            .count();
        assert_eq!(page_refs, expected);
        if name == "Button" {
            assert!(
                !references.iter().any(|record| record
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|span| span.file.as_str() == "other.tsx")),
                "same-name sibling must not retarget"
            );
        }
        if name == "OtherButton" {
            assert!(references.iter().any(|record| {
                record
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|span| span.file.as_str() == "other.tsx")
                    && record.relation == backend_engine::SemanticLinkKind::Reads
            }));
            assert!(
                matches!(coverage, backend_engine::ReferenceCoverage::Partial { reason } if (complete || reason.as_str().contains("profile partial")) && reason.as_str().contains("checker diagnostics"))
            );
        } else if expected > 0 {
            assert!(
                matches!(coverage, backend_engine::ReferenceCoverage::Partial { reason } if (complete || reason.as_str().contains("profile partial")) && reason.as_str().contains("checker diagnostics"))
            );
        } else {
            assert!(references.is_empty());
            assert!(
                matches!(coverage, backend_engine::ReferenceCoverage::Unknown { reason } if (complete || reason.as_str().contains("profile partial")) && reason.as_str().contains("checker diagnostics"))
            );
        }
    }
    if !private {
        let changed_page = format!("{page}// newer source\n");
        fs::write(project.join("page.tsx"), &changed_page)?;
        let changed_file = ProductSourceRecord::file_with_retention(
            package.to_bytes(),
            "page.tsx",
            backend_engine::SourceLanguage::TypeScript,
            *blake3::hash(changed_page.as_bytes()).as_bytes(),
            [3; 32],
            Vec::<backend_compile::SourceDeclaration>::new(),
            backend_engine::DeclarationRetention::Unavailable(
                backend_library::SourceUnavailableReason::Unparsed,
            ),
        )?
        .with_source_identity(ContentId::<SourceFactDomain>::from_canonical_bytes(
            changed_page.as_bytes(),
        ))?;
        let changes = vec![
            BuiltinSourceChange {
                key: package.to_bytes(),
                after: Some(ProductSourceRecord::project(
                    "fixture",
                    *blake3::hash(changed_page.as_bytes()).as_bytes(),
                    ids,
                )?),
            },
            BuiltinSourceChange {
                key: backend_engine::product_source_file_key(package.to_bytes(), "page.tsx"),
                after: Some(changed_file),
            },
        ];
        let intent = BuiltinIntent::index_with_semantics(package, "fixture", changes, Vec::new())?;
        crate::builtin::commands::commit_builtin_intent(&mut daemon, 912, &intent)?;
        let previous_rows = daemon
            .engine()
            .daemon()
            .library()
            .view()
            .row_refs()
            .cloned()
            .collect::<Vec<_>>();
        let snapshot = daemon.engine().daemon().owner().snapshot();
        let (base, _) = crate::builtin::initial_view_for_workspace(&snapshot)?;
        let rows = previous_rows
            .into_iter()
            .map(|mut row| {
                row.basis = base.basis();
                row
            })
            .collect();
        let view = backend_engine::ViewRoot::new_checked(
            base.recipe(),
            base.basis(),
            base.frontier(),
            rows,
            vec![backend_engine::ViewCoverage::Partial {
                lane: backend_engine::Lane::Semantic,
                completed: 1,
                total: 2,
            }],
            crate::builtin::builtin_view_capability_for_workspace(&snapshot)?,
        )
        .map_err(|error| format!("stale fixture view: {error:?}"))?;
        let cursor = backend_engine::Cursor::for_view_root(&view);
        let admission = BuiltinViewAdmission {
            workspace_root: snapshot.root(),
            source_root: view.basis().root,
        };
        daemon
            .engine_mut()
            .daemon_mut()
            .publish_view(view, cursor, &admission, None)?;
        let stale_query = backend_engine::GraphNeighborhoodQuery::new(
            page_symbol.ok_or("Page missing")?,
            daemon.engine().daemon().library().revision_root(),
        );
        let stale_graph = super::super::execute_semantic_graph(
            &mut structural_calls, &daemon, &client, &mut generations, &mut image_rows,
            stale_query, true,
        )?;
        assert!(stale_graph.is_none(), "retained stale view rows must not serve old native graph evidence");
        let target =
            backend_engine::ProductText::new(names.get("Button").ok_or("target absent")?.clone())
                .map_err(|error| format!("target: {error:?}"))?;
        let reply = execute_references(
            &mut structural_calls,
            &daemon,
            &client,
            &mut generations,
            &mut image_rows,
            &target,
        )?;
        let backend_engine::SurfaceReply::References {
            references,
            coverage,
            ..
        } = reply
        else {
            return Err("wrong stale reply".into());
        };
        eprintln!("selected stale coverage={coverage:?} refs={references:?}");
        assert!(
            !references.iter().any(|record| record
                .evidence
                .source
                .as_ref()
                .is_some_and(|span| span.file.as_str() == "page.tsx")),
            "stale source must not retain Page occurrences"
        );
        assert!(
            matches!(coverage, backend_engine::ReferenceCoverage::Unknown { reason } if reason.as_str().contains("stale") && reason.as_str().contains("checker diagnostics"))
        );
    }
    drop(client);
    Ok(())
}
