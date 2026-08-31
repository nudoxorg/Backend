//! Exercises the `heart-object-pack` tests support object-pack-index contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Declarative fixed wire rows shared by object-pack index tests.

use heart_identity::ObjectDomain;
use heart_object::{ObjectDescriptorWireRecord, ObjectKind, ObjectRef};
use heart_schema::SchemaId;
use zerocopy::{
    Immutable, IntoBytes, Unaligned,
    byteorder::{BigEndian, U64},
};

const FIXTURE_OBJECT_KIND: u16 = 7;

#[repr(C)]
#[derive(Clone, Copy, Immutable, IntoBytes, Unaligned)]
pub(crate) struct TestDirectoryRow {
    pub descriptor: ObjectDescriptorWireRecord,
    pub body_end: U64<BigEndian>,
}

#[repr(C)]
#[derive(Immutable, IntoBytes, Unaligned)]
pub(crate) struct TestIndex<const ROW_COUNT: usize> {
    pub count: U64<BigEndian>,
    pub rows: [TestDirectoryRow; ROW_COUNT],
}

pub(crate) fn row(content: [u8; 32], length: u64, body_end: u64) -> TestDirectoryRow {
    let reference = ObjectRef::<ObjectDomain> {
        content: heart_identity::ContentId::from_digest(content),
        length: length.into(),
        schema: SchemaId::Object,
        kind: ObjectKind::from(FIXTURE_OBJECT_KIND),
    };
    TestDirectoryRow {
        descriptor: ObjectDescriptorWireRecord::from(&reference),
        body_end: U64::new(body_end),
    }
}

pub(crate) fn three_rows() -> [TestDirectoryRow; 3] {
    [row([1; 32], 1, 1), row([2; 32], 2, 3), row([3; 32], 3, 6)]
}
