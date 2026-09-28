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

/// Canonical bytes of rows that state no declaration facts, captured before
/// the facts suffix existed. A row that observed nothing must keep exactly
/// these bytes, so no existing view root moves when the carrier is added.
fn no_fact_rows() -> Vec<(&'static str, Row)> {
    let basis = Basis::new(
        canonical_empty::<ViewRelation>().commitment(),
        object_version(b"golden-basis"),
    );
    let package = package_key("/golden");
    let structural = Row::in_package(
        RowId::Symbol(symbol_key("/golden::src/lib.rs:3::answer")),
        basis,
        package,
        "/golden::src/lib.rs:3::answer",
    )
    .with_parent(symbol_key("/golden::src/lib.rs"))
    .with_document(vec![
        crate::Fragment::Text("Answers.".to_owned()),
        crate::Fragment::Break,
        crate::Fragment::Code("let a = answer();".to_owned()),
        crate::Fragment::Link {
            label: "Question".to_owned(),
            target: symbol_key("/golden::src/lib.rs:9::Question"),
        },
    ])
    .with_signature("pub fn answer() -> u8")
    .with_kind(DeclarationKind::Function)
    .with_source(SourceLocation::new("src/lib.rs", 3).expect("location"))
    .with_excerpt(
        crate::SourceExcerpt::captured(
            "pub fn answer() -> u8 {\n    42\n}",
            crate::SourceExcerptExtent::Complete,
        )
        .expect("excerpt"),
    );
    let mut semantic = Row::in_package(
        RowId::Symbol(symbol_key("/golden\0semantic\0answer")),
        basis,
        package,
        "/golden::semantic::0123::answer",
    )
    .try_with_identity_preimage("/golden\0semantic\0answer")
    .expect("preimage")
    .with_document(Vec::<crate::Fragment>::new())
    .with_signature("function(parameters=[],result=builtin(u8))")
    .with_kind(DeclarationKind::Function);
    semantic.score = Some(7);
    let bare = Row::new(
        RowId::Object(object_version(b"golden-object")),
        basis,
        "bare",
    );
    vec![
        ("structural", structural),
        ("semantic", semantic),
        ("bare", bare),
    ]
}

#[test]
fn rows_that_state_no_facts_keep_their_canonical_bytes() {
    let expected = [
        (
            "structural",
            "a869e2ace4b99ab6304f5bbaa12b4fc0201d59478d6fa16b7d52adcc8a445374",
        ),
        (
            "semantic",
            "8d5e2ab93640ba4262b100653ea878556f2f758e158015830c26fe4dbc38dab8",
        ),
        (
            "bare",
            "eec1d4cb338c73c69aba95b8cbbb043d160c0b40c40c2e88cc809dfa2437109a",
        ),
    ];
    let mut actual = Vec::new();
    for (name, row) in no_fact_rows() {
        let mut encoded = Vec::new();
        encode_row(&row, &mut encoded);
        actual.push((name, blake3::hash(&encoded).to_hex().to_string()));
    }
    let actual: Vec<(&str, &str)> = actual
        .iter()
        .map(|(name, digest)| (*name, digest.as_str()))
        .collect();
    assert_eq!(actual, expected);
}

fn stated_facts() -> crate::DeclarationFacts {
    crate::DeclarationFacts {
        deprecation: crate::Fact::Present(crate::Deprecation::new(
            Some("1.2.0"),
            Some("use `fresh`"),
        )),
        obligation: crate::Fact::Present(crate::Obligation::Required),
    }
}

#[test]
fn stated_facts_are_an_append_only_suffix_that_commits_their_text() {
    for (name, row) in no_fact_rows() {
        let mut historical = Vec::new();
        encode_row(&row, &mut historical);
        let mut stated = Vec::new();
        encode_row(&row.clone().with_facts(stated_facts()), &mut stated);
        assert_eq!(
            stated.get(..historical.len()),
            Some(historical.as_slice()),
            "{name}"
        );
        let suffix = &stated[historical.len()..];
        assert_eq!(
            suffix.first(),
            Some(&2),
            "{name}: the facts tag follows every field"
        );
        for text in [&b"1.2.0"[..], b"use `fresh`"] {
            assert!(
                suffix.windows(text.len()).any(|window| window == text),
                "{name}: {} is committed",
                String::from_utf8_lossy(text)
            );
        }
    }
    // Absent and unobserved are different statements with different bytes,
    // and so is a different note.
    let (_, row) = no_fact_rows().remove(0);
    let encode = |facts: crate::DeclarationFacts| {
        let mut out = Vec::new();
        encode_row(&row.clone().with_facts(facts), &mut out);
        out
    };
    let absent = encode(crate::DeclarationFacts {
        deprecation: crate::Fact::Absent,
        obligation: crate::Fact::Unobserved,
    });
    let unobserved = encode(crate::DeclarationFacts::UNOBSERVED);
    let other_note = encode(crate::DeclarationFacts {
        deprecation: crate::Fact::Present(crate::Deprecation::new(
            Some("1.2.0"),
            Some("use `new`"),
        )),
        obligation: crate::Fact::Present(crate::Obligation::Required),
    });
    assert_ne!(absent, unobserved);
    assert_ne!(other_note, encode(stated_facts()));
}
