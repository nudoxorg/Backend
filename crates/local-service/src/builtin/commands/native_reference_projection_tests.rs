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

#[test]
fn native_ts_cross_file_calls_reach_reference_projection() -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot(std::env::temp_dir().join(format!(
        "nudox-native-ts-call-projection-{}-{}", std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    )));
    let package = root.0.join("package");
    fs::create_dir_all(package.join("src"))?;
    let csv = "export function loadSectionsCsv(): string { return 'csv'; }\n";
    let scheduler = "import { loadSectionsCsv } from './csv';\nexport function generateSchedule(): string { return loadSectionsCsv(); }\n";
    let route = "import { generateSchedule } from './scheduler';\nexport function POST(): string { return generateSchedule(); }\n";
    for (path, text) in [("src/csv.ts", csv), ("src/scheduler.ts", scheduler), ("src/route.ts", route)] {
        fs::write(package.join(path), text)?;
    }
    fs::write(package.join("tsconfig.json"), r#"{"compilerOptions":{"noLib":true,"types":[],"module":"ESNext","moduleResolution":"bundler"},"files":["src/csv.ts","src/scheduler.ts","src/route.ts"]}"#)?;
    let client = LocalCompilerHost::new(Environment(root.0.join("data")), LocalHostDiscovery::ExplicitOnly).open()?;
    let profile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
    let request = PackageCompileRequest::new(
        GenerateTarget { correlation: CorrelationId(415), profile, stage: Stage::LowerIr },
        PackageUrl::parse("pkg:npm/ts-call-fixture@1.0.0".to_owned()).map_err(|e| format!("package: {e:?}"))?,
    ).map_err(|e| format!("request: {e:?}"))?;
    let sources = [("src/csv.ts", csv), ("src/scheduler.ts", scheduler), ("src/route.ts", route)]
        .into_iter().map(|(path, text)| OwnedPackageSource::new(path, text)).collect::<Result<Vec<_>, _>>()?;
    let compiled = client.compile_package_sources(OwnedPackageSourceSet::new(request, package, sources.into_boxed_slice())?)?;
    let images = compiled.images.iter().map(|image| image.reopen()).collect::<Result<Vec<_>, _>>()?;
    let key = publication_key(profile, "pkg:npm/ts-call-fixture@1.0.0")?;
    let view = projected_view(key.package_key(), &images)?;
    let target = |name: &[u8]| -> Result<_, Box<dyn std::error::Error>> {
        for image in &images {
            for entity in DocumentationSession::new(image).publication_entities() {
                let entity = entity?;
                if entity.entity.kind == ItemKind::Function && entity.name.named_bytes() == Some(name) {
                    return Ok(entity.entity.version.identity());
                }
            }
        }
        Err(format!("missing declaration {name:?}").into())
    };
    let scope = BTreeMap::from([(Language::TypeScript, view_build::RetargetingScope::SelectedArtifacts)]);
    for (callee, caller, source, text) in [
        (b"loadSectionsCsv".as_slice(), b"generateSchedule".as_slice(), "src/scheduler.ts", scheduler),
        (b"generateSchedule".as_slice(), b"POST".as_slice(), "src/route.ts", route),
    ] {
        let callee_symbol = view_build::semantic_symbol(key.package_key(), target(callee)?);
        let caller_symbol = view_build::semantic_symbol(key.package_key(), target(caller)?);
        let facts = project_native_reference_facts(&images, &view, key.package_key(), callee_symbol, &scope, &[], true)?;
        let exact_call = facts.iter().filter(|fact| fact.site == caller_symbol
            && fact.relation == backend_engine::SemanticLinkKind::Calls
            && fact.evidence.confidence == backend_engine::SemanticConfidence::Compiler
            && fact.evidence.source.as_ref().is_some_and(|span| span.file.as_str() == source
                && text.get(span.start as usize..span.end as usize).is_some_and(|token| token.as_bytes() == callee)))
            .count();
        assert_eq!(exact_call, 1, "exact caller reference for {callee:?}: {facts:?}");
    }
    Ok(())
}

