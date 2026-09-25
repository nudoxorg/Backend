//! Adversarial graph extension tests.

#![allow(
    clippy::expect_used,
    reason = "test fixtures use expect to make invariant failures local"
)]

use super::*;
use backend_version::{
    AuthorityScopeClaim, Coverage, CoverageWitness, ProducerObservationClaims,
    ProducerObservationVerifier, RelationState, ScopeRoot, UntrustedProducerObservation,
    WorkspaceManifest, WorkspaceRoot, admit_complete_scope, admit_producer_observation,
};
use backend_semantic::ir::{
    DeclarationFamilyId, DeclarationIdentity, ExternalFragmentId, ExternalTarget,
    ExternalTargetIdentity, IrBuilder, SemanticCoreReader as _, SourceIdentity, StableRef,
    VariantFingerprint,
};
use backend_semantic::vocabulary::{
    CompileRecipeFact, LanguageProfile, NativeTool, PackageUrl, RustEdition, Stage,
};
use futures_util::StreamExt as _;
use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
use std::collections::BTreeMap;

struct FixtureCoverageVerifier {
    producer: [u8; 32],
    scope: ScopeRoot,
    context: [u8; 32],
    evidence: Vec<u8>,
}

impl ProducerObservationVerifier for FixtureCoverageVerifier {
    type Error = ();

    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        (observation.producer_identity() == self.producer
            && observation.scope_root() == self.scope
            && observation.context() == self.context
            && observation.evidence() == self.evidence.as_slice())
        .then(|| {
            ProducerObservationClaims::new(
                self.producer,
                self.scope,
                self.context,
                *blake3::hash(&self.evidence).as_bytes(),
            )
        })
        .ok_or(())
    }
}

fn authorized_coverage(bytes: &[u8; 32]) -> CoverageWitness {
    let authority = Authority::from_value(bytes);
    let declaration = AuthorityScopeClaim::from_object_version(authority);
    let verifier = FixtureCoverageVerifier {
        producer: [9; 32],
        scope: declaration.scope_root(),
        context: [4; 32],
        evidence: vec![1, 2, 3],
    };
    let observation = UntrustedProducerObservation::new(
        verifier.producer,
        declaration.scope_root(),
        verifier.context,
        verifier.evidence.clone(),
    );
    let admitted = admit_producer_observation(observation, &verifier).expect("admitted producer");
    CoverageWitness::Complete(admit_complete_scope(declaration, admitted).expect("scope match"))
}

fn forged_complete_coverage() -> CoverageWitness {
    let manifest = WorkspaceManifest::from_versions(
        1,
        Vec::new(),
        Vec::new(),
        Authority::from_value(&[7; 32]),
        authorized_coverage(&[7; 32]),
    )
    .expect("complete manifest");
    WorkspaceManifest::decode_untrusted(&manifest.encode())
        .expect("decode manifest")
        .coverage()
}

fn workspace() -> WorkspaceRoot {
    WorkspaceManifest::from_versions(
        1,
        Vec::new(),
        Vec::new(),
        Authority::from_value(&[1; 32]),
        authorized_coverage(&[3; 32]),
    )
    .expect("valid workspace")
    .root()
}

fn compiler_external_fixture() -> (
    PackageUrl,
    LanguageProfile,
    DeclarationIdentity,
    ExternalTargetIdentity,
    backend_semantic::ir::SemanticImageFacts,
) {
    let coordinate = PackageUrl::parse("pkg:cargo/acme/demo@1.0.0".to_owned())
        .expect("canonical compiler package URL");
    let profile = LanguageProfile::Rust(RustEdition::Rust2021);
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"trustfall-external"),
        byte_len: 18,
    };
    let recipe = CompileRecipeFact::derive(
        profile,
        Stage::LowerIr,
        NativeTool::Rustc,
        source.identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"trustfall-toolchain"),
    );
    let declaration = DeclarationIdentity {
        family: DeclarationFamilyId::from_raw([3; 16]),
        variant: VariantFingerprint::from_raw([4; 16]),
    };
    let mut builder = IrBuilder::new();
    builder
        .set_image_provenance_for_package(source, recipe, &coordinate, "src/lib.rs")
        .expect("captured compiler image provenance");
    let external = builder
        .intern_external(ExternalTarget::Stable {
            target: StableRef {
                fragment: ExternalFragmentId::from_canonical_bytes(b"external-fragment"),
                declaration: DeclarationIdentity {
                    family: DeclarationFamilyId::from_raw([5; 16]),
                    variant: VariantFingerprint::from_raw([6; 16]),
                },
            },
        })
        .expect("external target");
    let image = builder.finish().expect("semantic image");
    let target = ExternalTargetIdentity::capture(&image, external)
        .expect("external coordinate belongs to image");
    (
        coordinate,
        profile,
        declaration,
        target,
        image.image_facts(),
    )
}

