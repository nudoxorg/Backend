//! Typed identities for physical encodings of logical content.
//! Two authority bytes bind each digest to both its encoding and its semantic content domain.
//! Incremental hashing accepts borrowed chunks so transport boundaries never require staging.
use core::{
    borrow::Borrow,
    cmp::Ordering,
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
    ops::Deref,
};

use crate::identity::{ARTIFACT_PERSONALIZATION, Domain, Encoding, HASH_BYTES, raw::write_hex};

const AUTHORITY_BYTES: usize = 2;
const DIGEST_BYTES: usize = HASH_BYTES - AUTHORITY_BYTES;

/// Identity of bytes encoded with `EncodingTag` for logical content in `DomainTag`.
///
/// Raw serialized bytes use checked [`TryFrom`]; there is intentionally no infallible
/// `From<[u8; 32]>` conversion.
///
/// ```compile_fail
/// use backend_version::{ArtifactId, FrameEncoding, ObjectDomain};
/// let _: ArtifactId<FrameEncoding, ObjectDomain> = [0; 32].into();
/// ```
#[repr(transparent)]
pub struct ArtifactId<EncodingTag, DomainTag> {
    bytes: [u8; HASH_BYTES],
    encoding: PhantomData<fn() -> EncodingTag>,
    domain: PhantomData<fn() -> DomainTag>,
}

/// Rejection while decoding a serialized artifact identity.
#[derive(Debug, thiserror::Error)]
pub enum ArtifactIdDecodeError {
    /// The input was not exactly one serialized identity; the standard array conversion retains
    /// the structural source and the complete observed width.
    #[error("artifact identity has {actual} bytes, expected {HASH_BYTES}")]
    Width {
        /// Complete observed input width.
        actual: usize,
        /// Structural array conversion failure.
        #[source]
        source: core::array::TryFromSliceError,
    },
    /// The serialized encoding or domain authority cell does not belong to this typed identity.
    #[error(
        "artifact identity authority is ({observed_encoding}, {observed_domain}), expected ({expected_encoding:?}, {expected_domain:?})"
    )]
    Authority {
        /// Expected closed encoding registry code.
        expected_encoding: crate::identity::EncodingCode,
        /// Expected closed domain registry code.
        expected_domain: crate::identity::DomainCode,
        /// Observed raw encoding registry code.
        observed_encoding: u8,
        /// Observed raw domain registry code.
        observed_domain: u8,
        /// Complete serialized bytes, retained at the exact wire width.
        raw: [u8; HASH_BYTES],
    },
}

impl PartialEq for ArtifactIdDecodeError {
    /// Compares the retained structural or authority evidence while ignoring opaque source identity.
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Width { actual, .. }, Self::Width { actual: other, .. }) => actual == other,
            (
                Self::Authority {
                    expected_encoding,
                    expected_domain,
                    observed_encoding,
                    observed_domain,
                    raw,
                },
                Self::Authority {
                    expected_encoding: other_expected_encoding,
                    expected_domain: other_expected_domain,
                    observed_encoding: other_observed_encoding,
                    observed_domain: other_observed_domain,
                    raw: other_raw,
                },
            ) => {
                expected_encoding == other_expected_encoding
                    && expected_domain == other_expected_domain
                    && observed_encoding == other_observed_encoding
                    && observed_domain == other_observed_domain
                    && raw == other_raw
            }
            _ => false,
        }
    }
}

impl Eq for ArtifactIdDecodeError {}

impl<EncodingTag: Encoding, DomainTag: Domain> ArtifactId<EncodingTag, DomainTag> {
    /// Projects a full BLAKE3 digest into the serialized identity, explicitly reserving encoding
    /// and domain authority bytes and retaining the first [`HASH_BYTES`]-2 digest bytes.
    #[must_use]
    pub fn from_digest(digest: [u8; HASH_BYTES]) -> Self {
        let mut bytes = [0; HASH_BYTES];
        bytes[0] = u8::from(EncodingTag::CODE);
        bytes[1] = u8::from(DomainTag::CODE);
        bytes[AUTHORITY_BYTES..].copy_from_slice(&digest[..DIGEST_BYTES]);
        Self {
            bytes,
            encoding: PhantomData,
            domain: PhantomData,
        }
    }

    /// Validates both embedded authority bytes before attaching compile-time marker types.
    fn decode(bytes: [u8; HASH_BYTES]) -> Result<Self, ArtifactIdDecodeError> {
        let expected_encoding = u8::from(EncodingTag::CODE);
        let expected_domain = u8::from(DomainTag::CODE);
        if bytes[0] != expected_encoding || bytes[1] != expected_domain {
            return Err(ArtifactIdDecodeError::Authority {
                expected_encoding: EncodingTag::CODE,
                expected_domain: DomainTag::CODE,
                observed_encoding: bytes[0],
                observed_domain: bytes[1],
                raw: bytes,
            });
        }
        Ok(Self {
            bytes,
            encoding: PhantomData,
            domain: PhantomData,
        })
    }
}

