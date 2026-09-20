use super::*;
use backend_flow::{CanonicalValue, Delta as FlowDelta, Time};
use backend_version::{
    AuthorityScopeClaim, AuthorizedCompleteCoverage, CoverageWitness, ObjectVersion,
    ProducerObservationVerifier, RelationState, Schema, ScopeRoot, UntrustedCoverageScope,
    UntrustedProducerObservation, admit_complete_scope, admit_producer_observation,
};

struct TestProducerVerifier {
    expected_identity: [u8; 32],
}

impl ProducerObservationVerifier for TestProducerVerifier {
    type Error = ();

    fn verify(&self, observation: &UntrustedProducerObservation) -> Result<(), Self::Error> {
        (observation.producer_identity() == self.expected_identity)
            .then_some(())
            .ok_or(())
    }
}

fn test_scope(seed: u8) -> ScopeRoot {
    ScopeRoot::from_bytes(
        ObjectVersion::<FacetValueSchema>::from_value(&FacetValue::new(
            FacetKind::Facet,
            vec![seed],
        ))
        .to_bytes(),
    )
}

fn complete(scope: u8) -> AuthorizedCompleteCoverage {
    let value = FacetValue::new(FacetKind::Facet, vec![scope]);
    let version = ObjectVersion::<FacetValueSchema>::from_value(&value);
    let claim = AuthorityScopeClaim::from_object_version(version);
    let admitted = admit_producer_observation(
        UntrustedProducerObservation::new(
            [scope; 32],
            claim.scope_root(),
            [scope; 32],
            vec![scope],
        ),
        &TestProducerVerifier {
            expected_identity: [scope; 32],
        },
    )
    .expect("producer admission");
    admit_complete_scope(claim, admitted).expect("matching admitted scope")
}

fn complete_for_scope(scope: &AuthorityScope) -> AuthorizedCompleteCoverage {
    let version = scope.version();
    let claim = AuthorityScopeClaim::from_object_version(version);
    let admitted = admit_producer_observation(
        UntrustedProducerObservation::new(
            *claim.scope_root().as_bytes(),
            claim.scope_root(),
            *claim.scope_root().as_bytes(),
            scope.canonical_bytes(),
        ),
        &TestProducerVerifier {
            expected_identity: *claim.scope_root().as_bytes(),
        },
    )
    .expect("producer admission");
    admit_complete_scope(claim, admitted).expect("matching admitted scope")
}

fn source() -> Source {
    Source::new(1, "a").expect("source")
}

fn entity(name: &str) -> EntityId {
    entity_key(&Entity::new(source(), name, None).expect("entity"))
}

fn authority() -> Authority {
    Authority::new("native", "semantic").expect("authority")
}

fn provenance(revision: u64) -> Provenance {
    Provenance::new(
        authority(),
        AuthorityValue::new(authority(), revision, vec![revision.to_le_bytes()[0]]),
        source(),
        SourceValue::new(
            vec![revision.to_le_bytes()[0]],
            SourceEncoding::Binary,
            revision,
        )
        .expect("source value"),
    )
    .expect("provenance")
}

fn authority_scope(package: &[u8], facets: Vec<FacetKind>) -> AuthorityScope {
    AuthorityScope::new(
        provenance(1),
        package.to_vec(),
        Some((b"a".to_vec(), b"z".to_vec())),
        facets,
    )
    .expect("authority scope")
}

fn occurrence(name: &str, ordinal: u32) -> OccurrenceFact {
    let source_id = entity(name);
    OccurrenceFact {
        target: entity("target"),
        source: source_id,
        key: OccurrenceKey {
            span: SourceSpan::new(source(), ordinal, ordinal + 1).expect("span"),
            ordinal,
        },
        multiplicity: 1,
        support: 1,
    }
}

fn flow_key() -> backend_flow::RowKey {
    backend_flow::RowKey {
        relation: backend_flow::RelationIdentity::from_value(&[1; 32]),
        object: backend_flow::ObjectIdentity::from_value(&[1; 32]),
        key: 1,
    }
}

#[test]
fn captured_empty_is_complete_but_unavailable_is_not() {
    let empty = FacetCoverage::captured_empty(complete(1));
    let unavailable = FacetCoverage::unavailable(1);
    assert_eq!(empty.availability(), Availability::CapturedEmpty);
    assert_ne!(empty.availability(), unavailable.availability());
    assert!(empty.authoritative());
    assert!(!unavailable.authoritative());
}

#[test]
fn coverage_states_keep_complete_empty_absence_deletion_and_unknown_distinct() {
    let root = test_scope(10);
    let admitted = complete(10);
    let live = FacetCoverage::complete(admitted);
    let empty = FacetCoverage::captured_empty(admitted);
    let absent = FacetCoverage::absent(admitted);
    let deleted = FacetCoverage::deleted(admitted);
    let partial = FacetCoverage::partial_scope(root);
    let unavailable = FacetCoverage::unavailable_scope(root);
    let unsupported = FacetCoverage::unsupported_scope(root);

    assert_eq!(live.availability(), Availability::Present);
    assert_eq!(empty.availability(), Availability::CapturedEmpty);
    assert_eq!(absent.availability(), Availability::Absent);
    assert_eq!(deleted.availability(), Availability::Deleted);
    assert_eq!(partial.availability(), Availability::Partial);
    assert_eq!(unavailable.availability(), Availability::Unavailable);
    assert_eq!(unsupported.availability(), Availability::Unsupported);
    assert!(live.authoritative());
    assert!(empty.authoritative());
    assert!(absent.authoritative());
    assert!(deleted.authoritative());
    assert!(!partial.authoritative());
    assert!(!unavailable.authoritative());
    assert!(!unsupported.authoritative());
    assert_eq!(partial.scope_root().as_bytes(), root.as_bytes());
    assert_eq!(unavailable.scope_root().as_bytes(), root.as_bytes());
    assert_eq!(unsupported.scope_root().as_bytes(), root.as_bytes());

    let encodings = [
        live,
        empty,
        absent,
        deleted,
        partial,
        unavailable,
        unsupported,
    ]
    .map(|coverage| {
        let mut encoded = Vec::new();
        canonical_coverage(&mut encoded, coverage);
        encoded
    });
    for (index, left) in encodings.iter().enumerate() {
        for right in encodings.iter().skip(index + 1) {
            assert_ne!(left, right);
        }
    }
}

