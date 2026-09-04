//! Explicit cells and directory vocabulary for the complete semantic image.
//!
//! The core image keeps its `NXSI` schema-1 grammar unchanged.  A full image
//! is a separate `NXFI` grammar whose directory is deliberately complete: a
//! validated full reader can never confuse a missing pool with an empty one.

use core::fmt;

use super::fault::{FullSemanticImageFault, FullSemanticImageField};

pub(super) const MAGIC: [u8; 4] = *b"NXFI";
pub(super) const SCHEMA: u16 = 1;
/// The first 176 bytes are the same explicitly documented image
/// authority/provenance cells as the subordinate core grammar.  The full
/// directory begins immediately afterwards with its independent count.
pub(super) const HEADER_BYTES: usize = 176;
pub(super) const DIRECTORY_BYTES: usize = 16;
pub(super) const NONE: u32 = u32::MAX;
pub(super) const ATOM_ROW_BYTES: usize = 8;
pub(super) const ENTITY_ROW_BYTES: usize = 136;
pub(super) const TYPED_NODE_ROW_BYTES: usize = 12;
pub(super) const TYPED_EDGE_ROW_BYTES: usize = 20;
pub(super) const RANGE_ROW_BYTES: usize = 8;
pub(super) const EXTERNAL_ROW_BYTES: usize = 80;
pub(super) const LINK_ROW_BYTES: usize = 28;
pub(super) const OCCURRENCE_ROW_BYTES: usize = 24;
pub(super) const SPARSE_BINDING_ROW_BYTES: usize = 8;

/// One fixed-order full-image directory.  The order is part of the grammar:
/// no directory lookup or producer-specific ordering can alter canonical
/// bytes.  Variable-sized list/fact values always have an adjacent ranges
/// directory and byte payload directory.
#[repr(u16)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FullDirectoryKind {
    Atoms = 1,
    AtomBytes = 2,
    Entities = 3,
    TypedNodes = 4,
    TypedEdges = 5,
    EntityLists = 6,
    EntityListBytes = 7,
    Documentation = 8,
    DocumentationBytes = 9,
    Externals = 10,
    Links = 11,
    Occurrences = 12,
    TypeScriptFacts = 13,
    TypeScriptBindings = 14,
    CSharpFacts = 15,
    CSharpBindings = 16,
    GoFacts = 17,
    GoBindings = 18,
    RustFacts = 19,
    RustBindings = 20,
    PythonFacts = 21,
    PythonBindings = 22,
    JavaFacts = 23,
    JavaBindings = 24,
    ClangFacts = 25,
    ClangBindings = 26,
}

impl FullDirectoryKind {
    pub(super) const ALL: [Self; 26] = [
        Self::Atoms,
        Self::AtomBytes,
        Self::Entities,
        Self::TypedNodes,
        Self::TypedEdges,
        Self::EntityLists,
        Self::EntityListBytes,
        Self::Documentation,
        Self::DocumentationBytes,
        Self::Externals,
        Self::Links,
        Self::Occurrences,
        Self::TypeScriptFacts,
        Self::TypeScriptBindings,
        Self::CSharpFacts,
        Self::CSharpBindings,
        Self::GoFacts,
        Self::GoBindings,
        Self::RustFacts,
        Self::RustBindings,
        Self::PythonFacts,
        Self::PythonBindings,
        Self::JavaFacts,
        Self::JavaBindings,
        Self::ClangFacts,
        Self::ClangBindings,
    ];

    pub(super) const fn code(self) -> u16 {
        match self {
            Self::Atoms => 1,
            Self::AtomBytes => 2,
            Self::Entities => 3,
            Self::TypedNodes => 4,
            Self::TypedEdges => 5,
            Self::EntityLists => 6,
            Self::EntityListBytes => 7,
            Self::Documentation => 8,
            Self::DocumentationBytes => 9,
            Self::Externals => 10,
            Self::Links => 11,
            Self::Occurrences => 12,
            Self::TypeScriptFacts => 13,
            Self::TypeScriptBindings => 14,
            Self::CSharpFacts => 15,
            Self::CSharpBindings => 16,
            Self::GoFacts => 17,
            Self::GoBindings => 18,
            Self::RustFacts => 19,
            Self::RustBindings => 20,
            Self::PythonFacts => 21,
            Self::PythonBindings => 22,
            Self::JavaFacts => 23,
            Self::JavaBindings => 24,
            Self::ClangFacts => 25,
            Self::ClangBindings => 26,
        }
    }

