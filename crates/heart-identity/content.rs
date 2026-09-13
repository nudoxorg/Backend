//! Domain-separated identities for canonical logical byte streams.
//! A one-byte registered authority precedes 31 digest bytes in the durable representation.
//! Typed fixed records provide copy-free streaming while arbitrary payload hashing stays private.
use core::{
    borrow::Borrow,
    cmp::Ordering,
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
    ops::Deref,
};

use crate::{CONTENT_PERSONALIZATION, Domain, HASH_BYTES, raw::write_hex};

/// Serialized digest payload retained after the one-byte domain authority.
pub const CONTENT_PAYLOAD_BYTES: usize = HASH_BYTES - 1;

/// One exact fixed-width canonical record that can lend its encoded bytes.
///
/// The trait deliberately accepts only a compile-time-sized array borrow. It
/// is the typed streaming seam for declared wire records; arbitrary-length
/// payload hashing remains private to [`ContentId::from_canonical_bytes`].
pub trait FixedCanonicalRecord<const RECORD_BYTES: usize> {
    /// Borrows this complete canonical record without materializing a copy.
    fn canonical_bytes(&self) -> &[u8; RECORD_BYTES];
}

/// BLAKE3 identity of canonical logical bytes in domain `DomainTag`.
///
/// Raw serialized bytes use checked [`TryFrom`]; there is intentionally no infallible
/// `From<[u8; 32]>` conversion.
///
/// ```compile_fail
/// use heart_identity::{ContentId, ObjectDomain};
/// let _: ContentId<ObjectDomain> = [0; 32].into();
/// ```
#[repr(transparent)]
pub struct ContentId<DomainTag> {
    bytes: [u8; HASH_BYTES],
    domain: PhantomData<fn() -> DomainTag>,
}

/// Rejection while decoding a serialized content identity.
#[derive(Debug, thiserror::Error)]
pub enum ContentIdDecodeError {
    /// The input was not exactly one serialized identity; the standard array conversion retains
    /// the structural source and the complete observed width.
    #[error("content identity has {actual} bytes, expected {HASH_BYTES}")]
    Width {
        /// Complete observed input width.
        actual: usize,
        /// Structural array conversion failure.
        #[source]
        source: core::array::TryFromSliceError,
    },
    /// The serialized authority cell does not belong to this typed domain.
    #[error("content identity domain code is {observed}, expected {expected:?}")]
    Domain {
        /// Expected closed registry code.
        expected: crate::DomainCode,
        /// Observed raw registry code.
        observed: u8,
        /// Complete serialized bytes, retained at the exact wire width.
        raw: [u8; HASH_BYTES],
    },
}

impl PartialEq for ContentIdDecodeError {
    /// Compares retained width or domain evidence while ignoring opaque source identity.
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Width { actual, .. }, Self::Width { actual: other, .. }) => actual == other,
            (
                Self::Domain {
                    expected,
                    observed,
                    raw,
                },
                Self::Domain {
                    expected: other_expected,
                    observed: other_observed,
                    raw: other_raw,
                },
            ) => expected == other_expected && observed == other_observed && raw == other_raw,
            _ => false,
        }
    }
}

impl Eq for ContentIdDecodeError {}

/// Compact routing word read from the first eight bytes of a content digest in little-endian order.
///
/// BLAKE3 output is already uniformly distributed, so indexes can consume this word directly
/// without introducing a second hash function or an implementation-defined native-byte order.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContentRoutingWord(u64);

impl<DomainTag> From<ContentId<DomainTag>> for ContentRoutingWord {
    /// Extracts the first eight digest bytes in explicit little-endian order.
    fn from(content: ContentId<DomainTag>) -> Self {
        let [
            _,
            first,
            second,
            third,
            fourth,
            fifth,
            sixth,
            seventh,
            eighth,
            ..,
        ] = content.bytes;
        Self(u64::from_le_bytes([
            first, second, third, fourth, fifth, sixth, seventh, eighth,
        ]))
    }
}

impl From<ContentRoutingWord> for u64 {
    /// Returns the routing scalar without changing its bits.
    fn from(word: ContentRoutingWord) -> Self {
        word.0
    }
}

