//! Defines locality artifact descriptor behavior for `heart-root`, whose purpose is to construct and validate immutable generation roots and locality metadata.
//! This module owns the locality artifact descriptor invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::mem::{align_of, offset_of, size_of};

use heart_identity::CONTENT_PAYLOAD_BYTES;
use heart_object::ObjectRef;
use heart_schema::SchemaId;
use zerocopy::{
    Immutable, IntoBytes, KnownLayout, TryFromBytes, Unalign, Unaligned,
    byteorder::{BigEndian, U16, U64},
};

/// Present-overlay record width after artifact-global domain authority is
/// removed from each descriptor.
pub(super) const LOCALITY_DESCRIPTOR_BYTES: usize = size_of::<LocalityDescriptorWireRecord>();

/// Compact physical descriptor for a locality artifact whose header owns the
/// content domain. Every payload therefore retains all meaningful content bits
/// without repeating one authority byte per present overlay.
#[repr(C)]
#[derive(Clone, Copy, Immutable, IntoBytes, KnownLayout, TryFromBytes, Unaligned)]
pub(in crate::locality) struct LocalityDescriptorWireRecord {
    pub(in crate::locality) content: [u8; CONTENT_PAYLOAD_BYTES],
    pub(in crate::locality) length: U64<BigEndian>,
    pub(in crate::locality) schema: Unalign<SchemaId>,
    pub(in crate::locality) kind: U16<BigEndian>,
}

pub(super) const SCHEMA_OFFSET: usize = offset_of!(LocalityDescriptorWireRecord, schema);

const _: [(); 1] = [(); align_of::<LocalityDescriptorWireRecord>()];
const _: [(); 45] = [(); LOCALITY_DESCRIPTOR_BYTES];

#[allow(
    clippy::indexing_slicing,
    reason = "ContentId is an exact 32-byte array and CONTENT_PAYLOAD_BYTES reserves its first authority cell"
)]
impl<DomainTag> From<&ObjectRef<DomainTag>> for LocalityDescriptorWireRecord {
    fn from(reference: &ObjectRef<DomainTag>) -> Self {
        let mut content = [0; CONTENT_PAYLOAD_BYTES];
        content.copy_from_slice(&reference.content[1..]);
        Self {
            content,
            length: U64::new(*reference.length),
            schema: Unalign::new(reference.schema),
            kind: U16::new(*reference.kind),
        }
    }
}