fn compiler_external_corpus(
    package: backend_library::PackageKey,
    image: [u8; 32],
    external_image: [u8; 32],
) -> Result<SemanticQueryCorpus, SemanticQueryError> {
    let (coordinate, profile, declaration, target, facts) = compiler_external_fixture();
    let project_id = backend_library::RowId::Package(package).stable_key();
    let external = CompilerExternalTargetEvidence::new(
        package,
        coordinate.clone(),
        profile,
        target,
        external_image,
        facts,
    );
    let external_id = external.row_id();
    let source =
        CompilerSemanticEvidence::new(package, coordinate, profile, declaration, image, facts);
    let source_id = source.row_id();
    SemanticQueryCorpus::admit(
        workspace(),
        vec![
            SemanticQueryFact::new(
                SemanticQueryEvidence::Package(PackageScopeEvidence::new(package)),
                SemanticQueryPresentation {
                    id: project_id.clone(),
                    kind: "project".to_owned(),
                    coordinate: "demo".to_owned(),
                    name: "demo".to_owned(),
                    signature: None,
                    documentation: String::new(),
                    score: None,
                    project: None,
                    parent: None,
                    related: Box::new([]),
                },
            ),
            SemanticQueryFact::new(
                SemanticQueryEvidence::Compiler(source),
                SemanticQueryPresentation {
                    id: source_id,
                    kind: "function".to_owned(),
                    coordinate: "demo::source".to_owned(),
                    name: "source".to_owned(),
                    signature: Some("fn source()".to_owned()),
                    documentation: String::new(),
                    score: None,
                    project: Some(project_id.clone()),
                    parent: None,
                    related: vec![external_id.clone()].into_boxed_slice(),
                },
            ),
            SemanticQueryFact::new(
                SemanticQueryEvidence::CompilerExternalTarget(external),
                SemanticQueryPresentation {
                    id: external_id,
                    kind: "external".to_owned(),
                    coordinate: "demo::external".to_owned(),
                    name: "external target".to_owned(),
                    signature: None,
                    documentation: String::new(),
                    score: None,
                    project: Some(project_id),
                    parent: None,
                    related: Box::new([]),
                },
            ),
        ],
    )
}

