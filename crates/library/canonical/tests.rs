//! Canonical identity and admission tests.

use super::*;
use crate::{Basis, DeclarationKind, Row, RowId, SourceLocation};
use backend_version::{ID_BYTES, IdContext, ObjectKey, WireId, canonical_empty};

#[test]
fn logical_values_round_trip_through_their_declared_admission() {
    let package = package_key("pkg");
    let symbol = symbol_key("pkg::Thing");
    let object = object_version(b"object");
    let recipe = view_key(b"recipe");
    let version = view_version(b"version");
    let intent = intent_id("add", b"pkg");

    assert_eq!(
        admit_key_value::<PackageSchema>(&encode_id(package.as_bytes()), "pkg").expect("package"),
        package
    );
    assert_eq!(
        admit_key_value::<SymbolSchema>(&encode_id(symbol.as_bytes()), "pkg::Thing")
            .expect("symbol"),
        symbol
    );
    assert_eq!(
        admit_version_value::<ObjectSchema>(&encode_id(object.as_bytes()), b"object")
            .expect("object"),
        object
    );
    assert_eq!(
        admit_key_value::<ViewRecipeSchema>(&encode_id(recipe.as_bytes()), b"recipe")
            .expect("recipe"),
        recipe
    );
    assert_eq!(
        admit_version_value::<ViewVersionSchema>(&encode_id(version.as_bytes()), b"version")
            .expect("version"),
        version
    );
    assert_eq!(
        admit_intent_value(&encode_id(intent.as_bytes()), "add", b"pkg").expect("intent"),
        intent
    );
}

#[test]
fn admission_rejects_malformed_wire_claims_before_typed_use() {
    assert_eq!(decode_id("00"), Err(IdentityError::InvalidHex));
    assert!(wire_key::<PackageSchema>("zzzz").is_err());
    let empty = canonical_empty::<ViewRelation>();
    assert!(
        admit_root_bytes::<ViewRelation>(&encode_id(&[0; ID_BYTES]), empty.as_bytes(),).is_err()
    );
    assert_eq!(
        admit_root_bytes::<ViewRelation>(
            &encode_id(empty.commitment().as_bytes()),
            empty.as_bytes(),
        )
        .expect("empty root"),
        empty.commitment()
    );
}

#[test]
fn admission_rejects_forged_context_and_preimage() {
    let package = package_key("pkg");
    let bytes = package.to_bytes();
    let wrong_context =
        WireId::<PackageSchema>::decode(&bytes, IdContext::schema::<SymbolSchema>())
            .expect("fixed-width claim");
    assert!(
        ObjectKey::<PackageSchema>::admit_value(wrong_context.into_untrusted(), "pkg").is_err()
    );

    let forged = encode_id(&package.to_bytes());
    assert!(admit_key_value::<PackageSchema>(&forged, "other").is_err());
    let object = object_version(b"object");
    assert!(admit_version_value::<ObjectSchema>(&encode_id(object.as_bytes()), b"other").is_err());
}

#[test]
fn intent_identity_changes_with_operation_token_and_complete_payload() {
    let add_pkg = intent_id("add", b"pkg");
    let remove_pkg = intent_id("remove", b"pkg");
    let add_other = intent_id("add", b"other");
    assert_ne!(add_pkg, remove_pkg);
    assert_ne!(add_pkg, add_other);
    assert_ne!(remove_pkg, add_other);
}

#[test]
fn canonical_rows_retain_typed_source_metadata() {
    let basis = Basis::new(
        canonical_empty::<ViewRelation>().commitment(),
        object_version(b"basis"),
    );
    let row = Row::new(RowId::Symbol(symbol_key("pkg::answer")), basis, "answer")
        .with_kind(DeclarationKind::Function)
        .with_source(SourceLocation::new("src/lib.rs", 7).expect("location"));
    let mut encoded = Vec::new();
    encode_row(&row, &mut encoded);
    assert!(
        encoded
            .windows("src/lib.rs".len())
            .any(|window| window == b"src/lib.rs")
    );
    assert!(encoded.contains(&DeclarationKind::Function.wire_tag()));
}
