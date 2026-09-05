//! The `server-index-vocabulary` crate exists to define index snapshot, segment, partition, model, and metric identities.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![allow(missing_docs)]
//! Typed identities for the index capabilities that exist today.
//!
//! ```compile_fail
//! use server_index_vocabulary::{ExactSegmentId, LexicalSegmentId};
//! let lexical = LexicalSegmentId::from_canonical_bytes(b"segment");
//! let _: ExactSegmentId = lexical;
//! ```
//!
//! ```compile_fail
//! use server_index_vocabulary::{ExactSegmentId, LexicalSegmentId};
//! let lexical = LexicalSegmentId::from_canonical_bytes(b"segment");
//! let _: ExactSegmentId = lexical.into();
//! ```
//!
//! ```compile_fail
//! use server_index_vocabulary::{ExactSegmentId, SemanticImageExtent, SemanticImageLocator};
//! let segment = ExactSegmentId::from_canonical_bytes(b"segment");
//! let extent = SemanticImageExtent::new(0, 1).unwrap();
//! let _ = SemanticImageLocator::new(segment, extent);
//! ```

use compiler_ir::{
    DeclarationIdentity, PackageLineage, SemanticImageIdentity, SemanticImageView, SemanticReader,
};
use heart_identity::{
    ArtifactId, CompilePublicationDomain, ContentId, GenerationId, IndexExactSegmentDomain,
    IndexLexicalSegmentDomain, IndexPackDomain, IndexPackEncoding, IndexSnapshotDomain,
    IndexVectorSegmentDomain,
};

/// Identity of one immutable index snapshot.
pub type IndexSnapshotId = ContentId<IndexSnapshotDomain>;

/// Identity of one immutable exact-key segment.
pub type ExactSegmentId = ContentId<IndexExactSegmentDomain>;

/// Identity of one immutable lexical segment.
pub type LexicalSegmentId = ContentId<IndexLexicalSegmentDomain>;

/// Identity of one immutable vector segment.
pub type VectorSegmentId = ContentId<IndexVectorSegmentDomain>;

/// Physical identity of one complete immutable exact-and-lexical index pack.
pub type IndexPackId = ArtifactId<IndexPackEncoding, IndexPackDomain>;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageVersion<'version>(&'version str);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageVersionFault;
impl<'version> PackageVersion<'version> {
    pub const fn new(value: &'version str) -> Result<Self, PackageVersionFault> {
        if value.is_empty() {
            Err(PackageVersionFault)
        } else {
            Ok(Self(value))
        }
    }
    pub const fn as_str(self) -> &'version str {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageCoordinate<'package> {
    pub lineage: PackageLineage<'package>,
    pub version: PackageVersion<'package>,
}
impl<'package> PackageCoordinate<'package> {
    pub const fn new(lineage: PackageLineage<'package>, version: PackageVersion<'package>) -> Self {
        Self { lineage, version }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SemanticImageExtent {
    offset: u64,
    byte_length: u32,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticImageExtentFault;
impl SemanticImageExtent {
    pub const fn new(offset: u64, byte_length: u32) -> Result<Self, SemanticImageExtentFault> {
        if byte_length == 0 {
            return Err(SemanticImageExtentFault);
        }
        if offset.checked_add(byte_length as u64).is_none() {
            return Err(SemanticImageExtentFault);
        }
        Ok(Self {
            offset,
            byte_length,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SemanticImageLocator {
    pub identity: SemanticImageIdentity,
    pub extent: SemanticImageExtent,
}
impl SemanticImageLocator {
    pub const fn new(identity: SemanticImageIdentity, extent: SemanticImageExtent) -> Self {
        Self { identity, extent }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct IndexLocatorFacts {
    pub generation: GenerationId,
    pub snapshot: IndexSnapshotId,
    pub publication: ContentId<CompilePublicationDomain>,
}
impl IndexLocatorFacts {
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

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CanonicalEntityLocator {
    pub image: SemanticImageLocator,
    pub ordinal: u32,
    pub declaration: DeclarationIdentity,
}
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VerifiedCanonicalEntityLocator(CanonicalEntityLocator);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CanonicalEntityLocatorFault {
    ImageIdentity,
    ImageExtent,
    Ordinal,
    DeclarationIdentity,
}
impl CanonicalEntityLocator {
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
    pub fn verify_reopened(
        self,
        image: &SemanticImageView<'_>,
    ) -> Result<VerifiedCanonicalEntityLocator, CanonicalEntityLocatorFault> {
        if SemanticImageIdentity::from_encoded_bytes(image.as_ref()) != self.image.identity {
            return Err(CanonicalEntityLocatorFault::ImageIdentity);
        }
        if image.as_ref().len() != self.image.extent.byte_length as usize {
            return Err(CanonicalEntityLocatorFault::ImageExtent);
        }
        let entity = image
            .canonical_entities()
            .nth(self.ordinal as usize)
            .ok_or(CanonicalEntityLocatorFault::Ordinal)?;
        if entity.version.identity() != self.declaration {
            return Err(CanonicalEntityLocatorFault::DeclarationIdentity);
        }
        Ok(VerifiedCanonicalEntityLocator(self))
    }
}
pub type PackageVersionedLineage<'package> = PackageCoordinate<'package>;
pub type SemanticImageId = SemanticImageIdentity;
