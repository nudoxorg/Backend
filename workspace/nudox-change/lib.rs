//! # nudox-change — content-addressed identity primitives
//!
//! The domain-agnostic identity layer for the IR-VCS: the BLAKE3 identity
//! newtypes (with the Hash ①/② discipline — [`CasKey`] ≠ [`GenerationStamp`] ≠
//! [`ChangeId`] ≠ [`ChangeSetFingerprint`] ≠ [`IntroId`]), the wire-stable
//! [`StableRef`] / [`PackageLineageId`], the typed [`domain::LinkDomainKey`], and
//! the canonical little-endian [`encode`] helpers used to build hash preimages.
//!
//! > **Note:** this crate previously also carried a hand-rolled `Atom`/`Change`
//! > algebra. That was superseded — the IR is versioned by **libpijul** (via
//! > `nudox-ir-vcs`), which owns changes, dependencies, apply, and unrecord. What
//! > remains here is only the identity vocabulary those higher layers share.

pub mod domain;
pub mod encode;
pub mod hash;
pub mod ids;

pub use domain::{DomainKey, GraphDocId, GraphRelationId, IriPolicy, LinkDomainKey};
pub use hash::{
    CasKey, ChangeId, ChangeSetFingerprint, ContentBlake3, GenerationStamp, IntroId, MerkleState,
};
pub use ids::{
    AuthorId, ChangeMessage, ChannelId, ChannelName, EcosystemId, PackageLineageId, PackageName,
    StableRef, TimestampUnixMs,
};
