use core::num::TryFromIntError;

use nudox_ir_vocab::{EntityId, TypeId};
use thiserror::Error;

use crate::{
    AtomFault, EntityFault, EntityRecordFault, SourceIdentityFault, TypeNodeFault,
    wire::SectionKind,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireField {
    DeclaredLength,
    SectionItemCount { ordinal: u16 },
    SectionOffset { ordinal: u16 },
    SectionByteLength { ordinal: u16 },
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

#[derive(Debug, Eq, Error, PartialEq)]
pub enum FragmentError {
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
}
