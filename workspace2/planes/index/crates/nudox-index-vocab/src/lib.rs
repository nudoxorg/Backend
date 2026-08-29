#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! Closed typed vocabulary for immutable index snapshots and segment identities.
//!
//! ```compile_fail
//! use nudox_index_vocab::{Exact, IndexSegmentId, Lexical};
//! let lexical = IndexSegmentId::<Lexical>::from_canonical_bytes(b"segment");
//! let _: IndexSegmentId<Exact> = lexical;
//! ```
//!
//! ```compile_fail
//! use nudox_index_vocab::{Exact, IndexSegmentId, Lexical};
//! let lexical = IndexSegmentId::<Lexical>::from_canonical_bytes(b"segment");
//! let _: IndexSegmentId<Exact> = lexical.into();
//! ```
//!
//! ```compile_fail
//! use nudox_id::RootDomain;
//! use nudox_index_vocab::IndexSegmentId;
//! let _ = IndexSegmentId::<RootDomain>::from_canonical_bytes(b"not an index segment");
//! ```

use nudox_id::{ContentId, Domain, IndexSnapshotDomain};

#[doc = "Identity brand for exact index segments."]
pub use nudox_id::IndexExactSegmentDomain as Exact;
#[doc = "Identity brand for lexical index segments."]
pub use nudox_id::IndexLexicalSegmentDomain as Lexical;

/// The closed family vocabulary for index segments.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SegmentFamily {
    /// Exact term or key identity.
    Exact = 1,
    /// Lexical text identity.
    Lexical = 2,
    /// Relation identity, reserved for a later proof.
    Relation = 3,
    /// Usage identity, reserved for a later proof.
    Usage = 4,
    /// Vector identity, reserved for a later proof.
    Vector = 5,
}

/// An unrecognized raw segment-family code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnknownSegmentFamily {
    /// Rejected code, retained without narrowing or substitution.
    pub code: u8,
}

impl From<SegmentFamily> for u8 {
    fn from(family: SegmentFamily) -> Self {
        family as Self
    }
}

impl TryFrom<u8> for SegmentFamily {
    type Error = UnknownSegmentFamily;

    fn try_from(code: u8) -> Result<Self, Self::Error> {
        match code {
            1 => Ok(Self::Exact),
            2 => Ok(Self::Lexical),
            3 => Ok(Self::Relation),
            4 => Ok(Self::Usage),
            5 => Ok(Self::Vector),
            code => Err(UnknownSegmentFamily { code }),
        }
    }
}

mod sealed {
    pub trait IndexSegmentFamily {}
}

/// Associates a segment family with its distinct identity domain.
pub trait IndexSegmentFamily: sealed::IndexSegmentFamily {
    /// The centrally registered domain used for this family identity.
    type IdentityDomain: Domain;
}

impl sealed::IndexSegmentFamily for Exact {}

impl IndexSegmentFamily for Exact {
    type IdentityDomain = Self;
}

impl sealed::IndexSegmentFamily for Lexical {}

impl IndexSegmentFamily for Lexical {
    type IdentityDomain = Self;
}

/// Identity of one immutable index snapshot.
pub type IndexSnapshotId = ContentId<IndexSnapshotDomain>;

/// Identity of one immutable index segment in a statically known family.
pub type IndexSegmentId<FamilyBrand> =
    ContentId<<FamilyBrand as IndexSegmentFamily>::IdentityDomain>;