#[test]
fn coverage_identity_keeps_all_scope_root_bytes() {
    let mut first = [0; 32];
    first[..8].copy_from_slice(&7_u64.to_be_bytes());
    let mut second = first;
    second[31] = 1;
    let first = FacetCoverage::partial_scope(ScopeRoot::from_bytes(first));
    let second = FacetCoverage::partial_scope(ScopeRoot::from_bytes(second));
    assert!(!first.same_scope(second));
    let mut first_bytes = Vec::new();
    let mut second_bytes = Vec::new();
    canonical_coverage(&mut first_bytes, first);
    canonical_coverage(&mut second_bytes, second);
    assert_ne!(first_bytes, second_bytes);
}

#[test]
fn incomplete_negative_facts_cannot_claim_absence() {
    let scope = test_scope(4);
    let read = Read::negative(FacetKind::Name, scope);
    let partial = CoverageWitness::Partial(UntrustedCoverageScope::from_scope_root(scope));
    assert_eq!(
        ReadManifest::require_complete_negative(read, partial),
        Err(SemanticError::IncompleteNegativeFact)
    );
    assert!(ReadObservation::negative(read, complete(4)).is_ok());
}

#[test]
fn occurrence_support_retracts_only_the_removed_observation() {
    let coverage = FacetCoverage::complete(complete(2));
    let first = occurrence("source", 0);
    let second = occurrence("source", 1);
    let mut ledger = OccurrenceLedger::replace(coverage, vec![first.clone(), second.clone()])
        .expect("replacement");
    let before = ledger.edges().expect("edges");
    assert_eq!(before.edges()[0].support, 2);
    let delta =
        FlowDelta::checked(flow_key(), first, Time::default(), -1).expect("nonzero retraction");
    let after = ledger
        .apply_deltas(coverage, &[delta])
        .expect("safe retraction");
    assert_eq!(after.edges()[0].support, 1);
}

#[test]
fn incomplete_occurrence_retraction_is_rejected() {
    let coverage = FacetCoverage::complete(complete(2));
    let first = occurrence("source", 0);
    let mut ledger = OccurrenceLedger::replace(coverage, vec![first.clone()]).expect("seed");
    let delta =
        FlowDelta::checked(flow_key(), first, Time::default(), -1).expect("nonzero retraction");
    assert_eq!(
        ledger.apply_deltas(FacetCoverage::partial(2), &[delta]),
        Err(SemanticError::IncompleteNegativeFact)
    );
}

#[test]
fn occurrence_retractions_preserve_other_support_and_are_transactional() {
    let coverage = FacetCoverage::complete(complete(3));
    let mut first = occurrence("source", 0);
    first.support = 2;
    let mut second = occurrence("source", 1);
    second.support = 3;
    let mut ledger = OccurrenceLedger::replace(coverage, vec![first.clone(), second.clone()])
        .expect("replacement");
    assert_eq!(ledger.edges().expect("initial edges").edges()[0].support, 5);

    let missing = occurrence("missing", 2);
    let missing_delta =
        FlowDelta::checked(flow_key(), missing, Time::default(), -1).expect("delta");
    assert_eq!(
        ledger.apply_deltas(coverage, &[missing_delta]),
        Err(SemanticError::MissingSupport)
    );
    assert_eq!(ledger.len(), 2);

    let first_delta = FlowDelta::checked(flow_key(), first, Time::default(), -1).expect("delta");
    let edges = ledger
        .apply_deltas(coverage, &[first_delta])
        .expect("retraction");
    assert_eq!(edges.edges()[0].support, 3);
    assert_eq!(ledger.len(), 1);

    let mismatch = FacetCoverage::complete(complete(4));
    let second_delta = FlowDelta::checked(flow_key(), second, Time::default(), -1).expect("delta");
    assert_eq!(
        ledger.apply_deltas(mismatch, &[second_delta]),
        Err(SemanticError::ScopeMismatch)
    );
    assert_eq!(ledger.len(), 1);
}

#[test]
fn occurrence_incremental_work_is_local_to_changed_rows_and_edges() {
    let coverage = FacetCoverage::complete(complete(30));
    let initial = (0..96)
        .map(|ordinal| occurrence(&format!("source-{ordinal}"), ordinal))
        .collect::<Vec<_>>();
    let mut ledger = OccurrenceLedger::replace(coverage, initial).expect("replacement");
    ledger.reset_work_counters();
    let before = ledger.edges().expect("edges");

    let added = occurrence("another-source", 97);
    let add = FlowDelta::checked(flow_key(), added.clone(), Time::default(), 1).expect("delta");
    let after_add = ledger.apply_deltas(coverage, &[add]).expect("addition");
    let work = ledger.work_counters();
    assert_eq!(work.input_rows, 1);
    assert_eq!(work.occurrence_updates, 1);
    assert_eq!(work.affected_edges, 1);
    assert!(work.occurrence_probes < 64);
    assert!(work.occurrence_nodes > 0);
    assert!(work.edge_probes < 32);
    assert_eq!(after_add.edges().len(), before.edges().len() + 1);

    let retract =
        FlowDelta::checked(flow_key(), added, Time::default(), -1).expect("retraction delta");
    let after_retract = ledger
        .apply_deltas(coverage, &[retract])
        .expect("retraction");
    assert_eq!(after_retract, before);
}

