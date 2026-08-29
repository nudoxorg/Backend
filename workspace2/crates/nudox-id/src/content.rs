use core::{
    borrow::Borrow,
    cmp::Ordering,
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
    ops::Deref,
};

use crate::{CONTENT_PERSONALIZATION, Domain, HASH_BYTES, raw::write_hex};

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
#[repr(transparent)]
pub struct ContentId<DomainTag> {
    bytes: [u8; HASH_BYTES],
    domain: PhantomData<fn() -> DomainTag>,
}

/// Compact routing word read from the first eight bytes of a content digest in little-endian order.
///
/// BLAKE3 output is already uniformly distributed, so indexes can consume this word directly
/// without introducing a second hash function or an implementation-defined native-byte order.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ContentRoutingWord(u64);

impl<DomainTag> From<ContentId<DomainTag>> for ContentRoutingWord {
    fn from(content: ContentId<DomainTag>) -> Self {
        let [
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
    fn from(word: ContentRoutingWord) -> Self {
        word.0
    }
}

impl Deref for ContentRoutingWord {
    type Target = u64;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<u64> for ContentRoutingWord {
    fn as_ref(&self) -> &u64 {
        self
    }
}

impl Borrow<u64> for ContentRoutingWord {
    fn borrow(&self) -> &u64 {
        self
    }
}

impl<DomainTag> From<[u8; HASH_BYTES]> for ContentId<DomainTag> {
    fn from(bytes: [u8; HASH_BYTES]) -> Self {
        Self {
            bytes,
            domain: PhantomData,
        }
    }
}

impl<DomainTag> TryFrom<&[u8]> for ContentId<DomainTag> {
    type Error = core::array::TryFromSliceError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        Ok(Self::from(<[u8; HASH_BYTES]>::try_from(bytes)?))
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

    fn write_payload(&mut self, bytes: &[u8]) {
        self.hasher.update(bytes);
    }

    /// Finishes this stream; the builder is consumed and cannot be reused.
    #[must_use]
    pub fn finalize(self) -> ContentId<DomainTag> {
        let digest = self.hasher.finalize();
        ContentId::from(*digest.as_bytes())
    }
}

impl<DomainTag: Domain> Default for ContentHasher<DomainTag> {
    fn default() -> Self {
        Self::new()
    }
}

impl<DomainTag> Copy for ContentId<DomainTag> {}

impl<DomainTag> Clone for ContentId<DomainTag> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<DomainTag> Deref for ContentId<DomainTag> {
    type Target = [u8; HASH_BYTES];

    fn deref(&self) -> &Self::Target {
        &self.bytes
    }
}

impl<DomainTag> AsRef<[u8; HASH_BYTES]> for ContentId<DomainTag> {
    fn as_ref(&self) -> &[u8; HASH_BYTES] {
        self
    }
}

impl<DomainTag> Borrow<[u8; HASH_BYTES]> for ContentId<DomainTag> {
    fn borrow(&self) -> &[u8; HASH_BYTES] {
        self
    }
}

impl<DomainTag> PartialEq for ContentId<DomainTag> {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl<DomainTag> Eq for ContentId<DomainTag> {}

impl<DomainTag> PartialOrd for ContentId<DomainTag> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<DomainTag> Ord for ContentId<DomainTag> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.bytes.cmp(&other.bytes)
    }
}

impl<DomainTag> Hash for ContentId<DomainTag> {
    fn hash<State: Hasher>(&self, state: &mut State) {
        self.bytes.hash(state);
    }
}

impl<DomainTag> fmt::Display for ContentId<DomainTag> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("content:")?;
        write_hex(formatter, &self.bytes)
    }
}

// Keep this manual formatter: complete lower-hex digest output is the useful identity diagnostic,
// whereas a structural derive would expose the phantom marker and decimal byte array instead.
impl<DomainTag> fmt::Debug for ContentId<DomainTag> {
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
            source: core::array::TryFromSliceError,
        },
    }

    #[test]
    fn content_id_has_exact_transparent_layout_and_checked_decoding() -> Result<(), TestError> {
        assert_eq!(size_of::<ContentId<ObjectDomain>>(), 32);
        assert_eq!(align_of::<ContentId<ObjectDomain>>(), 1);
        match ContentId::<ObjectDomain>::try_from(&[0; 31][..]) {
            Err(_) => {}
            Ok(_) => return Err(TestError::ShortAccepted { length: 31 }),
        }
        let exact = match ContentId::<ObjectDomain>::try_from(&[0; 32][..]) {
            Ok(exact) => exact,
            Err(source) => {
                return Err(TestError::ExactRejected { length: 32, source });
            }
        };
        assert_eq!(exact, ContentId::from([0; 32]));
        Ok(())
    }

    #[test]
    fn streamed_content_is_golden_compatible_with_one_shot_content() {
        let expected = ContentId::<ObjectDomain>::from_canonical_bytes(b"logical bytes");
        assert_eq!(
            expected,
            ContentId::from([
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
    fn routing_word_is_compact_and_uses_a_fixed_little_endian_digest_prefix() {
        let content = ContentId::<ObjectDomain>::from([
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
