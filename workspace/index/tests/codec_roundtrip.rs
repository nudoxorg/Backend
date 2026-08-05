//! Codec property tests (adversarial): every enum and id round-trips, and an
//! unknown TEXT token is a typed error — never a panic.

use index::enums::{
    AliasConfidence, CompileCacheKind, EdgeKind, EdgeSource, IrStatus, LineageEvidence,
    LineageRelation, ListingStatus, LocationStatus, OutboxOperation, ParseState, SinkKind,
    SourceKind, StoreKind, TextEnum,
};
use index::ids::{GenerationStamp, IdDecodeError, ObjectPackHash, PackageStemId, StoreId};

/// Assert every variant of a `TextEnum` round-trips token → variant → token,
/// and that an unknown token is a typed error.
fn assert_text_enum_roundtrips<E: TextEnum + PartialEq + std::fmt::Debug>() {
    for variant in E::all_variants() {
        let token = variant.as_token();
        let parsed = E::from_token(token).expect("known token must parse");
        assert_eq!(&parsed, variant, "round-trip must recover the variant");
    }
    // An adversarial unknown token is a typed error, not a panic.
    let error = E::from_token("\u{0000}definitely-not-a-variant");
    assert!(error.is_err(), "unknown token must be a typed error");
}

#[test]
fn every_text_enum_roundtrips_and_rejects_unknown() {
    assert_text_enum_roundtrips::<ParseState>();
    assert_text_enum_roundtrips::<IrStatus>();
    assert_text_enum_roundtrips::<SourceKind>();
    assert_text_enum_roundtrips::<ListingStatus>();
    assert_text_enum_roundtrips::<StoreKind>();
    assert_text_enum_roundtrips::<LocationStatus>();
    assert_text_enum_roundtrips::<SinkKind>();
    assert_text_enum_roundtrips::<OutboxOperation>();
    assert_text_enum_roundtrips::<EdgeKind>();
    assert_text_enum_roundtrips::<EdgeSource>();
    assert_text_enum_roundtrips::<AliasConfidence>();
    assert_text_enum_roundtrips::<LineageRelation>();
    assert_text_enum_roundtrips::<LineageEvidence>();
    assert_text_enum_roundtrips::<CompileCacheKind>();
}

#[test]
fn blob16_ids_roundtrip() {
    for seed in 0u8..=32 {
        let mut bytes = [0u8; 16];
        bytes[0] = seed;
        bytes[15] = seed.wrapping_mul(3);
        let id = PackageStemId::from_uuid(uuid::Uuid::from_bytes(bytes));
        let blob = id.to_blob();
        let recovered = PackageStemId::from_blob(&blob).expect("16-byte blob decodes");
        assert_eq!(id, recovered);

        let store = StoreId::from_uuid(uuid::Uuid::from_bytes(bytes));
        assert_eq!(store, StoreId::from_blob(&store.to_blob()).unwrap());
    }
}

#[test]
fn blob32_ids_roundtrip() {
    for seed in 0u8..=32 {
        let mut bytes = [0u8; 32];
        bytes[0] = seed;
        bytes[31] = seed ^ 0xAB;
        let stamp = GenerationStamp::from_bytes(bytes);
        assert_eq!(stamp, GenerationStamp::from_blob(&stamp.to_blob()).unwrap());

        let pack = ObjectPackHash::from_bytes(bytes);
        assert_eq!(pack, ObjectPackHash::from_blob(&pack.to_blob()).unwrap());
    }
}

#[test]
fn wrong_length_blob_is_typed_error_not_panic() {
    let too_short = [0u8; 8];
    assert!(matches!(
        PackageStemId::from_blob(&too_short),
        Err(IdDecodeError::WrongLength {
            expected: 16,
            actual: 8
        })
    ));
    let too_long = [0u8; 40];
    assert!(matches!(
        GenerationStamp::from_blob(&too_long),
        Err(IdDecodeError::WrongLength {
            expected: 32,
            actual: 40
        })
    ));
}

#[test]
fn tokens_are_distinct_within_each_enum() {
    // A duplicated token would silently break decoding; guard it.
    fn all_distinct<E: TextEnum>() {
        let tokens: Vec<&str> = E::all_variants().iter().map(|v| v.as_token()).collect();
        let mut sorted = tokens.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), tokens.len(), "duplicate token in enum");
    }
    all_distinct::<EdgeKind>();
    all_distinct::<SinkKind>();
    all_distinct::<ListingStatus>();
}