    pub(super) const fn index(self) -> usize {
        match self {
            Self::Atoms => 0,
            Self::AtomBytes => 1,
            Self::Entities => 2,
            Self::TypedNodes => 3,
            Self::TypedEdges => 4,
            Self::EntityLists => 5,
            Self::EntityListBytes => 6,
            Self::Documentation => 7,
            Self::DocumentationBytes => 8,
            Self::Externals => 9,
            Self::Links => 10,
            Self::Occurrences => 11,
            Self::TypeScriptFacts => 12,
            Self::TypeScriptBindings => 13,
            Self::CSharpFacts => 14,
            Self::CSharpBindings => 15,
            Self::GoFacts => 16,
            Self::GoBindings => 17,
            Self::RustFacts => 18,
            Self::RustBindings => 19,
            Self::PythonFacts => 20,
            Self::PythonBindings => 21,
            Self::JavaFacts => 22,
            Self::JavaBindings => 23,
            Self::ClangFacts => 24,
            Self::ClangBindings => 25,
        }
    }

    pub(super) const fn count() -> u16 { 26 }
}

impl fmt::Display for FullDirectoryKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

/// A validated full-image directory payload.  It stores only byte spans and
/// row counts, never pointers into owned `Ir` storage, so a future mmap view
/// can carry it without allocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FullDirectoryEntry {
    pub(super) offset: usize,
    pub(super) length: usize,
    pub(super) count: u32,
}

/// Borrowed directory facts held by a fully validated full-image view.
#[derive(Clone, Copy, Debug)]
pub(super) struct FullImageLayout {
    pub(super) entries: [FullDirectoryEntry; 26],
}

impl FullImageLayout {
    pub(super) const fn entry(self, kind: FullDirectoryKind) -> FullDirectoryEntry {
        self.entries[kind.index()]
    }
}

#[inline]
pub(super) fn get_u16(
    bytes: &[u8],
    offset: usize,
    field: FullSemanticImageField,
) -> Result<u16, FullSemanticImageFault> {
    Ok(u16::from_le_bytes(read_array::<2>(bytes, offset, field)?))
}

#[inline]
pub(super) fn get_u32(
    bytes: &[u8],
    offset: usize,
    field: FullSemanticImageField,
) -> Result<u32, FullSemanticImageFault> {
    Ok(u32::from_le_bytes(read_array::<4>(bytes, offset, field)?))
}

#[inline]
pub(super) fn get_u64(
    bytes: &[u8],
    offset: usize,
    field: FullSemanticImageField,
) -> Result<u64, FullSemanticImageFault> {
    Ok(u64::from_le_bytes(read_array::<8>(bytes, offset, field)?))
}

#[inline]
pub(super) fn read_array<const N: usize>(
    bytes: &[u8],
    offset: usize,
    field: FullSemanticImageField,
) -> Result<[u8; N], FullSemanticImageFault> {
    let end = offset.checked_add(N).ok_or(FullSemanticImageFault::Truncated { field, offset })?;
    let slice = bytes
        .get(offset..end)
        .ok_or(FullSemanticImageFault::Truncated { field, offset })?;
    <[u8; N]>::try_from(slice).map_err(|_| FullSemanticImageFault::Truncated { field, offset })
}

#[inline]
pub(super) fn put_u16(output: &mut [u8], offset: usize, value: u16) {
    output[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

#[inline]
pub(super) fn put_u32(output: &mut [u8], offset: usize, value: u32) {
    output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[inline]
pub(super) fn put_u64(output: &mut [u8], offset: usize, value: u64) {
    output[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
