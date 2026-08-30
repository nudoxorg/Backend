#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! Typed identities for the index capabilities that exist today.
//!
//! ```compile_fail
//! use nudox_index_vocab::{ExactSegmentId, LexicalSegmentId};
//! let lexical = LexicalSegmentId::from_canonical_bytes(b"segment");
//! let _: ExactSegmentId = lexical;
//! ```
//!
//! ```compile_fail
//! use nudox_index_vocab::{ExactSegmentId, LexicalSegmentId};
//! let lexical = LexicalSegmentId::from_canonical_bytes(b"segment");
//! let _: ExactSegmentId = lexical.into();
//! ```

use nudox_id::{
    ContentId, IndexExactSegmentDomain, IndexLexicalSegmentDomain, IndexSnapshotDomain,
};

/// Identity of one immutable index snapshot.
pub type IndexSnapshotId = ContentId<IndexSnapshotDomain>;

/// Identity of one immutable exact-key segment.
pub type ExactSegmentId = ContentId<IndexExactSegmentDomain>;

/// Identity of one immutable lexical segment.
pub type LexicalSegmentId = ContentId<IndexLexicalSegmentDomain>;
