//! Real linked-TSZ artifacts at the native reference projection boundary.
use super::*;
use backend_engine::application::{
    LocalCompilerHost, LocalHostDiscovery, LocalHostEnvironment, LocalHostVariable,
    OwnedPackageSource, OwnedPackageSourceSet,
};
use backend_library::interface::{CorrelationId, GenerateTarget, PackageCompileRequest};
use backend_semantic::ir::ItemKind;
use backend_semantic::vocabulary::{Language, LanguageProfile, Stage, TypeScriptSource};
use std::{
    ffi::OsString,
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

struct TestRoot(PathBuf);
impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Environment(PathBuf);
impl LocalHostEnvironment for Environment {
    fn value(&self, variable: LocalHostVariable) -> Option<OsString> {
        (variable == LocalHostVariable::NudoxDataRoot).then(|| self.0.clone().into_os_string())
    }
}

#[test]
fn real_native_images_enter_partial_reference_projection() -> Result<(), Box<dyn std::error::Error>>
{
    project_fixture(false, false)?;
    project_fixture(true, false)
}

#[test]
fn native_incoming_graph_edge_uses_producing_publication() -> Result<(), Box<dyn std::error::Error>>
{
    project_fixture(false, true)
}

fn project_fixture(private: bool, graph_origin: bool) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot(std::env::temp_dir().join(format!(
        "nudox-native-reference-projection-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    )));
    let package = root.0.join("package");
    fs::create_dir_all(package.join("ui"))?;
    let button = if private {
        "function Button() { return 1; }\n"
    } else if graph_origin {
        "export function Button() { return 1; }\nexport function LocalCaller() { return Button(); }\n"
    } else {
        "export function Button() { return 1; }\n"
    };
    let unused = "export function Unused() { return 0; }\n";
    let barrel = "export { Button as Action } from './button';\n";
    let page = "// λ\nimport { Action as Confirm } from './ui';\nexport function Page() { return <><Confirm /><Confirm /></>; }\n";
    fs::write(package.join("ui/button.ts"), button)?;
    fs::write(package.join("ui/unused.ts"), unused)?;
    fs::write(package.join("ui/index.ts"), barrel)?;
    fs::write(package.join("page.tsx"), page)?;
    fs::write(
        package.join("tsconfig.json"),
        r#"{"compilerOptions":{"noLib":true,"types":[],"module":"ESNext","moduleResolution":"bundler","jsx":"preserve"},"files":["ui/button.ts","ui/index.ts","ui/unused.ts","page.tsx"]}"#,
    )?;
    let client = LocalCompilerHost::new(
        Environment(root.0.join("data")),
        LocalHostDiscovery::ExplicitOnly,
    )
    .open()?;
    let package_url = PackageUrl::parse("pkg:npm/fixture@1.0.0".to_owned())
        .map_err(|e| format!("package: {e:?}"))?;
    let make_request = |profile| {
        PackageCompileRequest::new(
            GenerateTarget {
                correlation: CorrelationId(414),
                profile,
                stage: Stage::LowerIr,
            },
            package_url.clone(),
        )
    };
    let ts = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
    let tsx = LanguageProfile::TypeScript(TypeScriptSource::Tsx);
    let ts_request = make_request(ts).map_err(|error| format!("TS profile: {error:?}"))?;
    let tsx_request = make_request(tsx).map_err(|error| format!("TSX profile: {error:?}"))?;
    let ts_sources = vec![
        OwnedPackageSource::new("ui/button.ts", button)?,
        OwnedPackageSource::new("ui/index.ts", barrel)?,
        OwnedPackageSource::new("ui/unused.ts", unused)?,
    ]
    .into_boxed_slice();
    let tsx_sources = vec![OwnedPackageSource::new("page.tsx", page)?].into_boxed_slice();
    let staged = client.compile_package_sources_staged(OwnedPackageSourceSet::new(
        tsx_request.clone(),
        package.clone(),
        tsx_sources.clone(),
    )?)?;
    assert_eq!(
        staged.input_witness().coverage().state(),
        backend_version::Coverage::Partial
    );
    let first = client.compile_package_sources(OwnedPackageSourceSet::new(
        ts_request,
        package.clone(),
        ts_sources,
    )?)?;
    let second = client.compile_package_sources(OwnedPackageSourceSet::new(
        tsx_request,
        package.clone(),
        tsx_sources,
    )?)?;
    let images = first
        .images
        .iter()
        .chain(second.images.iter())
        .map(|image| image.reopen())
        .collect::<Result<Vec<_>, _>>()?;
    let key = publication_key(ts, "pkg:npm/fixture@1.0.0")?;
    let view = projected_view(key.package_key(), &images)?;
    let target = |name: &[u8]| {
        for image in &images {
            let session = DocumentationSession::new(image);
            for entity in session.publication_entities() {
                let entity = entity.map_err(|error| format!("entity: {error}"))?;
                if entity.entity.kind == ItemKind::Function
                    && entity.name.named_bytes() == Some(name)
                {
                    return Ok(entity.entity.version.identity());
                }
            }
        }
        Err(format!("published declaration {name:?} missing"))
    };
    let scope = BTreeMap::from([(
        Language::TypeScript,
        view_build::RetargetingScope::SelectedArtifacts,
    )]);
    let project =
        |name: &[u8]| -> Result<Vec<backend_engine::ReferenceFact>, Box<dyn std::error::Error>> {
            let symbol = view_build::semantic_symbol(key.package_key(), target(name)?);
            Ok(project_native_reference_facts(
                &images,
                &view,
                key.package_key(),
                symbol,
                &scope,
                &[],
                true,
            )?)
        };
    let facts = project(b"Button")?;
    let button_symbol = view_build::semantic_symbol(key.package_key(), target(b"Button")?);
    if graph_origin {
        let tsx_key = publication_key(tsx, "pkg:npm/fixture@1.0.0")?;
        let claim = backend_engine::builtin::SemanticPublicationClaim::admit(
            first.publication.manifest,
            first.publication.binding,
        )?;
        let tsx_claim = backend_engine::builtin::SemanticPublicationClaim::admit(
            second.publication.manifest,
            second.publication.binding,
        )?;
        let mut generations =
            crate::builtin::generation_residence::SemanticGenerationResidence::default();
        let mut image_rows = view_build::ImageRowResidence::default();
        let activated_ts =
            activate_semantic_publication(&client, &key, claim, &mut generations, &mut image_rows)?;
        let activated_tsx = activate_semantic_publication(
            &client,
            &tsx_key,
            tsx_claim,
            &mut generations,
            &mut image_rows,
        )?;
        assert_eq!(
            activated_ts
                .images()
                .iter()
                .map(|image| image.content_digest())
                .collect::<Vec<_>>(),
            first
                .images
                .iter()
                .map(|image| image.content_digest())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            activated_tsx
                .images()
                .iter()
                .map(|image| image.content_digest())
                .collect::<Vec<_>>(),
            second
                .images
                .iter()
                .map(|image| image.content_digest())
                .collect::<Vec<_>>()
        );
        let rejected = activate_semantic_publication(
            &client,
            &tsx_key,
            claim,
            &mut generations,
            &mut image_rows,
        )
        .err()
        .map(|error| error.to_string());
        assert!(
            rejected.as_deref().is_some_and(|message| message.contains("bind semantic publication to product key")),
            "the TS publication must fail product-key admission under the TSX profile: {rejected:?}"
        );
        let graph_images = activated_ts
            .images()
            .iter()
            .chain(activated_tsx.images().iter())
            .map(|image| image.reopen())
            .collect::<Result<Vec<_>, _>>()?;
        let page_symbol = view_build::semantic_symbol(key.package_key(), target(b"Page")?);
        let local_symbol = view_build::semantic_symbol(key.package_key(), target(b"LocalCaller")?);
        let relations = project_opened_semantic_graph_origins(
            &graph_images,
            &view,
            key.package_key(),
            button_symbol,
            backend_engine::RowId::Symbol(button_symbol),
            true,
            &BTreeSet::from([
                "ui/button.ts".to_owned(),
                "ui/index.ts".to_owned(),
                "ui/unused.ts".to_owned(),
                "page.tsx".to_owned(),
            ]),
            &scope,
            &[],
            true,
            true,
        )?;
        let (incoming, _) = relations
            .iter()
            .find(|(relation, _)| {
                relation.from == backend_engine::RowId::Symbol(page_symbol)
                    && relation.to == backend_engine::RowId::Symbol(button_symbol)
            })
            .ok_or("native Page -> Button graph edge missing")?;
        assert_eq!(
            incoming.confidence,
            Some(backend_engine::SemanticConfidence::Compiler)
        );
        let center_binding = first.publication.binding;
        let producing_binding = second.publication.binding;
        assert_eq!(
            backend_engine::builtin::SemanticPublicationClaim::admit(
                first.publication.manifest,
                producing_binding
            )
            .err(),
            Some("manifest and binding identities differ"),
        );
        let revision = backend_engine::RichGraphRevision::new(
            view.root().to_bytes(),
            backend_engine::SemanticGenerationId::new(*center_binding.identity.as_ref()),
            *center_binding.generation.pinned_root.as_ref(),
        );
        let origins = activated_ts
            .images()
            .iter()
            .map(|image| (center_binding, image.content_digest()))
            .chain(
                activated_tsx
                    .images()
                    .iter()
                    .map(|image| (producing_binding, image.content_digest())),
            )
            .collect::<Vec<_>>();
        let candidates = bind_projected_graph_origins(relations, &origins);
        let candidate = *candidates
            .iter()
            .find(|(relation, _)| {
                relation.from == backend_engine::RowId::Symbol(page_symbol)
                    && relation.to == backend_engine::RowId::Symbol(button_symbol)
            })
            .ok_or("incoming candidate missing")?;
        let (same_relations, same_edges, same_conflict) =
            resolve_graph_edge_origins(vec![candidate, candidate]);
        assert_eq!(
            (same_relations.len(), same_edges.len(), same_conflict),
            (1, 1, false)
        );
        let mut other = candidate;
        other.1.semantic_generation = Some(backend_engine::SemanticGenerationId::new(
            *center_binding.identity.as_ref(),
        ));
        other.1.semantic_root = Some(*center_binding.generation.pinned_root.as_ref());
        for pair in [[candidate, other], [other, candidate]] {
            let (relations, edges, conflict) = resolve_graph_edge_origins(pair.to_vec());
            assert!(
                conflict && relations.is_empty() && edges.is_empty(),
                "conflicting origins must be withheld in either order"
            );
        }
        let (_, edges, conflicting) = resolve_graph_edge_origins(candidates);
        assert!(!conflicting);
        let stale_revision = backend_engine::RichGraphRevision::new(
            [0; 32],
            revision.semantic_generation,
            revision.semantic_root,
        );
        assert!(
            rich_graph_for_root(
                &view,
                backend_engine::RowId::Symbol(button_symbol),
                stale_revision,
                edges.clone()
            )
            .is_err(),
            "a graph with the wrong view root must be refused"
        );
        let graph = rich_graph_for_root(
            &view,
            backend_engine::RowId::Symbol(button_symbol),
            revision,
            edges,
        )?;
        let edge = graph
            .edges
            .iter()
            .find(|edge| {
                edge.from
                    == backend_engine::GraphNodeId::for_row(backend_engine::RowId::Symbol(
                        page_symbol,
                    ))
                    && edge.to
                        == backend_engine::GraphNodeId::for_row(backend_engine::RowId::Symbol(
                            button_symbol,
                        ))
            })
            .ok_or("rich edge missing")?;
        assert_eq!(
            edge.provenance.semantic_generation,
            Some(backend_engine::SemanticGenerationId::new(
                *producing_binding.identity.as_ref()
            )),
            "incoming edge must retain Page publication, not center publication"
        );
        assert_eq!(
            edge.provenance.semantic_root,
            Some(*producing_binding.generation.pinned_root.as_ref())
        );
        let page_image = second
            .images
            .iter()
            .find(|image| {
                image
                    .reopen()
                    .ok()
                    .and_then(|image| view_build::compiled_source_path(&image).ok())
                    .as_deref()
                    == Some("page.tsx")
            })
            .ok_or("Page image missing")?;
        assert_eq!(edge.provenance.source, page_image.content_digest());
        let local = graph
            .edges
            .iter()
            .find(|edge| {
                edge.from
                    == backend_engine::GraphNodeId::for_row(backend_engine::RowId::Symbol(
                        local_symbol,
                    ))
                    && edge.to
                        == backend_engine::GraphNodeId::for_row(backend_engine::RowId::Symbol(
                            button_symbol,
                        ))
            })
            .ok_or("native local incoming edge missing")?;
        assert_eq!(
            local.provenance.semantic_generation,
            Some(backend_engine::SemanticGenerationId::new(
                *center_binding.identity.as_ref()
            ))
        );
        assert_eq!(
            local.provenance.semantic_root,
            Some(*center_binding.generation.pinned_root.as_ref())
        );
        assert_eq!(
            local.provenance.confidence,
            Some(backend_engine::SemanticConfidence::Compiler)
        );
        let local_image = first
            .images
            .iter()
            .find(|image| {
                image
                    .reopen()
                    .ok()
                    .and_then(|image| view_build::compiled_source_path(&image).ok())
                    .as_deref()
                    == Some("ui/button.ts")
            })
            .ok_or("button image missing")?;
        assert_eq!(local.provenance.source, local_image.content_digest());
        let outgoing = project_opened_semantic_graph_origins(
            &graph_images,
            &view,
            key.package_key(),
            page_symbol,
            backend_engine::RowId::Symbol(page_symbol),
            false,
            &BTreeSet::from([
                "ui/button.ts".to_owned(),
                "ui/index.ts".to_owned(),
                "ui/unused.ts".to_owned(),
                "page.tsx".to_owned(),
            ]),
            &scope,
            &[],
            true,
            true,
        )?;
        let (relation, index) = outgoing
            .iter()
            .find(|(relation, _)| {
                relation.from == backend_engine::RowId::Symbol(page_symbol)
                    && relation.to == backend_engine::RowId::Symbol(button_symbol)
            })
            .ok_or("native outgoing Page edge missing")?;
        assert_eq!(
            relation.confidence,
            Some(backend_engine::SemanticConfidence::Compiler)
        );
        assert_eq!(origins[*index].0.identity, producing_binding.identity);
        assert_eq!(origins[*index].1, page_image.content_digest());
    }
    let unused_symbol = view_build::semantic_symbol(key.package_key(), target(b"Unused")?);
    let library = backend_library::Library::from_view(
        view.clone(),
        backend_library::Cursor::for_view_root(&view),
    )?;
    let references = library.references_for_symbol(button_symbol, &facts)?;
    assert_eq!(references.len(), facts.len());
    eprintln!("native reference projection private={private}: {facts:#?}");
    let page_reads = facts
        .iter()
        .filter(|fact| {
            fact.relation == backend_engine::SemanticLinkKind::Reads
                && fact.evidence.confidence == backend_engine::SemanticConfidence::Compiler
                && fact.evidence.source.as_ref().is_some_and(|span| {
                    span.file.as_str() == "page.tsx"
                        && page.get(span.start as usize..span.end as usize) == Some("Confirm")
                })
        })
        .count();
    assert_eq!(
        page_reads, 2,
        "two actual Page Reads must survive native reference joining"
    );
    let reason = BTreeSet::from([
        "TSX native input coverage is Partial; inspect package_profile".to_owned(),
    ]);
    assert!(matches!(
        reference_coverage(&reason, !references.is_empty())?,
        backend_engine::ReferenceCoverage::Partial { reason } if reason.as_str().contains("TSX native input coverage is Partial")
    ));
    let empty = project(b"Unused")?;
    assert!(empty.is_empty(), "unused control: {empty:?}");
    let empty_references = library.references_for_symbol(unused_symbol, &empty)?;
    assert!(empty_references.is_empty());
    assert!(matches!(
        reference_coverage(&reason, !empty_references.is_empty())?,
        backend_engine::ReferenceCoverage::Unknown { reason } if reason.as_str().contains("TSX native input coverage is Partial")
    ));
    drop(client);
    Ok(())
}