#[test]
fn occurrence_ledger_clone_shares_the_persistent_row_root() {
    let coverage = FacetCoverage::complete(complete(34));
    let facts = (0..256)
        .map(|ordinal| occurrence(&format!("shared-source-{ordinal}"), ordinal))
        .collect::<Vec<_>>();
    let ledger = OccurrenceLedger::replace(coverage, facts).expect("replacement");
    let snapshot = ledger.clone();
    assert!(ledger.rows_root_is_shared_with(&snapshot));
    assert!(
        ledger
            .edges()
            .expect("edges")
            .root_is_shared_with(&snapshot.edges().expect("snapshot edges"))
    );
    assert_eq!(ledger, snapshot);
}

#[test]
fn occurrence_failed_batch_preserves_edges_and_work_counters() {
    let coverage = FacetCoverage::complete(complete(31));
    let first = occurrence("source", 0);
    let mut ledger = OccurrenceLedger::replace(coverage, vec![first.clone()]).expect("seed");
    let before = ledger.clone();
    let before_edges = ledger.edges().expect("edges");
    let before_work = ledger.work_counters();
    let missing = occurrence("missing", 1);
    let delta = FlowDelta::checked(flow_key(), missing, Time::default(), -1).expect("delta");
    assert_eq!(
        ledger.apply_deltas(coverage, &[delta]),
        Err(SemanticError::MissingSupport)
    );
    assert_eq!(ledger.edges().expect("edges after failure"), before_edges);
    assert_eq!(ledger, before);
    assert!(ledger.rows_root_is_shared_with(&before));
    assert_eq!(ledger.work_counters(), before_work);
    assert_eq!(ledger.len(), 1);
}

#[test]
fn occurrence_incremental_overflow_rolls_back_the_edge_tree() {
    let coverage = FacetCoverage::complete(complete(32));
    let mut first = occurrence("source", 0);
    first.multiplicity = u32::MAX;
    let mut ledger = OccurrenceLedger::replace(coverage, vec![first.clone()]).expect("seed");
    let before_edges = ledger.edges().expect("edges");
    let before_work = ledger.work_counters();
    let delta = FlowDelta::checked(flow_key(), first, Time::default(), 1).expect("delta");
    assert_eq!(
        ledger.apply_deltas(coverage, &[delta]),
        Err(SemanticError::Overflow)
    );
    assert_eq!(ledger.edges().expect("edges after failure"), before_edges);
    assert_eq!(ledger.work_counters(), before_work);
    assert_eq!(ledger.len(), 1);
}

#[test]
fn occurrence_incremental_batches_match_weighted_reference_rebuild() {
    let coverage = FacetCoverage::complete(complete(33));
    let facts = (0..24)
        .map(|ordinal| occurrence(&format!("random-source-{}", ordinal % 6), ordinal))
        .collect::<Vec<_>>();
    let mut ledger = OccurrenceLedger::replace(coverage, Vec::new()).expect("empty seed");
    let mut counts = vec![0_i64; facts.len()];
    let mut state = 0x9e37_79b9_u64;

    for _ in 0..256 {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        let index =
            usize::try_from(state % u64::try_from(facts.len()).expect("fact count fits in u64"))
                .expect("index");
        let diff = if counts[index] == 0 || state & 1 == 0 {
            1
        } else {
            -1
        };
        let delta = FlowDelta::checked(flow_key(), facts[index].clone(), Time::default(), diff)
            .expect("delta");
        ledger
            .apply_deltas(coverage, &[delta])
            .expect("valid randomized update");
        counts[index] += diff;

        let weighted = facts
            .iter()
            .zip(&counts)
            .filter(|(_, count)| **count > 0)
            .map(|(fact, count)| {
                let mut weighted = fact.clone();
                weighted.multiplicity *= u32::try_from(*count).expect("small count");
                weighted.support *= u16::try_from(*count).expect("small count");
                weighted
            })
            .collect::<Vec<_>>();
        let expected = EdgeSet::from_occurrences(coverage, weighted).expect("reference rebuild");
        assert_eq!(ledger.edges().expect("incremental edges"), expected);
    }
}

#[test]
fn component_identity_uses_sorted_scc_inputs() {
    let a = entity("a");
    let b = entity("b");
    let edge_ab = ComponentEdge::new(a, b, EdgeKind::TypeDependency);
    let edge_ba = ComponentEdge::new(b, a, EdgeKind::TypeDependency);
    let first = ComponentDescriptor::new(vec![a, b], vec![edge_ab, edge_ba], Vec::new())
        .expect("component");
    let second = ComponentDescriptor::new(vec![b, a], vec![edge_ba, edge_ab], Vec::new())
        .expect("component");
    assert_eq!(first.canonical_bytes(), second.canonical_bytes());
    assert_eq!(first.version(), second.version());
}

