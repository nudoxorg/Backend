use crate::native_protocol::{PAYLOAD_HEADER_BYTES, RECORD_HEADER_BYTES, REQUEST_MAGIC};
use crate::{
    AuthorityIdentity, Input, InputKind, InputManifest, NativeCoverage, NativeEnvelope,
    NativeProtocolError, NativeRecord, NativeRecordKind, NativeRequest, NativeRequestInput,
    SessionKey, typed_of,
};
use std::error::Error;

fn key() -> Result<SessionKey, Box<dyn Error>> {
    let authority = authority();
    let manifest = InputManifest::new(vec![Input::new(InputKind::Source, "main", b"source")?])?;
    Ok(SessionKey::new(
        authority,
        &manifest,
        typed_of(b"profile"),
        typed_of(b"flow"),
        typed_of(b"semantic"),
    ))
}

fn authority() -> AuthorityIdentity {
    AuthorityIdentity {
        producer: typed_of(b"producer"),
        toolchain: typed_of(b"toolchain"),
        contract: typed_of(b"contract"),
    }
}

#[test]
fn payload_round_trip_is_canonical() -> Result<(), Box<dyn Error>> {
    let records = vec![
        NativeRecord::new(NativeRecordKind::Type, "T", b"type")?,
        NativeRecord::new(NativeRecordKind::Declaration, "A", b"decl")?,
    ];
    let envelope = NativeEnvelope::unbound(
        "rust",
        [1; 32],
        [2; 32],
        [3; 32],
        1,
        NativeCoverage::Complete,
        records,
    )?;
    let bytes = envelope.encode()?;
    let decoded = NativeEnvelope::decode(&bytes)?;
    assert_eq!(decoded.records()[0].kind(), NativeRecordKind::Declaration);
    assert_eq!(decoded, envelope);
    Ok(())
}

#[test]
fn payload_rejects_duplicate_and_noncanonical_records() -> Result<(), Box<dyn Error>> {
    let duplicate = NativeEnvelope::unbound(
        "rust",
        [1; 32],
        [2; 32],
        [3; 32],
        1,
        NativeCoverage::Partial,
        vec![
            NativeRecord::new(NativeRecordKind::Edge, "x", b"1")?,
            NativeRecord::new(NativeRecordKind::Edge, "x", b"2")?,
        ],
    );
    assert!(matches!(
        duplicate,
        Err(NativeProtocolError::DuplicateRecord { .. })
    ));

    let first = NativeRecord::new(NativeRecordKind::Type, "x", b"1")?;
    let second = NativeRecord::new(NativeRecordKind::Declaration, "x", b"2")?;
    let envelope = NativeEnvelope::unbound(
        "rust",
        [1; 32],
        [2; 32],
        [3; 32],
        1,
        NativeCoverage::Complete,
        vec![first, second],
    )?;
    let mut bytes = envelope.encode()?;
    // The fixed header includes the language length and bytes. Both
    // records have one-byte keys and values, so swapping their kind tags
    // preserves all lengths while making the stream non-canonical.
    let first_header = PAYLOAD_HEADER_BYTES + envelope.language.len();
    let second_header = first_header + RECORD_HEADER_BYTES + 1 + 1;
    bytes.swap(first_header, second_header);
    assert!(NativeEnvelope::decode(&bytes).is_err());
    Ok(())
}

#[test]
fn request_carries_exact_inputs_and_roots() -> Result<(), Box<dyn Error>> {
    let request = NativeRequest::new(
        "rust",
        key()?,
        vec![
            NativeRequestInput::new("source", b"fn main() {}".to_vec())?,
            NativeRequestInput::new("config", b"edition=2024".to_vec())?,
        ],
    )?;
    let bytes = request.encode()?;
    assert!(bytes.starts_with(&REQUEST_MAGIC));
    assert_eq!(request.inputs()[0].name(), "config");
    assert_eq!(request.inputs()[1].bytes(), b"fn main() {}");
    Ok(())
}

#[test]
fn bound_payload_rejects_an_authority_mismatch() -> Result<(), Box<dyn Error>> {
    let key = key()?;
    let other = AuthorityIdentity {
        producer: typed_of(b"other-producer"),
        toolchain: typed_of(b"other-toolchain"),
        contract: typed_of(b"other-contract"),
    };
    assert_eq!(
        NativeEnvelope::bound("rust", key, other, 0, NativeCoverage::Complete, Vec::new(),),
        Err(NativeProtocolError::BindingMismatch)
    );
    Ok(())
}

#[test]
fn decoded_claims_require_exact_binding_before_admission() -> Result<(), Box<dyn Error>> {
    let key = key()?;
    let authority = authority();
    let envelope = NativeEnvelope::bound(
        "rust",
        key,
        authority,
        7,
        NativeCoverage::Partial,
        vec![NativeRecord::new(
            NativeRecordKind::Declaration,
            "main",
            b"fn",
        )?],
    )?;
    let decoded = NativeEnvelope::decode(&envelope.encode()?)?;

    assert_eq!(
        decoded.clone().admit(key, authority, "rust", 7)?.manifest(),
        key.manifest()
    );
    assert!(decoded.clone().admit(key, authority, "rust", 8).is_err());
    assert!(decoded.admit(key, authority, "other", 7).is_err());

    let mut forged_bytes = envelope.encode()?;
    // The first identity starts after magic, version, flags, and count.
    forged_bytes[12] ^= 1;
    let forged = NativeEnvelope::decode(&forged_bytes)?;
    assert!(forged.admit(key, authority, "rust", 7).is_err());
    Ok(())
}
