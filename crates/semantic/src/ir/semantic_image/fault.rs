//! Closed diagnostics retained by canonical semantic-image planning and
//! shared image-header/provenance admission.

use thiserror::Error;

/// Named shared image field retained by every planning/header fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoreSemanticImageField {
    /// Fixed shared semantic-image header.
    Header,
    /// Language/profile authority plane.
    Authority,
    /// Source, recipe, and identity provenance plane.
    Provenance,
    /// Directory rows that locate the image's lanes.
    Directory,
    /// Byte range backing the terminal atom pool.
    AtomRange,
    /// Canonical order of atom-pool entries.
    AtomOrder,
    /// Entity-name terminal range or reference.
    EntityName,
    /// Encoded entity-kind value.
    EntityKind,
    /// Entity visibility authority/value.
    EntityVisibility,
    /// Entity parent coordinate.
    EntityParent,
    /// Entity parentage evidence.
    EntityParentage,
    /// Entity availability evidence.
    EntityAvailability,
    /// Entity source-span evidence.
    EntitySource,
    /// Entity version evidence.
    EntityVersion,
    /// External declaration identity evidence.
    External,
}

/// Closed image-provenance mismatch retained without a textly explanation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoreProvenanceFault {
    /// Recipe profile bytes disagree with the image authority profile.
    AuthorityProfile {
        /// Profile bytes retained from the authority lane.
        authority: [u8; 2],
        /// Profile bytes retained from the recipe lane.
        recipe: [u8; 2],
    },
    /// Recipe stage code is outside the accepted compiler-stage range.
    RecipeStage {
        /// Unsupported stage code read from the recipe lane.
        observed: u8,
    },
    /// Recipe tool code does not name a supported compiler tool.
    RecipeTool {
        /// Unsupported tool code read from the recipe lane.
        observed: u8,
    },
    /// Stored recipe identity does not match the identity derived from its fields.
    RecipeIdentity {
        /// Recipe identity recomputed from the admitted image provenance.
        expected: [u8; 32],
        /// Recipe identity bytes stored in the image.
        observed: [u8; 32],
    },
    /// Stored scope claim does not match the claim derived from its components.
    ScopeClaim {
        /// Scope identity recomputed from the retained scope components.
        expected: [u8; 32],
        /// Scope identity bytes stored in the image.
        observed: [u8; 32],
    },
    /// A scope component points beyond the shared atom table.
    ScopeAtom {
        /// Scope component whose atom ordinal could not be admitted.
        component: ScopeComponent,
        /// Raw atom ordinal encoded for this component.
        raw: u32,
        /// Number of atoms present in the shared atom pool.
        atom_count: u32,
    },
    /// The retained exact package coordinate was malformed or contradicted
    /// its profile/lineage cells.
    Coordinate,
}

/// Exact image-level identity cell rejected during provenance decoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CoreProvenanceIdentityField {
    /// Source-content identity cell.
    Source,
    /// Recipe identity cell.
    Recipe,
    /// Toolchain identity cell.
    Toolchain,
    /// Scope-claim identity cell.
    ScopeClaim,
}

/// One retained package/file scope component.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScopeComponent {
    /// Package ecosystem atom.
    Ecosystem,
    /// Package name atom.
    Package,
    /// File path atom.
    Path,
    /// Package coordinate atom.
    Coordinate,
}

