//! One owner-revalidated Cargo file, kept separate from indexed declarations.

use super::{PackageRef, SourceText};
use crate::navigation::CargoSourcePath;

/// Current file bytes read under the exact Cargo metadata authority in the
/// package reference. This page is never seeded from a launch snapshot:
/// restoring its address must ask the owner to revalidate the source root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CargoSourcePage {
    /// Full source-qualified package reference.
    pub package: PackageRef,
    /// Package-relative path the owner read.
    pub file: CargoSourcePath,
    /// Current UTF-8 text with an immutable sparse line index.
    pub source: SourceText,
    /// BLAKE3 of the exact returned bytes, independent of the metadata receipt.
    pub content_digest: [u8; 32],
    /// Cargo observation revision revalidated for this read.
    pub source_revision: [u8; 32],
}
