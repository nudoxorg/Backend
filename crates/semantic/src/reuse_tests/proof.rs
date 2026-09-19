//! Exact reuse proof tests.

use super::*;

#[test]
fn reuse_proof_checks_recipe_input_manifest_and_authority_roots() {
    let authority = authority_scope(5);
    let scope_root = authority.scope_root();
    let reads = ScopedReadManifest::new(vec![
        ScopedReadObservation::positive(
            ScopedRead::exact(FacetKind::Name, scope_root, b"name".to_vec()),
            ObjectVersion::from_value(&FacetValue::new(FacetKind::Name, vec![1])),
            complete_for_scope(&authority),
        )
        .expect("read"),
    ])
    .expect("reads");
    let request = ReuseRequest::<InputRelation>::new(
        recipe(1),
        canonical_empty::<InputRelation>().commitment(),
        &reads,
        &authority,
        complete_for_scope(&authority),
    )
    .expect("request");
    let output = canonical_empty::<InputRelation>().commitment();
    let candidate = ReusableOutput::new(request, output);
    let proof = check_reuse(&request, &candidate).expect("proof");
    assert!(proof.matches(&request, output));

    let other_reads = ScopedReadManifest::new(vec![
        ScopedReadObservation::positive(
            ScopedRead::exact(FacetKind::Name, scope_root, b"name".to_vec()),
            ObjectVersion::from_value(&FacetValue::new(FacetKind::Name, vec![2])),
            complete_for_scope(&authority),
        )
        .expect("read"),
    ])
    .expect("reads");
    let other = ReuseRequest::<InputRelation>::new(
        recipe(1),
        output,
        &other_reads,
        &authority,
        complete_for_scope(&authority),
    )
    .expect("request");
    assert_eq!(
        check_reuse(&other, &candidate),
        Err(SemanticError::ReuseMismatch)
    );
}

#[test]
fn reuse_rejects_a_same_scope_result_from_the_wrong_producer() {
    let authority = authority_scope(66);
    let scope = authority.scope_root();
    let first_coverage = authorized_for_scope(&authority, 1);
    let second_coverage = authorized_for_scope(&authority, 2);
    assert_ne!(
        first_coverage.producer_identity(),
        second_coverage.producer_identity()
    );
    let reads = ScopedReadManifest::new(vec![
        ScopedReadObservation::positive(
            ScopedRead::exact(FacetKind::Name, scope, b"name".to_vec()),
            ObjectVersion::from_value(&FacetValue::new(FacetKind::Name, vec![1])),
            first_coverage,
        )
        .expect("read"),
    ])
    .expect("reads");
    let first_request = ReuseRequest::<InputRelation>::new(
        recipe(1),
        canonical_empty::<InputRelation>().commitment(),
        &reads,
        &authority,
        first_coverage,
    )
    .expect("first request");
    let second_reads = ScopedReadManifest::new(vec![
        ScopedReadObservation::positive(
            ScopedRead::exact(FacetKind::Name, scope, b"name".to_vec()),
            ObjectVersion::from_value(&FacetValue::new(FacetKind::Name, vec![1])),
            second_coverage,
        )
        .expect("read"),
    ])
    .expect("reads");
    let second_request = ReuseRequest::<InputRelation>::new(
        recipe(1),
        canonical_empty::<InputRelation>().commitment(),
        &second_reads,
        &authority,
        second_coverage,
    )
    .expect("second request");
    assert_ne!(first_request.version(), second_request.version());
    let candidate = ReusableOutput::new(
        first_request,
        canonical_empty::<InputRelation>().commitment(),
    );
    assert_eq!(
        check_reuse(&second_request, &candidate),
        Err(SemanticError::ReuseMismatch)
    );
}
