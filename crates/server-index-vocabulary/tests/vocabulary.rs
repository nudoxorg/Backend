//! Exercises the `server-index-vocabulary` tests vocabulary contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Public laws for the minimal immutable-index identity vocabulary.

use allocation_counter::{AllocationInfo, measure};
use compiler_ir::{DeclarationIdentity, PackageLineage, SemanticImageIdentity};
use core::{
    mem::{align_of, size_of},
    ops::Deref,
};
use backend_version::{
    CompilePublicationDomain, ContentId, ContentIdDecodeError, DomainCode, GenerationId,
};
use server_index_vocabulary::{
    CanonicalEntityLocator, ExactSegmentId, IndexLocatorFacts, IndexSnapshotId, LexicalSegmentId,
    PackageVersion, SemanticImageExtent, SemanticImageLocator, VectorSegmentId,
};
use std::hint::black_box;

const CANONICAL_BYTES: usize = 24;
const LOCAL_BYTE: u8 = 7;
const MUTATED_BYTE: u8 = 8;
const MUTATED_POSITION: usize = CANONICAL_BYTES - 1;
const WARMUP_BYTE: u8 = 3;

#[test]
fn local_and_remote_bytes_produce_the_same_typed_ids() {
    let local = [LOCAL_BYTE; CANONICAL_BYTES];
    let remote = [LOCAL_BYTE; CANONICAL_BYTES];
    assert_ne!(local.as_ptr(), remote.as_ptr());
    let mut different = local;
    different[MUTATED_POSITION] = MUTATED_BYTE;
    let local_snapshot = IndexSnapshotId::from_canonical_bytes(&local);
    let local_exact = ExactSegmentId::from_canonical_bytes(&local);
    let local_lexical = LexicalSegmentId::from_canonical_bytes(&local);
    let local_vector = VectorSegmentId::from_canonical_bytes(&local);

    assert_eq!(
        local_snapshot,
        IndexSnapshotId::from_canonical_bytes(&remote)
    );
    assert_eq!(local_exact, ExactSegmentId::from_canonical_bytes(&remote));
    assert_eq!(
        local_lexical,
        LexicalSegmentId::from_canonical_bytes(&remote)
    );
    assert_ne!(local_exact.as_ref(), local_lexical.as_ref());
    assert_ne!(local_vector.as_ref(), local_lexical.as_ref());
    assert_ne!(
        local_snapshot,
        IndexSnapshotId::from_canonical_bytes(&different)
    );
    assert_ne!(
        local_exact,
        ExactSegmentId::from_canonical_bytes(&different)
    );
    assert_ne!(
        local_lexical,
        LexicalSegmentId::from_canonical_bytes(&different)
    );
    assert_ne!(
        local_vector,
        VectorSegmentId::from_canonical_bytes(&different)
    );
}

#[test]
fn lexical_wire_bytes_cannot_be_rebranded_as_an_exact_segment() {
    let lexical = LexicalSegmentId::from_canonical_bytes(b"segment");
    let raw = *lexical.as_ref();
    assert_eq!(
        ExactSegmentId::try_from(raw),
        Err(ContentIdDecodeError::Domain {
            expected: DomainCode::IndexExactSegment,
            observed: u8::from(DomainCode::IndexLexicalSegment),
            raw,
        })
    );
}

#[test]
fn ids_retain_the_content_digest_layout() {
    type SnapshotDigest = <IndexSnapshotId as Deref>::Target;
    type ExactDigest = <ExactSegmentId as Deref>::Target;
    type LexicalDigest = <LexicalSegmentId as Deref>::Target;
    type VectorDigest = <VectorSegmentId as Deref>::Target;
    assert_eq!(size_of::<IndexSnapshotId>(), size_of::<SnapshotDigest>());
    assert_eq!(align_of::<IndexSnapshotId>(), align_of::<SnapshotDigest>());
    assert_eq!(size_of::<ExactSegmentId>(), size_of::<ExactDigest>());
    assert_eq!(align_of::<ExactSegmentId>(), align_of::<ExactDigest>());
    assert_eq!(size_of::<LexicalSegmentId>(), size_of::<LexicalDigest>());
    assert_eq!(align_of::<LexicalSegmentId>(), align_of::<LexicalDigest>());
    assert_eq!(size_of::<VectorSegmentId>(), size_of::<VectorDigest>());
    assert_eq!(align_of::<VectorSegmentId>(), align_of::<VectorDigest>());
}

#[test]
fn canonical_construction_allocates_nothing_after_warmup() {
    let bytes = [WARMUP_BYTE; CANONICAL_BYTES];
    black_box(IndexSnapshotId::from_canonical_bytes(&bytes));
    black_box(ExactSegmentId::from_canonical_bytes(&bytes));
    black_box(LexicalSegmentId::from_canonical_bytes(&bytes));
    black_box(VectorSegmentId::from_canonical_bytes(&bytes));
    let allocations = measure(|| {
        black_box(IndexSnapshotId::from_canonical_bytes(&bytes));
        black_box(ExactSegmentId::from_canonical_bytes(&bytes));
        black_box(LexicalSegmentId::from_canonical_bytes(&bytes));
        black_box(VectorSegmentId::from_canonical_bytes(&bytes));
    });
    assert_eq!(allocations, AllocationInfo::default());
}

#[test]
fn typed_locator_vocabulary_retains_borrowed_facts() {
    assert!(PackageVersion::new("").is_err());
    let version = PackageVersion::new("1.2.3").expect("nonempty version");
    let lineage = PackageLineage::new("cargo", "serde").expect("valid lineage");
    let coordinate = server_index_vocabulary::PackageCoordinate::new(lineage, version);
    assert_eq!(coordinate.version.as_str(), "1.2.3");
    let identity = SemanticImageIdentity::from_encoded_bytes(b"canonical-image");
    let extent = SemanticImageExtent::new(64, 15).expect("checked extent");
    let image = SemanticImageLocator::new(identity, extent);
    assert_eq!((image.identity, image.extent), (identity, extent));
    let generation = GenerationId::from_digest([4; 32]);
    let snapshot = IndexSnapshotId::from_canonical_bytes(b"snapshot");
    let publication = ContentId::<CompilePublicationDomain>::from_canonical_bytes(b"publication");
    let facts = IndexLocatorFacts::new(generation, snapshot, publication);
    assert_eq!(
        (facts.generation, facts.snapshot, facts.publication),
        (generation, snapshot, publication)
    );
    let declaration = DeclarationIdentity {
        family: compiler_ir::DeclarationFamilyId::from_raw([7; 16]),
        variant: compiler_ir::VariantFingerprint::from_raw([8; 16]),
    };
    let locator = CanonicalEntityLocator::new(image, 3, declaration);
    assert_eq!(
        (locator.image, locator.ordinal, locator.declaration),
        (image, 3, declaration)
    );
}
