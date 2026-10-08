//! Genuine native Python project output through the product row and search sinks.

#![allow(clippy::expect_used, clippy::too_many_lines)]

use super::*;
use backend_engine::application::{
    DocumentationSession, LocalCompilerHost, LocalHostDiscovery, LocalHostEnvironment,
    LocalHostVariable, OwnedPackageSource, OwnedPackageSourceSet,
};
use backend_engine::{Row, RowId, ViewCoverage, ViewRoot};
use backend_extension_trustfall::{
    CompilerExternalTargetEvidence, CompilerSemanticEvidence, PackageScopeEvidence,
    SemanticQueryCorpus, SemanticQueryEvidence, SemanticQueryFact, SemanticQueryPresentation,
};
use backend_library::interface::{CorrelationId, GenerateTarget, PackageCompileRequest};
use backend_semantic::ir::{
    ExternalTargetIdentity, LinkTarget, SemanticCoreReader as _, SemanticImageView,
    SemanticReader as _,
};
use backend_semantic::vocabulary::{LanguageProfile, PackageUrl, PythonVersion, Stage};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

struct NativePythonHost(PathBuf);

impl LocalHostEnvironment for NativePythonHost {
    fn value(&self, variable: LocalHostVariable) -> Option<OsString> {
        (variable == LocalHostVariable::NudoxDataRoot).then(|| self.0.clone().into_os_string())
    }

    fn search_path(&self) -> Option<OsString> {
        Some(OsString::new())
    }
}

fn display(
    row: &Row,
    name: String,
    kind: String,
    project: Option<String>,
) -> SemanticQueryPresentation {
    SemanticQueryPresentation {
        id: row.id.stable_key(),
        kind,
        coordinate: row.label.clone(),
        name,
        signature: row.signature.clone(),
        documentation: String::new(),
        score: None,
        project,
        parent: None,
        related: Box::new([]),
    }
}