impl Deref for ContentRoutingWord {
    type Target = u64;

    /// Borrows the routing scalar for direct comparison and arithmetic-free lookup.
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<u64> for ContentRoutingWord {
    /// Borrows the routing scalar for generic lookup APIs.
    fn as_ref(&self) -> &u64 {
        self
    }
}

impl Borrow<u64> for ContentRoutingWord {
    /// Borrows the routing scalar as an ordered-map key.
    fn borrow(&self) -> &u64 {
        self
    }
}

impl<DomainTag: Domain> ContentId<DomainTag> {
    /// Projects a full BLAKE3 digest into the serialized identity, explicitly reserving one
    /// authority byte and retaining the first [`HASH_BYTES`]-1 digest bytes.
    #[must_use]
    pub fn from_digest(digest: [u8; HASH_BYTES]) -> Self {
        let mut payload = [0; CONTENT_PAYLOAD_BYTES];
        payload.copy_from_slice(&digest[..CONTENT_PAYLOAD_BYTES]);
        Self::assemble(payload)
    }

    /// Prepends the registered domain authority to an already-derived digest payload.
    fn assemble(payload: [u8; CONTENT_PAYLOAD_BYTES]) -> Self {
        let mut bytes = [0; HASH_BYTES];
        bytes[0] = u8::from(DomainTag::CODE);
        bytes[1..].copy_from_slice(&payload);
        Self {
            bytes,
            domain: PhantomData,
        }
    }

    /// Validates the serialized domain byte before attaching the compile-time domain marker.
    fn decode(bytes: [u8; HASH_BYTES]) -> Result<Self, ContentIdDecodeError> {
        let expected = u8::from(DomainTag::CODE);
        if bytes[0] != expected {
            return Err(ContentIdDecodeError::Domain {
                expected: DomainTag::CODE,
                observed: bytes[0],
                raw: bytes,
            });
        }
        Ok(Self {
            bytes,
            domain: PhantomData,
        })
    }
}

impl<DomainTag: Domain> ContentId<DomainTag> {
    /// Combines a validated compact-format authority witness with its 31-byte digest payload.
    pub(crate) fn bind_authority(
        _authority: crate::ContentAuthority<DomainTag>,
        payload: [u8; CONTENT_PAYLOAD_BYTES],
    ) -> Self {
        Self::assemble(payload)
    }
}

impl<DomainTag: Domain> TryFrom<[u8; HASH_BYTES]> for ContentId<DomainTag> {
    type Error = ContentIdDecodeError;

    /// Validates an exactly sized serialized identity without another copy.
    fn try_from(bytes: [u8; HASH_BYTES]) -> Result<Self, Self::Error> {
        Self::decode(bytes)
    }
}

impl<DomainTag: Domain> TryFrom<&[u8]> for ContentId<DomainTag> {
    type Error = ContentIdDecodeError;

    /// Copies and validates an arbitrary slice while retaining the exact rejected width.
    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        let raw =
            <[u8; HASH_BYTES]>::try_from(bytes).map_err(|source| ContentIdDecodeError::Width {
                actual: bytes.len(),
                source,
            })?;
        Self::decode(raw)
    }
}

impl<DomainTag: Domain> ContentId<DomainTag> {
    /// Hashes one complete canonical logical byte stream.
    #[must_use]
    pub fn from_canonical_bytes(bytes: &[u8]) -> Self {
        let mut hasher = ContentHasher::<DomainTag>::new();
        hasher.write_payload(bytes);
        hasher.finalize()
    }
}

/// Allocation-free incremental construction of a [`ContentId`].
pub struct ContentHasher<DomainTag: Domain> {
    hasher: blake3::Hasher,
    domain: PhantomData<fn() -> DomainTag>,
}

