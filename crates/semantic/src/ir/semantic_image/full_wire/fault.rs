//! Closed full-image grammar diagnostics.

use thiserror::Error;

use super::wire::FullDirectoryKind;

/// Exact full-image lane retained by every portable grammar failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FullSemanticImageField {
    /// Fixed image header.
    Header,
    /// Language and schema authority bytes.
    Authority,
    /// Source, recipe, and content-identity provenance.
    Provenance,
    /// Fixed directory of lane locations and counts.
    Directory,
    /// Interned atom records and their byte pool.
    Atoms,
    /// Full entity rows.
    Entities,
    /// Canonical typed dependency nodes.
    TypedNodes,
    /// Edges from typed dependency nodes.
    TypedEdges,
    /// Interned entity-list records.
    EntityLists,
    /// Documentation records and fragments.
    Documentation,
    /// External declaration identities.
    Externals,
    /// Canonical relation rows.
    Links,
    /// Relation occurrence evidence rows.
    Occurrences,
    /// Sparse extension fact rows.
    ExtensionFacts,
    /// Sparse entity-to-extension-fact bindings.
    ExtensionBindings,
    /// Packed signature-carrier role bits.
    SignatureCarrierRoles,
    /// Signature-carrier parameter/result binding ranges.
    SignatureCarrierBindingRanges,
    /// Signature-carrier parameter/result target entity rows.
    SignatureCarrierBindingTargets,
}

/// Exact content-identity cell whose domain tag failed to reopen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FullSemanticImageIdentityField {
    /// Stable identity for the source fragment.
    StableFragment,
    /// Identity for one fragment entity.
    FragmentEntityFragment,
}