#[test]
fn compiler_external_edge_is_typed_scoped_and_queryable() {
    let package = backend_library::package_key("demo");
    let corpus =
        compiler_external_corpus(package, [7; 32], [7; 32]).expect("same-image compiler edge");
    let mut relabeled = corpus.facts().to_vec();
    let source = relabeled[1].clone();
    let mut presentation = source.presentation().clone();
    presentation.id = backend_library::RowId::Symbol(backend_library::symbol_key(
        "hostile-presentation-only-label",
    ))
    .stable_key();
    relabeled[1] = SemanticQueryFact::new(source.evidence().clone(), presentation);
    assert!(matches!(
        SemanticQueryCorpus::admit(corpus.workspace(), relabeled),
        Err(SemanticQueryError::Evidence(message))
            if message.contains("typed identity")
    ));

    let (cancellation, _) = SemanticQueryCancellation::new();
    let request = SemanticQueryRequest::admit_page(
        corpus,
        "{ Declaration { name @filter(op: \"=\", value: [\"$selected\"]) related { name @output state @output } } }",
        BTreeMap::from([("selected".to_owned(), "source".into())]),
        0,
        8,
        cancellation,
    )
    .expect("typed query request");
    let events = futures_executor::block_on(
        execute_semantic_query(request)
            .expect("Trustfall query")
            .collect::<Vec<_>>(),
    );
    let row = events
        .iter()
        .find_map(|event| match event {
            SemanticQueryEvent::Row(row) => Some(row.row()),
            SemanticQueryEvent::Terminal(_) => None,
        })
        .expect("external neighbor row");
    assert_eq!(row.get("name"), Some(&"external target".into()));
    assert_eq!(row.get("state"), Some(&"compiler_external_target".into()));

    assert!(matches!(
        compiler_external_corpus(package, [7; 32], [8; 32]),
        Err(SemanticQueryError::Evidence(message))
            if message.contains("exact image authority")
    ));

    let (_, _, _, target, _) = compiler_external_fixture();
    let first = CompilerExternalTargetEvidence::new(
        backend_library::package_key("first"),
        PackageUrl::parse("pkg:cargo/acme/demo@1.0.0".to_owned()).expect("coordinate"),
        LanguageProfile::Rust(RustEdition::Rust2021),
        target,
        [7; 32],
        compiler_external_fixture().4,
    );
    let second = CompilerExternalTargetEvidence::new(
        backend_library::package_key("second"),
        PackageUrl::parse("pkg:cargo/acme/demo@1.0.0".to_owned()).expect("coordinate"),
        LanguageProfile::Rust(RustEdition::Rust2021),
        target,
        [7; 32],
        compiler_external_fixture().4,
    );
    assert_ne!(first.row_id(), second.row_id());
}

#[test]
fn referenced_by_is_the_reverse_of_related() {
    let package = backend_library::package_key("demo");
    let corpus =
        compiler_external_corpus(package, [7; 32], [7; 32]).expect("same-image compiler edge");
    let (cancellation, _) = SemanticQueryCancellation::new();
    let request = SemanticQueryRequest::admit_page(
        corpus,
        "{ ExternalTarget { name @filter(op: \"=\", value: [\"$selected\"]) referencedBy { name @output } } }",
        BTreeMap::from([("selected".to_owned(), "external target".into())]),
        0,
        8,
        cancellation,
    )
    .expect("impact query");
    let events = futures_executor::block_on(
        execute_semantic_query(request)
            .expect("Trustfall query")
            .collect::<Vec<_>>(),
    );
    let row = events
        .iter()
        .find_map(|event| match event {
            SemanticQueryEvent::Row(row) => Some(row.row()),
            SemanticQueryEvent::Terminal(_) => None,
        })
        .expect("impact row");
    assert_eq!(row.get("name"), Some(&"source".into()));
}

fn structural_facts(count: usize) -> Vec<SemanticQueryFact> {
    let package = backend_library::package_key("large-graph");
    let project = backend_library::RowId::Package(package).stable_key();
    let profile = LanguageProfile::Rust(RustEdition::Rust2021);
    let mut facts = Vec::with_capacity(count.saturating_add(1));
    facts.push(SemanticQueryFact::new(
        SemanticQueryEvidence::Package(PackageScopeEvidence::new(package)),
        SemanticQueryPresentation {
            id: project.clone(),
            kind: "project".to_owned(),
            coordinate: "large-graph".to_owned(),
            name: "large-graph".to_owned(),
            signature: None,
            documentation: String::new(),
            score: None,
            project: None,
            parent: None,
            related: Box::new([]),
        },
    ));
    facts.extend((0..count).map(|index| {
        SemanticQueryFact::new(
            SemanticQueryEvidence::StructuralFallback(StructuralFallbackEvidence::new(
                package,
                profile,
                *blake3::hash(&index.to_be_bytes()).as_bytes(),
                [8; 32],
            )),
            SemanticQueryPresentation {
                id: backend_library::RowId::Symbol(backend_library::symbol_key(&format!(
                    "structural::{index}"
                )))
                .stable_key(),
                kind: "function".to_owned(),
                coordinate: format!("large-graph::{index}"),
                name: format!("item-{index}"),
                signature: None,
                documentation: String::new(),
                score: None,
                project: Some(project.clone()),
                parent: None,
                related: Box::new([]),
            },
        )
    }));
    facts
}

