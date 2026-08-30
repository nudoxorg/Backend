#![no_std]

use nudox_ir_vocab::{EntityId, TypeId};

pub const HEADER_BYTES: usize = 4;
pub const ENTITY_BYTES: usize = 4;
pub const TYPE_BYTES: usize = 4;
pub const MAGIC: u8 = 0xc1;
pub const SCHEMA: u8 = 1;
pub const MAX_LANE_ITEMS: u8 = 2;

const MAGIC_OFFSET: usize = 0;
const SCHEMA_OFFSET: usize = 1;
const ENTITY_COUNT_OFFSET: usize = 2;
const TYPE_COUNT_OFFSET: usize = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FragmentError {
    TruncatedEnvelope { actual: usize },
    Magic { actual: u8 },
    Schema { actual: u8 },
    EntityCount { actual: u8 },
    TypeCount { actual: u8 },
    Geometry { expected: usize, actual: usize },
}

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
        if envelope[MAGIC_OFFSET] != MAGIC {
            return Err(FragmentError::Magic {
                actual: envelope[MAGIC_OFFSET],
            });
        }
        if envelope[SCHEMA_OFFSET] != SCHEMA {
            return Err(FragmentError::Schema {
                actual: envelope[SCHEMA_OFFSET],
            });
        }
        let entity_count = envelope[ENTITY_COUNT_OFFSET];
        if entity_count > MAX_LANE_ITEMS {
            return Err(FragmentError::EntityCount {
                actual: entity_count,
            });
        }
        let type_count = envelope[TYPE_COUNT_OFFSET];
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

    pub fn entity_ids(&self) -> EntityCursor<'fragment> {
        EntityCursor {
            lane: self.entity_lane,
        }
    }

    pub fn type_ids(&self) -> TypeCursor<'fragment> {
        TypeCursor {
            lane: self.type_lane,
        }
    }

    pub fn input_len(&self) -> usize {
        self.envelope.len()
    }
}

impl AsRef<[u8]> for FragmentView<'_> {
    fn as_ref(&self) -> &[u8] {
        self.envelope
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

    fn size_hint(&self) -> (usize, Option<usize>) {
        let count = self.lane.len() / ENTITY_BYTES;
        (count, Some(count))
    }
}

impl ExactSizeIterator for EntityCursor<'_> {
    fn len(&self) -> usize {
        self.lane.len() / ENTITY_BYTES
    }
}

impl core::iter::FusedIterator for EntityCursor<'_> {}

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

    fn size_hint(&self) -> (usize, Option<usize>) {
        let count = self.lane.len() / TYPE_BYTES;
        (count, Some(count))
    }
}

impl ExactSizeIterator for TypeCursor<'_> {
    fn len(&self) -> usize {
        self.lane.len() / TYPE_BYTES
    }
}

impl core::iter::FusedIterator for TypeCursor<'_> {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrepareError {
    EntityCount { actual: usize },
    TypeCount { actual: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteError {
    OutputTooSmall { required: usize, available: usize },
}

pub struct PreparedFragment<'facts> {
    entities: &'facts [EntityId],
    types: &'facts [TypeId],
    entity_count: u8,
    type_count: u8,
    output_len: usize,
}

impl<'facts> PreparedFragment<'facts> {
    pub fn prepare(
        entities: &'facts [EntityId],
        types: &'facts [TypeId],
    ) -> Result<Self, PrepareError> {
        let entity_actual = entities.len();
        let entity_count = match u8::try_from(entity_actual) {
            Ok(count) => count,
            Err(_) => {
                return Err(PrepareError::EntityCount {
                    actual: entity_actual,
                });
            }
        };
        if entity_count > MAX_LANE_ITEMS {
            return Err(PrepareError::EntityCount {
                actual: entity_actual,
            });
        }
        let type_actual = types.len();
        let type_count = match u8::try_from(type_actual) {
            Ok(count) => count,
            Err(_) => {
                return Err(PrepareError::TypeCount {
                    actual: type_actual,
                });
            }
        };
        if type_count > MAX_LANE_ITEMS {
            return Err(PrepareError::TypeCount {
                actual: type_actual,
            });
        }
        let output_len = HEADER_BYTES + entities.len() * ENTITY_BYTES + types.len() * TYPE_BYTES;
        Ok(Self {
            entities,
            types,
            entity_count,
            type_count,
            output_len,
        })
    }