#[test]
fn flow_edge_encoding_is_schema_bound_and_complete() {
    let edge = Edge {
        from: entity("flow-edge-source"),
        to: entity("flow-edge-target"),
        kind: EdgeKind::Reference,
        multiplicity: 2,
        support: 3,
    };
    let encode = |value: &Edge| {
        let mut bytes = Vec::new();
        value.encode_canonical(&mut bytes);
        bytes
    };
    let encoded = encode(&edge);
    let edge_type = EdgeSchema::TYPE.to_be_bytes();
    let value_type = EdgeValueSchema::TYPE.to_be_bytes();
    let prefix = [
        EdgeSchema::DOMAIN,
        edge_type[0],
        edge_type[1],
        EdgeSchema::VERSION,
        EdgeValueSchema::DOMAIN,
        value_type[0],
        value_type[1],
        EdgeValueSchema::VERSION,
    ];
    assert!(encoded.starts_with(&prefix));
    for changed in [
        Edge {
            from: entity("flow-edge-other-source"),
            ..edge.clone()
        },
        Edge {
            to: entity("flow-edge-other-target"),
            ..edge.clone()
        },
        Edge {
            kind: EdgeKind::Call,
            ..edge.clone()
        },
        Edge {
            multiplicity: 4,
            ..edge.clone()
        },
        Edge {
            support: 5,
            ..edge.clone()
        },
    ] {
        assert_ne!(encoded, encode(&changed));
    }
}

#[test]
fn deterministic_encodings_include_scope_and_nested_fields() {
    let first = TypeExpr::Function {
        args: vec![TypeExpr::Named("A".into())],
        result: Box::new(TypeExpr::Tuple(vec![TypeExpr::Infer])),
    };
    let second = first.clone();
    let mut one = Vec::new();
    let mut two = Vec::new();
    canonical_type(&first, &mut one);
    canonical_type(&second, &mut two);
    assert_eq!(one, two);
    assert_ne!(one, Vec::<u8>::new());
    let manifest = ReadManifest::new(vec![Read::negative_range(
        FacetKind::Type,
        test_scope(8),
        2,
    )])
    .expect("manifest");
    let same_manifest = manifest.clone();
    let changed_manifest = ReadManifest::new(vec![Read::negative_range(
        FacetKind::Type,
        test_scope(8),
        3,
    )])
    .expect("changed");
    assert_eq!(manifest.version(), same_manifest.version());
    assert_ne!(manifest.version(), changed_manifest.version());
}

fn assert_value_version_changes<S: Schema>(first: &S::Value, changed: &S::Value) {
    let first_version = ObjectVersion::<S>::from_value(first);
    assert_eq!(first_version, ObjectVersion::<S>::from_value(first));
    assert_ne!(first_version, ObjectVersion::<S>::from_value(changed));
}

#[test]
fn every_registered_facet_and_nested_value_has_an_explicit_version_contract() {
    let mut tags: Vec<_> = FacetKind::ALL.iter().map(|facet| facet.tag()).collect();
    let unsorted = tags.clone();
    tags.sort_unstable();
    tags.dedup();
    assert_eq!(tags.len(), FacetKind::ALL.len());
    assert_eq!(unsorted, tags);

    for facet in FacetKind::ALL {
        let first = FacetValue::new(facet, vec![0, 1]);
        let changed = FacetValue::new(facet, vec![0, 2]);
        assert_value_version_changes::<FacetValueSchema>(&first, &changed);
    }
}

#[test]
fn source_entity_authority_type_and_document_versions_include_nested_fields() {
    let source_value =
        SourceValue::new(b"fn f() {}".to_vec(), SourceEncoding::Utf8, 1).expect("source value");
    let changed_source =
        SourceValue::new(b"fn f() {}".to_vec(), SourceEncoding::Utf8, 2).expect("source value");
    assert_value_version_changes::<SourceValueSchema>(&source_value, &changed_source);

    let entity_value = EntityValue::new(
        Some("f".into()),
        EntityKind::Function,
        VisibilityState::Public,
        Parent::Root,
    );
    let changed_entity = EntityValue::new(
        Some("f".into()),
        EntityKind::Function,
        VisibilityState::Public,
        Parent::Unrepresented,
    );
    assert_value_version_changes::<EntityValueSchema>(&entity_value, &changed_entity);

    let authority_value = AuthorityValue::new(authority(), 1, vec![1]);
    let changed_authority = AuthorityValue::new(authority(), 1, vec![1, 2]);
    assert_value_version_changes::<AuthorityValueSchema>(&authority_value, &changed_authority);

    let type_key = type_key(&TypeIdentity::new(entity("owner"), "return").expect("type identity"));
    let type_node = TypeNode {
        tag: TypeTag::Applied,
        roles: vec!["result".into()],
        operands: vec![
            TypeOperand::External {
                namespace: "std".into(),
                identity: "Vec".into(),
                version: Some(vec![1, 2]),
            },
            TypeOperand::Literal(b"u8".to_vec()),
        ],
        qualifiers: vec!["const".into()],
        literal: Some(b"payload".to_vec()),
        bounds: vec![type_key],
        state: TypeState::Concrete,
    };
    let mut changed_type = type_node.clone();
    changed_type.qualifiers.push("volatile".into());
    assert_value_version_changes::<TypeValueSchema>(&type_node, &changed_type);

    let documentation = DocumentationValue {
        fragments: vec![
            DocFragment::Text(b"docs".to_vec()),
            DocFragment::Code(b"f()".to_vec()),
            DocFragment::Link(DocLink {
                label: b"target".to_vec(),
                target: DocTarget::Foreign {
                    namespace: "rust".into(),
                    identity: "crate::Target".into(),
                    variant: Some(vec![3]),
                },
            }),
            DocFragment::Break,
        ],
    };
    let mut changed_documentation = documentation.clone();
    if let DocFragment::Link(link) = &mut changed_documentation.fragments[2] {
        link.label.push(b'!');
    }
    assert_value_version_changes::<DocumentationValueSchema>(
        &documentation,
        &changed_documentation,
    );
}

