//! Defines view error behavior for `backend-semantic::ir`, whose purpose is to encode, validate, map, and borrow canonical compiler IR fragments.
//! This module owns the view error invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::num::TryFromIntError;

use crate::ir::{EntityId, ProductChildRole, ProductConstructorFault, TypeId};
use backend_version::{DomainCode, HASH_BYTES};
use thiserror::Error;

use crate::ir::{
    AtomFault, EntityFault, EntityRecordFault, RecipeFactFault, SourceIdentityFault, TypeNodeFault,
    wire::SectionKind,
};

/// Wire coordinate whose decoded value could not be represented on this host.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireField {
    /// Total envelope byte length declared by the header.
    DeclaredLength,
    /// Item count in a directory entry at this zero-based entry ordinal.
    SectionItemCount {
        /// Zero-based directory entry whose item count was decoded.
        ordinal: u16,
    },
    /// Payload byte offset in a directory entry at this zero-based entry ordinal.
    SectionOffset {
        /// Zero-based directory entry whose payload offset was decoded.
        ordinal: u16,
    },
    /// Payload byte length in a directory entry at this zero-based entry ordinal.
    SectionByteLength {
        /// Zero-based directory entry whose payload length was decoded.
        ordinal: u16,
    },
    /// Atom-pool start offset in the record for this atom ordinal.
    AtomStart {
        /// Atom coordinate whose encoded start offset could not be represented.
        ordinal: crate::ir::AtomId,
    },
    /// Atom-pool byte length in the record for this atom ordinal.
    AtomLength {
        /// Atom coordinate whose encoded byte length could not be represented.
        ordinal: crate::ir::AtomId,
    },
}

/// Rejection of section-directory ordering or geometry.
#[derive(Debug, Eq, Error, PartialEq)]
pub enum DirectoryFault {
    #[error("section kind {actual} does not follow {previous}")]
    /// Kinds must be strictly increasing by their encoded `u16` value.
    Order {
        /// Encoded kind in the preceding directory row.
        previous: u16,
        /// Encoded kind in the current directory row.
        actual: u16,
    },
    #[error("section {kind} has invalid flags {actual}")]
    /// Requirement flags contain an unsupported encoded value.
    Flags {
        /// Encoded section kind.
        kind: u16,
        /// Requirement bits read from the directory row.
        actual: u16,
    },
    #[error("unknown section {kind} is marked required")]
    /// An unrecognized section cannot be skipped when marked required.
    RequiredUnknown {
        /// Unrecognized encoded section kind.
        kind: u16,
    },
    #[error("section {kind} starts at {actual}, not canonical offset {expected}")]
    /// A section does not begin at the contiguous canonical offset.
    Offset {
        /// Encoded section kind.
        kind: u16,
        /// Canonical payload offset required for this directory position, in host bytes.
        expected: usize,
        /// Offset declared in the wire entry.
        actual: u32,
    },
    #[error("section {kind} has {actual} bytes, not {expected}")]
    /// The lane's byte size disagrees with its count and record width.
    ByteLength {
        /// Encoded section kind.
        kind: u16,
        /// Number of bytes required by the section's declared items.
        expected: usize,
        /// Byte length declared by the wire entry.
        actual: u32,
    },
    #[error("section {kind} has {actual} records, not {expected}")]
    /// A fixed-record lane's item count disagrees with the fragment header.
    Count {
        /// Encoded section kind.
        kind: u16,
        /// Item count derived from the fragment's canonical geometry.
        expected: u32,
        /// Item count declared by the wire entry.
        actual: u32,
    },
    #[error("section {kind} count {count} times width {width} overflows")]
    /// Multiplying a declared record count by its fixed record width overflowed.
    ///
    /// The operands are the original wire count and the record width in bytes.
    CountWidthOverflow {
        /// Encoded section kind.
        kind: u16,
        /// Item count read from the directory row.
        count: u32,
        /// Fixed record width used to compute the required lane size.
        width: usize,
    },
    #[error("section {kind} range {start}+{length} overflows")]
    /// Adding the encoded start and byte length overflowed the wire coordinate width.
    RangeOverflow {
        /// Encoded section kind.
        kind: u16,
        /// Payload start offset from the directory row.
        start: u32,
        /// Payload length from the directory row.
        length: u32,
    },
}