impl<DomainTag: Domain> ContentHasher<DomainTag> {
    /// Starts one canonical content stream in domain `DomainTag`.
    #[must_use]
    pub fn new() -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(CONTENT_PERSONALIZATION.as_ref());
        hasher.update(DomainTag::TAG.as_ref());
        Self {
            hasher,
            domain: PhantomData,
        }
    }

    /// Test-only compatibility stream for proving chunked bytes preserve the
    /// one-shot digest. Production grammars use declared records.
    #[cfg(test)]
    pub(crate) fn write<const BYTES: usize>(&mut self, bytes: &[u8; BYTES]) {
        self.hasher.update(bytes);
    }

    /// Streams one declared fixed-width canonical record without copying it.
    pub fn write_record<const RECORD_BYTES: usize>(
        &mut self,
        record: &impl FixedCanonicalRecord<RECORD_BYTES>,
    ) {
        self.hasher.update(record.canonical_bytes());
    }

    /// Feeds the one-shot logical payload into the same incremental identity kernel.
    fn write_payload(&mut self, bytes: &[u8]) {
        self.hasher.update(bytes);
    }

    /// Finishes this stream; the builder is consumed and cannot be reused.
    #[must_use]
    pub fn finalize(self) -> ContentId<DomainTag> {
        let digest = self.hasher.finalize();
        ContentId::from_digest(*digest.as_bytes())
    }
}

impl<DomainTag: Domain> Default for ContentHasher<DomainTag> {
    /// Starts a fresh hasher personalized for the requested registered domain.
    fn default() -> Self {
        Self::new()
    }
}

impl<DomainTag> Copy for ContentId<DomainTag> {}

impl<DomainTag> Clone for ContentId<DomainTag> {
    /// Copies the transparent identity bytes and their zero-sized domain marker.
    fn clone(&self) -> Self {
        *self
    }
}

impl<DomainTag> Deref for ContentId<DomainTag> {
    type Target = [u8; HASH_BYTES];

    /// Borrows the complete serialized content identity.
    fn deref(&self) -> &Self::Target {
        &self.bytes
    }
}

impl<DomainTag> AsRef<[u8; HASH_BYTES]> for ContentId<DomainTag> {
    /// Borrows the complete serialized identity for canonical I/O.
    fn as_ref(&self) -> &[u8; HASH_BYTES] {
        self
    }
}

impl<DomainTag> Borrow<[u8; HASH_BYTES]> for ContentId<DomainTag> {
    /// Borrows the serialized identity as an ordered-map lookup key.
    fn borrow(&self) -> &[u8; HASH_BYTES] {
        self
    }
}

impl<DomainTag> PartialEq for ContentId<DomainTag> {
    /// Compares the domain authority and every retained digest byte.
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl<DomainTag> Eq for ContentId<DomainTag> {}

impl<DomainTag> PartialOrd for ContentId<DomainTag> {
    /// Delegates partial ordering to the identity's total byte order.
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<DomainTag> Ord for ContentId<DomainTag> {
    /// Orders identities lexicographically by complete serialized bytes.
    fn cmp(&self, other: &Self) -> Ordering {
        self.bytes.cmp(&other.bytes)
    }
}

impl<DomainTag> Hash for ContentId<DomainTag> {
    /// Feeds the authority byte and every digest byte into the caller's hash state.
    fn hash<State: Hasher>(&self, state: &mut State) {
        self.bytes.hash(state);
    }
}

impl<DomainTag> fmt::Display for ContentId<DomainTag> {
    /// Writes the stable `content:` prefix followed by the complete lower-hex identity.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("content:")?;
        write_hex(formatter, &self.bytes)
    }
}

// Keep this manual formatter: complete lower-hex digest output is the useful identity diagnostic,
// whereas a structural derive would expose the phantom marker and decimal byte array instead.
impl<DomainTag> fmt::Debug for ContentId<DomainTag> {
    /// Writes a compact typed wrapper around the complete lower-hex identity.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ContentId(")?;
        write_hex(formatter, &self.bytes)?;
        formatter.write_str(")")
    }
}

#[cfg(test)]
mod tests {
    use core::{
        borrow::Borrow,
        mem::{align_of, size_of},
    };

    use crate::{ContentHasher, ContentId, ContentRoutingWord, FixedCanonicalRecord, ObjectDomain};

    struct FixedRecord([u8; 3]);

