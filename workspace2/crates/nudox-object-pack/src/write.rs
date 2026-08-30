use core::ops::Deref;

use nudox_id::{ContentId, ObjectDomain};
use nudox_object::{ObjectDescriptorWireRecord, ObjectRef};
use zerocopy::{
    IntoBytes,
    byteorder::{BigEndian, U64},
};

use crate::{
    ObjectPackBytes, ObjectPackError,
    format::{
        COUNT_BYTES, DIRECTORY_BYTES, DirectoryRecord, ObjectCountRecord, checked_add, index_bytes,
        u64_from_usize,
    },
};

/// Caller-owned immutable object bytes and their exact descriptor.
#[derive(Clone, Copy, Debug)]
pub struct PackInput<'bytes> {
    /// Immutable descriptor whose content identity names `bytes`.
    pub reference: ObjectRef<ObjectDomain>,
    /// Canonical payload written exactly once into the pack body region.
    pub bytes: &'bytes [u8],
}

/// Public immutable facts established by pack preparation.
pub struct ObjectPackFacts {
    /// Exact canonical output length accepted by [`PreparedObjectPack::write`].
    pub required_bytes: ObjectPackBytes,
    /// Exact count-header and directory prefix length. Remote/file adapters
    /// can write this prefix once and stream body segments without assembling
    /// the complete pack in memory.
    pub index_bytes: ObjectPackBytes,
}

/// Fully measured immutable sparse input ready for a direct caller-buffer write.
pub struct PreparedObjectPack<'inputs, 'bytes> {
    facts: ObjectPackFacts,
    inputs: &'inputs [PackInput<'bytes>],
}

impl Deref for PreparedObjectPack<'_, '_> {
    type Target = ObjectPackFacts;

    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

impl<'inputs, 'bytes> PreparedObjectPack<'inputs, 'bytes> {
    /// Validates canonical input order, descriptor/body coherence, and exact layout.
    ///
    /// # Errors
    ///
    /// Returns the first length, order, layout, or content-identity rejection.
    pub fn prepare(inputs: &'inputs [PackInput<'bytes>]) -> Result<Self, ObjectPackError> {
        let index_bytes = index_bytes(inputs.len())?;
        let mut body_bytes = 0_usize;
        let mut previous = None;
        for (ordinal, input) in inputs.iter().enumerate() {
            let declared = usize::try_from(*input.reference.length).map_err(|source| {
                ObjectPackError::DescriptorLengthAddressSpace {
                    ordinal,
                    declared: input.reference.length,
                    source,
                }
            })?;
            if input.bytes.len() != declared {
                return Err(ObjectPackError::InputLength {
                    ordinal,
                    declared: input.reference.length,
                    actual: input.bytes.len().into(),
                });
            }
            if let Some(previous) = previous
                && previous >= input.reference.content
            {
                return Err(ObjectPackError::InputOrder {
                    ordinal,
                    previous,
                    current: input.reference.content,
                });
            }
            previous = Some(input.reference.content);
            body_bytes = checked_add(body_bytes, declared)?;
        }
        for (ordinal, input) in inputs.iter().enumerate() {
            let actual = ContentId::from_canonical_bytes(input.bytes);
            if actual != input.reference.content {
                return Err(ObjectPackError::InputContent {
                    ordinal,
                    expected: input.reference.content,
                    actual,
                });
            }
        }
        Ok(Self {
            facts: ObjectPackFacts {
                required_bytes: checked_add(index_bytes, body_bytes)?.into(),
                index_bytes: index_bytes.into(),
            },
            inputs,
        })
    }

    /// Writes one canonical pack prefix after the exact output preflight.
    ///
    /// # Errors
    ///
    /// Returns [`ObjectPackError::OutputTooSmall`] before changing caller bytes.
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "prepare proved the same immutable input lengths sum within the exact stored output extent"
    )]
    pub fn write<'output>(
        &self,
        output: &'output mut [u8],
    ) -> Result<&'output [u8], ObjectPackError> {
        let required = usize::from(self.facts.required_bytes);
        if output.len() < required {
            return Err(ObjectPackError::OutputTooSmall {
                required: self.facts.required_bytes,
                available: output.len().into(),
            });
        }
        #[allow(
            clippy::indexing_slicing,
            reason = "the immediately preceding exact prepared length preflight proves this prefix"
        )]
        let output = &mut output[..required];
        let (index, bodies) = output.split_at_mut(usize::from(self.facts.index_bytes));
        self.write_index_preflighted(index);
        let mut body = bodies;
        for segment in self.body_segments() {
            let (target, rest) = body.split_at_mut(segment.len());
            target.copy_from_slice(segment);
            body = rest;
        }
        Ok(output)
    }

    /// Writes only the canonical count-and-directory prefix.
    ///
    /// File, object-store, and vectored-I/O adapters can retain this small
    /// prefix and consume [`Self::body_segments`] directly, avoiding one
    /// complete-pack allocation and payload copy.
    ///
    /// # Errors
    ///
    /// Returns [`ObjectPackError::OutputTooSmall`] before changing `output`
    /// when the exact index prefix is unavailable.
    pub fn write_index<'output>(
        &self,
        output: &'output mut [u8],
    ) -> Result<&'output [u8], ObjectPackError> {
        let required = usize::from(self.facts.index_bytes);
        if output.len() < required {
            return Err(ObjectPackError::OutputTooSmall {
                required: self.facts.index_bytes,
                available: output.len().into(),
            });
        }
        #[allow(
            clippy::indexing_slicing,
            reason = "the exact index-length preflight proves this output prefix"
        )]
        let output = &mut output[..required];
        self.write_index_preflighted(output);
        Ok(output)
    }

    /// Lends the already verified canonical bodies in directory order.
    ///
    /// The iterator allocates nothing and its slices point at the original
    /// caller-owned inputs. Adapters choose bounded coalescing or vectored-I/O
    /// policy without changing pack identity or copying through the core.
    #[must_use]
    pub fn body_segments(&self) -> impl ExactSizeIterator<Item = &'bytes [u8]> + '_ {
        self.inputs.iter().map(|input| input.bytes)
    }

    fn write_index_preflighted(&self, index: &mut [u8]) {
        let (count, mut directory) = index.split_at_mut(COUNT_BYTES);
        count.copy_from_slice(
            ObjectCountRecord(U64::<BigEndian>::new(u64_from_usize(self.inputs.len()))).as_bytes(),
        );
        let mut end = 0_usize;
        for input in self.inputs {
            end += input.bytes.len();
            let record = DirectoryRecord {
                descriptor: ObjectDescriptorWireRecord::from(&input.reference),
                body_end: U64::<BigEndian>::new(u64_from_usize(end)),
            };
            let (target, rest) = directory.split_at_mut(DIRECTORY_BYTES);
            target.copy_from_slice(record.as_bytes());
            directory = rest;
        }
    }
}
