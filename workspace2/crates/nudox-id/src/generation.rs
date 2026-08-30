//! Generation-root identity aliases over the common content identity kernel.

use crate::{ContentHasher, ContentId, RootDomain};

/// BLAKE3 identity of one canonical immutable published generation root.
///
/// This semantic alias deliberately uses the `RootDomain` content preimage. It therefore cannot
/// drift from the one-shot and incremental content identity implementation.
pub type GenerationId = ContentId<RootDomain>;

/// Allocation-free incremental construction of a [`GenerationId`].
pub type GenerationHasher = ContentHasher<RootDomain>;

#[cfg(test)]
mod tests {
    use core::mem::{align_of, size_of};

    use crate::{ContentId, GenerationHasher, GenerationId, HASH_BYTES, RootDomain};

    #[test]
    fn generation_is_the_root_domain_content_identity_without_a_second_kernel() {
        assert_eq!(size_of::<GenerationId>(), HASH_BYTES);
        assert_eq!(align_of::<GenerationId>(), 1);
        assert_eq!(
            GenerationId::from_canonical_bytes(b"canonical root"),
            ContentId::<RootDomain>::from_canonical_bytes(b"canonical root")
        );
        assert_eq!(
            GenerationId::from_canonical_bytes(b"canonical root"),
            GenerationId::from_digest([
                183, 115, 129, 71, 108, 4, 131, 43, 47, 18, 229, 162, 52, 75, 153, 206, 231, 42,
                24, 117, 209, 249, 184, 205, 170, 200, 198, 233, 71, 130, 23, 136,
            ])
        );
        let mut hasher = GenerationHasher::new();
        hasher.write(b"canonical");
        hasher.write(b" root");
        assert_eq!(
            hasher.finalize(),
            GenerationId::from_canonical_bytes(b"canonical root")
        );
    }
}
