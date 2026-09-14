//! The `backend-semantic::index_vocabulary` module exists to define index snapshot, segment, partition, model, and metric identities.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//! Typed identities for the index capabilities that exist today.
//!
//! ```compile_fail
//! use backend_semantic::index_vocabulary::{ExactSegmentId, LexicalSegmentId};
//! let lexical = LexicalSegmentId::from_canonical_bytes(b"segment");
//! let _: ExactSegmentId = lexical;
//! ```
//!
//! ```compile_fail
//! use backend_semantic::index_vocabulary::{ExactSegmentId, LexicalSegmentId};
//! let lexical = LexicalSegmentId::from_canonical_bytes(b"segment");
//! let _: ExactSegmentId = lexical.into();
//! ```
//!
//! ```compile_fail
//! use backend_semantic::index_vocabulary::{ExactSegmentId, SemanticImageExtent, SemanticImageLocator};
//! let segment = ExactSegmentId::from_canonical_bytes(b"segment");
//! let extent = SemanticImageExtent::new(0, 1).unwrap();
//! let _ = SemanticImageLocator::new(segment, extent);
//! ```

use crate::ir::{
    DeclarationIdentity, PackageLineage, SemanticImageIdentity, SemanticImageView, SemanticReader,
};
use backend_version::{
    ArtifactId, CompilePublicationDomain, ContentId, GenerationId, IndexExactSegmentDomain,
    IndexLexicalSegmentDomain, IndexPackDomain, IndexPackEncoding, IndexVectorSegmentDomain,
};

/// Identity of one immutable index snapshot.
pub use backend_version::IndexSnapshotId;

/// Identity of one immutable exact-key segment.
pub type ExactSegmentId = ContentId<IndexExactSegmentDomain>;

/// Identity of one immutable lexical segment.
pub type LexicalSegmentId = ContentId<IndexLexicalSegmentDomain>;

/// Identity of one immutable vector segment.
pub type VectorSegmentId = ContentId<IndexVectorSegmentDomain>;

/// Physical identity of one complete immutable exact-and-lexical index pack.
pub type IndexPackId = ArtifactId<IndexPackEncoding, IndexPackDomain>;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
/// Validated opaque package-version text.
pub struct PackageVersion<'version>(&'version str);

/// Exact reason package-version admission failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageVersionFault {
    /// A version must retain at least one byte.
    Empty,
}

impl<'version> PackageVersion<'version> {
    /// Validates nonempty opaque version text without normalizing its ecosystem grammar.
    pub const fn new(value: &'version str) -> Result<Self, PackageVersionFault> {
        if value.is_empty() {
            Err(PackageVersionFault::Empty)
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the exact borrowed version text.
    pub const fn as_str(self) -> &'version str {
        self.0
    }
}

/// One version selected within a validated compiler package lineage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageCoordinate<'package> {
    /// Ecosystem and package name shared across versions.
    pub lineage: PackageLineage<'package>,
    /// Exact ecosystem-owned version spelling.
    pub version: PackageVersion<'package>,
}
impl<'package> PackageCoordinate<'package> {
    /// Pairs independently validated lineage and version facts.
    pub const fn new(lineage: PackageLineage<'package>, version: PackageVersion<'package>) -> Self {
        Self { lineage, version }
    }
}

/// Checked byte range locating a full semantic image in its compiler publication.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SemanticImageExtent {
    offset: u64,
    byte_length: u32,
}

/// Exact reason a semantic-image byte extent was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticImageExtentFault {
    /// A semantic image cannot occupy an empty range.
    Empty,
    /// Offset plus byte length exceeded the durable coordinate width.
    Overflow,
}

impl SemanticImageExtent {
    /// Validates a nonempty half-open byte range.
    pub fn new(offset: u64, byte_length: u32) -> Result<Self, SemanticImageExtentFault> {
        if byte_length == 0 {
            return Err(SemanticImageExtentFault::Empty);
        }
        if offset.checked_add(u64::from(byte_length)).is_none() {
            return Err(SemanticImageExtentFault::Overflow);
        }
        Ok(Self {
            offset,
            byte_length,
        })
    }

    /// Returns the inclusive byte offset inside the compiler publication.
    pub const fn offset(self) -> u64 {
        self.offset
    }

    /// Returns the exact nonzero encoded image width.
    pub const fn byte_length(self) -> u32 {
        self.byte_length
    }

    /// Returns the exclusive end offset proved during construction.
    pub fn end(self) -> u64 {
        self.offset + u64::from(self.byte_length)
    }
}

/// Content identity and checked publication extent of one canonical semantic image.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SemanticImageLocator {
    /// Compiler-owned full semantic-image artifact identity.
    pub identity: SemanticImageIdentity,
    /// Exact image location in its immutable publication artifact.
    pub extent: SemanticImageExtent,
}

/// A semantic image locator proved against bytes accepted by `SemanticImageView::reopen`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VerifiedSemanticImageLocator(SemanticImageLocator);

/// Exact failure while binding a locator to reopened semantic-image bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerifiedSemanticImageLocatorFault {
    /// Reopened bytes hash to another semantic-image identity.
    Identity,
    /// Reopened bytes have another exact encoded byte extent.
    Extent,
}

impl SemanticImageLocator {
    /// Proves this locator against an already reopened full semantic image.
    pub fn verify_reopened(
        self,
        image: &SemanticImageView<'_>,
    ) -> Result<VerifiedSemanticImageLocator, VerifiedSemanticImageLocatorFault> {
        if self.identity != SemanticImageIdentity::from_encoded_bytes(image.as_ref()) {
            return Err(VerifiedSemanticImageLocatorFault::Identity);
        }
        if usize::try_from(self.extent.byte_length()).ok() != Some(image.as_ref().len()) {
            return Err(VerifiedSemanticImageLocatorFault::Extent);
        }
        Ok(VerifiedSemanticImageLocator(self))
    }
}

