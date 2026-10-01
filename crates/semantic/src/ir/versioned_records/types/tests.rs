use super::wire::{atom_key, external_identity_from_payload, put_bytes, typed_row_key};
use super::*;
use crate::ir::SemanticPlaneRecordError;

#[test]
fn empty_types_catalog_rejects_missing_extension_roots() {
    let catalog = CheckedTypesFamilyV2::from_records(core::iter::empty::<([u8; 32], u8, &[u8])>())
        .expect("an explicit empty Types family is valid");
    let missing_parameters = TypesReferenceV2 {
        domain: TypesRowDomainV2::TypeParameters,
        key: [0x31; 32],
    };
    let missing_free_predicates = TypesReferenceV2 {
        domain: TypesRowDomainV2::FreePredicates,
        key: [0x42; 32],
    };
    assert!(catalog.verify_reachable_closure(&[]).is_ok());

    assert!(matches!(
        catalog.require_reference(missing_parameters),
        Err(SemanticPlaneRecordError::ReaderReference)
    ));
    assert!(matches!(
        catalog.require_reference(missing_free_predicates),
        Err(SemanticPlaneRecordError::ReaderReference)
    ));
}

#[test]
fn explicit_payload_limit_still_reports_row_too_large() {
    let identity = [0x34; 32];
    let mut payload = Vec::new();
    payload.extend_from_slice(&identity);
    payload.push(0);
    payload.extend_from_slice(&[0; 32]);
    let limits = TypesFamilyVerificationLimitsV2::bounded(64, 1, 1);

    let result = CheckedTypesFamilyV2::from_records_with_limits(
        [(identity, ROOT_TAG, payload.as_slice())],
        limits,
    );

    assert!(matches!(result, Err(SemanticPlaneRecordError::RowTooLarge)));
}

#[test]
fn catalog_exposes_each_entity_root_type_presence_bit() {
    let missing_identity = [0x11; 32];
    let present_identity = [0x22; 32];
    let mut type_payload = Vec::new();
    type_payload.extend_from_slice(&2_u32.to_be_bytes());
    for (role, value) in [(0_u8, 1_u64), (1, 0)] {
        type_payload.push(role);
        type_payload.extend_from_slice(&0_u32.to_be_bytes());
        type_payload.push(4); // scalar
        type_payload.extend_from_slice(&[0; 24]);
        type_payload.extend_from_slice(&value.to_be_bytes());
    }
    let type_key = typed_row_key(TYPE_TAG, &type_payload);
    let mut missing_payload = Vec::new();
    missing_payload.extend_from_slice(&missing_identity);
    missing_payload.push(0);
    missing_payload.extend_from_slice(&[0; 32]);
    let mut present_payload = Vec::new();
    present_payload.extend_from_slice(&present_identity);
    present_payload.push(1);
    present_payload.extend_from_slice(&type_key);
    let mut records = vec![
        (missing_identity, ROOT_TAG, &missing_payload[..]),
        (present_identity, ROOT_TAG, &present_payload[..]),
        (type_key, TYPE_TAG, &type_payload[..]),
    ];
    records.sort_unstable_by_key(|(key, _, _)| *key);

    let catalog = CheckedTypesFamilyV2::from_records(records)
        .expect("both root associations and the referenced type are complete");
    let expected = [(missing_identity, false), (present_identity, true)];
    assert_eq!(catalog.root_type_presence(), expected.as_slice());
    assert!(catalog.verify_reachable_closure(&[]).is_ok());
}

#[test]
fn reachable_closure_rejects_orphan_rows_and_accepts_extension_seeds() {
    let mut type_payload = Vec::new();
    type_payload.extend_from_slice(&2_u32.to_be_bytes());
    for (role, value) in [(0_u8, 1_u64), (1, 0)] {
        type_payload.push(role);
        type_payload.extend_from_slice(&0_u32.to_be_bytes());
        type_payload.push(4); // scalar
        type_payload.extend_from_slice(&[0; 24]);
        type_payload.extend_from_slice(&value.to_be_bytes());
    }
    let type_key = typed_row_key(TYPE_TAG, &type_payload);
    let catalog = CheckedTypesFamilyV2::from_records([(type_key, TYPE_TAG, &type_payload[..])])
        .expect("the row is locally valid");

    assert!(matches!(
        catalog.verify_reachable_closure(&[]),
        Err(SemanticPlaneRecordError::ReaderReference)
    ));
    let extension_roots = [TypesReferenceV2 {
        domain: TypesRowDomainV2::Type,
        key: type_key,
    }];
    assert!(catalog.verify_reachable_closure(&extension_roots).is_ok());
}

#[test]
fn extension_type_parameter_and_free_predicate_refs_keep_their_domains() {
    let payload = [0_u8, 0, 0, 0];
    let key = typed_row_key(TYPE_PARAMETERS_TAG, &payload);
    let catalog = CheckedTypesFamilyV2::from_records([(key, TYPE_PARAMETERS_TAG, &payload[..])])
        .expect("an explicit empty parameter list is a complete row");

    assert!(
        catalog
            .require_reference(TypesReferenceV2 {
                domain: TypesRowDomainV2::TypeParameters,
                key,
            })
            .is_ok()
    );
    assert!(matches!(
        catalog.require_reference(TypesReferenceV2 {
            domain: TypesRowDomainV2::FreePredicates,
            key,
        }),
        Err(SemanticPlaneRecordError::ReaderReference)
    ));
}