#[test]
fn explicit_corpus_envelope_scales_past_the_default_and_bounds_direct_queries() {
    let facts = structural_facts(4_096);
    assert!(matches!(
        SemanticQueryCorpus::admit(workspace(), facts.clone()),
        Err(SemanticQueryError::InvalidLimit)
    ));
    let limits = Limits {
        max_rows: 4_097,
        ..Limits::default()
    };
    let corpus = SemanticQueryCorpus::admit_with_limits(workspace(), facts, limits)
        .expect("explicit product-sized corpus");
    assert_eq!(corpus.facts().len(), 4_097);
    assert_eq!(corpus.limits(), limits);

    let (cancellation, _) = SemanticQueryCancellation::new();
    assert!(matches!(
        SemanticQueryRequest::admit_page(
            corpus.clone(),
            "x".repeat(limits.max_field_bytes.saturating_add(1)),
            BTreeMap::new(),
            0,
            1,
            cancellation,
        ),
        Err(SemanticQueryError::InvalidLimit)
    ));

    let too_many_variables = (0..=limits.max_fields_per_row)
        .map(|index| (format!("variable-{index}"), FieldValue::Null))
        .collect();
    let (cancellation, _) = SemanticQueryCancellation::new();
    assert!(matches!(
        SemanticQueryRequest::admit_page(
            corpus,
            "{ Item { id @output } }",
            too_many_variables,
            0,
            1,
            cancellation,
        ),
        Err(SemanticQueryError::InvalidLimit)
    ));
}

#[test]
fn evidence_digest_frames_parent_and_related_edges_distinctly() {
    let package = backend_library::package_key("framing");
    let project = backend_library::RowId::Package(package).stable_key();
    let source =
        backend_library::RowId::Symbol(backend_library::symbol_key("framing::source")).stable_key();
    let target =
        backend_library::RowId::Symbol(backend_library::symbol_key("framing::target")).stable_key();
    let row = |id: String, parent: Option<String>, related: Box<[String]>| {
        SemanticQueryFact::new(
            SemanticQueryEvidence::StructuralFallback(StructuralFallbackEvidence::new(
                package,
                LanguageProfile::Rust(RustEdition::Rust2021),
                [7; 32],
                [8; 32],
            )),
            SemanticQueryPresentation {
                coordinate: id.clone(),
                name: id.clone(),
                id,
                kind: "function".to_owned(),
                signature: None,
                documentation: String::new(),
                score: None,
                project: Some(project.clone()),
                parent,
                related,
            },
        )
    };
    let package_fact = || {
        SemanticQueryFact::new(
            SemanticQueryEvidence::Package(PackageScopeEvidence::new(package)),
            SemanticQueryPresentation {
                id: project.clone(),
                kind: "project".to_owned(),
                coordinate: "framing".to_owned(),
                name: "framing".to_owned(),
                signature: None,
                documentation: String::new(),
                score: None,
                project: None,
                parent: None,
                related: Box::new([]),
            },
        )
    };
    let parent = SemanticQueryCorpus::admit(
        workspace(),
        vec![
            package_fact(),
            row(source.clone(), Some(target.clone()), Box::new([])),
            row(target.clone(), None, Box::new([])),
        ],
    )
    .expect("parent graph");
    let related = SemanticQueryCorpus::admit(
        workspace(),
        vec![
            package_fact(),
            row(source, None, vec![target.clone()].into_boxed_slice()),
            row(target, None, Box::new([])),
        ],
    )
    .expect("related graph");
    assert_ne!(parent.evidence_digest(), related.evidence_digest());
}

fn binding(rows: &[GraphRow]) -> (Binding, CoverageWitness) {
    let coverage = authorized_coverage(&[7; 32]);
    let entries = rows
        .iter()
        .map(|row| (row.key, row.values.clone()))
        .collect::<Vec<_>>();
    let state = RelationState::<SemanticRelation>::from_entries(entries, coverage).expect("state");
    let reads = vec![Read::new("semantic".into(), "name".into())];
    let query = Query::new(reads, Limits::default()).expect("query");
    (
        Binding::new(
            workspace(),
            state.root(),
            Recipe::from_value(&[1; 32]),
            Authority::from_value(&[2; 32]),
            query.read_manifest,
        ),
        coverage,
    )
}