/// Exact rejection from canonical image planning or shared header admission.
#[derive(Debug, Error)]
pub enum CoreSemanticImageFault {
    #[error("semantic image length overflow while measuring {field:?}")]
    /// Computing the host byte geometry for a named image lane overflowed.
    LengthOverflow {
        /// Lane whose checked byte geometry could not be represented.
        field: CoreSemanticImageField,
    },
    #[error("semantic image is truncated while reading {field:?} at byte {offset}")]
    /// The image ended before a lane value could be read at its byte offset.
    Truncated {
        /// Lane containing the incomplete value.
        field: CoreSemanticImageField,
        /// Zero-based byte offset from the image start where the read began.
        offset: usize,
    },
    #[error("semantic image {field:?} row {row} references {observed}, outside {expected}")]
    /// A row coordinate points beyond the named pool or row table.
    Reference {
        /// Lane containing the invalid reference.
        field: CoreSemanticImageField,
        /// Zero-based row ordinal that contains the reference.
        row: u32,
        /// Number of target rows admitted for this reference.
        expected: u32,
        /// Target coordinate encoded in the row.
        observed: u32,
    },
    #[error("semantic image {field:?} row {row} is not canonical after row {previous}")]
    /// A lane row does not follow its predecessor in canonical sort order.
    CanonicalOrder {
        /// Lane whose canonical ordering was checked.
        field: CoreSemanticImageField,
        /// Zero-based ordinal of the earlier row in the comparison.
        previous: u32,
        /// Zero-based ordinal of the later row that violates strict order.
        row: u32,
    },
    #[error("semantic image {field:?} row {row} has discriminant {observed}")]
    /// A row contains a tag outside the field's supported values.
    Discriminant {
        /// Lane containing the invalid tag.
        field: CoreSemanticImageField,
        /// Zero-based row ordinal containing the tag.
        row: u32,
        /// Encoded tag byte.
        observed: u8,
    },
    #[error("semantic image {field:?} row {row} has reserved byte {observed}")]
    /// A reserved row byte is nonzero.
    Reserved {
        /// Lane containing the reserved byte.
        field: CoreSemanticImageField,
        /// Zero-based row ordinal containing the byte.
        row: u32,
        /// Byte observed where the format requires zero.
        observed: u8,
    },
    #[error("semantic image profile bytes {observed:?} are not a known profile")]
    /// The two profile bytes do not identify a supported language profile.
    Profile {
        /// Profile bytes read from the image authority lane.
        observed: [u8; 2],
        /// Vocabulary decoder's exact unknown-profile result.
        #[source]
        source: crate::vocabulary::UnknownLanguageProfile,
    },
    #[error("semantic image provenance is invalid: {cause:?}")]
    /// Cross-checking image provenance fields failed.
    Provenance {
        /// Exact provenance contradiction retained by the shared header reader.
        cause: CoreProvenanceFault,
    },
    #[error("semantic image {field:?} provenance identity is invalid")]
    /// A typed identity field could not be decoded in its required domain.
    ProvenanceIdentity {
        /// Provenance identity cell that failed decoding.
        field: CoreProvenanceIdentityField,
        /// Domain-tag or content-ID framing failure from the identity decoder.
        #[source]
        source: backend_version::ContentIdDecodeError,
    },
    #[error("semantic image scope {component:?} is not UTF-8")]
    /// A scope atom does not decode as UTF-8.
    ScopeUtf8 {
        /// Scope component whose bytes failed UTF-8 decoding.
        component: ScopeComponent,
        /// UTF-8 error from the selected atom bytes.
        #[source]
        source: core::str::Utf8Error,
    },
    #[error("semantic image scope lineage is invalid: {cause:?}")]
    /// Scope components do not form the retained package lineage.
    ScopeLineage {
        /// Exact package-lineage validation fault.
        cause: crate::ir::PackageLineageFault,
    },
    #[error("semantic image scope declaration key is invalid: {cause:?}")]
    /// Scope declaration bytes cannot be admitted as a declaration key.
    ScopeKey {
        /// Exact declaration-key validation fault.
        cause: crate::ir::DeclarationKeyFault,
    },
    #[error("semantic image scope preimage cannot be framed: {cause:?}")]
    /// Scope identity preimage length cannot be represented for hashing.
    ScopePreimage {
        /// Checked framing overflow from the identity preimage builder.
        cause: crate::ir::PreimageOverflow,
    },
}
