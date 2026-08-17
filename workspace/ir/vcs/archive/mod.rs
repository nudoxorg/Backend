//! # nudox-ir-archive — the `NdIr` PackageArchive
//!
//! A [`seal`]ed, content-addressed, **IR-only** serve snapshot of one package
//! generation. The layout is sectional (a POD [`header::ArchiveHeader`] + TOC of
//! [`header::TocEntry`] pointing at [`header::SectionId`]-tagged sections) so a
//! reader can `mmap` the bytes and answer name/alias, intro, tree, link, and
//! type-skeleton queries with zero copies via [`view::PackageArchiveView`] /
//! [`view::YokedArchive`].
//!
//! Seal is **deterministic** (design Issue 20): the same intros + payloads +
//! links always produce byte-identical output and thus the same
//! [`ir::change::CasKey`]. Forbidden in the archive: source bytes, tar, CST,
//! Terminus documents, unbounded embeddings.
//!
//! Normative spec: `docs/research/ir-vcs/design/IR-NATIVE-VCS-DESIGN.md` Rev 3.2,
//! "PackageArchive Layout" + decisions K2, K3, K11, K16.

pub mod error;
pub mod header;
pub mod index;
pub mod open;
pub mod seal;
pub mod section;
pub mod view;

#[cfg(test)]
mod tests;

pub use error::Error;
pub use header::{
    ArchiveHeader, EntryHead, MAX_ENTRIES, MAX_SECTION_UNCOMPRESSED, MAX_STRING_BLOB, SectionId,
    TocEntry,
};
pub use seal::{
    Error as SealError, SealEntry, SealedArchive, seal_from_entries, seal_package_archive,
};
pub use view::{LinkEnd, PackageArchiveView, YokedArchive};