impl<EncodingTag: Encoding, DomainTag: Domain> TryFrom<[u8; HASH_BYTES]>
    for ArtifactId<EncodingTag, DomainTag>
{
    type Error = ArtifactIdDecodeError;

    /// Validates an exactly sized serialized identity without another copy.
    fn try_from(bytes: [u8; HASH_BYTES]) -> Result<Self, Self::Error> {
        Self::decode(bytes)
    }
}

impl<EncodingTag: Encoding, DomainTag: Domain> TryFrom<&[u8]>
    for ArtifactId<EncodingTag, DomainTag>
{
    type Error = ArtifactIdDecodeError;

    /// Copies and validates an arbitrary slice while retaining the exact rejected width.
    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        let raw =
            <[u8; HASH_BYTES]>::try_from(bytes).map_err(|source| ArtifactIdDecodeError::Width {
                actual: bytes.len(),
                source,
            })?;
        Self::decode(raw)
    }
}

impl<EncodingTag: Encoding, DomainTag: Domain> ArtifactId<EncodingTag, DomainTag> {
    /// Hashes one complete encoded byte stream.
    #[must_use]
    pub fn from_encoded_bytes(bytes: &[u8]) -> Self {
        let mut hasher = ArtifactHasher::<EncodingTag, DomainTag>::new();
        hasher.write_chunk(bytes);
        hasher.finalize()
    }
}

/// Allocation-free incremental construction of an [`ArtifactId`].
pub struct ArtifactHasher<EncodingTag: Encoding, DomainTag: Domain> {
    hasher: blake3::Hasher,
    encoding: PhantomData<fn() -> EncodingTag>,
    domain: PhantomData<fn() -> DomainTag>,
}

impl<EncodingTag: Encoding, DomainTag: Domain> ArtifactHasher<EncodingTag, DomainTag> {
    /// Starts one encoded stream under fixed encoding and content-domain labels.
    #[must_use]
    pub fn new() -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(ARTIFACT_PERSONALIZATION.as_ref());
        hasher.update(EncodingTag::TAG.as_ref());
        hasher.update(DomainTag::TAG.as_ref());
        Self {
            hasher,
            encoding: PhantomData,
            domain: PhantomData,
        }
    }

    /// Extends the physical artifact identity with one borrowed transport or storage chunk.
    ///
    /// Chunk boundaries are deliberately not part of the identity. Callers can therefore hash a
    /// mapped local artifact, an object-store range stream, or a network body without staging the
    /// complete encoded object.
    pub fn write_chunk(&mut self, bytes: &[u8]) {
        self.hasher.update(bytes);
    }

    /// Finishes this stream; the builder is consumed and cannot be reused.
    #[must_use]
    pub fn finalize(self) -> ArtifactId<EncodingTag, DomainTag> {
        let digest = self.hasher.finalize();
        ArtifactId::from_digest(*digest.as_bytes())
    }
}

impl<EncodingTag: Encoding, DomainTag: Domain> Default for ArtifactHasher<EncodingTag, DomainTag> {
    /// Starts a fresh hasher with the registered encoding and domain personalization.
    fn default() -> Self {
        Self::new()
    }
}

impl<EncodingTag, DomainTag> Copy for ArtifactId<EncodingTag, DomainTag> {}

impl<EncodingTag, DomainTag> Clone for ArtifactId<EncodingTag, DomainTag> {
    /// Copies the transparent identity bytes and their zero-sized type markers.
    fn clone(&self) -> Self {
        *self
    }
}

impl<EncodingTag, DomainTag> Deref for ArtifactId<EncodingTag, DomainTag> {
    type Target = [u8; HASH_BYTES];

    /// Borrows the complete serialized artifact identity.
    fn deref(&self) -> &Self::Target {
        &self.bytes
    }
}

impl<EncodingTag, DomainTag> AsRef<[u8; HASH_BYTES]> for ArtifactId<EncodingTag, DomainTag> {
    /// Borrows the complete serialized identity for canonical I/O.
    fn as_ref(&self) -> &[u8; HASH_BYTES] {
        self
    }
}

impl<EncodingTag, DomainTag> Borrow<[u8; HASH_BYTES]> for ArtifactId<EncodingTag, DomainTag> {
    /// Borrows the serialized identity as an ordered-map lookup key.
    fn borrow(&self) -> &[u8; HASH_BYTES] {
        self
    }
}

