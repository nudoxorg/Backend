//! Closed full-image grammar diagnostics.

use thiserror::Error;

use super::wire::FullDirectoryKind;

/// Exact full-image lane retained by every portable grammar failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FullSemanticImageField {
    Header,
    Authority,
    Provenance,
    Directory,
    Atoms,
    Entities,
    TypedNodes,
    TypedEdges,
    EntityLists,
    Documentation,
    Externals,
    Links,
    Occurrences,
    ExtensionFacts,
    ExtensionBindings,
}

/// Exact content-identity cell whose domain tag failed to reopen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FullSemanticImageIdentityField {
    StableFragment,
    FragmentEntityFragment,
}

/// Full-image fault.  Every row/reference carries both its semantic lane and
/// exact observed coordinate; no generic "invalid image" terminal erases the
/// producer's source.
#[derive(Debug, Error)]
pub enum FullSemanticImageFault {
    #[error("full semantic image output needs {required} bytes but only {actual} were supplied")]
    OutputTooShort { required: usize, actual: usize },
    #[error("full semantic image length overflow while measuring {field:?}")]
    LengthOverflow { field: FullSemanticImageField },
    #[error("full semantic image is truncated while reading {field:?} at byte {offset}")]
    Truncated {
        field: FullSemanticImageField,
        offset: usize,
    },
    #[error("full semantic image magic is {observed:?}, not {expected:?}")]
    Magic {
        expected: [u8; 4],
        observed: [u8; 4],
    },
    #[error("full semantic image schema is {observed}, not {expected}")]
    Schema { expected: u16, observed: u16 },
    #[error("full semantic image claims {claimed} bytes but input has {actual}")]
    Length { claimed: u32, actual: usize },
    #[error("full semantic image directory count is {observed}, not {expected}")]
    DirectoryCount { expected: u16, observed: u16 },
    #[error("full semantic image directory {entry} has kind {observed}, expected {expected:?}")]
    DirectoryKind {
        entry: u16,
        expected: FullDirectoryKind,
        observed: u16,
    },
    #[error(
        "full semantic image directory {kind:?} range {offset}+{length} is outside {image_bytes}"
    )]
    DirectoryRange {
        kind: FullDirectoryKind,
        offset: u32,
        length: u32,
        image_bytes: usize,
    },
    #[error("full semantic image directory {kind:?} count is {observed}, expected {expected}")]
    DirectoryCountLane {
        kind: FullDirectoryKind,
        expected: u32,
        observed: u32,
    },
    #[error("full semantic image {field:?} row {row} references {observed}, outside {expected}")]
    Reference {
        field: FullSemanticImageField,
        row: u32,
        expected: u32,
        observed: u32,
    },
    #[error("full semantic image {field:?} row {row} has discriminant {observed}")]
    Discriminant {
        field: FullSemanticImageField,
        row: u32,
        observed: u8,
    },
    #[error("full semantic image {field:?} row {row} has invalid {identity:?} content identity")]
    ContentIdentity {
        field: FullSemanticImageField,
        row: u32,
        identity: FullSemanticImageIdentityField,
        #[source]
        cause: backend_version::ContentIdDecodeError,
    },
    #[error("full semantic image entity {row} has kind code {observed}")]
    EntityKind { row: u32, observed: u16 },
    #[error("full semantic image entity {row} has invalid source span {start}..{end}")]
    SourceSpan { row: u32, start: u32, end: u32 },
    #[error(
        "full semantic image entity {row} has inconsistent authority plane {plane} (claimed {claimed}, present {present})"
    )]
    Authority {
        row: u32,
        plane: u8,
        claimed: u8,
        present: bool,
    },
    #[error("full semantic image entity {row} duplicates declaration identity at row {existing}")]
    DuplicateIdentity { row: u32, existing: u32 },
    #[error("full semantic image {field:?} row {row} duplicates canonical row {existing}")]
    DuplicateCanonicalRow {
        field: FullSemanticImageField,
        row: u32,
        existing: u32,
    },
    #[error(
        "full semantic image {plane:?} sparse binding row {row} repeats entity {entity} from row {existing}"
    )]
    DuplicateExtensionBinding {
        plane: FullDirectoryKind,
        entity: u32,
        existing: u32,
        row: u32,
    },
    #[error("full semantic image {plane:?} fact row {fact} is not bound by any declaration")]
    UnboundExtensionFact { plane: FullDirectoryKind, fact: u32 },
    #[error(
        "full semantic image {plane:?} has {facts} facts/{fact_bytes} bytes and {bindings} bindings/{binding_bytes} bytes under incompatible authority {authority:?}"
    )]
    ExtensionPlaneAuthority {
        plane: FullDirectoryKind,
        authority: crate::SemanticImageAuthority,
        facts: u32,
        fact_bytes: u32,
        bindings: u32,
        binding_bytes: u32,
    },
    #[error("full semantic image entity {entity} has a local parent cycle through {parent}")]
    ParentCycle { entity: u32, parent: u32 },
    #[error("full semantic image {field:?} row {row} has reserved byte {observed}")]
    Reserved {
        field: FullSemanticImageField,
        row: u32,
        observed: u8,
    },
    #[error("full semantic image {field:?} row {row} is not canonical after row {previous}")]
    CanonicalOrder {
        field: FullSemanticImageField,
        previous: u32,
        row: u32,
    },
    #[error("full semantic image typed node {node} has invalid role {role} at edge {edge}")]
    TypedRole { node: u32, edge: u32, role: u8 },
    #[error("full semantic image typed node {node} has invalid target tag {tag} at edge {edge}")]
    TypedTarget { node: u32, edge: u32, tag: u8 },
    #[error("full semantic image typed node {node} has invalid domain {domain}")]
    TypedDomain { node: u32, domain: u8 },
    #[error("full semantic image typed node {node} is semantically malformed")]
    TypedShape { node: u32 },
}

/// Exact composed reopening failure. Canonical planning and shared
/// header/provenance validation retain their established cause rather than
/// being recast as a generic full-row error.
#[derive(Debug, Error)]
pub enum FullSemanticImageError {
    #[error(transparent)]
    Core(#[from] super::super::fault::CoreSemanticImageFault),
    #[error(transparent)]
    Full(#[from] FullSemanticImageFault),
}