/// Full-image fault.  Every row/reference carries both its semantic lane and
/// exact observed coordinate; no generic "invalid image" terminal erases the
/// producer's source.
#[derive(Debug, Error)]
pub enum FullSemanticImageFault {
    #[error("full semantic image output needs {required} bytes but only {actual} were supplied")]
    /// Caller-provided output buffer cannot hold the encoded image.
    OutputTooShort {
        /// Minimum output capacity required by the computed layout.
        required: usize,
        /// Supplied output capacity.
        actual: usize,
    },
    #[error("full semantic image length overflow while measuring {field:?}")]
    /// Checked image byte geometry overflowed while measuring a lane.
    LengthOverflow {
        /// Lane whose byte geometry overflowed.
        field: FullSemanticImageField,
    },
    #[error("full semantic image is truncated while reading {field:?} at byte {offset}")]
    /// Image bytes ended before the requested lane value was present.
    Truncated {
        /// Lane containing the incomplete value.
        field: FullSemanticImageField,
        /// Zero-based byte offset from the image start where the incomplete read began.
        offset: usize,
    },
    #[error("full semantic image magic is {observed:?}, not {expected:?}")]
    /// Four-byte image marker does not match the supported full-image magic.
    Magic {
        /// Supported marker required by this reader.
        expected: [u8; 4],
        /// Marker read from the image header.
        observed: [u8; 4],
    },
    #[error("full semantic image schema is {observed}, not {expected}")]
    /// Encoded schema is unsupported by this reader.
    Schema {
        /// Schema version required by the reader.
        expected: u16,
        /// Schema version read from the header.
        observed: u16,
    },
    #[error("full semantic image claims {claimed} bytes but input has {actual}")]
    /// Header-declared total byte length differs from the supplied image.
    Length {
        /// Total byte length claimed by the image header.
        claimed: u32,
        /// Actual length of the supplied byte slice.
        actual: usize,
    },
    #[error("full semantic image directory count is {observed}, not {expected}")]
    /// Directory does not contain the schema-required number of entries.
    DirectoryCount {
        /// Number of entries required by the schema.
        expected: u16,
        /// Number of entries declared by the image.
        observed: u16,
    },
    #[error("full semantic image directory {entry} has kind {observed}, expected {expected:?}")]
    /// Directory entry kind does not match its canonical position.
    DirectoryKind {
        /// Zero-based directory entry ordinal.
        entry: u16,
        /// Kind required at this directory position.
        expected: FullDirectoryKind,
        /// Encoded kind value found in this entry.
        observed: u16,
    },
    #[error(
        "full semantic image directory {kind:?} range {offset}+{length} is outside {image_bytes}"
    )]
    /// Directory lane range extends beyond the declared full image.
    DirectoryRange {
        /// Lane named by the directory entry.
        kind: FullDirectoryKind,
        /// Lane start offset from the image start.
        offset: u32,
        /// Lane byte length from the entry.
        length: u32,
        /// Total image byte length used as the range boundary.
        image_bytes: usize,
    },
    #[error("full semantic image directory {kind:?} count is {observed}, expected {expected}")]
    /// Lane item count differs from the count derived from image geometry.
    DirectoryCountLane {
        /// Lane named by this count entry.
        kind: FullDirectoryKind,
        /// Count required by the schema and lane layout.
        expected: u32,
        /// Count declared in the directory entry.
        observed: u32,
    },
    #[error("full semantic image {field:?} row {row} references {observed}, outside {expected}")]
    /// A row coordinate points outside its referenced lane or table.
    Reference {
        /// Lane containing the reference.
        field: FullSemanticImageField,
        /// Zero-based source row ordinal.
        row: u32,
        /// Number of rows available in the referenced domain.
        expected: u32,
        /// Referenced row coordinate encoded in the source row.
        observed: u32,
    },
    #[error("full semantic image {field:?} row {row} has discriminant {observed}")]
    /// A row tag is not one of the values allowed for this lane.
    Discriminant {
        /// Lane containing the invalid tag.
        field: FullSemanticImageField,
        /// Zero-based row ordinal containing the tag.
        row: u32,
        /// Encoded discriminant byte.
        observed: u8,
    },
    #[error("full semantic image {field:?} row {row} has invalid {identity:?} content identity")]
    /// A content identity has the wrong authority code or digest framing.
    ContentIdentity {
        /// Lane containing the invalid identity.
        field: FullSemanticImageField,
        /// Zero-based row ordinal containing the identity.
        row: u32,
        /// Identity role within the selected row.
        identity: FullSemanticImageIdentityField,
        /// Domain or digest framing failure from the identity decoder.
        #[source]
        cause: backend_version::ContentIdDecodeError,
    },
    #[error("full semantic image entity {row} has kind code {observed}")]
    /// Entity kind code does not identify a supported IR item kind.
    EntityKind {
        /// Zero-based entity row ordinal.
        row: u32,
        /// Encoded entity-kind code.
        observed: u16,
    },
    #[error("full semantic image signature-role lane has {observed} rows, expected {expected}")]
    /// Packed role lane does not have one role cell per entity row.
    SignatureCarrierRoleCount {
        /// Number of role cells required by entity geometry.
        expected: u32,
        /// Number of role cells declared by the lane.
        observed: u32,
    },
    #[error("full semantic image signature-role lane has {observed} bytes, expected {expected}")]
    /// Packed role lane byte length does not match its entity count.
    SignatureCarrierRoleLength {
        /// Number of bytes required to pack all entity role cells.
        expected: u32,
        /// Byte count declared for the role lane.
        observed: u32,
    },
    #[error(
        "full semantic image signature-role row {row} has role {role} on non-parameter kind {kind:?}"
    )]
    /// A signature role is assigned to an entity whose kind is not a parameter.
    SignatureCarrierRoleKind {
        /// Zero-based entity row ordinal.
        row: u32,
        /// Role code packed for this entity.
        role: u8,
        /// Entity kind decoded from the entity lane.
        kind: crate::ir::ItemKind,
    },
    #[error(
        "full semantic image signature-role byte {byte} has nonzero padding bits {observed:#04x}"
    )]
    /// Unused high bits in the final packed role byte must be zero.
    SignatureCarrierRolePadding {
        /// Zero-based byte offset within the packed role lane.
        byte: usize,
        /// Nonzero padding bits observed in that byte.
        observed: u8,
    },
    #[error("signature-carrier binding owner row {row} is {observed:?}, expected {expected:?}")]
    /// Binding-range owner rows differ from the function entity rows in canonical order.
    SignatureCarrierBindingOwnerSet {
        /// Zero-based binding-range row being examined.
        row: u32,
        /// Canonical row of the next function entity, or `None` when none remains.
        expected: Option<u32>,
        /// Canonical entity-row ordinal encoded in the binding-range row.
        observed: Option<u32>,
    },
    #[error(
        "signature-carrier binding range row {row} for owner {owner} has start {start}, counts {parameters}+{results}"
    )]
    /// An owner's range geometry or parameter/result counts disagree with its function signature.
    SignatureCarrierBindingRange {
        /// Zero-based binding-range row ordinal.
        row: u32,
        /// Entity row that owns this range.
        owner: u32,
        /// First target ordinal in the shared target pool, or `NONE` when both counts are zero.
        start: u32,
        /// Number of parameter targets in the range.
        parameters: u32,
        /// Number of result targets in the range.
        results: u32,
    },
    #[error("signature-carrier binding target pool has {observed} rows, expected {expected}")]
    /// Target pool cardinality differs from the sum of all binding ranges.
    SignatureCarrierBindingTargetCount {
        /// Number of target rows required by the ranges.
        expected: u32,
        /// Number of target rows present in the pool.
        observed: u32,
    },
    #[error(
        "signature-carrier target {target} at owner {owner} {role:?} slot {position} has kind {kind:?}"
    )]
    /// A signature-carrier slot targets an entity of the wrong item kind.
    SignatureCarrierBindingTargetKind {
        /// Entity row whose signature owns the slot.
        owner: u32,
        /// Whether the slot carries a parameter or result entity.
        role: crate::ir::SignatureCarrierBindingRole,
        /// Zero-based position within the owner and role.
        position: u32,
        /// Entity row referenced by this slot.
        target: u32,
        /// Kind decoded for the referenced entity row.
        kind: crate::ir::ItemKind,
    },
    #[error(
        "signature-carrier owner {owner} {role:?} slot {position} target {target} type {observed:?} differs from tuple type {expected:?}"
    )]
    /// A bound entity's type differs from the corresponding signature tuple entry.
    SignatureCarrierBindingType {
        /// Entity row whose signature owns the slot.
        owner: u32,
        /// Whether the slot belongs to the parameter or result tuple.
        role: crate::ir::SignatureCarrierBindingRole,
        /// Zero-based position within the owner and role.
        position: u32,
        /// Entity row referenced by the slot.
        target: u32,
        /// Type coordinate expected from the owner's signature tuple.
        expected: Option<crate::ir::TypeId>,
        /// Type coordinate recorded on the referenced entity row.
        observed: Option<crate::ir::TypeId>,
    },
    #[error("signature-carrier role union for entity {entity} is {observed}, expected {expected}")]
    /// Entity role-bit union differs from the roles represented by binding ranges.
    SignatureCarrierBindingRoleUnion {
        /// Entity row whose role bits disagree with bindings.
        entity: u32,
        /// Role bitset implied by the binding rows.
        expected: u8,
        /// Role bitset stored in the packed lane.
        observed: u8,
    },
    #[error("cannot allocate {bytes} bytes to validate signature-carrier role union")]
    /// Scratch role-union table could not be allocated for validation.
    SignatureCarrierBindingScratch {
        /// Scratch byte count requested by the validator.
        bytes: usize,
    },
    #[error("cannot reserve {bytes} bytes for signature-carrier image {field:?}")]
    /// Reserving memory for signature-carrier cross-plane validation failed.
    SignatureCarrierBindingAllocation {
        /// Lane-sized allocation associated with the failed reservation.
        field: FullSemanticImageField,
        /// Number of bytes the validator tried to reserve.
        bytes: usize,
        /// Allocation failure returned by the vector reservation.
        #[source]
        cause: alloc::collections::TryReserveError,
    },
    #[error("full semantic image entity {row} has invalid source span {start}..{end}")]
    /// Entity source span has its end before its start.
    SourceSpan {
        /// Zero-based entity row ordinal.
        row: u32,
        /// Relative byte offset where the source span begins.
        start: u32,
        /// Relative byte offset where the source span ends.
        end: u32,
    },
    #[error(
        "full semantic image entity {row} has inconsistent authority plane {plane} (claimed {claimed}, present {present})"
    )]
    /// Entity authority bits disagree with the presence of a fact plane.
    Authority {
        /// Zero-based entity row ordinal.
        row: u32,
        /// Authority-plane selector used to identify the failed validation check.
        plane: u8,
        /// Authority code claimed by the entity row.
        claimed: u8,
        /// Whether the selected fact plane actually contains this entity's fact.
        present: bool,
    },
    #[error("full semantic image entity {row} duplicates declaration identity at row {existing}")]
    /// Two entity rows carry the same declaration identity.
    DuplicateIdentity {
        /// Later duplicate entity row ordinal.
        row: u32,
        /// Earlier entity row with the same identity.
        existing: u32,
    },
    #[error("full semantic image {field:?} row {row} duplicates canonical row {existing}")]
    /// A row duplicates an earlier row under the lane's canonical key.
    DuplicateCanonicalRow {
        /// Lane in which the canonical-key collision occurred.
        field: FullSemanticImageField,
        /// Later duplicate row ordinal.
        row: u32,
        /// Earlier row with the same canonical key.
        existing: u32,
    },
    #[error(
        "full semantic image {plane:?} sparse binding row {row} repeats entity {entity} from row {existing}"
    )]
    /// Sparse binding rows repeat an entity for the same extension plane.
    DuplicateExtensionBinding {
        /// Sparse fact plane containing the duplicate binding.
        plane: FullDirectoryKind,
        /// Entity row named by both bindings.
        entity: u32,
        /// Earlier binding row that named the entity.
        existing: u32,
        /// Later duplicate binding row.
        row: u32,
    },
    #[error("full semantic image {plane:?} fact row {fact} is not bound by any declaration")]
    /// An extension fact row is not referenced by any entity binding.
    UnboundExtensionFact {
        /// Sparse fact plane containing the unbound row.
        plane: FullDirectoryKind,
        /// Zero-based fact row ordinal.
        fact: u32,
    },
    #[error(
        "full semantic image {plane:?} has {facts} facts/{fact_bytes} bytes and {bindings} bindings/{binding_bytes} bytes under incompatible authority {authority:?}"
    )]
    /// Sparse lanes contain rows or bytes for a language excluded by image authority.
    ExtensionPlaneAuthority {
        /// Sparse plane whose authority is inconsistent.
        plane: FullDirectoryKind,
        /// Image-level authority value read from the shared header.
        authority: crate::ir::SemanticImageAuthority,
        /// Number of fact records in the plane.
        facts: u32,
        /// Total fact-lane bytes declared by the directory.
        fact_bytes: u32,
        /// Number of entity-to-fact binding records.
        bindings: u32,
        /// Total binding-lane bytes declared by the directory.
        binding_bytes: u32,
    },
    #[error("full semantic image entity {entity} has a local parent cycle through {parent}")]
    /// An entity's local parent chain reaches an already visited entity.
    ParentCycle {
        /// Entity whose parent chain was being checked.
        entity: u32,
        /// Parent entity that closes the cycle.
        parent: u32,
    },
    #[error("full semantic image {field:?} row {row} has reserved byte {observed}")]
    /// A reserved lane byte is nonzero.
    Reserved {
        /// Lane containing the reserved byte.
        field: FullSemanticImageField,
        /// Zero-based row ordinal containing the byte.
        row: u32,
        /// Byte observed where the format requires zero.
        observed: u8,
    },
    #[error("full semantic image {field:?} row {row} is not canonical after row {previous}")]
    /// A lane row is not ordered canonically after its predecessor.
    CanonicalOrder {
        /// Lane whose ordering failed.
        field: FullSemanticImageField,
        /// Earlier row ordinal in the ordering comparison.
        previous: u32,
        /// Current row ordinal in the ordering comparison.
        row: u32,
    },
    #[error("full semantic image typed node {node} has invalid role {role} at edge {edge}")]
    /// Typed edge role code is unsupported for the source node.
    TypedRole {
        /// Typed-node row owning this edge.
        node: u32,
        /// Zero-based edge ordinal within the node.
        edge: u32,
        /// Encoded edge-role byte.
        role: u8,
    },
    #[error("full semantic image typed node {node} has invalid target tag {tag} at edge {edge}")]
    /// Typed edge target tag is not a defined node or terminal target.
    TypedTarget {
        /// Typed-node row owning this edge.
        node: u32,
        /// Zero-based edge ordinal within the node.
        edge: u32,
        /// Encoded target tag byte.
        tag: u8,
    },
    #[error("full semantic image typed node {node} has invalid domain {domain}")]
    /// Typed-node domain byte does not identify a supported pool domain.
    TypedDomain {
        /// Typed-node row ordinal.
        node: u32,
        /// Encoded typed-pool domain byte.
        domain: u8,
    },
    #[error("full semantic image typed node {node} is semantically malformed")]
    /// Typed node violates the shape rules for its decoded node kind.
    TypedShape {
        /// Typed-node row ordinal that failed shape validation.
        node: u32,
    },
}

/// Exact composed reopening failure. Canonical planning and shared
/// header/provenance validation retain their established cause rather than
/// being recast as a generic full-row error.
#[derive(Debug, Error)]
pub enum FullSemanticImageError {
    #[error(transparent)]
    /// Shared header or provenance admission rejected the image.
    Core(#[from] super::super::fault::CoreSemanticImageFault),
    #[error(transparent)]
    /// Full-image wire grammar or cross-plane validation rejected the image.
    Full(#[from] FullSemanticImageFault),
    #[error("full semantic image proof does not name these exact backing bytes")]
    /// A cached proof was presented with a different byte allocation or extent.
    ProofBackingMismatch,
}
