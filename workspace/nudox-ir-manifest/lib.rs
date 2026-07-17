//! # nudox-ir-manifest — generation manifest + generation identity
//!
//! A package *generation* is a sealed snapshot: its source [`FileEntry`]s (in
//! the source CAS), its IR [`nudox_change::CasKey`] archive reference, and an
//! optional [`ChangeSetRef`] pinning the channel tip. [`BlobManifestV3`] ties
//! those together, and [`generation_stamp_v3`] derives the logical
//! [`nudox_change::GenerationStamp`] (Hash ①) from a *versioned* canonical
//! preimage — never from the manifest's own CAS bytes (Hash ②).
//!
//! The outbox API here takes a `GenerationStamp` ONLY, structurally preventing
//! the Hash ①/② confusion (design K12 / Issue 5).
//!
//! Normative spec: `.research/ir-vcs/design/IR-NATIVE-VCS-DESIGN.md` Rev 3.2,
//! "Source Separation + BlobManifest v3" + Appendix C.

pub mod generation;
pub mod manifest;
pub mod outbox;

#[cfg(test)]
mod tests;

pub use generation::{generation_stamp_v3, GenerationStampError};
pub use manifest::{BlobManifestV3, ChangeSetRef, FileEntry};
pub use outbox::{Outbox, OutboxEntry, OutboxError};