#[test]
fn relation_recipe_and_embedding_versions_include_all_inputs() {
    let member = MemberRecord::new(
        MemberKey::new(entity("owner"), entity("member"), 0),
        FacetCoverage::complete(complete(1)),
        provenance(1),
    )
    .expect("member");
    let changed_member = MemberRecord::new(
        MemberKey::new(entity("owner"), entity("member"), 1),
        FacetCoverage::complete(complete(1)),
        provenance(1),
    )
    .expect("member");
    assert_value_version_changes::<MemberValueSchema>(&member, &changed_member);

    let attribute = Attribute {
        name: AttributeKind::Custom,
        value: b"derive".to_vec(),
    };
    let mut changed_attribute = attribute.clone();
    changed_attribute.value.push(b'!');
    assert_value_version_changes::<AttributeValueSchema>(&attribute, &changed_attribute);

    let extension = ExtensionFact {
        entity: entity("owner"),
        language: ExtensionKind::Rust,
        tag: 1,
        payload: b"async".to_vec(),
    };
    let mut changed_extension = extension.clone();
    changed_extension.payload.push(b'!');
    assert_value_version_changes::<ExtensionValueSchema>(&extension, &changed_extension);

    let occurrence = occurrence("owner", 1);
    let mut changed_occurrence = occurrence.clone();
    changed_occurrence.support = 2;
    assert_value_version_changes::<OccurrenceValueSchema>(&occurrence, &changed_occurrence);

    let edge = Edge {
        from: entity("owner"),
        to: entity("target"),
        kind: EdgeKind::Reference,
        multiplicity: 1,
        support: 1,
    };
    let mut changed_edge = edge.clone();
    changed_edge.support = 2;
    assert_value_version_changes::<EdgeValueSchema>(&edge, &changed_edge);

    let component = ComponentDescriptor::new(
        vec![entity("owner"), entity("target")],
        vec![
            ComponentEdge::new(entity("owner"), entity("target"), EdgeKind::TypeDependency),
            ComponentEdge::new(entity("target"), entity("owner"), EdgeKind::TypeDependency),
        ],
        vec![ComponentInput::new(
            entity("owner"),
            FacetValue::new(FacetKind::Type, vec![1]),
        )],
    )
    .expect("component");
    let changed_component = ComponentDescriptor::new(
        vec![entity("owner"), entity("target")],
        vec![
            ComponentEdge::new(entity("owner"), entity("target"), EdgeKind::TypeDependency),
            ComponentEdge::new(entity("target"), entity("owner"), EdgeKind::TypeDependency),
        ],
        vec![ComponentInput::new(
            entity("owner"),
            FacetValue::new(FacetKind::Type, vec![2]),
        )],
    )
    .expect("component");
    assert_value_version_changes::<ComponentSchema>(&component, &changed_component);

    let manifest =
        ReadManifest::new(vec![Read::exact(FacetKind::Name, test_scope(1))]).expect("manifest");
    let recipe = Recipe::EmbeddingInput.typed(1, vec![1], manifest.clone());
    let changed_recipe = Recipe::EmbeddingInput.typed(1, vec![1, 2], manifest.clone());
    assert_value_version_changes::<RecipeSchema>(&recipe, &changed_recipe);

    let model = EmbeddingModel::new("local", "v1", 3, b"cosine".to_vec()).expect("model");
    let changed_model = EmbeddingModel::new("local", "v1", 4, b"cosine".to_vec()).expect("model");
    assert_value_version_changes::<EmbeddingModelSchema>(&model, &changed_model);

    let input_entity_value = EntityValue::new(
        Some("owner".into()),
        EntityKind::Function,
        VisibilityState::Public,
        Parent::Root,
    );
    let input = EmbeddingInputValue::new(
        entity("owner"),
        ObjectVersion::from_value(&input_entity_value),
        ObjectVersion::from_value(&model),
        manifest.clone(),
    );
    let changed_input = EmbeddingInputValue::new(
        entity("owner"),
        ObjectVersion::from_value(&input_entity_value),
        ObjectVersion::from_value(&model),
        ReadManifest::new(vec![Read::range(FacetKind::Name, test_scope(1), 2)])
            .expect("changed manifest"),
    );
    assert_value_version_changes::<EmbeddingInputValueSchema>(&input, &changed_input);
}

#[test]
fn exact_reads_invalidate_positive_negative_and_ranges() {
    let scope = test_scope(7);
    let manifest = ReadManifest::new(vec![
        Read::exact(FacetKind::Name, scope),
        Read::negative(FacetKind::Type, scope),
        Read::range(FacetKind::Documentation, scope, 3),
    ])
    .expect("manifest");
    assert!(manifest.invalidated_by(&[Read::exact(FacetKind::Name, scope)]));
    assert!(manifest.invalidated_by(&[Read::range(FacetKind::Type, scope, 8)]));
    assert!(manifest.invalidated_by(&[Read::exact(FacetKind::Documentation, scope)]));
    assert!(!manifest.invalidated_by(&[Read::exact(FacetKind::Name, test_scope(9))]));
}