#[test]
fn stale_root_is_rejected_before_graph_state_admission() {
    let (binding, coverage) = binding(&[GraphRow {
        key: 1,
        values: vec!["alpha".into()],
    }]);
    let input = QueryInput {
        root: binding.root,
        recipe: binding.recipe,
        authority: binding.authority,
        reads: vec![Read::new("semantic".into(), "name".into())],
        rows: vec![vec!["alpha".into()]],
    };
    let projection = execute(&input).expect("projection");
    let stale =
        RelationState::<SemanticRelation>::from_entries([(1, vec!["beta".into()])], coverage)
            .expect("stale state")
            .root();
    assert_eq!(projection.root, binding.root);
    assert_ne!(projection.root, stale);
    let mut stale_binding = binding;
    stale_binding.root = stale;
    assert_eq!(
        GraphState::new(
            stale_binding,
            coverage,
            vec![GraphRow {
                key: 1,
                values: vec!["alpha".into()],
            }],
            Limits::default(),
        ),
        Err(Error::StaleRoot)
    );
}

#[test]
fn forged_complete_coverage_is_rejected_at_provider_boundary() {
    let (binding, _) = binding(&[]);
    let forged = forged_complete_coverage();
    assert_eq!(
        MemorySource::new(
            binding,
            QueryVersion::from_value(b"query".as_slice()),
            forged,
            Vec::new(),
            Limits::default(),
        ),
        Err(Error::IncompleteCoverage)
    );
}

#[test]
fn wrong_producer_is_rejected_before_complete_admission() {
    let authority = Authority::from_value(&[7; 32]);
    let declaration = AuthorityScopeClaim::from_object_version(authority);
    let verifier = FixtureCoverageVerifier {
        producer: [9; 32],
        scope: declaration.scope_root(),
        context: [4; 32],
        evidence: vec![1, 2, 3],
    };
    let observation = UntrustedProducerObservation::new(
        [8; 32],
        declaration.scope_root(),
        verifier.context,
        verifier.evidence.clone(),
    );
    assert!(admit_producer_observation(observation, &verifier).is_err());
}

#[test]
fn delete_and_readd_are_exact_transitions() {
    let initial = [GraphRow {
        key: 1,
        values: vec!["alpha".into()],
    }];
    let (binding, coverage) = binding(&initial);
    let state =
        GraphState::new(binding, coverage, initial.to_vec(), Limits::default()).expect("state");
    let deletion = state
        .prepare_delta(vec![GraphChange::Delete { key: 1 }])
        .expect("delete");
    let empty = state.apply_delta(&deletion).expect("apply delete");
    let addition = empty
        .prepare_delta(vec![GraphChange::Upsert {
            key: 1,
            values: vec!["beta".into()],
        }])
        .expect("readd");
    let restored = empty.apply_delta(&addition).expect("apply readd");
    assert_eq!(restored.iter().count(), 1);
    assert_ne!(deletion.delta.id(), addition.delta.id());
}

#[test]
fn delta_frontier_is_bound_to_the_state() {
    let initial = [GraphRow {
        key: 1,
        values: vec!["alpha".into()],
    }];
    let (binding, coverage) = binding(&initial);
    let state =
        GraphState::new(binding, coverage, initial.to_vec(), Limits::default()).expect("state");
    let mut delta = state
        .prepare_delta(vec![GraphChange::Delete { key: 1 }])
        .expect("delta");
    delta.binding = delta.binding.with_frontier(Frontier::from_value(&[9; 32]));
    assert_eq!(state.apply_delta(&delta), Err(Error::StaleRoot));
}

#[test]
fn delta_bytes_are_bounded_across_the_whole_change() {
    let (binding, coverage) = binding(&[]);
    let limits = Limits {
        max_total_bytes: 5,
        ..Limits::default()
    };
    let state = GraphState::new(binding, coverage, Vec::new(), limits).expect("empty state");
    assert_eq!(
        state.prepare_delta(vec![
            GraphChange::Upsert {
                key: 1,
                values: vec!["abc".into()],
            },
            GraphChange::Upsert {
                key: 2,
                values: vec!["abc".into()],
            },
        ]),
        Err(Error::SizeLimit)
    );
}