impl VerifiedSemanticImageLocator {
    /// Returns the proven semantic-image locator for durable encoding.
    pub const fn as_locator(self) -> SemanticImageLocator {
        self.0
    }
}

/// Compiler/index authorities bound to a semantic image proved by reopening its bytes.
///
/// ```compile_fail
/// use backend_semantic::index_vocabulary::{SemanticImageLocator, VerifiedSemanticPublication};
///
/// fn bypass(image: SemanticImageLocator) -> VerifiedSemanticPublication {
///     image
/// }
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VerifiedSemanticPublication {
    authority: IndexLocatorFacts,
    image: VerifiedSemanticImageLocator,
    entity_count: usize,
}

impl VerifiedSemanticPublication {
    /// Binds immutable authorities only after proving the image locator against reopened bytes.
    pub fn verify_reopened(
        authority: IndexLocatorFacts,
        image: SemanticImageLocator,
        reopened: &SemanticImageView<'_>,
    ) -> Result<Self, VerifiedSemanticImageLocatorFault> {
        Ok(Self {
            authority,
            image: image.verify_reopened(reopened)?,
            entity_count: reopened.canonical_entities().len(),
        })
    }
    /// Returns the immutable compiler/index authorities.
    pub const fn authority(self) -> IndexLocatorFacts {
        self.authority
    }
    /// Returns the semantic-image locator proved against reopened bytes.
    pub const fn image(self) -> SemanticImageLocator {
        self.image.as_locator()
    }
    /// Returns the exact count of canonical declarations admitted in the reopened image.
    pub const fn entity_count(self) -> usize {
        self.entity_count
    }
}
impl SemanticImageLocator {
    /// Pairs a typed image authority with its checked extent.
    pub const fn new(identity: SemanticImageIdentity, extent: SemanticImageExtent) -> Self {
        Self { identity, extent }
    }
}

/// Generation, index snapshot, and compiler publication authorities for one catalog row.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct IndexLocatorFacts {
    /// Immutable compiler generation containing the image.
    pub generation: GenerationId,
    /// Immutable index snapshot derived from the generation.
    pub snapshot: IndexSnapshotId,
    /// Durable compiler publication containing the image bytes.
    pub publication: ContentId<CompilePublicationDomain>,
}
impl IndexLocatorFacts {
    /// Binds the three independent typed authorities without erasing their domains.
    pub const fn new(
        generation: GenerationId,
        snapshot: IndexSnapshotId,
        publication: ContentId<CompilePublicationDomain>,
    ) -> Self {
        Self {
            generation,
            snapshot,
            publication,
        }
    }
}

/// Unverified declaration coordinate within one named canonical semantic image.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CanonicalEntityLocator {
    /// Canonical image expected to contain the declaration.
    pub image: SemanticImageLocator,
    /// Dense canonical entity ordinal in that image.
    pub ordinal: u32,
    /// Compiler declaration identity expected at the ordinal.
    pub declaration: DeclarationIdentity,
}

/// Entity locator proved against a reopened canonical semantic image.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VerifiedCanonicalEntityLocator(CanonicalEntityLocator);

/// Exact mismatch found while verifying a declaration locator against an image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CanonicalEntityLocatorFault {
    /// Reopened bytes hash to another semantic-image identity.
    ImageIdentity,
    /// Reopened bytes have another encoded extent.
    ImageExtent,
    /// The declared ordinal is absent from the canonical image.
    Ordinal,
    /// The entity at the ordinal has another compiler declaration identity.
    DeclarationIdentity,
}
impl CanonicalEntityLocator {
    /// Creates an unverified locator that cannot enter durable catalog rows until verified.
    pub const fn new(
        image: SemanticImageLocator,
        ordinal: u32,
        declaration: DeclarationIdentity,
    ) -> Self {
        Self {
            image,
            ordinal,
            declaration,
        }
    }

    /// Verifies image identity, encoded extent, ordinal presence, and declaration identity.
    ///
    /// # Errors
    ///
    /// Returns the first exact locator fact that disagrees with the reopened semantic image.
    pub fn verify_reopened(
        self,
        image: &SemanticImageView<'_>,
    ) -> Result<VerifiedCanonicalEntityLocator, CanonicalEntityLocatorFault> {
        if SemanticImageIdentity::from_encoded_bytes(image.as_ref()) != self.image.identity {
            return Err(CanonicalEntityLocatorFault::ImageIdentity);
        }
        if u32::try_from(image.as_ref().len()).ok() != Some(self.image.extent.byte_length) {
            return Err(CanonicalEntityLocatorFault::ImageExtent);
        }
        let ordinal =
            usize::try_from(self.ordinal).map_err(|_| CanonicalEntityLocatorFault::Ordinal)?;
        let entity = image
            .canonical_entities()
            .nth(ordinal)
            .ok_or(CanonicalEntityLocatorFault::Ordinal)?;
        if entity.version.identity() != self.declaration {
            return Err(CanonicalEntityLocatorFault::DeclarationIdentity);
        }
        Ok(VerifiedCanonicalEntityLocator(self))
    }
}

impl VerifiedCanonicalEntityLocator {
    /// Returns the exact locator facts proved by the reopened image.
    pub const fn as_locator(self) -> CanonicalEntityLocator {
        self.0
    }
}