#[test]
fn typed_child_references_require_the_exact_domain() {
    let empty_tuple = [0_u8, 0, 0, 0];
    let tuple_key = typed_row_key(TUPLE_ELEMENTS_TAG, &empty_tuple);
    let mut list_payload = Vec::new();
    list_payload.extend_from_slice(&1_u32.to_be_bytes());
    list_payload.push(2);
    list_payload.extend_from_slice(&0_u32.to_be_bytes());
    list_payload.push(0);
    list_payload.extend_from_slice(&tuple_key);
    let list_key = typed_row_key(TYPE_LIST_TAG, &list_payload);
    let mut records = vec![
        (tuple_key, TUPLE_ELEMENTS_TAG, &empty_tuple[..]),
        (list_key, TYPE_LIST_TAG, &list_payload[..]),
    ];
    records.sort_unstable_by_key(|(key, _, _)| *key);

    assert!(matches!(
        CheckedTypesFamilyV2::from_records(records),
        Err(SemanticPlaneRecordError::ReaderReference)
    ));
}

#[test]
fn fragment_external_identity_commits_its_kind_and_atom_rows() {
    let display = b"remote declaration";
    let mut payload = Vec::new();
    payload.push(2);
    payload.extend_from_slice(&[0x31; 32]);
    payload.extend_from_slice(&7_u32.to_be_bytes());
    put_bytes(&mut payload, display).expect("short test atom fits u32");

    let mut expected = blake3::Hasher::new();
    expected.update(b"compiler-ir.external-target.v1\0");
    expected.update(&[2]);
    expected.update(&[0x31; 32]);
    expected.update(&7_u32.to_be_bytes());
    expected.update(&(display.len() as u64).to_be_bytes());
    expected.update(display);
    let expected_key = *expected.finalize().as_bytes();
    let (actual_key, references) =
        external_identity_from_payload(&payload).expect("valid fragment target");
    assert_eq!(actual_key, expected_key);
    let expected_references = [TypesReferenceV2 {
        domain: TypesRowDomainV2::Atom,
        key: atom_key(display).expect("short test atom fits u32"),
    }];
    assert_eq!(references.as_slice(), expected_references.as_slice());

    let mut atom_payload = Vec::new();
    put_bytes(&mut atom_payload, display).expect("short test atom fits u32");
    let atom_row_key = atom_key(display).expect("short test atom fits u32");
    let mut records = vec![
        (expected_key, EXTERNAL_TARGET_TAG, &payload[..]),
        (atom_row_key, ATOM_TAG, &atom_payload[..]),
    ];
    records.sort_unstable_by_key(|(key, _, _)| *key);
    let catalog = CheckedTypesFamilyV2::from_records(records)
        .expect("external endpoint atom must be present in the closure");
    assert_eq!(catalog.external_target_keys(), &[expected_key]);
    assert!(catalog.verify_reachable_closure(&[]).is_ok());
}

#[test]
fn stable_external_identity_commits_its_kind() {
    let fragment = [0x61; 32];
    let declaration = [0x72; 32];
    let mut payload = Vec::new();
    payload.push(0);
    payload.extend_from_slice(&fragment);
    payload.extend_from_slice(&declaration);

    let mut expected = blake3::Hasher::new();
    expected.update(b"compiler-ir.external-target.v1\0");
    expected.update(&[0]);
    expected.update(&fragment);
    expected.update(&declaration);
    assert_eq!(
        external_identity_from_payload(&payload)
            .expect("valid stable target")
            .0,
        *expected.finalize().as_bytes()
    );
}

#[test]
fn foreign_external_identity_commits_origin_and_all_atom_rows() {
    let atoms: [&[u8]; 3] = [b"ecosystem", b"pkg/path", b"display"];
    let mut payload = Vec::new();
    payload.push(1);
    payload.extend_from_slice(&[0x51; 16]);
    payload.push(0); // variant unavailable
    payload.push(3); // unspecified origin
    for atom in atoms {
        put_bytes(&mut payload, atom).expect("short test atom fits u32");
    }
    payload.push(0); // item kind unavailable

    let mut expected = blake3::Hasher::new();
    expected.update(b"compiler-ir.external-target.v1\0");
    expected.update(&[1]);
    expected.update(&[0x51; 16]);
    expected.update(&[0]);
    expected.update(&[3]);
    for atom in atoms {
        expected.update(&(atom.len() as u64).to_be_bytes());
        expected.update(atom);
    }
    expected.update(&[0]);
    let expected_key = *expected.finalize().as_bytes();
    let (actual_key, references) =
        external_identity_from_payload(&payload).expect("valid foreign target");
    assert_eq!(actual_key, expected_key);
    let expected_references = atoms.map(|bytes| TypesReferenceV2 {
        domain: TypesRowDomainV2::Atom,
        key: atom_key(bytes).expect("short test atom fits u32"),
    });
    assert_eq!(references.as_slice(), expected_references.as_slice());
}
