use core::{
    borrow::Borrow,
    cmp::Ordering,
    fmt,
    hash::{Hash, Hasher},
    marker::PhantomData,
    ops::Deref,
};

use crate::{ARTIFACT_PERSONALIZATION, Domain, Encoding, HASH_BYTES, raw::write_hex};

/// Identity of bytes encoded with `EncodingTag` for logical content in `DomainTag`.
#[repr(transparent)]
pub struct ArtifactId<EncodingTag, DomainTag> {
    bytes: [u8; HASH_BYTES],
    encoding: PhantomData<fn() -> EncodingTag>,
    domain: PhantomData<fn() -> DomainTag>,
}

impl<EncodingTag, DomainTag> From<[u8; HASH_BYTES]> for ArtifactId<EncodingTag, DomainTag> {
    fn from(bytes: [u8; HASH_BYTES]) -> Self {
        Self {
            bytes,
            encoding: PhantomData,
            domain: PhantomData,
        }
    }
}

impl<EncodingTag, DomainTag> TryFrom<&[u8]> for ArtifactId<EncodingTag, DomainTag> {
    type Error = core::array::TryFromSliceError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        Ok(Self::from(<[u8; HASH_BYTES]>::try_from(bytes)?))
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
        ArtifactId::from(*digest.as_bytes())
    }
}

impl<EncodingTag: Encoding, DomainTag: Domain> Default for ArtifactHasher<EncodingTag, DomainTag> {
    fn default() -> Self {
        Self::new()
    }
}

impl<EncodingTag, DomainTag> Copy for ArtifactId<EncodingTag, DomainTag> {}

impl<EncodingTag, DomainTag> Clone for ArtifactId<EncodingTag, DomainTag> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<EncodingTag, DomainTag> Deref for ArtifactId<EncodingTag, DomainTag> {
    type Target = [u8; HASH_BYTES];

    fn deref(&self) -> &Self::Target {
        &self.bytes
    }
}

impl<EncodingTag, DomainTag> AsRef<[u8; HASH_BYTES]> for ArtifactId<EncodingTag, DomainTag> {
    fn as_ref(&self) -> &[u8; HASH_BYTES] {
        self
    }
}

impl<EncodingTag, DomainTag> Borrow<[u8; HASH_BYTES]> for ArtifactId<EncodingTag, DomainTag> {
    fn borrow(&self) -> &[u8; HASH_BYTES] {
        self
    }
}

impl<EncodingTag, DomainTag> PartialEq for ArtifactId<EncodingTag, DomainTag> {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}

impl<EncodingTag, DomainTag> Eq for ArtifactId<EncodingTag, DomainTag> {}

impl<EncodingTag, DomainTag> PartialOrd for ArtifactId<EncodingTag, DomainTag> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<EncodingTag, DomainTag> Ord for ArtifactId<EncodingTag, DomainTag> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.bytes.cmp(&other.bytes)
    }
}

impl<EncodingTag, DomainTag> Hash for ArtifactId<EncodingTag, DomainTag> {
    fn hash<State: Hasher>(&self, state: &mut State) {
        self.bytes.hash(state);
    }
}

impl<EncodingTag, DomainTag> fmt::Display for ArtifactId<EncodingTag, DomainTag> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("artifact:")?;
        write_hex(formatter, &self.bytes)
    }
}

// Keep this manual formatter: complete lower-hex digest output is the useful artifact diagnostic,
// whereas a structural derive would expose phantom markers and decimal byte arrays instead.
impl<EncodingTag, DomainTag> fmt::Debug for ArtifactId<EncodingTag, DomainTag> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ArtifactId(")?;
        write_hex(formatter, &self.bytes)?;
        formatter.write_str(")")
    }
}

#[cfg(test)]
mod tests {
    use core::mem::{align_of, size_of};

    use crate::{ArtifactHasher, ArtifactId, FrameEncoding, ObjectDomain};

    #[test]
    fn artifact_id_is_transparent_and_streaming_is_compatible() {
        assert_eq!(size_of::<ArtifactId<FrameEncoding, ObjectDomain>>(), 32);
        assert_eq!(align_of::<ArtifactId<FrameEncoding, ObjectDomain>>(), 1);
        let expected = ArtifactId::<FrameEncoding, ObjectDomain>::from_encoded_bytes(b"bytes");
        assert_eq!(
            expected,
            ArtifactId::from([
                44, 120, 217, 207, 188, 168, 209, 24, 43, 231, 113, 132, 7, 108, 251, 10, 196, 149,
                134, 208, 90, 172, 97, 131, 186, 1, 222, 60, 69, 222, 110, 247,
            ])
        );
        let mut hasher = ArtifactHasher::<FrameEncoding, ObjectDomain>::new();
        hasher.write_chunk(b"by");
        hasher.write_chunk(b"tes");
        assert_eq!(hasher.finalize(), expected);
    }
}