#[test]
fn source_declaration_ranking_native_python_import_types_keep_real_function_first() {
    let temporary = tempfile::tempdir().expect("owned fixture directory");
    let project_root = temporary.path().join("project");
    std::fs::create_dir_all(project_root.join("core")).expect("source directory");
    let mut modules = vec![
        ("core/__init__.py".to_owned(), String::new()),
        ("core/config.py".to_owned(), "from functools import lru_cache\n\nclass AppSettings:\n    pass\n\n@lru_cache\ndef get_app_settings() -> AppSettings:\n    return AppSettings()\n".to_owned()),
    ];
    for index in 0..32 {
        modules.push((format!("caller_{index}.py"), format!("from core.config import get_app_settings\n\ndef use_settings_{index}():\n    return get_app_settings()\n")));
    }
    for (path, source) in &modules {
        std::fs::write(project_root.join(path), source).expect("actual native source bytes");
    }
    let profile = LanguageProfile::Python(PythonVersion::Python314);
    let coordinate =
        PackageUrl::parse("pkg:pypi/source-placement@1.0.0".to_owned()).expect("package");
    let client = LocalCompilerHost::new(
        NativePythonHost(temporary.path().join("compiler")),
        LocalHostDiscovery::ExplicitOnly,
    )
    .open()
    .expect("compiled native authority with no external tools");
    let request = PackageCompileRequest::new(
        GenerateTarget {
            correlation: CorrelationId(173),
            profile,
            stage: Stage::LowerIr,
        },
        coordinate.clone(),
    )
    .expect("native request");
    let sources = modules
        .iter()
        .map(|(path, source)| OwnedPackageSource::new(path, source).expect("source"))
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let staged = client
        .compile_package_sources_staged(
            OwnedPackageSourceSet::new(request, project_root.clone(), sources)
                .expect("complete fixture frontier"),
        )
        .expect("genuine selected native Python compiler output");
    assert_eq!(staged.artifacts().len(), modules.len());
    let (base, _) = super::super::initial_view().expect("base view");
    let workspace = super::super::genesis().expect("workspace").root();
    let package = backend_engine::package_key("native-source-placement");
    let project = super::super::IndexedProject {
        package,
        label: "native-source-placement".to_owned(),
        files: Arc::from([]),
    };
    let package_row = Row::new(RowId::Package(package), base.basis(), project.label.clone());
    let package_id = package_row.id.stable_key();
    let mut rows = BTreeMap::from([(package_row.id, package_row.clone())]);
    let mut facts = BTreeMap::from([(
        package_id.clone(),
        SemanticQueryFact::new(
            SemanticQueryEvidence::Package(PackageScopeEvidence::new(package)),
            display(
                &package_row,
                project.label.clone(),
                "project".to_owned(),
                None,
            ),
        ),
    )]);
    let syntax = backend_frontend_python::syntax_frontend().expect("Python source frontend");
    for index in 0..staged.artifacts().len() {
        let bytes = staged
            .semantic_output_object(index)
            .expect("actual selected image")
            .bytes();
        let image = SemanticImageView::reopen(bytes).expect("canonical native image");
        let digest = *blake3::hash(bytes).as_bytes();
        let path = compiled_source_path(&image).expect("native source provenance");
        let (_, source) = modules
            .iter()
            .find(|(candidate, _)| candidate == &path)
            .expect("exact source path");
        let analysis = syntax
            .analyze(std::path::Path::new(&path), source.as_bytes())
            .expect("written source declarations");
        let declarations = analysis.declarations().iter().collect::<Vec<_>>();
        let projected = semantic::project_image_rows(
            &image,
            digest,
            &project,
            profile,
            base.basis(),
            false,
            &path,
            &declarations,
        )
        .expect("product source projection");
        for projected in projected.rows {
            rows.entry(projected.row.id).or_insert(projected.row);
        }
        let session = DocumentationSession::new(&image);
        let mut externals = BTreeMap::new();
        for entity in session.canonical_entities() {
            let entity = entity.expect("native entity");
            let identity = entity.entity.version.identity();
            let row_id = RowId::Symbol(identity::semantic_symbol(package, identity));
            let row = rows.get(&row_id).expect("projected native row");
            let name = semantic_display_name(entity.name)
                .expect("native name")
                .to_owned();
            let mut presentation = display(
                row,
                name,
                identity::declaration_kind(entity.entity.kind)
                    .name()
                    .to_owned(),
                Some(package_id.clone()),
            );
            if let Some(parent) = entity.entity.parent {
                let parent = session.entity(parent).expect("native parent");
                presentation.parent = Some(identity::query_semantic_id(
                    package,
                    parent.entity.version.identity(),
                ));
            }
            facts.insert(
                presentation.id.clone(),
                SemanticQueryFact::new(
                    SemanticQueryEvidence::Compiler(CompilerSemanticEvidence::new(
                        package,
                        coordinate.clone(),
                        profile,
                        identity,
                        digest,
                        image.image_facts(),
                    )),
                    presentation,
                ),
            );
            let content = semantic::semantic_row_content(profile, &image, &entity, package, digest)
                .expect("native documentation target projection");
            for (target, external) in content.documentation_targets {
                externals.insert(
                    identity::query_external_id(package, digest, target),
                    (target, external),
                );
            }
            for (_, link) in image.links_from(entity.entity.id) {
                if let LinkTarget::External(external) = link.target {
                    let target =
                        ExternalTargetIdentity::capture(&image, external).expect("native target");
                    externals.insert(
                        identity::query_external_id(package, digest, target),
                        (target, external),
                    );
                }
            }
        }
        for (_, (target, external)) in externals {
            let row_id = RowId::Symbol(identity::external_semantic_symbol(package, digest, target));
            let row = rows.get(&row_id).expect("projected external row");
            let name = call_join::foreign_display_name(&image, external)
                .expect("target display")
                .unwrap_or_else(|| "external semantic target".to_owned());
            let presentation = display(row, name, "external".to_owned(), Some(package_id.clone()));
            facts.insert(
                presentation.id.clone(),
                SemanticQueryFact::new(
                    SemanticQueryEvidence::CompilerExternalTarget(
                        CompilerExternalTargetEvidence::new(
                            package,
                            coordinate.clone(),
                            profile,
                            target,
                            digest,
                            image.image_facts(),
                        ),
                    ),
                    presentation,
                ),
            );
        }
    }
    assert_eq!(rows.len(), facts.len(), "actual selected corpus closure");
    let view = ViewRoot::new_checked(
        base.recipe(),
        base.basis(),
        base.frontier(),
        rows.into_values().collect(),
        vec![ViewCoverage::Complete],
        base.capability().expect("capability"),
    )
    .expect("native selected view");
    let corpus = SemanticQueryCorpus::admit(workspace, facts.into_values().collect())
        .expect("native selected evidence");
    let inferred = corpus
        .facts()
        .iter()
        .filter(|fact| {
            fact.presentation().name == "get_app_settings" && fact.presentation().kind == "type"
        })
        .count();
    assert!(
        inferred >= 25,
        "fixture must reproduce the first-page inferred-type crowding, got {inferred}"
    );
    let function = corpus
        .facts()
        .iter()
        .find(|fact| {
            fact.presentation().name == "get_app_settings" && fact.presentation().kind == "function"
        })
        .expect("actual native function")
        .presentation()
        .id
        .clone();
    let coordinator = crate::builtin::query::QueryCoordinator::new(
        workspace,
        view.clone(),
        view.capability().expect("selected capability"),
        super::super::admitted_coverage().expect("coverage"),
        corpus,
    )
    .expect("native search corpus");
    let result = coordinator
        .search_local(
            crate::builtin::query::LocalQuery::prefix("get_app_settings", 25).expect("first page"),
        )
        .expect("search");
    let first = &result.rows[0].row;
    assert_eq!(first.id.stable_key(), function);
    assert_eq!(
        first
            .source
            .captured()
            .expect("actual source definition")
            .path(),
        "core/config.py"
    );
    assert_eq!(
        first.source.captured().expect("source site").start_line(),
        7
    );
    assert!(
        first
            .signature
            .as_deref()
            .is_some_and(|signature| signature.starts_with("def get_app_settings"))
    );
    let full = coordinator
        .search_local(
            crate::builtin::query::LocalQuery::prefix("get_app_settings", 200)
                .expect("all matches"),
        )
        .expect("full search");
    assert_eq!(
        full.rows.len(),
        full.total_matches,
        "all inferred types remain searchable"
    );
    assert!(full.rows.len() >= 26);
    let library = backend_library::Library::from_view(
        view.clone(),
        backend_library::Cursor::for_view_root(&view),
    )
    .expect("product library");
    let request = backend_engine::Query::new(
        "get_app_settings",
        view.root(),
        backend_engine::QueryLimit::new(25).expect("limit"),
    );
    let (page, _) = crate::builtin::query::search_page(
        &coordinator,
        &library,
        &mut crate::builtin::query::RemoteSemantic::Unconfigured,
        super::super::admitted_coverage().expect("coverage"),
        &request,
    )
    .expect("actual CLI/MCP search projection seam");
    let first = page
        .root
        .rows()
        .iter()
        .max_by_key(|row| row.score)
        .expect("ranked product definition");
    assert_eq!(first.id.stable_key(), function);
    assert!(
        page.next.is_some(),
        "inferred types remain in continuation pages"
    );
}
