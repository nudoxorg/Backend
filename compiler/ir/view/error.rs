//! Defines view error behavior for `compiler-ir`, whose purpose is to encode, validate, map, and borrow canonical compiler IR fragments.
//! This module owns the view error invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::num::TryFromIntError;

use crate::{EntityId, ProductChildRole, ProductConstructorFault, TypeId};
use heart_identity::{DomainCode, HASH_BYTES};
use thiserror::Error;

use crate::{
    AtomFault, EntityFault, EntityRecordFault, RecipeFactFault, SourceIdentityFault, TypeNodeFault,
    wire::SectionKind,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireField {
    DeclaredLength,
    SectionItemCount { ordinal: u16 },
    SectionOffset { ordinal: u16 },
    SectionByteLength { ordinal: u16 },
    AtomStart { ordinal: crate::AtomId },
    AtomLength { ordinal: crate::AtomId },
}

#[derive(Debug, Eq, Error, PartialEq)]
pub enum DirectoryFault {
    #[error("section kind {actual} does not follow {previous}")]
    Order { previous: u16, actual: u16 },
    #[error("section {kind} has invalid flags {actual}")]
    Flags { kind: u16, actual: u16 },
    #[error("unknown section {kind} is marked required")]
    RequiredUnknown { kind: u16 },
    #[error("section {kind} starts at {actual}, not canonical offset {expected}")]
    Offset {
        kind: u16,
        expected: usize,
        actual: u32,
    },
    #[error("section {kind} has {actual} bytes, not {expected}")]
    ByteLength {
        kind: u16,
        expected: usize,
        actual: u32,
    },
    #[error("section {kind} has {actual} records, not {expected}")]
    Count {
        kind: u16,
        expected: u32,
        actual: u32,
    },
    #[error("section {kind} count {count} times width {width} overflows")]
    CountWidthOverflow { kind: u16, count: u32, width: usize },
    #[error("section {kind} range {start}+{length} overflows")]
    RangeOverflow { kind: u16, start: u32, length: u32 },
}

/// Exact semantic-data section rejection retaining every wire operand.
///
/// Ordinals stay raw `u32` wire values because a rejected section may claim
/// counts that never form valid typed coordinates.
#[derive(Debug, Eq, Error, PartialEq)]
pub enum SemanticDataFault {
    #[error("semantic data header needs {required} bytes but only {actual} are present")]
    Header { required: usize, actual: usize },
    #[error("semantic atom {ordinal} declares {length} bytes but only {available} remain")]
    AtomLength {
        ordinal: u32,
        length: u32,
        available: usize,
    },
    #[error("semantic product {product} head {target} is outside atom count {atom_count}")]
    ProductHead {
        product: u32,
        target: u32,
        atom_count: u32,
    },
    #[error("semantic product {product} list {target} is outside list count {list_count}")]
    ProductList {
        product: u32,
        target: u32,
        list_count: u32,
    },
    #[error(
        "semantic constructor count {constructor_count} does not equal product count {product_count}"
    )]
    ConstructorCount {
        product_count: u32,
        constructor_count: u32,
    },
    #[error("semantic entity-root count {actual} does not match entity count {expected}")]
    EntityRootCount { expected: u32, actual: u32 },
    #[error("semantic entity {entity} root product {target} is outside product count {product_count}")]
    EntityRoot {
        entity: u32,
        target: u32,
        product_count: u32,
    },
    #[error("semantic product {product} constructor is invalid: {fault:?}")]
    Constructor {
        product: u32,
        fault: ProductConstructorFault,
    },
    #[error("semantic list {list} span {start}+{length} is outside child count {child_count}")]
    ListExtent {
        list: u32,
        start: u32,
        length: u32,
        child_count: u32,
    },
    #[error("semantic child {child} has unknown role byte {actual}")]
    ChildRoleCode { child: u32, actual: u8 },
    #[error(
        "semantic child {child} carries role {actual:?} but its constructor position requires {expected:?}"
    )]
    ChildRole {
        child: u32,
        expected: ProductChildRole,
        actual: ProductChildRole,
    },
    #[error("semantic child {child} has unknown tag {actual}")]
    ChildTag { child: u32, actual: u8 },
    #[error("semantic local child {child} targets product {target} outside count {product_count}")]
    LocalChild {
        child: u32,
        target: u32,
        product_count: u32,
    },
    #[error("semantic local child {child} has nonzero external bytes {actual:?}")]
    LocalReserved {
        child: u32,
        actual: [u8; HASH_BYTES],
    },
    #[error("semantic external child {child} authority is {observed}, expected {expected}")]
    ExternalAuthority {
        child: u32,
        expected: u8,
        observed: u8,
        raw: [u8; HASH_BYTES],
    },
    #[error("semantic data has {actual} trailing bytes after its declared lanes")]
    Trailing { actual: usize },
}

