//! Explicit cells and directory vocabulary for the complete semantic image.
//!
//! `NXFI` has one deliberately complete directory: a validated full reader
//! can never confuse a missing pool with an empty one.

use core::fmt;

use super::fault::{FullSemanticImageFault, FullSemanticImageField};

pub(crate) const MAGIC: [u8; 4] = *b"NXFI";
pub(crate) const SCHEMA_LEGACY: u16 = 1;
/// Schema 2 adds the complete packed signature-carrier-role lane.  This is
/// deliberately independent from the semantic projection epoch used in build
/// identities: future projection changes must not reinterpret this grammar.
pub(crate) const SCHEMA_CARRIER_ROLES: u16 = 2;
/// Schema 3 adds exact owner/role/slot carrier bindings beside the role union.
pub(crate) const SCHEMA_CARRIER_BINDINGS: u16 = 3;
/// Schema 4 adds a tagged declaration-name cell for source-backed anonymous
/// callable anchors while retaining the schema-3 directory layout.
pub(crate) const SCHEMA_TYPED_NAMES: u16 = 4;
/// The first 176 bytes are the same explicitly documented image
/// authority/provenance cells as the subordinate core grammar.  The full
/// directory begins immediately afterwards with its independent count.
pub(crate) const HEADER_BYTES: usize = 176;
pub(crate) const HEADER_BYTES_U32: u32 = 176;
pub(crate) const DIRECTORY_BYTES: usize = 16;
pub(crate) const NONE: u32 = u32::MAX;
pub(crate) const ATOM_ROW_BYTES: usize = 8;
pub(crate) const ENTITY_ROW_BYTES: usize = 136;
pub(crate) const TYPED_NODE_ROW_BYTES: usize = 16;
pub(crate) const TYPED_EDGE_ROW_BYTES: usize = 20;
pub(crate) const RANGE_ROW_BYTES: usize = 8;
pub(crate) const EXTERNAL_ROW_BYTES: usize = 96;
pub(crate) const LINK_ROW_BYTES: usize = 28;
pub(crate) const OCCURRENCE_ROW_BYTES: usize = 24;
pub(crate) const SPARSE_BINDING_ROW_BYTES: usize = 8;
pub(crate) const SIGNATURE_CARRIER_RANGE_ROW_BYTES: usize = 16;
pub(crate) const SIGNATURE_CARRIER_TARGET_ROW_BYTES: usize = 4;

/// One fixed-order full-image directory.  The order is part of the grammar:
/// no directory lookup or producer-specific ordering can alter canonical
/// bytes.  Variable-sized list/fact values always have an adjacent ranges
/// directory and byte payload directory.
#[repr(u16)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FullDirectoryKind {
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
    SignatureCarrierRoles = 27,
    SignatureCarrierBindingRanges = 28,
    SignatureCarrierBindingTargets = 29,
}

impl FullDirectoryKind {
    pub const ALL: [Self; 26] = [
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

    /// Schema-2 directory order, extending schema 1 with one trailing lane.
    pub const ALL_WITH_CARRIER_ROLES: [Self; 27] = [
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
        Self::SignatureCarrierRoles,
    ];

    /// Schema-3 directory order, extending schema 2 with the owner-range and
    /// typed-target lanes.
    pub const ALL_WITH_CARRIER_BINDINGS: [Self; 29] = [
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
        Self::SignatureCarrierRoles,
        Self::SignatureCarrierBindingRanges,
        Self::SignatureCarrierBindingTargets,
    ];

    pub(crate) const fn kinds_for_schema(schema: u16) -> Option<&'static [Self]> {
        match schema {
            SCHEMA_LEGACY => Some(&Self::ALL),
            SCHEMA_CARRIER_ROLES => Some(&Self::ALL_WITH_CARRIER_ROLES),
            SCHEMA_CARRIER_BINDINGS => Some(&Self::ALL_WITH_CARRIER_BINDINGS),
            SCHEMA_TYPED_NAMES => Some(&Self::ALL_WITH_CARRIER_BINDINGS),
            _ => None,
        }
    }