#[test]
fn scoped_range_and_prefix_reads_obey_half_open_boundaries() {
    let scope = test_scope(11);
    let range =
        ScopedRead::range(FacetKind::Name, scope, b"a".to_vec(), b"m".to_vec()).expect("range");
    assert!(range.intersects(&ScopedRead::exact(FacetKind::Name, scope, b"a".to_vec(),)));
    assert!(range.intersects(&ScopedRead::exact(FacetKind::Name, scope, b"l".to_vec(),)));
    assert!(!range.intersects(&ScopedRead::exact(FacetKind::Name, scope, b"m".to_vec(),)));
    assert!(!range.intersects(&ScopedRead::exact(FacetKind::Name, scope, b"z".to_vec(),)));

    let prefix = ScopedRead::prefix(FacetKind::Name, scope, b"ab".to_vec());
    assert!(prefix.intersects(&ScopedRead::exact(FacetKind::Name, scope, b"abc".to_vec(),)));
    assert!(!prefix.intersects(&ScopedRead::exact(FacetKind::Name, scope, b"ac".to_vec(),)));
    let max_prefix = ScopedRead::prefix(FacetKind::Name, scope, vec![u8::MAX]);
    assert!(max_prefix.intersects(&ScopedRead::exact(FacetKind::Name, scope, vec![u8::MAX, 0],)));
    assert!(!max_prefix.intersects(&ScopedRead::exact(
        FacetKind::Name,
        scope,
        vec![u8::MAX - 1, u8::MAX],
    )));

    let manifest = ScopedReadManifest::new(vec![
        ScopedReadObservation::positive(
            range,
            ObjectVersion::from_value(&FacetValue::new(FacetKind::Name, vec![1])),
            complete(11),
        )
        .expect("range observation"),
    ])
    .expect("range manifest");
    assert_eq!(manifest.version(), manifest.clone().version());
    let changed = ScopedReadManifest::new(vec![
        ScopedReadObservation::positive(
            ScopedRead::range(FacetKind::Name, scope, b"a".to_vec(), b"n".to_vec()).expect("range"),
            ObjectVersion::from_value(&FacetValue::new(FacetKind::Name, vec![1])),
            complete(11),
        )
        .expect("range observation"),
    ])
    .expect("changed range manifest");
    assert_ne!(manifest.version(), changed.version());
}

#[test]
fn exact_read_versions_include_value_polarity_and_coverage_witnesses() {
    let scope = test_scope(12);
    let positive_read = Read::exact(FacetKind::Name, scope);
    let negative_read = Read::negative(FacetKind::Type, scope);
    let positive = ReadObservation::positive(
        positive_read,
        ObjectVersion::from_value(&FacetValue::new(FacetKind::Name, vec![1])),
        complete(12),
    )
    .expect("positive");
    let negative = ReadObservation::negative(negative_read, complete(12)).expect("negative");
    let first = ExactReadManifest::new(vec![positive, negative]).expect("manifest");
    let same = first.clone();
    assert_eq!(first.version(), same.version());

    let changed_positive = ReadObservation::positive(
        positive_read,
        ObjectVersion::from_value(&FacetValue::new(FacetKind::Name, vec![2])),
        complete(12),
    )
    .expect("positive");
    let changed_value = ExactReadManifest::new(vec![changed_positive, negative]).expect("changed");
    assert_ne!(first.version(), changed_value.version());

    let partial_positive = ReadObservation::witnessed(
        positive_read,
        None,
        CoverageWitness::Partial(UntrustedCoverageScope::from_scope_root(scope)),
    )
    .expect("partial positive");
    let partial = ExactReadManifest::new(vec![partial_positive, negative]).expect("partial");
    assert_ne!(first.version(), partial.version());
    let unavailable = ExactReadManifest::new(vec![
        ReadObservation::witnessed(
            positive_read,
            None,
            CoverageWitness::Unavailable(UntrustedCoverageScope::from_scope_root(scope)),
        )
        .expect("unavailable positive"),
    ])
    .expect("unavailable");
    assert_ne!(partial.version(), unavailable.version());
}

#[test]
fn scoped_reads_preserve_exact_keys_and_full_scope_invalidation() {
    let scope = test_scope(4);
    let negative = ScopedRead::negative(FacetKind::Name, scope, b"absent".to_vec());
    let range =
        ScopedRead::range(FacetKind::Name, scope, b"m".to_vec(), b"z".to_vec()).expect("range");
    let manifest = ScopedReadManifest::new(vec![
        ScopedReadObservation::negative(negative.clone(), complete(4)).expect("negative"),
        ScopedReadObservation::positive(
            range.clone(),
            ObjectVersion::from_value(&FacetValue::new(FacetKind::Name, vec![1])),
            complete(4),
        )
        .expect("range observation"),
    ])
    .expect("manifest");
    assert!(manifest.invalidated_by(&[ScopedRead::exact(
        FacetKind::Name,
        scope,
        b"absent".to_vec(),
    )]));
    assert!(manifest.invalidated_by(&[ScopedRead::exact(
        FacetKind::Name,
        scope,
        b"middle".to_vec(),
    )]));
    assert!(!manifest.invalidated_by(&[ScopedRead::exact(
        FacetKind::Name,
        test_scope(5),
        b"middle".to_vec(),
    )]));
    assert!(!manifest.canonical_bytes().is_empty());
}

#[test]
fn scoped_negative_reads_require_complete_coverage() {
    let scope = test_scope(6);
    let read = ScopedRead::negative(FacetKind::Type, scope, b"T".to_vec());
    let observation = ScopedReadObservation::witnessed(
        read,
        None,
        CoverageWitness::Partial(UntrustedCoverageScope::from_scope_root(scope)),
    )
    .expect("partial negative");
    assert_eq!(
        ScopedReadManifest::new(vec![observation]),
        Err(SemanticError::IncompleteNegativeFact)
    );
}