#[test]
fn paging_and_cursor_binding_are_deterministic() {
    let rows = vec![
        GraphRow {
            key: 2,
            values: vec!["b".into()],
        },
        GraphRow {
            key: 1,
            values: vec!["a".into()],
        },
        GraphRow {
            key: 3,
            values: vec!["c".into()],
        },
    ];
    let (binding, coverage) = binding(&[]);
    let query = Query::new(
        vec![Read::new("semantic".into(), "name".into())],
        Limits::default(),
    )
    .expect("query");
    let source = MemorySource::new(
        binding,
        query.version,
        coverage,
        rows,
        Limits {
            max_page: 2,
            ..Limits::default()
        },
    )
    .expect("source");
    let adapter = Adapter::new(
        source,
        Limits {
            max_page: 2,
            ..Limits::default()
        },
    )
    .expect("adapter");
    let first = adapter
        .query(&QueryRequest {
            binding,
            query: query.clone(),
            cursor: None,
            limit: 2,
        })
        .expect("page");
    assert_eq!(
        first.rows.iter().map(|row| row.key).collect::<Vec<_>>(),
        vec![1, 2]
    );
    let cursor = first.next.expect("cursor");
    let other = Query::new(
        vec![Read::new("semantic".into(), "type".into())],
        Limits::default(),
    )
    .expect("other query");
    assert!(matches!(
        adapter.query(&QueryRequest {
            binding,
            query: other,
            cursor: Some(cursor),
            limit: 1,
        }),
        Err(AdapterError::Extension(Error::StaleCursor))
    ));
}

#[test]
fn malformed_reads_and_size_limits_are_rejected() {
    assert_eq!(
        Query::new(
            vec![Read::new(String::new(), "field".into())],
            Limits::default()
        ),
        Err(Error::UndeclaredRead)
    );
    assert_eq!(
        Limits {
            max_page: 0,
            ..Limits::default()
        }
        .validate(),
        Err(Error::InvalidLimits)
    );
    let (binding, coverage) = binding(&[]);
    assert_eq!(
        MemorySource::new(
            binding,
            QueryVersion::from_value(b"query".as_slice()),
            incomplete_coverage(1, Coverage::Partial),
            Vec::new(),
            Limits::default(),
        ),
        Err(Error::IncompleteCoverage)
    );
    assert_eq!(
        GraphState::new(
            binding,
            coverage,
            vec![GraphRow {
                key: 1,
                values: vec!["x".into(), "y".into()],
            }],
            Limits {
                max_fields_per_row: 1,
                ..Limits::default()
            },
        ),
        Err(Error::SizeLimit)
    );
}

struct SubstitutingSource {
    page: GraphPage,
}

impl GraphSource for SubstitutingSource {
    type Error = ();

    fn fetch(&self, _: &QueryRequest) -> Result<GraphPage, Self::Error> {
        Ok(self.page.clone())
    }
}

#[test]
fn provider_substitution_cannot_change_binding_or_coverage() {
    let (binding, coverage) = binding(&[]);
    let query = Query::new(
        vec![Read::new("semantic".into(), "name".into())],
        Limits::default(),
    )
    .expect("query");
    let mut wrong = binding;
    wrong.root =
        RelationState::<SemanticRelation>::from_entries([(1, vec!["other".into()])], coverage)
            .expect("wrong root")
            .root();
    let page = GraphPage {
        schema: SchemaVersion::CURRENT,
        binding: wrong,
        query: query.version,
        rows: vec![GraphRow {
            key: 1,
            values: vec!["alpha".into()],
        }],
        next: None,
        coverage,
    };
    let adapter = Adapter::new(SubstitutingSource { page }, Limits::default()).expect("adapter");
    let request = QueryRequest {
        binding,
        query,
        cursor: None,
        limit: 1,
    };
    assert!(matches!(
        adapter.query(&request),
        Err(AdapterError::Extension(Error::StaleRoot))
    ));

    let incomplete = GraphPage {
        schema: SchemaVersion::CURRENT,
        binding,
        query: request.query.version,
        rows: vec![GraphRow {
            key: 1,
            values: vec!["alpha".into()],
        }],
        next: None,
        coverage: incomplete_coverage(7, Coverage::Partial),
    };
    let adapter =
        Adapter::new(SubstitutingSource { page: incomplete }, Limits::default()).expect("adapter");
    assert!(matches!(
        adapter.query(&request),
        Err(AdapterError::Extension(Error::IncompleteCoverage))
    ));
}

