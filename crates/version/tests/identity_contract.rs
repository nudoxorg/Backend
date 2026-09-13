//! Public wire, authority, routing, and streaming proofs for typed identities.
//! Literal goldens pin every durable byte while exhaustive authority attacks retain rejected input.
//! Split-stream tests prove callers can hash borrowed ranges without changing identity semantics.

use std::error::Error as _;

use backend_version::{
    ArtifactHasher, ArtifactId, ArtifactIdDecodeError, ContentHasher, ContentId,
    ContentIdDecodeError, ContentRoutingWord, DomainCode, EncodingCode, FixedCanonicalRecord,
    FrameEncoding, ObjectDomain,
};
use thiserror::Error;

const CONTENT_WIRE_GOLDEN: [u8; 32] = [
    1, 149, 97, 208, 38, 211, 107, 93, 103, 121, 162, 207, 178, 11, 22, 102, 58, 174, 233, 69, 255,
    245, 214, 252, 76, 129, 65, 132, 133, 73, 255, 25,
];
const ARTIFACT_WIRE_GOLDEN: [u8; 32] = [
    1, 1, 44, 120, 217, 207, 188, 168, 209, 24, 43, 231, 113, 132, 7, 108, 251, 10, 196, 149, 134,
    208, 90, 172, 97, 131, 186, 1, 222, 60, 69, 222,
];

