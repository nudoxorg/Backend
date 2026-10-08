//! Source placement controls over admitted canonical rows and compiler evidence.

use super::*;
use backend_extension_trustfall::{
    CompilerExternalTargetEvidence, CompilerSemanticEvidence, PackageScopeEvidence,
    SemanticQueryCorpus, SemanticQueryEvidence, SemanticQueryFact, SemanticQueryPresentation,
};
use backend_semantic::ir::{
    DeclarationFamilyId, DeclarationIdentity, ExternalFragmentId, ExternalTarget,
    ExternalTargetIdentity, IrBuilder, SourceIdentity, StableRef, VariantFingerprint,
};
use backend_semantic::vocabulary::{
    CompileRecipeFact, GoVersion, Language, LanguageProfile, PackageUrl, PythonVersion,
    RustEdition, Stage, TypeScriptSource,
};
use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
use std::collections::BTreeSet;

struct Fixture {
    coordinator: QueryCoordinator,
    library: backend_library::Library,
    function: RowId,
    alias: RowId,
    external: RowId,
    inferred: Vec<RowId>,
}

fn fixture(profile: LanguageProfile, path: &str) -> Fixture {
    let (workspace, base) = selected_view();
    let package = backend_engine::package_key("source-placement");
    let coordinate = PackageUrl::parse(
        match profile {
            LanguageProfile::Python(_) => "pkg:pypi/source-placement@1.0.0",
            LanguageProfile::TypeScript(_) => "pkg:npm/source-placement@1.0.0",
            LanguageProfile::Go(_) => "pkg:golang/example.com/source-placement@v1.0.0",
            LanguageProfile::Rust(_) => "pkg:cargo/source-placement@1.0.0",
            _ => unreachable!("fixture profiles are explicitly enumerated"),
        }
        .to_owned(),
    )
    .expect("fixture package");
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"source-placement"),
        byte_len: 16,
    };
    let mut builder = IrBuilder::new();
    builder
        .set_image_provenance_for_package(
            source,
            CompileRecipeFact::derive(
                profile,
                Stage::LowerIr,
                Language::from(profile).native_tool(),
                source.identity,
                ContentId::<ToolchainDomain>::from_canonical_bytes(b"fixture compiler"),
            ),
            &coordinate,
            path,
        )
        .expect("typed fixture image provenance");
    let target = builder
        .intern_external(ExternalTarget::Stable {
            target: StableRef {
                fragment: ExternalFragmentId::from_canonical_bytes(b"foreign image"),
                declaration: DeclarationIdentity {
                    family: DeclarationFamilyId::from_raw([81; 16]),
                    variant: VariantFingerprint::from_raw([82; 16]),
                },
            },
        })
        .expect("foreign endpoint");
    let image = builder.finish().expect("fixture image");
    let image_facts = image.image_facts();
    let external_evidence = CompilerExternalTargetEvidence::new(
        package,
        coordinate.clone(),
        profile,
        ExternalTargetIdentity::capture(&image, target).expect("actual image endpoint"),
        [41; 32],
        image_facts,
    );
    let project = Row::new(RowId::Package(package), base.basis(), "source-placement");
    let project_id = project.id.stable_key();
    let mut rows = vec![project.clone()];
    let mut facts = vec![SemanticQueryFact::new(
        SemanticQueryEvidence::Package(PackageScopeEvidence::new(package)),
        presentation(&project, "source-placement", "project", None),
    )];
    let mut inferred = Vec::new();
    let mut function = None;
    let mut alias = None;
    for index in 1..=35u8 {
        let evidence = CompilerSemanticEvidence::new(
            package,
            coordinate.clone(),
            profile,
            DeclarationIdentity {
                family: DeclarationFamilyId::from_raw([index; 16]),
                variant: VariantFingerprint::from_raw([index; 16]),
            },
            [41; 32],
            image_facts,
        );
        let mut identity = [index; 32];
        identity[..16].copy_from_slice(evidence.declaration().family.as_bytes());
        identity[16..].copy_from_slice(evidence.declaration().variant.as_bytes());
        let id = RowId::Symbol(backend_engine::symbol_key(&format!(
            "{}::{}",
            backend_engine::encode_id(package.as_bytes()),
            backend_engine::encode_id(&identity),
        )));
        assert_eq!(id.stable_key(), evidence.row_id());
        let (name, kind) = if index == 1 {
            function = Some(id);
            (
                "get_app_settings",
                backend_engine::DeclarationKind::Function,
            )
        } else if index == 2 {
            alias = Some(id);
            ("SettingsAlias", backend_engine::DeclarationKind::Type)
        } else {
            inferred.push(id);
            ("get_app_settings", backend_engine::DeclarationKind::Type)
        };
        let mut row = Row::in_package(
            id,
            base.basis(),
            package,
            format!("fixture::{index:03}::{name}"),
        )
        .with_kind(kind)
        .with_signature("unknown(unannotated)");
        if index <= 2 {
            row = row
                .with_source(backend_library::SourceLocation::new(path, 42).expect("source site"));
        }
        let mut display = presentation(&row, name, kind.name(), Some(project_id.clone()));
        // Matching documentation must not invent a source declaration, and
        // signature spelling must not demote an actual declared type alias.
        display.documentation = "get_app_settings SettingsAlias".to_owned();
        facts.push(SemanticQueryFact::new(
            SemanticQueryEvidence::Compiler(evidence),
            display,
        ));
        rows.push(row);
    }
    let external = RowId::Symbol(backend_engine::symbol_key(&backend_engine::encode_id(
        external_evidence
            .target()
            .in_scope(package.to_bytes(), [41; 32])
            .as_bytes(),
    )));
    assert_eq!(external.stable_key(), external_evidence.row_id());
    let external_row = Row::in_package(external, base.basis(), package, "get_app_settings")
        .with_kind(backend_engine::DeclarationKind::Function)
        .with_source(backend_library::SourceLocation::new(path, 42).expect("misleading site"));
    // Even an external endpoint's canonical row carrying a function kind and
    // source site cannot outrank declarations: retain its admitted typed role.
    facts.push(SemanticQueryFact::new(
        SemanticQueryEvidence::CompilerExternalTarget(external_evidence),
        presentation(
            &external_row,
            "get_app_settings",
            "external",
            Some(project_id),
        ),
    ));
    rows.push(external_row);
    let view = backend_engine::ViewRoot::new_checked(
        base.recipe(),
        base.basis(),
        base.frontier(),
        rows,
        vec![ViewCoverage::Complete],
        base.capability().expect("fixture capability"),
    )
    .expect("selected canonical view");
    let corpus = SemanticQueryCorpus::admit(workspace, facts).expect("compiler query evidence");
    let library = backend_library::Library::from_view(
        view.clone(),
        backend_library::Cursor::for_view_root(&view),
    )
    .expect("selected library");
    let coordinator = QueryCoordinator::new(
        workspace,
        view.clone(),
        view.capability().expect("selected capability"),
        crate::builtin::admitted_coverage().expect("coverage"),
        corpus,
    )
    .expect("query coordinator");
    Fixture {
        coordinator,
        library,
        function: function.expect("function"),
        alias: alias.expect("alias"),
        external,
        inferred,
    }
}

