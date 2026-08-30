#![no_std]

use core::{ops::Range, slice::ChunksExact};
use nudox_ir_vocab::{EntityId, TypeId};

pub enum FragmentLane {
    Entity,
    Type,
}

pub enum FragmentError {
    InputTooShort { available: usize },
    Magic { observed: [u8; 4] },
    Schema { observed: u8 },
    HeaderPadding { offset: u8, observed: u8 },
    DescriptorTag { lane: FragmentLane, observed: u8 },
    DescriptorPadding { lane: FragmentLane, offset: u8, observed: u8 },
    LaneByteLengthOverflow { lane: FragmentLane, count: u32 },
    LaneStart { lane: FragmentLane, expected: u32, observed: u32 },
    LaneEnd { lane: FragmentLane, expected: u32, available: usize },
    FinalEnd { expected: u32, available: usize },
}

pub struct FragmentView<'bytes> {
    bytes: &'bytes [u8],
    entity: Range<usize>,
    ty: Range<usize>,
}

pub struct EntityLane<'bytes> {
    bytes: &'bytes [u8],
}

pub struct TypeLane<'bytes> {
    bytes: &'bytes [u8],
}

pub struct EntityIds<'bytes> {
    chunks: ChunksExact<'bytes, u8>,
    remaining: usize,
}

pub struct TypeIds<'bytes> {
    chunks: ChunksExact<'bytes, u8>,
    remaining: usize,
}

impl<'bytes> FragmentView<'bytes> {
    pub fn validate(input: &'bytes [u8]) -> Result<Self, FragmentError>;
    pub fn entities(&self) -> EntityLane<'bytes>;
    pub fn types(&self) -> TypeLane<'bytes>;
}

impl AsRef<[u8]> for EntityLane<'_> {
    fn as_ref(&self) -> &[u8];
}

impl AsRef<[u8]> for TypeLane<'_> {
    fn as_ref(&self) -> &[u8];
}

impl<'bytes> EntityLane<'bytes> {
    pub fn iter(&self) -> EntityIds<'bytes>;
}

impl<'bytes> TypeLane<'bytes> {
    pub fn iter(&self) -> TypeIds<'bytes>;
}
impl Iterator for EntityIds<'_> {
    type Item = EntityId;
    fn next(&mut self) -> Option<Self::Item>;
    fn size_hint(&self) -> (usize, Option<usize>);
}
impl ExactSizeIterator for EntityIds<'_> {
    fn len(&self) -> usize;
}
impl Iterator for TypeIds<'_> {
    type Item = TypeId;
    fn next(&mut self) -> Option<Self::Item>;
    fn size_hint(&self) -> (usize, Option<usize>);
}
impl ExactSizeIterator for TypeIds<'_> {
    fn len(&self) -> usize;
}

// `validate` is the sole raw decoder. `next` only advances a private `ChunksExact<u8>` and reads one
// already-proved four-byte record; it cannot reach the view header, descriptor ranges, or validation.
