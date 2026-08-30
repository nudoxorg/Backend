#![no_std]

// Frozen inventory: existing validator/view/cursors remain; this is the added writer shape only.
pub enum PrepareError {
    EntityCount { actual: usize },
    TypeCount { actual: usize },
}

pub enum WriteError {
    OutputTooSmall { required: usize, available: usize },
}

pub struct PreparedFragment<'facts> {
    entities: &'facts [EntityId],
    types: &'facts [TypeId],
    output_len: usize,
}

impl<'facts> PreparedFragment<'facts> {
    pub fn prepare(
        entities: &'facts [EntityId],
        types: &'facts [TypeId],
    ) -> Result<Self, PrepareError> {
        todo!()
    }

    pub fn output_len(&self) -> usize {
        self.output_len
    }

    pub fn write_into<'output>(
        self,
        output: &'output mut [u8],
    ) -> Result<&'output [u8], WriteError> {
        todo!()
    }
}