impl<EncodingTag, DomainTag> PartialEq for ArtifactId<EncodingTag, DomainTag> {
    /// Compares all authority and digest bytes.
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl<EncodingTag, DomainTag> Eq for ArtifactId<EncodingTag, DomainTag> {}

impl<EncodingTag, DomainTag> PartialOrd for ArtifactId<EncodingTag, DomainTag> {
    /// Delegates partial ordering to the identity's total byte order.
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<EncodingTag, DomainTag> Ord for ArtifactId<EncodingTag, DomainTag> {
    /// Orders identities lexicographically by their complete serialized bytes.
    fn cmp(&self, other: &Self) -> Ordering {
        self.bytes.cmp(&other.bytes)
    }
}

impl<EncodingTag, DomainTag> Hash for ArtifactId<EncodingTag, DomainTag> {
    /// Feeds every authority and digest byte into the caller's hash state.
    fn hash<State: Hasher>(&self, state: &mut State) {
        self.bytes.hash(state);
    }
}

impl<EncodingTag, DomainTag> fmt::Display for ArtifactId<EncodingTag, DomainTag> {
    /// Writes the stable `artifact:` prefix followed by the complete lower-hex identity.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("artifact:")?;
        write_hex(formatter, &self.bytes)
    }
}

// Keep this manual formatter: complete lower-hex digest output is the useful artifact diagnostic,
// whereas a structural derive would expose phantom markers and decimal byte arrays instead.
impl<EncodingTag, DomainTag> fmt::Debug for ArtifactId<EncodingTag, DomainTag> {
    /// Writes a compact typed wrapper around the complete lower-hex identity.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ArtifactId(")?;
        write_hex(formatter, &self.bytes)?;
        formatter.write_str(")")
    }
}

#[cfg(test)]
mod tests {
    use core::mem::{align_of, size_of};

    use crate::identity::{
        ArtifactHasher, ArtifactId, ArtifactIdDecodeError, DomainCode, EncodingCode, FrameEncoding,
        ObjectDomain,
    };

    #[test]
    /// Proves transparent layout and equality between one-shot and arbitrarily chunked hashing.
    fn artifact_id_is_transparent_and_streaming_is_compatible() {
        assert_eq!(size_of::<ArtifactId<FrameEncoding, ObjectDomain>>(), 32);
        assert_eq!(align_of::<ArtifactId<FrameEncoding, ObjectDomain>>(), 1);
        let expected = ArtifactId::<FrameEncoding, ObjectDomain>::from_encoded_bytes(b"bytes");
        assert_eq!(
            expected,
            ArtifactId::from_digest([
                44, 120, 217, 207, 188, 168, 209, 24, 43, 231, 113, 132, 7, 108, 251, 10, 196, 149,
                134, 208, 90, 172, 97, 131, 186, 1, 222, 60, 69, 222, 110, 247,
            ])
        );
        let mut hasher = ArtifactHasher::<FrameEncoding, ObjectDomain>::new();
        hasher.write_chunk(b"by");
        hasher.write_chunk(b"tes");
        assert_eq!(hasher.finalize(), expected);
    }

    #[test]
    /// Proves a rejected identity retains both expected authorities and all observed bytes.
    fn artifact_decode_retains_both_authorities_and_raw_bytes() {
        let raw = [0_u8; 32];
        let result = ArtifactId::<FrameEncoding, ObjectDomain>::try_from(raw);
        assert!(matches!(
            result,
            Err(ArtifactIdDecodeError::Authority { .. })
        ));
        if let Err(ArtifactIdDecodeError::Authority {
            expected_encoding,
            expected_domain,
            observed_encoding,
            observed_domain,
            raw: retained,
        }) = result
        {
            assert_eq!(expected_encoding, EncodingCode::Frame);
            assert_eq!(expected_domain, DomainCode::Object);
            assert_eq!(observed_encoding, 0);
            assert_eq!(observed_domain, 0);
            assert_eq!(retained, raw);
        }
    }

    #[test]
    /// Rejects both adjacent serialized widths rather than truncating or padding the input.
    fn artifact_decode_rejects_adjacent_widths() {
        let short = [0_u8; 31];
        let long = [0_u8; 33];
        assert!(matches!(
            ArtifactId::<FrameEncoding, ObjectDomain>::try_from(&short[..]),
            Err(ArtifactIdDecodeError::Width { actual: 31, .. })
        ));
        assert!(matches!(
            ArtifactId::<FrameEncoding, ObjectDomain>::try_from(&long[..]),
            Err(ArtifactIdDecodeError::Width { actual: 33, .. })
        ));
    }
}
