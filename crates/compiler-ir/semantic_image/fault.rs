//! Closed diagnostics retained by canonical semantic-image planning and
//! shared image-header/provenance admission.

use thiserror::Error;

/// Named shared image field retained by every planning/header fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoreSemanticImageField {
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
    External,
}

/// Closed image-provenance mismatch retained without a textly explanation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoreProvenanceFault {
    AuthorityProfile {
        authority: [u8; 2],
        recipe: [u8; 2],
    },
    RecipeStage {
        observed: u8,
    },
    RecipeTool {
        observed: u8,
    },
    RecipeIdentity {
        expected: [u8; 32],
        observed: [u8; 32],
    },
    ScopeClaim {
        expected: [u8; 32],
        observed: [u8; 32],
    },
    ScopeAtom {
        component: ScopeComponent,
        raw: u32,
        atom_count: u32,
    },
    /// The retained exact package coordinate was malformed or contradicted
    /// its profile/lineage cells.
    Coordinate,
}

/// Exact image-level identity cell rejected during provenance decoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoreProvenanceIdentityField {
    Source,
    Recipe,
    Toolchain,
    ScopeClaim,
}

/// One retained package/file scope component.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScopeComponent {
    Ecosystem,
    Package,
    Path,
    Coordinate,
}

/// Exact rejection from canonical image planning or shared header admission.
#[derive(Debug, Error)]
pub enum CoreSemanticImageFault {
    #[error("semantic image length overflow while measuring {field:?}")]
    LengthOverflow { field: CoreSemanticImageField },
    #[error("semantic image is truncated while reading {field:?} at byte {offset}")]
    Truncated {
        field: CoreSemanticImageField,
        offset: usize,
    },
    #[error("semantic image {field:?} row {row} references {observed}, outside {expected}")]
    Reference {
        field: CoreSemanticImageField,
        row: u32,
        expected: u32,
        observed: u32,
    },
    #[error("semantic image {field:?} row {row} is not canonical after row {previous}")]
    CanonicalOrder {
        field: CoreSemanticImageField,
        previous: u32,
        row: u32,
    },
    #[error("semantic image {field:?} row {row} has discriminant {observed}")]
    Discriminant {
        field: CoreSemanticImageField,
        row: u32,
        observed: u8,
    },
    #[error("semantic image {field:?} row {row} has reserved byte {observed}")]
    Reserved {
        field: CoreSemanticImageField,
        row: u32,
        observed: u8,
    },
    #[error("semantic image profile bytes {observed:?} are not a known profile")]
    Profile {
        observed: [u8; 2],
        #[source]
        source: backend_semantic::vocabulary::UnknownLanguageProfile,
    },
    #[error("semantic image provenance is invalid: {cause:?}")]
    Provenance { cause: CoreProvenanceFault },
    #[error("semantic image {field:?} provenance identity is invalid")]
    ProvenanceIdentity {
        field: CoreProvenanceIdentityField,
        #[source]
        source: backend_version::ContentIdDecodeError,
    },
    #[error("semantic image scope {component:?} is not UTF-8")]
    ScopeUtf8 {
        component: ScopeComponent,
        #[source]
        source: core::str::Utf8Error,
    },
    #[error("semantic image scope lineage is invalid: {cause:?}")]
    ScopeLineage { cause: crate::PackageLineageFault },
    #[error("semantic image scope declaration key is invalid: {cause:?}")]
    ScopeKey { cause: crate::DeclarationKeyFault },
    #[error("semantic image scope preimage cannot be framed: {cause:?}")]
    ScopePreimage { cause: crate::PreimageOverflow },
}