    pub fn output_len(&self) -> usize {
        self.output_len
    }

    pub fn write_into<'output>(
        self,
        output: &'output mut [u8],
    ) -> Result<&'output [u8], WriteError> {
        if output.len() < self.output_len {
            return Err(WriteError::OutputTooSmall {
                required: self.output_len,
                available: output.len(),
            });
        }
        let written = &mut output[..self.output_len];
        written[MAGIC_OFFSET] = MAGIC;
        written[SCHEMA_OFFSET] = SCHEMA;
        written[ENTITY_COUNT_OFFSET] = self.entity_count;
        written[TYPE_COUNT_OFFSET] = self.type_count;
        let mut cursor = HEADER_BYTES;
        for entity in self.entities {
            written[cursor..cursor + ENTITY_BYTES].copy_from_slice(&entity.raw.to_le_bytes());
            cursor += ENTITY_BYTES;
        }
        for ty in self.types {
            written[cursor..cursor + TYPE_BYTES].copy_from_slice(&ty.raw.to_le_bytes());
            cursor += TYPE_BYTES;
        }
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_fields_retain_the_exact_caller_slices() {
        let entities = [EntityId::new(0x0102_0304)];
        let types = [TypeId::new(0xa0b0_c0d0)];
        let prepared = match PreparedFragment::prepare(&entities, &types) {
            Ok(prepared) => prepared,
            Err(error) => panic!("valid facts rejected: {error:?}"),
        };
        assert_eq!(prepared.entities.as_ptr(), entities.as_ptr());
        assert_eq!(prepared.entities.len(), entities.len());
        assert_eq!(prepared.types.as_ptr(), types.as_ptr());
        assert_eq!(prepared.types.len(), types.len());
    }

    #[test]
    fn prepared_source_keeps_borrowed_slice_provenance() {
        let source = include_str!("lib.rs");
        let entity_array = [
            b'e', b'n', b't', b'i', b't', b'i', b'e', b's', b':', b' ', b'[', b'E', b'n', b't',
            b'i', b't', b'y', b'I', b'd', b';',
        ];
        let type_array = [
            b't', b'y', b'p', b'e', b's', b':', b' ', b'[', b'T', b'y', b'p', b'e', b'I', b'd',
            b';',
        ];
        let phantom_data = [
            b'P', b'h', b'a', b'n', b't', b'o', b'm', b'D', b'a', b't', b'a',
        ];
        assert!(source.contains("entities: &'facts [EntityId]"));
        assert!(source.contains("types: &'facts [TypeId]"));
        let struct_start = match source.find("pub struct PreparedFragment<'facts> {") {
            Some(offset) => offset,
            None => panic!("prepared struct missing"),
        };
        let struct_tail = &source[struct_start..];
        let struct_end = match struct_tail.find("\n}") {
            Some(offset) => offset + 2,
            None => panic!("prepared struct closing brace missing"),
        };
        let struct_body = &source[struct_start..struct_start + struct_end];
        assert_eq!(
            struct_body
                .lines()
                .filter(|line| line.trim_start().contains(": "))
                .count(),
            5
        );
        assert!(struct_body.contains("entity_count: u8"));
        assert!(struct_body.contains("type_count: u8"));
        assert!(struct_body.contains("output_len: usize"));
        assert!(
            !source
                .as_bytes()
                .windows(entity_array.len())
                .any(|w| w == entity_array)
        );
        assert!(
            !source
                .as_bytes()
                .windows(type_array.len())
                .any(|w| w == type_array)
        );
        assert!(
            !source
                .as_bytes()
                .windows(phantom_data.len())
                .any(|w| w == phantom_data)
        );
    }
}