fn presentation(
    row: &Row,
    name: &str,
    kind: &str,
    project: Option<String>,
) -> SemanticQueryPresentation {
    SemanticQueryPresentation {
        id: row.id.stable_key(),
        kind: kind.to_owned(),
        coordinate: row.label.clone(),
        name: name.to_owned(),
        signature: row.signature.clone(),
        documentation: String::new(),
        score: None,
        project,
        parent: None,
        related: Box::new([]),
    }
}

#[test]
fn source_declaration_ranking_keeps_python_typescript_go_and_rust_types_and_aliases() {
    for (profile, path) in [
        (
            LanguageProfile::Python(PythonVersion::Python314),
            "core/config.py",
        ),
        (
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            "src/config.ts",
        ),
        (LanguageProfile::Go(GoVersion::Go124), "config.go"),
        (
            LanguageProfile::Rust(RustEdition::Rust2024),
            "src/config.rs",
        ),
    ] {
        let fixture = fixture(profile, path);
        let result = fixture
            .coordinator
            .search_local(LocalQuery::prefix("get_app_settings", 1).expect("one result"))
            .expect("search");
        assert_eq!(
            result.rows[0].row.id, fixture.function,
            "profile {profile:?}"
        );
        assert_eq!(result.total_matches, 36);
        let alias = fixture
            .coordinator
            .search_local(LocalQuery::prefix("SettingsAlias", 1).expect("alias query"))
            .expect("alias search");
        assert_eq!(
            alias.rows[0].row.id, fixture.alias,
            "declared aliases stay searchable"
        );
        let full = fixture
            .coordinator
            .search_local(LocalQuery::prefix("get_app_settings", 64).expect("full query"))
            .expect("full search");
        assert_eq!(
            full.rows.last().expect("external retained").row.id,
            fixture.external
        );
        assert_eq!(
            full.rows.len(),
            full.total_matches,
            "no inferred or external record is hidden"
        );
        let ids = full
            .rows
            .iter()
            .map(|ranked| ranked.row.id)
            .collect::<BTreeSet<_>>();
        assert!(fixture.inferred.iter().all(|id| ids.contains(id)));
        assert!(ids.contains(&fixture.alias));
        assert!(matches!(
            full.lanes[1].coverage,
            CoverageBasis::CompleteView { .. }
        ));

        let root = fixture.coordinator.corpus.view.root();
        let query = backend_engine::Query::new(
            "get_app_settings",
            root,
            backend_engine::QueryLimit::new(1).expect("limit"),
        );
        let mut next = Some(query.clone());
        let mut paged = Vec::new();
        while let Some(request) = next.take() {
            let page = route_search_page(&fixture.coordinator, &fixture.library, &request)
                .expect("product page");
            paged.extend(ranked_page_ids(&page));
            next = page.next.map(|cursor| query.clone().with_cursor(cursor));
        }
        assert_eq!(
            paged,
            full.ranked_row_ids(),
            "stable complete paging {profile:?}"
        );
    }
}