/// Exact semantic-data section rejection retaining every wire operand.
///
/// Ordinals stay raw `u32` wire values because a rejected section may claim
/// counts that never form valid typed coordinates.
#[derive(Debug, Eq, Error, PartialEq)]
pub enum SemanticDataFault {
    #[error("semantic data header needs {required} bytes but only {actual} are present")]
    /// The section ends before its fixed semantic-data header is complete.
    Header {
        /// Minimum header byte length needed for decoding.
        required: usize,
        /// Header bytes available in the section.
        actual: usize,
    },
    #[error("semantic atom {ordinal} declares {length} bytes but only {available} remain")]
    /// A semantic atom's declared byte string extends beyond the remaining lane.
    AtomLength {
        /// Zero-based atom record ordinal.
        ordinal: u32,
        /// Byte length declared by this atom record.
        length: u32,
        /// Bytes remaining in the section when the record was read.
        available: usize,
    },
    #[error("semantic product {product} head {target} is outside atom count {atom_count}")]
    /// A product head names an atom ordinal outside the atom pool.
    ProductHead {
        /// Zero-based product row ordinal.
        product: u32,
        /// Atom ordinal encoded as the product head.
        target: u32,
        /// Number of atom rows declared by the section.
        atom_count: u32,
    },
    #[error("semantic product {product} list {target} is outside list count {list_count}")]
    /// A product names a child-list ordinal outside the list table.
    ProductList {
        /// Zero-based product row ordinal.
        product: u32,
        /// Child-list ordinal encoded by the product.
        target: u32,
        /// Number of child-list rows declared by the section.
        list_count: u32,
    },
    #[error(
        "semantic constructor count {constructor_count} does not equal product count {product_count}"
    )]
    /// The constructor table does not contain exactly one row per product row.
    ConstructorCount {
        /// Number of product rows declared by the semantic-data header.
        product_count: u32,
        /// Number of constructor rows present in the lane.
        constructor_count: u32,
    },
    #[error("semantic entity-root count {actual} does not match entity count {expected}")]
    /// Root table cardinality must equal the fragment entity count.
    EntityRootCount {
        /// Number of entities in the enclosing fragment.
        expected: u32,
        /// Number of root entries encoded in the semantic-data section.
        actual: u32,
    },
    #[error(
        "semantic entity {entity} root product {target} is outside product count {product_count}"
    )]
    /// An entity's root ordinal does not identify a product row.
    EntityRoot {
        /// Zero-based fragment entity ordinal.
        entity: u32,
        /// Product ordinal encoded as this entity's root.
        target: u32,
        /// Number of product rows in the section.
        product_count: u32,
    },
    #[error("semantic product {product} constructor is invalid: {fault:?}")]
    /// A constructor encoding is invalid for this product's semantic kind.
    Constructor {
        /// Zero-based product row ordinal.
        product: u32,
        /// Exact constructor admission fault.
        fault: ProductConstructorFault,
    },
    #[error("semantic list {list} span {start}+{length} is outside child count {child_count}")]
    /// A child-list span extends beyond the child table.
    ListExtent {
        /// Zero-based child-list row ordinal.
        list: u32,
        /// First child ordinal selected by this span.
        start: u32,
        /// Number of child rows selected by this span.
        length: u32,
        /// Number of child rows present in the table.
        child_count: u32,
    },
    #[error("semantic child {child} has unknown role byte {actual}")]
    /// A child row's role byte is not a defined `ProductChildRole` code.
    ChildRoleCode {
        /// Zero-based child row ordinal.
        child: u32,
        /// Role byte read from this child row.
        actual: u8,
    },
    #[error(
        "semantic child {child} carries role {actual:?} but its constructor position requires {expected:?}"
    )]
    /// A child role differs from the role prescribed by its constructor slot.
    ChildRole {
        /// Zero-based child row ordinal.
        child: u32,
        /// Role required at this child position.
        expected: ProductChildRole,
        /// Role decoded from the child row.
        actual: ProductChildRole,
    },
    #[error("semantic child {child} has unknown tag {actual}")]
    /// A child target tag is not a defined local or external tag.
    ChildTag {
        /// Zero-based child row ordinal.
        child: u32,
        /// Target tag byte read from this child row.
        actual: u8,
    },
    #[error("semantic local child {child} targets product {target} outside count {product_count}")]
    /// A local child targets a nonexistent product ordinal.
    LocalChild {
        /// Zero-based child row ordinal.
        child: u32,
        /// Product ordinal encoded by the child.
        target: u32,
        /// Number of product rows in the section.
        product_count: u32,
    },
    #[error("semantic local child {child} has nonzero external bytes {actual:?}")]
    /// Local child rows must leave the external identity bytes zero-filled.
    LocalReserved {
        /// Zero-based child row ordinal.
        child: u32,
        /// Complete reserved identity cell as observed on the wire.
        actual: [u8; HASH_BYTES],
    },
    #[error("semantic external child {child} authority is {observed}, expected {expected}")]
    /// The external child identity does not carry the required domain authority.
    ExternalAuthority {
        /// Zero-based child row ordinal.
        child: u32,
        /// Domain code required by the external-child encoding.
        expected: u8,
        /// Domain code observed in the identity cell.
        observed: u8,
        /// Complete hash-sized identity cell retained for diagnosis.
        raw: [u8; HASH_BYTES],
    },
    #[error("semantic data has {actual} trailing bytes after its declared lanes")]
    /// Bytes remain after all header-declared semantic lanes have been consumed.
    Trailing {
        /// Number of bytes remaining after all declared lanes.
        actual: usize,
    },
}

