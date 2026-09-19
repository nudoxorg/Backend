//! Fact and mixed-dependency reuse tests.

use super::*;

#[test]
fn dependency_facts_are_version_bound_and_reject_self_validation() {
    let manifest = ReadManifest::new(Vec::new()).expect("manifest");
    let recipe_value = Recipe::Names.typed(1, vec![1], manifest);
    let recipe_root = ObjectVersion::<RecipeSchema>::from_value(&recipe_value);
    let dependency = RecipeDependencyFact::new(recipe_root, recipe_root);
    assert_eq!(dependency, Err(SemanticError::DependencyCycle));

    let first = recipe(1);
    let second = recipe(2);
    let facts = vec![
        DependencyFact::recipe(RecipeDependencyFact::new(first, second).expect("edge")),
        DependencyFact::recipe(RecipeDependencyFact::new(second, first).expect("edge")),
    ];
    assert_eq!(
        DependencyManifest::new(facts),
        Err(SemanticError::DependencyCycle)
    );

    let mut readers = RetainedReaders::default();
    let first_fact =
        DependencyFact::recipe(RecipeDependencyFact::new(first, second).expect("edge"));
    let second_fact =
        DependencyFact::recipe(RecipeDependencyFact::new(second, first).expect("edge"));
    readers
        .register_fact(work(47), &first_fact)
        .expect("first retained edge");
    assert_eq!(
        readers.register_fact(work(47), &second_fact),
        Err(SemanticError::DependencyCycle)
    );
    assert_eq!(readers.dependency_graph().edge_count(), 1);
    readers.unregister_reader(work(47));
    assert_eq!(readers.dependency_graph().edge_count(), 0);
}

#[test]
fn mixed_dependency_changes_use_recipe_and_authority_reverse_arrangements() {
    let authority = authority_scope(6);
    let recipe_root = recipe(6);
    let observation = ScopedReadObservation::negative(
        ScopedRead::negative(FacetKind::Name, authority.scope_root(), b"missing".to_vec()),
        complete_for_scope(&authority),
    )
    .expect("negative");
    let read_fact = DependencyFact::read(
        ReadDependencyFact::witnessed(recipe_root, observation).expect("fact"),
    );
    let authority_fact = DependencyFact::authority(
        AuthorityDependencyFact::new(recipe_root, &authority, complete_for_scope(&authority))
            .expect("fact"),
    );
    let mut index = RetainedReaders::default();
    index.register_fact(work(20), &read_fact).expect("register");
    index
        .register_fact(work(21), &authority_fact)
        .expect("register");
    let report = index
        .invalidate_changes(&[
            DependencyChange::Recipe(recipe_root),
            DependencyChange::Authority(authority.authority_version()),
        ])
        .expect("invalidate");
    let readers = report.readers.into_iter().collect::<BTreeSet<_>>();
    assert_eq!(readers, BTreeSet::from([work(20), work(21)]));
}

#[test]
fn wrong_producer_cannot_enter_authority_invalidation_arrangement() {
    let authority = authority_scope(67);
    let claim = AuthorityScopeClaim::from_object_version(authority.version());
    let rejected = admit_producer_observation(
        UntrustedProducerObservation::new(
            [2; 32],
            claim.scope_root(),
            *claim.scope_root().as_bytes(),
            authority.canonical_bytes(),
        ),
        &FixtureProducerVerifier {
            expected_identity: [1; 32],
        },
    );
    assert!(
        rejected.is_err(),
        "wrong producer must be rejected at admission"
    );

    let recipe_root = recipe(67);
    let first_fact = DependencyFact::authority(
        AuthorityDependencyFact::new(recipe_root, &authority, authorized_for_scope(&authority, 1))
            .expect("valid admitted authority fact"),
    );
    let second_fact = DependencyFact::authority(
        AuthorityDependencyFact::new(recipe_root, &authority, authorized_for_scope(&authority, 2))
            .expect("valid second producer fact"),
    );
    let mut index = RetainedReaders::default();
    index
        .register_fact(work(67), &first_fact)
        .expect("register first producer");
    index
        .register_fact(work(67), &second_fact)
        .expect("retain second producer");
    assert_eq!(index.relation_record_count(), 2);
    let report = index
        .invalidate_changes(&[DependencyChange::Authority(authority.authority_version())])
        .expect("invalidate");
    assert_eq!(report.readers, vec![work(67)]);
}

#[test]
fn dependency_manifest_normalizes_selector_polarity_before_retention() {
    let authority = authority_scope(7);
    let scope = authority.scope_root();
    let read = ScopedRead::exact(FacetKind::Name, scope, b"same".to_vec());
    let observed = ObjectVersion::from_value(&FacetValue::new(FacetKind::Name, vec![7]));
    let positive = ReadDependencyFact::positive(
        recipe(7),
        read.clone(),
        observed,
        complete_for_scope(&authority),
    )
    .expect("positive fact");
    let negative = ReadDependencyFact::negative(
        recipe(8),
        ScopedRead::negative(FacetKind::Name, scope, b"same".to_vec()),
        complete_for_scope(&authority),
    )
    .expect("negative fact");
    assert_eq!(
        DependencyManifest::new(vec![
            DependencyFact::read(positive),
            DependencyFact::read(negative),
        ]),
        Err(SemanticError::InvalidSelector)
    );
}
