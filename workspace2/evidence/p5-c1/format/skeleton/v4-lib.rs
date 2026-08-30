#![no_std]

use core::{iter::FusedIterator, ops::Range, slice::ChunksExact};
use nudox_ir_vocab::{EntityId, TypeId};

const MAGIC_WORD_BYTES: usize = 4;
const HEADER_BYTES: usize = 32;
const RECORD_BYTES: usize = 4;
const MAGIC: [u8; MAGIC_WORD_BYTES] = *b"NIRF";
const SCHEMA: u8 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FragmentLane {
    Entity,
    Type,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FragmentError {
    InputTooShort { available: usize },
    Magic { observed: [u8; MAGIC_WORD_BYTES] },
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

impl FusedIterator for EntityIds<'_> {}

impl Iterator for TypeIds<'_> {
    type Item = TypeId;

    fn next(&mut self) -> Option<Self::Item>;
    fn size_hint(&self) -> (usize, Option<usize>);
}

impl ExactSizeIterator for TypeIds<'_> {
    fn len(&self) -> usize;
}

impl FusedIterator for TypeIds<'_> {}

// Only `validate` reaches raw header/descriptor cells. `next` advances an already-proved chunk iterator.
