#![no_std]

use core::{iter::FusedIterator, slice::ChunksExact};
use nudox_ir_vocab::{EntityId, TypeId};

const MAGIC_WORD_BYTES: usize = 4;
const HEADER_BYTES: usize = 23;
const RECORD_BYTES: usize = 4;
const ENTITY_TAG: u8 = 1;
const TYPE_TAG: u8 = 2;
const SCHEMA: u8 = 1;
const MAGIC: [u8; MAGIC_WORD_BYTES] = *b"NIRF";

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
    DescriptorTag { lane: FragmentLane, observed: u8 },
    LaneByteLengthOverflow { lane: FragmentLane, count: u32 },
    LaneStart { lane: FragmentLane, expected: u32, observed: u32 },
    LaneEnd { lane: FragmentLane, expected: u32, available: usize },
    FinalEnd { expected: u32, available: usize },
}

pub struct FragmentView<'bytes> {
    bytes: &'bytes [u8],
    entity: core::ops::Range<usize>,
    ty: core::ops::Range<usize>,
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

// Only validate decodes raw header and descriptors; cursors consume proved four-byte chunks.
// Evidence mutation decks use fixed arrays; this no-alloc surface has no Vec-backed storage.