    pub(crate) const fn count_for_schema(schema: u16) -> Option<u16> {
        match schema {
            SCHEMA_LEGACY => Some(26),
            SCHEMA_CARRIER_ROLES => Some(27),
            SCHEMA_CARRIER_BINDINGS => Some(29),
            SCHEMA_TYPED_NAMES => Some(29),
            _ => None,
        }
    }

    pub const fn code(self) -> u16 {
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
            Self::SignatureCarrierRoles => 27,
            Self::SignatureCarrierBindingRanges => 28,
            Self::SignatureCarrierBindingTargets => 29,
        }
    }

    pub const fn index(self) -> usize {
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
            Self::SignatureCarrierRoles => 26,
            Self::SignatureCarrierBindingRanges => 27,
            Self::SignatureCarrierBindingTargets => 28,
        }
    }
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
pub(crate) struct FullDirectoryEntry {
    pub(crate) offset: usize,
    pub(crate) length: usize,
    pub(crate) offset_wire: u32,
    pub(crate) length_wire: u32,
    pub(crate) count: u32,
}

impl FullDirectoryEntry {
    pub(crate) const EMPTY: Self = Self {
        offset: 0,
        length: 0,
        offset_wire: 0,
        length_wire: 0,
        count: 0,
    };
}

/// Borrowed directory facts held by a fully validated full-image view.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FullImageLayout {
    pub(crate) schema: u16,
    pub(crate) entries: [FullDirectoryEntry; 29],
}

impl FullImageLayout {
    pub(crate) const fn entry(self, kind: FullDirectoryKind) -> FullDirectoryEntry {
        match (self.schema, kind) {
            (SCHEMA_LEGACY, FullDirectoryKind::SignatureCarrierRoles)
            | (SCHEMA_LEGACY, FullDirectoryKind::SignatureCarrierBindingRanges)
            | (SCHEMA_LEGACY, FullDirectoryKind::SignatureCarrierBindingTargets)
            | (SCHEMA_CARRIER_ROLES, FullDirectoryKind::SignatureCarrierBindingRanges)
            | (SCHEMA_CARRIER_ROLES, FullDirectoryKind::SignatureCarrierBindingTargets) => {
                FullDirectoryEntry::EMPTY
            }
            _ => self.entries[kind.index()],
        }
    }
}

#[inline]
pub(crate) fn get_u16(
    bytes: &[u8],
    offset: usize,
    field: FullSemanticImageField,
) -> Result<u16, FullSemanticImageFault> {
    Ok(u16::from_le_bytes(read_array::<2>(bytes, offset, field)?))
}

#[inline]
pub(crate) fn get_u32(
    bytes: &[u8],
    offset: usize,
    field: FullSemanticImageField,
) -> Result<u32, FullSemanticImageFault> {
    Ok(u32::from_le_bytes(read_array::<4>(bytes, offset, field)?))
}

#[inline]
pub(crate) fn read_array<const N: usize>(
    bytes: &[u8],
    offset: usize,
    field: FullSemanticImageField,
) -> Result<[u8; N], FullSemanticImageFault> {
    let end = offset
        .checked_add(N)
        .ok_or(FullSemanticImageFault::Truncated { field, offset })?;
    let slice = bytes
        .get(offset..end)
        .ok_or(FullSemanticImageFault::Truncated { field, offset })?;
    <[u8; N]>::try_from(slice).map_err(|_| FullSemanticImageFault::Truncated { field, offset })
}

#[inline]
pub(crate) fn put_u16(output: &mut [u8], offset: usize, value: u16) {
    output[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

#[inline]
pub(crate) fn put_u32(output: &mut [u8], offset: usize, value: u32) {
    output[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
