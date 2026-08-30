#![no_std]

// Exact appended inventory over the immutable validator/view/cursors in the baseline.
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
        Ok(Self {
            entities,
            types,
            entity_count,
            type_count,
            output_len: HEADER_BYTES + entities.len() * ENTITY_BYTES + types.len() * TYPE_BYTES,
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
        written[0] = MAGIC;
        written[1] = SCHEMA;
        written[2] = self.entity_count;
        written[3] = self.type_count;
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

impl AsRef<[u8]> for FragmentView<'_> {
    fn as_ref(&self) -> &[u8] {
        self.envelope
    }
}