/// Rejection of a fragment before a validated borrowed view can be returned.
#[derive(Debug, Eq, Error, PartialEq)]
pub enum FragmentError {
    #[error(
        "language extensions and extension pools must occur together (extensions={extensions}, pools={pools})"
    )]
    /// The paired optional extension sections are either both present or both absent.
    ExtensionPoolPair {
        /// Whether the language-extension section was present.
        extensions: bool,
        /// Whether the extension-pooled-lanes section was present.
        pools: bool,
    },
    #[error("documentation lane rejected: {fault}")]
    /// Documentation fact grammar rejected its section.
    Documentation {
        /// Detailed rejection from the documentation fact reader.
        #[source]
        fault: crate::ir::docs_facts::DocFactFault,
    },
    #[error("extension pooled lanes rejected: {fault}")]
    /// Shared byte pools referenced by language extensions failed admission.
    ExtensionPools {
        /// Detailed rejection from pooled-lane grammar or reference validation.
        #[source]
        fault: crate::ir::extension_pools::ExtensionPoolFault,
    },
    #[error("language extension section rejected: {fault}")]
    /// Language-extension rows or their pool references failed reopening.
    LanguageExtensions {
        /// Detailed failure while reopening language extension columns.
        #[source]
        fault: crate::ir::LanguageExtensionReopenError,
    },
    #[error("fragment header needs {required} bytes but only {actual} are present")]
    /// The envelope is shorter than the fixed header needed to read its geometry.
    TruncatedHeader {
        /// Minimum number of envelope bytes needed to read the fixed header.
        required: usize,
        /// Number of envelope bytes supplied.
        actual: usize,
    },
    #[error("fragment magic {actual:?} is unknown")]
    /// The four-byte envelope marker does not identify a supported fragment.
    Magic {
        /// Four-byte marker read from the supplied envelope.
        actual: [u8; 4],
    },
    #[error("fragment schema {actual} is unknown")]
    /// The encoded schema version is unsupported by this reader.
    Schema {
        /// Schema number read from the fragment header.
        actual: u16,
    },
    #[error("fragment declares {declared} bytes but received {actual}")]
    /// Header-declared envelope length differs from the supplied slice length.
    DeclaredLength {
        /// Envelope byte length claimed by the header.
        declared: usize,
        /// Actual length of the supplied byte slice.
        actual: usize,
    },
    #[error("fragment requires extent {required} but received {actual} bytes")]
    /// A directory-selected lane extends past the supplied envelope.
    Extent {
        /// Minimum envelope length required to contain the selected section.
        required: usize,
        /// Actual supplied envelope length.
        actual: usize,
    },
    #[error("wire field {field:?} value {actual} does not fit this platform")]
    /// A `u32` wire coordinate could not be converted to a host `usize`.
    WireWidth {
        /// Header or record field whose conversion failed.
        field: WireField,
        /// Original coordinate read from the wire.
        actual: u32,
        /// Host conversion failure explaining why the value was not representable.
        #[source]
        source: TryFromIntError,
    },
    #[error("directory entry {ordinal} is invalid: {fault}")]
    /// One directory row failed ordering, flags, count, or byte-range admission.
    Directory {
        /// Zero-based directory row ordinal.
        ordinal: u16,
        /// Exact ordering, flag, count, or geometry failure from this directory row.
        #[source]
        fault: DirectoryFault,
    },
    #[error("required section {section:?} is missing")]
    /// The fragment omitted a section required by the decoded schema.
    MissingSection {
        /// Required section kind absent from the directory.
        section: SectionKind,
    },
    #[error("entity {ordinal:?} is invalid: {fault}")]
    /// A decoded entity row refers to a nonexistent type or violates entity invariants.
    Entity {
        /// Entity row ordinal in the fragment.
        ordinal: EntityId,
        /// Semantic invariant violated by the decoded entity coordinates.
        #[source]
        fault: EntityFault,
    },
    #[error("entity {ordinal:?} is invalid: {fault}")]
    /// An entity row failed while its fixed wire record was decoded.
    EntityRecord {
        /// Entity row ordinal in the fragment.
        ordinal: EntityId,
        /// Decode failure from the fixed-width entity record.
        #[source]
        fault: EntityRecordFault,
    },
    #[error("atom is invalid: {fault}")]
    /// Atom records or their byte-pool ranges are invalid.
    Atom {
        /// Exact atom-record or pool-range rejection.
        #[source]
        fault: AtomFault,
    },
    #[error("type node {ordinal:?} is invalid: {fault}")]
    /// A type-node row failed decoding or semantic validation.
    TypeNode {
        /// Type-node row ordinal in the fragment.
        ordinal: TypeId,
        /// Decode or semantic failure for this type-node record.
        #[source]
        fault: TypeNodeFault,
    },
    #[error("source identity is invalid: {fault}")]
    /// The required source identity lane failed decoding.
    SourceIdentity {
        /// Exact rejection from the source-identity decoder.
        #[source]
        fault: SourceIdentityFault,
    },
    #[error("recipe fact is invalid: {fault}")]
    /// The required compiler recipe lane failed decoding.
    RecipeFact {
        /// Exact rejection from the recipe-fact decoder.
        #[source]
        fault: RecipeFactFault,
    },
    #[error("semantic data is invalid: {fault}")]
    /// The semantic-product section failed grammar or reference validation.
    SemanticData {
        /// Exact grammar or cross-reference failure in the semantic-data lane.
        #[source]
        fault: SemanticDataFault,
    },
    #[error("occurrence plane is invalid: {fault}")]
    /// The optional occurrence fact section contains an invalid row or count.
    Occurrences {
        /// Exact occurrence-plane admission failure.
        #[source]
        fault: OccurrenceFault,
    },
    #[error("type-fact plane is invalid: {fault}")]
    /// The optional type-fact section failed admission.
    TypeFacts {
        /// Exact type-fact-plane admission failure.
        #[source]
        fault: crate::ir::TypeFactFault,
    },
}