#[derive(Debug, Error)]
enum ContractError {
    #[error("a {kind} identity unexpectedly decoded from {actual} bytes")]
    WidthAccepted { kind: &'static str, actual: usize },
    #[error("content width rejection had the wrong shape")]
    ContentWidth {
        #[source]
        observed: ContentIdDecodeError,
    },
    #[error("artifact width rejection had the wrong shape")]
    ArtifactWidth {
        #[source]
        observed: ArtifactIdDecodeError,
    },
}

struct Record<const RECORD_BYTES: usize>([u8; RECORD_BYTES]);

impl<const RECORD_BYTES: usize> FixedCanonicalRecord<RECORD_BYTES> for Record<RECORD_BYTES> {
    /// Borrows the fixture's complete compile-time-sized canonical record.
    fn canonical_bytes(&self) -> &[u8; RECORD_BYTES] {
        &self.0
    }
}

/// Hashes two fixed records and requires their concatenation to preserve the expected identity.
fn assert_content_split<const LEFT_BYTES: usize, const RIGHT_BYTES: usize>(
    left: [u8; LEFT_BYTES],
    right: [u8; RIGHT_BYTES],
    expected: ContentId<ObjectDomain>,
) {
    let mut hasher = ContentHasher::<ObjectDomain>::new();
    hasher.write_record(&Record(left));
    hasher.write_record(&Record(right));
    assert_eq!(hasher.finalize(), expected);
}

/// Requires content decoding to reject the supplied width and returns its complete typed cause.
fn content_width_error(bytes: &[u8]) -> Result<ContentIdDecodeError, ContractError> {
    match ContentId::<ObjectDomain>::try_from(bytes) {
        Err(observed) => Ok(observed),
        Ok(_) => Err(ContractError::WidthAccepted {
            kind: "content",
            actual: bytes.len(),
        }),
    }
}

/// Requires artifact decoding to reject the supplied width and returns its complete typed cause.
fn artifact_width_error(bytes: &[u8]) -> Result<ArtifactIdDecodeError, ContractError> {
    match ArtifactId::<FrameEncoding, ObjectDomain>::try_from(bytes) {
        Err(observed) => Ok(observed),
        Ok(_) => Err(ContractError::WidthAccepted {
            kind: "artifact",
            actual: bytes.len(),
        }),
    }
}

#[test]
/// Pins literal content and artifact bytes, including their registered authority cells.
fn literal_wire_goldens_pin_authority_and_digest_cells() {
    assert_eq!(
        ContentId::<ObjectDomain>::from_canonical_bytes(b"logical bytes").as_ref(),
        &CONTENT_WIRE_GOLDEN
    );
    assert_eq!(
        ArtifactId::<FrameEncoding, ObjectDomain>::from_encoded_bytes(b"bytes").as_ref(),
        &ARTIFACT_WIRE_GOLDEN
    );
}

#[test]
/// Proves both adjacent widths preserve the standard array-conversion source error.
fn every_adjacent_width_retains_its_structural_source() -> Result<(), ContractError> {
    for bytes in [&[0; 31][..], &[0; 33][..]] {
        let observed = content_width_error(bytes)?;
        assert!(observed.source().is_some());
        match observed {
            ContentIdDecodeError::Width { actual, source: _ } if actual == bytes.len() => {}
            observed => return Err(ContractError::ContentWidth { observed }),
        }

        let observed = artifact_width_error(bytes)?;
        assert!(observed.source().is_some());
        match observed {
            ArtifactIdDecodeError::Width { actual, source: _ } if actual == bytes.len() => {}
            observed => return Err(ContractError::ArtifactWidth { observed }),
        }
    }
    Ok(())
}

#[test]
/// Attacks every non-object domain byte and requires full-wire retention on rejection.
fn every_wrong_content_authority_retains_the_complete_wire() {
    for observed in u8::MIN..=u8::MAX {
        if observed == u8::from(DomainCode::Object) {
            continue;
        }
        let mut raw = CONTENT_WIRE_GOLDEN;
        raw[0] = observed;
        assert_eq!(
            ContentId::<ObjectDomain>::try_from(raw),
            Err(ContentIdDecodeError::Domain {
                expected: DomainCode::Object,
                observed,
                raw,
            })
        );
    }
}

#[test]
/// Attacks every invalid encoding-domain pair and requires both observed cells plus raw bytes.
fn every_wrong_artifact_authority_pair_retains_the_complete_wire() {
    let expected_encoding = u8::from(EncodingCode::Frame);
    let expected_domain = u8::from(DomainCode::Object);
    for observed_encoding in u8::MIN..=u8::MAX {
        for observed_domain in u8::MIN..=u8::MAX {
            if (observed_encoding, observed_domain) == (expected_encoding, expected_domain) {
                continue;
            }
            let mut raw = ARTIFACT_WIRE_GOLDEN;
            raw[0] = observed_encoding;
            raw[1] = observed_domain;
            assert_eq!(
                ArtifactId::<FrameEncoding, ObjectDomain>::try_from(raw),
                Err(ArtifactIdDecodeError::Authority {
                    expected_encoding: EncodingCode::Frame,
                    expected_domain: DomainCode::Object,
                    observed_encoding,
                    observed_domain,
                    raw,
                })
            );
        }
    }
}

#[test]
/// Proves every fixed-record split, including empty edges, hashes as one logical stream.
fn all_content_record_splits_and_empty_records_preserve_identity() {
    let expected = ContentId::<ObjectDomain>::from_canonical_bytes(b"wire");
    assert_content_split([], *b"wire", expected);
    assert_content_split(*b"w", *b"ire", expected);
    assert_content_split(*b"wi", *b"re", expected);
    assert_content_split(*b"wir", *b"e", expected);
    assert_content_split(*b"wire", [], expected);
}

#[test]
/// Proves every artifact chunk split, including empty chunks, hashes as one encoded stream.
fn all_artifact_splits_and_empty_chunks_preserve_identity() {
    let bytes = b"streamed artifact";
    let expected = ArtifactId::<FrameEncoding, ObjectDomain>::from_encoded_bytes(bytes);
    for split in 0..=bytes.len() {
        let (left, right) = bytes.split_at(split);
        let mut hasher = ArtifactHasher::<FrameEncoding, ObjectDomain>::new();
        hasher.write_chunk(&[]);
        hasher.write_chunk(left);
        hasher.write_chunk(right);
        hasher.write_chunk(&[]);
        assert_eq!(hasher.finalize(), expected);
    }
}

#[test]
/// Proves routing ignores the fixed authority byte and consumes eight distributed digest bytes.
fn routing_uses_digest_entropy_instead_of_the_authority_cell() {
    let content = ContentId::<ObjectDomain>::from_digest([
        0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ]);
    assert_ne!(content.as_ref()[0], content.as_ref()[1]);
    assert_eq!(
        u64::from(ContentRoutingWord::from(content)),
        0x10ef_cdab_8967_4523
    );
}
