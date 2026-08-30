#![no_std]

use nudox_ir_vocab::{EntityId, TypeId};

pub const HEADER_BYTES: usize = 4;
pub const ENTITY_BYTES: usize = 4;
pub const TYPE_BYTES: usize = 4;
pub const MAGIC: u8 = 0xc1;
pub const SCHEMA: u8 = 1;
pub const MAX_LANE_ITEMS: u8 = 2;

/// The exact reason a fragment cannot become a borrowed view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FragmentError {
    TruncatedEnvelope { actual: usize },
    Magic { actual: u8 },
    Schema { actual: u8 },
    EntityCount { actual: u8 },
    TypeCount { actual: u8 },
    Geometry { expected: usize, actual: usize },
}

/// A validated, non-owning view of exactly two dense-ID lanes.
pub struct FragmentView<'fragment> {
    envelope: &'fragment [u8],
    entity_lane: &'fragment [u8],
    type_lane: &'fragment [u8],
}

impl<'fragment> FragmentView<'fragment> {
    pub fn validate(envelope: &'fragment [u8]) -> Result<Self, FragmentError> {
        if envelope.len() < HEADER_BYTES {
            return Err(FragmentError::TruncatedEnvelope {
                actual: envelope.len(),
            });
        }
        if envelope[0] != MAGIC {
            return Err(FragmentError::Magic {
                actual: envelope[0],
            });
        }
        if envelope[1] != SCHEMA {
            return Err(FragmentError::Schema {
                actual: envelope[1],
            });
        }
        let entity_count = envelope[2];
        if entity_count > MAX_LANE_ITEMS {
            return Err(FragmentError::EntityCount {
                actual: entity_count,
            });
        }
        let type_count = envelope[3];
        if type_count > MAX_LANE_ITEMS {
            return Err(FragmentError::TypeCount { actual: type_count });
        }
        let entity_end = HEADER_BYTES + usize::from(entity_count) * ENTITY_BYTES;
        let expected = entity_end + usize::from(type_count) * TYPE_BYTES;
        if envelope.len() != expected {
            return Err(FragmentError::Geometry {
                expected,
                actual: envelope.len(),
            });
        }
        Ok(Self {
            envelope,
            entity_lane: &envelope[HEADER_BYTES..entity_end],
            type_lane: &envelope[entity_end..],
        })
    }

    pub fn entity_ids(&self) -> EntityCursor<'_> {
        EntityCursor {
            lane: self.entity_lane,
        }
    }

    pub fn type_ids(&self) -> TypeCursor<'_> {
        TypeCursor {
            lane: self.type_lane,
        }
    }

    pub fn input_len(&self) -> usize {
        self.envelope.len()
    }
}

pub struct EntityCursor<'fragment> {
    lane: &'fragment [u8],
}

impl Iterator for EntityCursor<'_> {
    type Item = EntityId;

    fn next(&mut self) -> Option<Self::Item> {
        let (head, tail) = self.lane.split_first_chunk::<ENTITY_BYTES>()?;
        self.lane = tail;
        Some(EntityId::new(u32::from_le_bytes(*head)))
    }
}

pub struct TypeCursor<'fragment> {
    lane: &'fragment [u8],
}

impl Iterator for TypeCursor<'_> {
    type Item = TypeId;

    fn next(&mut self) -> Option<Self::Item> {
        let (head, tail) = self.lane.split_first_chunk::<TYPE_BYTES>()?;
        self.lane = tail;
        Some(TypeId::new(u32::from_le_bytes(*head)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validated_lane_borrows_are_contained_by_the_one_caller_slice() {
        let bytes = [MAGIC, SCHEMA, 1, 1, 7, 0, 0, 0, 9, 0, 0, 0];
        let view = FragmentView::validate(&bytes).unwrap();
        let start = bytes.as_ptr() as usize;
        let end = start + bytes.len();
        for lane in [view.envelope, view.entity_lane, view.type_lane] {
            let lane_start = lane.as_ptr() as usize;
            assert!(start <= lane_start);
            assert!(lane_start + lane.len() <= end);
        }
    }
}