#[test]
fn source_declaration_ranking_survives_semantic_scores_for_inferred_types_and_external_targets() {
    let fixture = fixture(
        LanguageProfile::Python(PythonVersion::Python314),
        "core/config.py",
    );
    let coverage = crate::builtin::admitted_coverage().expect("coverage");
    let embedding = recipe();
    let mut candidates = Vec::new();
    let mut payloads = Vec::new();
    for (row, distance) in [
        (fixture.inferred[0], 0.0),
        (fixture.external, 0.1),
        (fixture.function, 2.0),
    ] {
        let candidate = fixture
            .coordinator
            .semantic_candidate(row)
            .expect("selected candidate");
        candidates.push(candidate);
        payloads.push((
            candidate.0,
            semantic::VectorPoint::new(candidate, vec![distance, distance])
                .expect("point")
                .to_payload(),
        ));
    }
    let relation =
        RelationState::<semantic::CandidateRelation>::from_entries(payloads.clone(), coverage)
            .expect("relation");
    let binding = fixture
        .coordinator
        .semantic_binding(relation.root(), embedding.version());
    let state = semantic::CandidateState::new(
        binding,
        coverage,
        payloads
            .into_iter()
            .map(|(id, payload)| (semantic::CandidateId(id), payload))
            .collect(),
        semantic::Limits::default(),
    )
    .expect("state");
    let facts = semantic::VectorFacts::from_recipe(state, embedding).expect("vectors");
    let base = semantic::AnnBase::from_facts(
        &facts,
        semantic::SearchQuality::Exact,
        semantic::Limits::default(),
    )
    .expect("base");
    let source =
        semantic::MemorySource::new(binding, coverage, candidates, semantic::Limits::default())
            .expect("provider");
    let index = semantic::VectorIndex::new(base, facts, source).expect("index");
    let vector = semantic::QueryVector::new(embedding, vec![0.0, 0.0]).expect("vector");
    for limit in [1, 64] {
        let local = fixture
            .coordinator
            .search_local(LocalQuery::prefix("get_app_settings", limit).expect("query"))
            .expect("local");
        let result = local.accelerate(SemanticAcceleration {
            index: &index,
            query: &vector,
        });
        assert_eq!(result.rows[0].row.id, fixture.function);
        if limit == 64 {
            assert_eq!(
                result.rows.last().expect("retained endpoint").row.id,
                fixture.external
            );
            assert_eq!(result.rows.len(), result.total_matches);
            assert_eq!(
                result.rows[2].row.id, fixture.inferred[0],
                "provider order within unsourced tier survives"
            );
        }
        assert_eq!(result.lanes[2].freshness, Freshness::Current);
    }
}