#[test]
fn authority_scope_versions_track_basis_ranges_and_facet_membership() {
    let first = authority_scope(b"package-a", vec![FacetKind::Name, FacetKind::Type]);
    let same = first.clone();
    let changed_package = authority_scope(b"package-b", vec![FacetKind::Name, FacetKind::Type]);
    let changed_range = AuthorityScope::new(
        provenance(1),
        b"package-a".to_vec(),
        Some((b"b".to_vec(), b"z".to_vec())),
        vec![FacetKind::Name, FacetKind::Type],
    )
    .expect("authority scope");
    let changed_facets = authority_scope(
        b"package-a",
        vec![FacetKind::Name, FacetKind::Type, FacetKind::Documentation],
    );

    assert_eq!(first.version(), same.version());
    assert_ne!(first.version(), changed_package.version());
    assert_ne!(first.version(), changed_range.version());
    assert_ne!(first.version(), changed_facets.version());

    let replacement_scope_root = changed_package.scope_root();
    let basis = ScopedReadManifest::new(vec![
        ScopedReadObservation::negative(
            ScopedRead::negative(FacetKind::Name, replacement_scope_root, b"name".to_vec()),
            complete_for_scope(&changed_package),
        )
        .expect("name basis"),
        ScopedReadObservation::negative(
            ScopedRead::negative(FacetKind::Type, replacement_scope_root, b"type".to_vec()),
            complete_for_scope(&changed_package),
        )
        .expect("type basis"),
    ])
    .expect("replacement basis");
    let replacement = ScopeReplacement::new(
        Some(first.version()),
        changed_package.clone(),
        complete_for_scope(&changed_package),
        basis,
        7,
    )
    .expect("replacement");
    assert_eq!(replacement.coverage().scope_root(), replacement_scope_root);
    assert_eq!(replacement.scope().version(), replacement.scope().version());

    let first_scope_root = first.scope_root();
    let partial_result = AuthorityResult::partial(
        first,
        CoverageWitness::Partial(UntrustedCoverageScope::from_scope_root(first_scope_root)),
        vec![entity("known")],
    )
    .expect("partial result");
    assert!(!partial_result.may_replace_scope());
}

#[test]
fn entity_key_and_entity_value_version_are_distinct() {
    let identity = Entity::new(source(), "x", None).expect("entity");
    let key = entity_key(&identity);
    let value = EntityValue::new(
        Some("x".into()),
        EntityKind::Function,
        VisibilityState::Public,
        Parent::Root,
    );
    let version = ObjectVersion::<EntityValueSchema>::from_value(&value);
    assert_ne!(key.as_bytes(), version.as_bytes());
}

#[test]
fn schema_relations_admit_complete_state_only_with_witness() {
    let admitted = complete(9);
    let key = FacetKey::new(entity("manifest"), FacetKind::Name);
    let provenance = provenance(1);
    let state: RelationState<Facets> = RelationState::from_entries(
        [(key, FacetRecord::captured_empty(key, admitted, provenance))],
        CoverageWitness::Complete(admitted),
    )
    .expect("state");
    assert!(state.coverage().state().is_complete());
}

#[test]
fn equal_values_stop_emission() {
    assert_eq!(suppress_equal(Some(&1), 1), Emission::Unchanged);
}

#[test]
fn positive_reads_bind_polarity_and_full_scope() {
    let scope = test_scope(20);
    let value_version = ObjectVersion::from_value(&FacetValue::new(FacetKind::Name, vec![1]));
    assert_eq!(
        ReadObservation::positive(
            Read::negative(FacetKind::Name, scope),
            value_version,
            complete(20),
        ),
        Err(SemanticError::PositiveReadRequired)
    );
    assert_eq!(
        ReadObservation::positive(
            Read::exact(FacetKind::Name, scope),
            value_version,
            complete(21),
        ),
        Err(SemanticError::ScopeMismatch)
    );

    let scoped = ScopedRead::negative(FacetKind::Name, scope, b"missing".to_vec());
    assert_eq!(
        ScopedReadObservation::positive(scoped, value_version, complete(20)),
        Err(SemanticError::PositiveReadRequired)
    );
}

#[test]
fn checked_records_reject_value_state_and_key_mixes() {
    assert_eq!(
        SourceRecord::new(
            source(),
            None,
            FacetCoverage::complete(complete(1)),
            provenance(1),
        ),
        Err(SemanticError::InvalidCoverageState)
    );
    assert_eq!(
        SourceRecord::new(
            source(),
            Some(SourceValue::new(vec![1], SourceEncoding::Binary, 1).expect("source value")),
            FacetCoverage::captured_empty(complete(1)),
            provenance(1),
        ),
        Err(SemanticError::InvalidCoverageState)
    );

    let key = FacetKey::new(entity("checked"), FacetKind::Name);
    assert_eq!(
        FacetRecord::present(
            key,
            FacetValue::new(FacetKind::Type, vec![1]),
            complete(1),
            provenance(1),
        ),
        Err(SemanticError::InvalidFacetBinding)
    );

    let extension_key = ExtensionKey::new(entity("checked"), ExtensionKind::Rust, 1);
    assert_eq!(
        ExtensionRecord::new(
            extension_key,
            Some(ExtensionFact {
                entity: entity("checked"),
                language: ExtensionKind::Python,
                tag: 1,
                payload: vec![1],
            }),
            FacetCoverage::complete(complete(1)),
            provenance(1),
        ),
        Err(SemanticError::InvalidFacetBinding)
    );
}

#[test]
fn component_descriptor_admission_binds_scc_inputs() {
    let member = entity("component-member");
    let external = entity("component-external");
    assert_eq!(
        ComponentDescriptor::new(Vec::new(), Vec::new(), Vec::new()),
        Err(SemanticError::InvalidComponent)
    );
    assert_eq!(
        ComponentDescriptor::new(
            vec![member, external],
            vec![ComponentEdge::new(member, external, EdgeKind::Reference)],
            Vec::new(),
        ),
        Err(SemanticError::InvalidComponent)
    );
    let input = ComponentInput::new(external, FacetValue::new(FacetKind::Documentation, vec![1]));
    let descriptor = ComponentDescriptor::new(vec![member], Vec::new(), vec![input])
        .expect("component descriptor");
    assert_eq!(
        descriptor.external_inputs()[0].facet(),
        FacetKind::Documentation
    );
}