#[derive(Debug, Eq, Error, PartialEq)]
pub enum FragmentError {
    #[error(
        "language extensions and extension pools must occur together (extensions={extensions}, pools={pools})"
    )]
    ExtensionPoolPair { extensions: bool, pools: bool },
    #[error("documentation lane rejected: {fault}")]
    Documentation {
        #[source]
        fault: crate::docs_facts::DocFactFault,
    },
    #[error("extension pooled lanes rejected: {fault}")]
    ExtensionPools {
        #[source]
        fault: crate::extension_pools::ExtensionPoolFault,
    },
    #[error("language extension section rejected: {fault}")]
    LanguageExtensions {
        #[source]
        fault: crate::LanguageExtensionReopenError,
    },
    #[error("fragment header needs {required} bytes but only {actual} are present")]
    TruncatedHeader { required: usize, actual: usize },
    #[error("fragment magic {actual:?} is unknown")]
    Magic { actual: [u8; 4] },
    #[error("fragment schema {actual} is unknown")]
    Schema { actual: u16 },
    #[error("fragment declares {declared} bytes but received {actual}")]
    DeclaredLength { declared: usize, actual: usize },
    #[error("fragment requires extent {required} but received {actual} bytes")]
    Extent { required: usize, actual: usize },
    #[error("wire field {field:?} value {actual} does not fit this platform")]
    WireWidth {
        field: WireField,
        actual: u32,
        #[source]
        source: TryFromIntError,
    },
    #[error("directory entry {ordinal} is invalid: {fault}")]
    Directory {
        ordinal: u16,
        #[source]
        fault: DirectoryFault,
    },
    #[error("required section {section:?} is missing")]
    MissingSection { section: SectionKind },
    #[error("entity {ordinal:?} is invalid: {fault}")]
    Entity {
        ordinal: EntityId,
        #[source]
        fault: EntityFault,
    },
    #[error("entity {ordinal:?} is invalid: {fault}")]
    EntityRecord {
        ordinal: EntityId,
        #[source]
        fault: EntityRecordFault,
    },
    #[error("atom is invalid: {fault}")]
    Atom {
        #[source]
        fault: AtomFault,
    },
    #[error("type node {ordinal:?} is invalid: {fault}")]
    TypeNode {
        ordinal: TypeId,
        #[source]
        fault: TypeNodeFault,
    },
    #[error("source identity is invalid: {fault}")]
    SourceIdentity {
        #[source]
        fault: SourceIdentityFault,
    },
    #[error("recipe fact is invalid: {fault}")]
    RecipeFact {
        #[source]
        fault: RecipeFactFault,
    },
    #[error("semantic data is invalid: {fault}")]
    SemanticData {
        #[source]
        fault: SemanticDataFault,
    },
    #[error("occurrence plane is invalid: {fault}")]
    Occurrences {
        #[source]
        fault: OccurrenceFault,
    },
    #[error("type-fact plane is invalid: {fault}")]
    TypeFacts {
        #[source]
        fault: crate::TypeFactFault,
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
    Owner {
        ordinal: u32,
        owner: u32,
        entity_count: u32,
    },
    #[error(
        "occurrence {ordinal} names local target {target} outside the fragment entity lane of {entity_count}"
    )]
    LocalTarget {
        ordinal: u32,
        target: u32,
        entity_count: u32,
    },
    #[error("occurrence {ordinal} carries an unknown target tag {actual}")]
    TargetTag { ordinal: u32, actual: u8 },
    #[error("occurrence {ordinal} carries an unknown foreign origin tag {actual}")]
    OriginTag { ordinal: u32, actual: u8 },
    #[error("occurrence {ordinal} carries an unknown reference kind {actual}")]
    ReferenceKind { ordinal: u32, actual: u8 },
    #[error("occurrence {ordinal} carries an unknown confidence {actual}")]
    Confidence { ordinal: u32, actual: u8 },
    #[error("occurrence {ordinal} carries an inverted relative span {start}..{end}")]
    Span { ordinal: u32, start: u32, end: u32 },
    #[error("occurrence {ordinal} carries an unknown foreign kind cell {actual}")]
    KindCell { ordinal: u32, actual: u16 },
    #[error("occurrence {ordinal} foreign key has an empty path")]
    EmptyPath { ordinal: u32 },
    #[error("occurrence record {ordinal} payload ended before {needed} bytes")]
    Truncated { ordinal: u32, needed: usize },
    #[error("occurrence section declares {declared} records but carries trailing bytes")]
    TrailingBytes { declared: u32 },
    #[error(
        "occurrence {ordinal} identity authority cell must encode domain {expected:?} but observes code {observed}"
    )]
    AuthorityDomain {
        ordinal: u32,
        expected: DomainCode,
        observed: u8,
        raw: [u8; heart_identity::HASH_BYTES],
    },
    #[error("occurrence {ordinal} identity cell carries {actual} bytes instead of 32")]
    AuthorityWidth { ordinal: u32, actual: usize },
}