#[test]
fn compiler_worker_captures_five_thousand_function_headers() -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot(std::env::temp_dir().join(format!(
        "nudox-native-header-many-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    )));
    let package = root.0.join("package");
    fs::create_dir_all(&package)?;
    let source = (0..5000).map(|i| format!("export function f{i}() {{ return {i}; }}\n")).collect::<String>();
    fs::write(package.join("many.ts"), &source)?;
    fs::write(package.join("tsconfig.json"), r#"{"compilerOptions":{"noLib":true,"types":[]},"files":["many.ts"]}"#)?;
    let client = LocalCompilerHost::new(Environment(root.0.join("data")), LocalHostDiscovery::ExplicitOnly).open()?;
    let request = PackageCompileRequest::new(GenerateTarget {
        correlation: CorrelationId(415), profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript), stage: Stage::LowerIr,
    }, PackageUrl::parse("pkg:npm/header-many@1.0.0".to_owned()).map_err(|e| format!("package: {e:?}"))?)
        .map_err(|e| format!("request: {e:?}"))?;
    let staged = client.compile_package_sources_staged(OwnedPackageSourceSet::new(request.clone(), package.clone(),
        vec![OwnedPackageSource::new("many.ts", &source)?].into_boxed_slice())?)?;
    let file = staged.native_typescript_headers().iter().find(|file| file.path == "many.ts").ok_or("headers absent")?;
    assert_eq!(file.entries.len(), 5000);
    assert!(file.entries.iter().any(|entry| source.get(entry.header_extent.0 as usize..entry.header_extent.1 as usize) == Some("export function f4999()")));
    drop(staged);
    let malformed = "export function broken() { return 1;\n";
    fs::write(package.join("many.ts"), malformed)?;
    let malformed_result = client.compile_package_sources_staged(OwnedPackageSourceSet::new(request, package,
        vec![OwnedPackageSource::new("many.ts", malformed)?].into_boxed_slice())?);
    if let Ok(staged) = malformed_result {
        assert!(staged.native_typescript_headers().iter().all(|file| file.path != "many.ts"));
    }
    Ok(())
}

#[test]
fn large_valid_typescript_keeps_compiler_publication_without_optional_headers() -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot(std::env::temp_dir().join(format!("nudox-native-header-large-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos())));
    let package = root.0.join("package");
    fs::create_dir_all(&package)?;
    let source = format!("/* {} */\nexport function available() {{ return 1; }}\n", "x".repeat(1024 * 1024));
    fs::write(package.join("large.ts"), &source)?;
    fs::write(package.join("tsconfig.json"), r#"{"compilerOptions":{"noLib":true,"types":[]},"files":["large.ts"]}"#)?;
    let client = LocalCompilerHost::new(Environment(root.0.join("data")), LocalHostDiscovery::ExplicitOnly).open()?;
    let request = PackageCompileRequest::new(GenerateTarget { correlation: CorrelationId(416), profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript), stage: Stage::LowerIr },
        PackageUrl::parse("pkg:npm/header-large@1.0.0".to_owned()).map_err(|e| format!("package: {e:?}"))?).map_err(|e| format!("request: {e:?}"))?;
    let staged = client.compile_package_sources_staged(OwnedPackageSourceSet::new(request, package,
        vec![OwnedPackageSource::new("large.ts", &source)?].into_boxed_slice())?)?;
    assert!(staged.semantic_output_object(0).is_some());
    assert!(staged.native_typescript_headers().iter().all(|file| file.path != "large.ts"));
    Ok(())
}

#[test]
fn compiler_worker_retains_headers_after_seventeen_small_files() -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot(std::env::temp_dir().join(format!("nudox-native-header-files-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos())));
    let package = root.0.join("package");
    fs::create_dir_all(&package)?;
    let mut paths = Vec::new();
    let mut sources = Vec::new();
    for i in 0..17 {
        let path = format!("file{i:02}.ts");
        let text = format!("export function f{i}() {{ return {i}; }}\n");
        fs::write(package.join(&path), &text)?;
        sources.push(OwnedPackageSource::new(&path, &text)?);
        paths.push(format!("\"{path}\""));
    }
    fs::write(package.join("tsconfig.json"), format!("{{\"compilerOptions\":{{\"noLib\":true,\"types\":[]}},\"files\":[{}]}}", paths.join(",")))?;
    let client = LocalCompilerHost::new(Environment(root.0.join("data")), LocalHostDiscovery::ExplicitOnly).open()?;
    let request = PackageCompileRequest::new(GenerateTarget { correlation: CorrelationId(417), profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript), stage: Stage::LowerIr },
        PackageUrl::parse("pkg:npm/header-files@1.0.0".to_owned()).map_err(|e| format!("package: {e:?}"))?).map_err(|e| format!("request: {e:?}"))?;
    let staged = client.compile_package_sources_staged(OwnedPackageSourceSet::new(request, package, sources.into_boxed_slice())?)?;
    assert_eq!(staged.native_typescript_headers().len(), 17);
    let last = staged.native_typescript_headers().iter().find(|file| file.path == "file16.ts").ok_or("last header missing")?;
    assert_eq!(last.entries.len(), 1);
    Ok(())
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
    let unused = "export async function Unused() { return 0; }\n";
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
    let staged_ts = client.compile_package_sources_staged(OwnedPackageSourceSet::new(
        ts_request.clone(), package.clone(), ts_sources.clone(),
    )?)?;
    let headers = staged_ts.native_typescript_headers().iter()
        .find(|file| file.path == "ui/unused.ts").ok_or("async source header absent")?;
    assert_eq!(headers.source, backend_semantic::ir::SourceIdentity::from_bytes(unused.as_bytes()).ok_or("source identity")?);
    let header = headers.entries.first().ok_or("async function header absent")?;
    assert_eq!(&unused[header.header_extent.0 as usize..header.header_extent.1 as usize], "export async function Unused()");
    assert!(header.header_extent.1 < header.compiler_extent.1);
    let original_source = headers.source;
    let original_image = headers.image_identity;
    drop(staged_ts);
    let changed = "export async function Unused() { return 2; }\n";
    fs::write(package.join("ui/unused.ts"), changed)?;
    let changed_sources = vec![
        OwnedPackageSource::new("ui/button.ts", button)?,
        OwnedPackageSource::new("ui/index.ts", barrel)?,
        OwnedPackageSource::new("ui/unused.ts", changed)?,
    ].into_boxed_slice();
    let changed_staged = client.compile_package_sources_staged(OwnedPackageSourceSet::new(
        ts_request.clone(), package.clone(), changed_sources,
    )?)?;
    let changed_headers = changed_staged.native_typescript_headers().iter()
        .find(|file| file.path == "ui/unused.ts").ok_or("changed source header absent")?;
    assert_ne!(changed_headers.source, original_source);
    assert_ne!(changed_headers.image_identity, original_image);
    drop(changed_staged);
    fs::write(package.join("ui/unused.ts"), unused)?;
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