#[test]
fn replacement_deletion_requires_scope_and_complete_basis() {
    let scope = authority_scope(b"replacement", vec![FacetKind::Name, FacetKind::Type]);
    let root = scope.scope_root();
    let name_read = ScopedRead::negative(FacetKind::Name, root, b"name".to_vec());
    let name_basis = ScopedReadManifest::new(vec![
        ScopedReadObservation::negative(name_read, complete_for_scope(&scope)).expect("name"),
    ])
    .expect("name basis");
    assert_eq!(
        ScopeReplacement::new(None, scope.clone(), complete(1), name_basis.clone(), 1,),
        Err(SemanticError::InvalidReplacement)
    );
    assert_eq!(
        ScopeReplacement::new(
            None,
            scope.clone(),
            complete_for_scope(&scope),
            name_basis,
            1,
        ),
        Err(SemanticError::InvalidReplacement)
    );

    let incomplete_positive = ScopedReadObservation::witnessed(
        ScopedRead::exact(FacetKind::Name, root, b"name".to_vec()),
        Some(ObjectVersion::from_value(&FacetValue::new(
            FacetKind::Name,
            vec![1],
        ))),
        CoverageWitness::Partial(UntrustedCoverageScope::from_scope_root(root)),
    )
    .expect("incomplete positive");
    let incomplete_basis = ScopedReadManifest::new(vec![incomplete_positive]).expect("basis");
    assert_eq!(
        ScopeReplacement::new(
            None,
            scope,
            complete_for_scope(&authority_scope(
                b"replacement",
                vec![FacetKind::Name, FacetKind::Type],
            )),
            incomplete_basis,
            1,
        ),
        Err(SemanticError::InvalidReplacement)
    );
}

#[test]
fn replacement_deletion_checks_expected_scope() {
    let replacement =
        FactSet::new(FacetCoverage::complete(complete(22)), vec![2_i32]).expect("fact set");
    assert_eq!(
        FactSet::deleted_by_replacement_in_scope(&[1, 2], &replacement, test_scope(23)),
        Err(SemanticError::ScopeMismatch)
    );
    assert_eq!(
        FactSet::deleted_by_replacement_in_scope(&[1, 2], &replacement, test_scope(22)),
        Ok(vec![1])
    );
}

#[test]
fn immutable_semantic_values_retain_shared_backing_storage() {
    let value = FacetValue::new(FacetKind::Documentation, vec![7; 4096]);
    assert!(std::sync::Arc::ptr_eq(
        &value.shared_bytes(),
        &value.clone().shared_bytes()
    ));

    let manifest =
        ReadManifest::new(vec![Read::exact(FacetKind::Name, test_scope(1))]).expect("manifest");
    assert!(std::sync::Arc::ptr_eq(
        &manifest.shared_reads(),
        &manifest.clone().shared_reads()
    ));

    let facts =
        FactSet::new(FacetCoverage::complete(complete(8)), vec![1_u32, 3, 5]).expect("fact set");
    assert!(std::sync::Arc::ptr_eq(
        &facts.shared_facts(),
        &facts.clone().shared_facts()
    ));
}

#[test]
fn prefix_range_intersection_matches_implicit_successor() {
    let scope = test_scope(31);
    let prefix = ScopedRead::prefix(FacetKind::Name, scope, vec![0x10, 0xff]);
    let inside = ScopedRead::range(FacetKind::Name, scope, vec![0x10, 0xff, 0x00], vec![0x11])
        .expect("range");
    let outside = ScopedRead::range(FacetKind::Name, scope, vec![0x11], vec![0x12]).expect("range");
    assert!(prefix.intersects(&inside));
    assert!(!prefix.intersects(&outside));

    let unbounded = ScopedRead::prefix(FacetKind::Name, scope, vec![0xff]);
    let tail = ScopedRead::range(FacetKind::Name, scope, vec![0xff, 0x00], vec![0xff, 0x01])
        .expect("range");
    assert!(unbounded.intersects(&tail));
}

#[test]
fn manifests_reject_opposite_polarity_for_overlapping_selectors() {
    let scope = test_scope(41);
    assert_eq!(
        ReadManifest::new(vec![
            Read::range(FacetKind::Name, scope, 7),
            Read::negative(FacetKind::Name, scope),
        ]),
        Err(SemanticError::InvalidSelector)
    );

    let positive =
        ScopedRead::range(FacetKind::Name, scope, b"a".to_vec(), b"m".to_vec()).expect("range");
    let negative = ScopedRead::negative_range(FacetKind::Name, scope, b"h".to_vec(), b"z".to_vec())
        .expect("range");
    let positive_observation = ScopedReadObservation::positive(
        positive,
        ObjectVersion::from_value(&FacetValue::new(FacetKind::Name, vec![1])),
        complete(41),
    )
    .expect("positive");
    let negative_observation =
        ScopedReadObservation::negative(negative, complete(41)).expect("negative");
    assert_eq!(
        ScopedReadManifest::new(vec![positive_observation, negative_observation]),
        Err(SemanticError::InvalidSelector)
    );
}

#[test]
fn facet_record_retains_the_admitted_payload() {
    let key = FacetKey::new(entity("retained"), FacetKind::Documentation);
    let value = FacetValue::new(FacetKind::Documentation, vec![9; 128]);
    let record =
        FacetRecord::present(key, value.clone(), complete(42), provenance(1)).expect("record");
    assert_eq!(record.value().map(FacetValue::bytes), Some(value.bytes()));
    assert!(std::sync::Arc::ptr_eq(
        &record.shared_value().expect("shared value").shared_bytes(),
        &value.shared_bytes()
    ));
}
