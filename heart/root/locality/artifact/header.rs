//! Defines locality artifact header behavior for `heart-root`, whose purpose is to construct and validate immutable generation roots and locality metadata.
//! This module owns the locality artifact header invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::mem::{align_of, size_of};

use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::{BigEndian, U32},
};

/// The only fixed binary declaration for sorted locality's generation binding
/// and semantic lane counts. `LocalitySortedEncoding` is selected by the
/// authenticated enclosing request, rather than duplicated in these bytes.
#[repr(C)]
#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
pub(super) struct HeaderWireRecord {
    pub(super) generation: [u8; 32],
    pub(super) content_domain: u8,
    pub(super) root_count: U32<BigEndian>,
    pub(super) exception_count: U32<BigEndian>,
    pub(super) promise_count: U32<BigEndian>,
    pub(super) present_overlay_count: U32<BigEndian>,
}

pub(super) const HEADER_BYTES: usize = size_of::<HeaderWireRecord>();

const _: [(); 49] = [(); HEADER_BYTES];
const _: [(); 1] = [(); align_of::<HeaderWireRecord>()];