#[test]
fn declared_read_budget_is_strictly_bounded() {
    let result = Query::new(
        vec![Read::new("contract".into(), "field".into())],
        Limits {
            max_total_bytes: 1,
            ..Limits::default()
        },
    );
    assert_eq!(result, Err(Error::SizeLimit));
}

#[test]
fn graph_arrangement_reuses_base_and_keeps_query_manifest_bound() {
    let initial = vec![
        GraphRow {
            key: 1,
            values: vec!["alpha".into()],
        },
        GraphRow {
            key: 2,
            values: vec!["beta".into()],
        },
    ];
    let (binding, coverage) = binding(&initial);
    let state = GraphState::new(binding, coverage, initial, Limits::default()).expect("state");
    let arrangement = GraphArrangement::from_state(&state).expect("arrangement");
    let query = Query::new(
        vec![Read::new("semantic".into(), "name".into())],
        Limits::default(),
    )
    .expect("query");
    let plan = QueryPlan::new(binding, coverage, query.clone()).expect("plan");
    assert_eq!(
        arrangement
            .execute(&plan)
            .expect("execute")
            .iter()
            .map(|row| row.key)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let delta = state
        .prepare_delta(vec![GraphChange::Upsert {
            key: 1,
            values: vec!["gamma".into()],
        }])
        .expect("delta");
    let outcome = arrangement
        .advance(&delta, ArrangementLimits::default())
        .expect("advance");
    let advanced = match outcome {
        RefreshOutcome::Advanced(advanced) => advanced,
        other => {
            assert!(matches!(other, RefreshOutcome::Advanced(_)));
            return;
        }
    };
    assert!(std::ptr::eq(arrangement.base(), advanced.base()));
    let target_plan = QueryPlan::new(delta.binding, delta.coverage, query).expect("target plan");
    assert_eq!(
        advanced.execute(&target_plan).expect("target execute")[0].values,
        vec!["gamma"]
    );
}

#[test]
fn graph_arrangement_pages_merge_replacements_and_deletions_once() {
    let initial = vec![
        GraphRow {
            key: 1,
            values: vec!["alpha".into()],
        },
        GraphRow {
            key: 2,
            values: vec!["beta".into()],
        },
        GraphRow {
            key: 3,
            values: vec!["gamma".into()],
        },
    ];
    let (binding, coverage) = binding(&initial);
    let state = GraphState::new(binding, coverage, initial, Limits::default()).expect("state");
    let arrangement = GraphArrangement::from_state(&state).expect("arrangement");
    let delta = state
        .prepare_delta(vec![
            GraphChange::Upsert {
                key: 2,
                values: vec!["replaced".into()],
            },
            GraphChange::Delete { key: 3 },
            GraphChange::Upsert {
                key: 4,
                values: vec!["added".into()],
            },
        ])
        .expect("delta");
    let advanced = match arrangement
        .advance(&delta, ArrangementLimits::default())
        .expect("advance")
    {
        RefreshOutcome::Advanced(value) => value,
        other => {
            assert!(matches!(other, RefreshOutcome::Advanced(_)));
            return;
        }
    };
    let query = Query::new(
        vec![Read::new("semantic".into(), "name".into())],
        Limits::default(),
    )
    .expect("query");
    let plan = QueryPlan::new(delta.binding, delta.coverage, query).expect("plan");
    let mut cursor = None;
    let mut keys = Vec::new();
    loop {
        let page = advanced
            .page(&plan, cursor, 1, Limits::default())
            .expect("page");
        keys.extend(page.rows.iter().map(|row| row.key));
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(keys, vec![1, 2, 4]);
}

#[test]
fn graph_arrangement_delete_readd_matches_a_restarted_base() {
    let initial = vec![
        GraphRow {
            key: 1,
            values: vec!["alpha".into()],
        },
        GraphRow {
            key: 2,
            values: vec!["beta".into()],
        },
    ];
    let (binding, coverage) = binding(&initial);
    let state = GraphState::new(binding, coverage, initial, Limits::default()).expect("state");
    let arrangement = GraphArrangement::from_state(&state).expect("base");

    let replacement = state
        .prepare_delta(vec![GraphChange::Upsert {
            key: 1,
            values: vec!["gamma".into()],
        }])
        .expect("replacement delta");
    let replaced_state = state.apply_delta(&replacement).expect("replaced state");
    let outcome = arrangement
        .advance(&replacement, ArrangementLimits::default())
        .expect("advance replacement");
    assert!(matches!(&outcome, RefreshOutcome::Advanced(_)));
    let RefreshOutcome::Advanced(arrangement) = outcome else {
        return;
    };

    let deletion = replaced_state
        .prepare_delta(vec![GraphChange::Delete { key: 1 }])
        .expect("delete delta");
    let deleted_state = replaced_state
        .apply_delta(&deletion)
        .expect("deleted state");
    let outcome = arrangement
        .advance(&deletion, ArrangementLimits::default())
        .expect("advance deletion");
    assert!(matches!(&outcome, RefreshOutcome::Advanced(_)));
    let RefreshOutcome::Advanced(arrangement) = outcome else {
        return;
    };

    let readd = deleted_state
        .prepare_delta(vec![GraphChange::Upsert {
            key: 1,
            values: vec!["delta".into()],
        }])
        .expect("readd delta");
    let restored_state = deleted_state.apply_delta(&readd).expect("restored state");
    let outcome = arrangement
        .advance(&readd, ArrangementLimits::default())
        .expect("advance readd");
    assert!(matches!(&outcome, RefreshOutcome::Advanced(_)));
    let RefreshOutcome::Advanced(arrangement) = outcome else {
        return;
    };

    let query = Query::new(
        vec![Read::new("semantic".into(), "name".into())],
        Limits::default(),
    )
    .expect("query");
    let plan = QueryPlan::new(restored_state.binding(), coverage, query.clone()).expect("plan");
    let incremental = arrangement.execute(&plan).expect("incremental query");

    let restarted_state = GraphState::new(
        restored_state.binding(),
        restored_state.coverage(),
        restored_state.iter().collect(),
        Limits::default(),
    )
    .expect("restarted state");
    let restarted = GraphArrangement::from_state(&restarted_state)
        .expect("restarted base")
        .execute(
            &QueryPlan::new(restarted_state.binding(), restarted_state.coverage(), query)
                .expect("restarted plan"),
        )
        .expect("restarted query");
    assert_eq!(incremental, restarted);
    assert_eq!(arrangement.binding(), restarted_state.binding());
}

#[test]
fn graph_arrangement_rejects_stale_plan_and_bounds_rebuild() {
    let initial = vec![GraphRow {
        key: 1,
        values: vec!["alpha".into()],
    }];
    let (binding, coverage) = binding(&initial);
    let state = GraphState::new(binding, coverage, initial, Limits::default()).expect("state");
    let arrangement = GraphArrangement::from_state(&state).expect("arrangement");
    let query = Query::new(
        vec![Read::new("semantic".into(), "name".into())],
        Limits::default(),
    )
    .expect("query");
    let mut stale_binding = binding;
    stale_binding.root =
        RelationState::<SemanticRelation>::from_entries([(1, vec!["other".into()])], coverage)
            .expect("stale")
            .root();
    assert_eq!(
        arrangement.execute(&QueryPlan::new(stale_binding, coverage, query).expect("plan")),
        Err(Error::StaleRoot)
    );
    let delta = state
        .prepare_delta(vec![GraphChange::Upsert {
            key: 1,
            values: vec!["beta".into()],
        }])
        .expect("delta");
    let outcome = arrangement
        .advance(
            &delta,
            ArrangementLimits {
                max_changed_rows: 1,
                max_bytes: 1,
                ..ArrangementLimits::default()
            },
        )
        .expect("plan");
    assert!(matches!(outcome, RefreshOutcome::RebuildRequired(_)));
}