/// Exact occurrence-plane section rejection retaining every Copy wire
/// operand. Authority failures retain the expected domain code or the
/// complete observed width; richer admission faults stay in
/// `semantic_facts::OccurrenceFault`.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum OccurrenceFault {
    #[error(
        "occurrence {ordinal} names entity {owner} outside the fragment entity lane of {entity_count}"
    )]
    /// An occurrence owner does not identify an entity row in the fragment.
    Owner {
        /// Zero-based occurrence record ordinal.
        ordinal: u32,
        /// Entity ordinal encoded as the occurrence owner.
        owner: u32,
        /// Number of entity rows in the fragment.
        entity_count: u32,
    },
    #[error(
        "occurrence {ordinal} names local target {target} outside the fragment entity lane of {entity_count}"
    )]
    /// A local occurrence target does not identify an entity row in the fragment.
    LocalTarget {
        /// Zero-based occurrence record ordinal.
        ordinal: u32,
        /// Entity ordinal encoded as the local target.
        target: u32,
        /// Number of entity rows in the fragment.
        entity_count: u32,
    },
    #[error("occurrence {ordinal} carries an unknown target tag {actual}")]
    /// Target tag or its associated cells cannot be decoded as a supported target.
    TargetTag {
        /// Zero-based occurrence record ordinal.
        ordinal: u32,
        /// Target tag byte read from this record.
        actual: u8,
    },
    #[error("occurrence {ordinal} carries an unknown foreign origin tag {actual}")]
    /// Foreign-origin tag or its associated cells cannot be decoded as a supported origin.
    OriginTag {
        /// Zero-based occurrence record ordinal.
        ordinal: u32,
        /// Foreign-origin tag byte read from this record.
        actual: u8,
    },
    #[error("occurrence {ordinal} carries an unknown reference kind {actual}")]
    /// Reference-kind byte is not a defined occurrence kind.
    ReferenceKind {
        /// Zero-based occurrence record ordinal.
        ordinal: u32,
        /// Reference-kind byte read from this record.
        actual: u8,
    },
    #[error("occurrence {ordinal} carries an unknown confidence {actual}")]
    /// Confidence byte is not a defined evidence-confidence value.
    Confidence {
        /// Zero-based occurrence record ordinal.
        ordinal: u32,
        /// Confidence byte read from this record.
        actual: u8,
    },
    #[error("occurrence {ordinal} carries an inverted relative span {start}..{end}")]
    /// Relative source-span end precedes its start.
    Span {
        /// Zero-based occurrence record ordinal.
        ordinal: u32,
        /// Relative source span start from the wire.
        start: u32,
        /// Relative source span end from the wire.
        end: u32,
    },
    #[error("occurrence {ordinal} carries an unknown foreign kind cell {actual}")]
    /// Foreign kind cell is not a supported external declaration kind.
    KindCell {
        /// Zero-based occurrence record ordinal.
        ordinal: u32,
        /// Foreign kind code read from the occurrence record.
        actual: u16,
    },
    #[error("occurrence {ordinal} foreign key has an empty path")]
    /// Foreign identity requires a non-empty encoded path.
    EmptyPath {
        /// Zero-based occurrence record ordinal.
        ordinal: u32,
    },
    #[error("occurrence record {ordinal} payload ended before {needed} bytes")]
    /// A variable-width occurrence record ended before its required payload.
    Truncated {
        /// Zero-based occurrence record ordinal.
        ordinal: u32,
        /// Minimum section-payload length needed to satisfy the current read.
        needed: usize,
    },
    #[error("occurrence section declares {declared} records but carries trailing bytes")]
    /// The declared record count was consumed with bytes left over in the section.
    TrailingBytes {
        /// Number of occurrence records claimed by the section header.
        declared: u32,
    },
    #[error(
        "occurrence {ordinal} uses a schema-{schema} family-only stable endpoint that cannot represent an exact declaration variant"
    )]
    /// Older schemas can identify only a stable family, not the exact declaration variant.
    LegacyStableTarget {
        /// Zero-based occurrence record ordinal.
        ordinal: u32,
        /// Fragment schema whose endpoint form lacks exact declaration identity.
        schema: u16,
    },
    #[error(
        "occurrence {ordinal} identity authority cell must encode domain {expected:?} but observes code {observed}"
    )]
    /// Identity cell uses a different domain code from the one required for this occurrence.
    AuthorityDomain {
        /// Occurrence coordinate supplied by the view-mirror conversion; the shared
        /// content-ID decoder currently loses the source row and reports zero here.
        ordinal: u32,
        /// Content-identity domain expected by the wire field.
        expected: DomainCode,
        /// Encoded domain byte observed in the identity cell.
        observed: u8,
        /// Complete serialized hash-sized content identity, including its authority byte.
        raw: [u8; HASH_BYTES],
    },
    #[error("occurrence {ordinal} identity cell carries {actual} bytes instead of 32")]
    /// Decoded identity payload does not have the required 32-byte digest width.
    AuthorityWidth {
        /// Occurrence coordinate supplied by the view-mirror conversion; the shared
        /// content-ID decoder currently loses the source row and reports zero here.
        ordinal: u32,
        /// Complete serialized identity width observed by the decoder.
        actual: usize,
    },
}
