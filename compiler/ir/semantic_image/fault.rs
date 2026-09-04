//! Closed diagnostics retained by portable core-image admission.

use thiserror::Error;

use crate::FactAvailability;

use super::wire::DirectoryKind;

/// Named core-image field retained by every structural fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CoreSemanticImageField {
    Header,
    Authority,
    Provenance,
    Directory,
    AtomRange,
    AtomOrder,
    EntityName,
    EntityKind,
    EntityVisibility,
    EntityParent,
    EntityParentage,
    EntityAvailability,
    EntitySource,
    EntityVersion,
}

/// Closed authority mismatch retained by a core row admission failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CoreAuthorityFault {
    Parentage { parentage: u8, parent: u32 },
    SourceAvailability { source: u8, source_file: u8, has_source: bool },
    Availability { plane: CoreAuthorityPlane, claimed: FactAvailability, present: bool },
}

/// Named authority plane retained by a core-image mismatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CoreAuthorityPlane {
    Visibility,
}

/// Closed image-provenance mismatch retained without a textly explanation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CoreProvenanceFault {
    AuthorityProfile { authority: [u8; 2], recipe: [u8; 2] },
    RecipeStage { observed: u8 },
    RecipeTool { observed: u8 },
    RecipeIdentity { expected: [u8; 32], observed: [u8; 32] },
    ScopeClaim { expected: [u8; 32], observed: [u8; 32] },
    ScopeAtom { component: ScopeComponent, raw: u32, atom_count: u32 },
}

/// Exact image-level identity cell rejected during provenance decoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CoreProvenanceIdentityField {
    Source,
    Recipe,
    Toolchain,
    ScopeClaim,
}

/// One retained package/file scope component.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ScopeComponent {
    Ecosystem,
    Package,
    Path,
}

/// Exact rejection from writing or reopening a portable core image.
#[derive(Debug, Error)]
pub(crate) enum CoreSemanticImageFault {
    #[error("core semantic image output needs {required} bytes but only {actual} were supplied")]
    OutputTooShort { required: usize, actual: usize },
    #[error("core semantic image length overflow while measuring {field:?}")]
    LengthOverflow { field: CoreSemanticImageField },
    #[error("core semantic image is truncated while reading {field:?} at byte {offset}")]
    Truncated { field: CoreSemanticImageField, offset: usize },
    #[error("core semantic image magic is {observed:?}, not {expected:?}")]
    Magic { expected: [u8; 4], observed: [u8; 4] },
    #[error("core semantic image schema is {observed}, not {expected}")]
    Schema { expected: u16, observed: u16 },
    #[error("core semantic image claims {claimed} bytes but input has {actual}")]
    Length { claimed: u32, actual: usize },
    #[error("core semantic image directory count is {observed}, not {expected}")]
    DirectoryCount { expected: u16, observed: u16 },
    #[error("core semantic image directory entry {entry} has kind {observed}, expected {expected:?}")]
    DirectoryKind { entry: u16, expected: DirectoryKind, observed: u16 },
    #[error("core semantic image directory {kind:?} range {offset}+{length} is outside {image_bytes}")]
    DirectoryRange { kind: DirectoryKind, offset: u32, length: u32, image_bytes: usize },
    #[error("core semantic image directory {kind:?} count is {observed}, expected {expected}")]
    DirectoryCountLane { kind: DirectoryKind, expected: u32, observed: u32 },
    #[error("core semantic image {field:?} row {row} references {observed}, outside {expected}")]
    Reference { field: CoreSemanticImageField, row: u32, expected: u32, observed: u32 },
    #[error("core semantic image {field:?} row {row} is not canonical after row {previous}")]
    CanonicalOrder { field: CoreSemanticImageField, previous: u32, row: u32 },
    #[error("core semantic image {field:?} row {row} has discriminant {observed}")]
    Discriminant { field: CoreSemanticImageField, row: u32, observed: u8 },
    #[error("core semantic image entity {row} has kind code {observed}")]
    EntityKind { row: u32, observed: u16 },
    #[error("core semantic image {field:?} row {row} has reserved byte {observed}")]
    Reserved { field: CoreSemanticImageField, row: u32, observed: u8 },
    #[error("core semantic image entity {row} has invalid source span {start}..{end}")]
    SourceSpan { row: u32, start: u32, end: u32 },
    #[error("core semantic image entity {row} has inconsistent authority: {cause:?}")]
    Authority { row: u32, cause: CoreAuthorityFault },
    #[error("core semantic image entity {row} duplicates declaration identity at row {existing}")]
    DuplicateIdentity { row: u32, existing: u32 },
    #[error("core semantic image entity {entity} has a local parent cycle through {parent}")]
    ParentCycle { entity: u32, parent: u32 },
    #[error("core semantic image profile bytes {observed:?} are not a known profile")]
    Profile { observed: [u8; 2], #[source] source: compiler_vocabulary::UnknownLanguageProfile },
    #[error("core semantic image provenance is invalid: {cause:?}")]
    Provenance { cause: CoreProvenanceFault },
    #[error("core semantic image {field:?} provenance identity is invalid")]
    ProvenanceIdentity { field: CoreProvenanceIdentityField, #[source] source: heart_identity::ContentIdDecodeError },
    #[error("core semantic image scope {component:?} is not UTF-8")]
    ScopeUtf8 { component: ScopeComponent, #[source] source: core::str::Utf8Error },
    #[error("core semantic image scope lineage is invalid: {cause:?}")]
    ScopeLineage { cause: crate::PackageLineageFault },
    #[error("core semantic image scope declaration key is invalid: {cause:?}")]
    ScopeKey { cause: crate::DeclarationKeyFault },
    #[error("core semantic image scope preimage cannot be framed: {cause:?}")]
    ScopePreimage { cause: crate::PreimageOverflow },
}