    impl FixedCanonicalRecord<3> for FixedRecord {
        /// Borrows all three canonical fixture bytes without materializing a second record.
        fn canonical_bytes(&self) -> &[u8; 3] {
            &self.0
        }
    }

    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error("a short {length}-byte content identity unexpectedly decoded")]
        ShortAccepted { length: usize },
        #[error("the exact {length}-byte content identity unexpectedly failed to decode")]
        ExactRejected {
            length: usize,
            #[source]
            source: crate::ContentIdDecodeError,
        },
    }

    #[test]
    /// Proves exact layout and rejects both adjacent serialized identity widths.
    fn content_id_has_exact_transparent_layout_and_checked_decoding() -> Result<(), TestError> {
        assert_eq!(size_of::<ContentId<ObjectDomain>>(), 32);
        assert_eq!(align_of::<ContentId<ObjectDomain>>(), 1);
        match ContentId::<ObjectDomain>::try_from(&[0; 31][..]) {
            Err(_) => {}
            Ok(_) => return Err(TestError::ShortAccepted { length: 31 }),
        }
        let mut raw = [0; 32];
        raw[0] = 1;
        let exact = match ContentId::<ObjectDomain>::try_from(&raw[..]) {
            Ok(exact) => exact,
            Err(source) => {
                return Err(TestError::ExactRejected { length: 32, source });
            }
        };
        assert_eq!(exact, ContentId::from_digest([0; 32]));
        let long = [0; 33];
        assert!(matches!(
            ContentId::<ObjectDomain>::try_from(&long[..]),
            Err(crate::ContentIdDecodeError::Width { actual: 33, .. })
        ));
        Ok(())
    }

    #[test]
    /// Proves a domain mismatch retains the expected authority and all observed bytes.
    fn content_decode_retains_authority_mismatch_and_raw_bytes() {
        let raw = [0_u8; 32];
        let result = ContentId::<ObjectDomain>::try_from(raw);
        assert!(matches!(
            result,
            Err(crate::ContentIdDecodeError::Domain { .. })
        ));
        if let Err(crate::ContentIdDecodeError::Domain {
            expected,
            observed,
            raw: retained,
        }) = result
        {
            assert_eq!(expected, crate::DomainCode::Object);
            assert_eq!(observed, 0);
            assert_eq!(retained, raw);
        }
    }

    #[test]
    /// Proves chunk boundaries do not change the canonical logical identity.
    fn streamed_content_is_golden_compatible_with_one_shot_content() {
        let expected = ContentId::<ObjectDomain>::from_canonical_bytes(b"logical bytes");
        assert_eq!(
            expected,
            ContentId::from_digest([
                149, 97, 208, 38, 211, 107, 93, 103, 121, 162, 207, 178, 11, 22, 102, 58, 174, 233,
                69, 255, 245, 214, 252, 76, 129, 65, 132, 133, 73, 255, 25, 18,
            ])
        );
        let mut hasher = ContentHasher::<ObjectDomain>::new();
        hasher.write(b"logical ");
        hasher.write(b"bytes");
        assert_eq!(hasher.finalize(), expected);
    }

    #[test]
    /// Proves fixed-record streaming matches one-shot hashing without copying the record.
    fn typed_fixed_record_streaming_matches_one_shot_content_without_a_record_copy() {
        let record = FixedRecord(*b"abc");
        let mut hasher = ContentHasher::<ObjectDomain>::new();
        hasher.write_record(&record);
        assert_eq!(
            hasher.finalize(),
            ContentId::<ObjectDomain>::from_canonical_bytes(b"abc")
        );
    }

    #[test]
    /// Proves routing extracts an endian-stable digest prefix into exactly one machine word.
    fn routing_word_is_compact_and_uses_a_fixed_little_endian_digest_prefix() {
        let content = ContentId::<ObjectDomain>::from_digest([
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]);
        assert_eq!(size_of::<ContentRoutingWord>(), size_of::<u64>());
        assert_eq!(align_of::<ContentRoutingWord>(), align_of::<u64>());
        assert_eq!(
            u64::from(ContentRoutingWord::from(content)),
            0xefcd_ab89_6745_2301
        );
        let word = ContentRoutingWord::from(content);
        assert_eq!(*word, 0xefcd_ab89_6745_2301);
        assert_eq!(word.as_ref(), &0xefcd_ab89_6745_2301);
        assert_eq!(Borrow::<u64>::borrow(&word), &0xefcd_ab89_6745_2301);
    }
}
