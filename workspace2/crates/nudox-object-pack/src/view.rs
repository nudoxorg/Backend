use core::ops::Deref;

use nudox_id::{ContentId, ObjectDomain};
use nudox_object::ObjectRef;

use crate::{ObjectPackError, ObjectPackIndex, ObjectPackIndexFacts};

/// A selected descriptor and its exact borrowed body in a complete pack.
pub struct ObjectPackObject<'pack> {
    /// Exact validated descriptor reconstructed from the selected directory row.
    pub reference: ObjectRef<ObjectDomain>,
    /// Exact body range borrowed from the complete pack backing bytes.
    pub body: &'pack [u8],
}

impl ObjectPackObject<'_> {
    /// Verifies this selected body's canonical identity.
    ///
    /// # Errors
    ///
    /// Returns [`ObjectPackError::ObjectContent`] with both identities when this body differs.
    pub fn verify(&self) -> Result<&[u8], ObjectPackError> {
        let actual = ContentId::from_canonical_bytes(self.body);
        if actual != self.reference.content {
            return Err(ObjectPackError::ObjectContent {
                expected: self.reference.content,
                actual,
            });
        }
        Ok(self.body)
    }
}

/// Borrowed view of one complete, validated object pack.
///
/// The backing bytes and their index witness remain private because their
/// pairing is the invariant that makes lookup projection infallible.
pub struct ObjectPackView<'pack> {
    bytes: &'pack [u8],
    index: ObjectPackIndex<'pack>,
}

impl<'pack> Deref for ObjectPackView<'pack> {
    type Target = ObjectPackIndexFacts<'pack>;

    fn deref(&self) -> &Self::Target {
        &self.index
    }
}

impl<'pack> TryFrom<&'pack [u8]> for ObjectPackView<'pack> {
    type Error = ObjectPackError;

    fn try_from(bytes: &'pack [u8]) -> Result<Self, Self::Error> {
        let index = ObjectPackIndex::complete(bytes)?;
        Ok(Self { bytes, index })
    }
}

impl<'pack> ObjectPackView<'pack> {
    /// Finds a descriptor by raw 32-byte identity using binary search.
    #[must_use]
    #[allow(
        clippy::indexing_slicing,
        reason = "complete construction proves every retained directory end and descriptor length indexes the complete backing bytes"
    )]
    pub fn lookup(&self, content: &ContentId<ObjectDomain>) -> Option<ObjectPackObject<'pack>> {
        let target = content.as_ref();
        let mut left = 0;
        let mut right = self.index.directory.len();
        while left < right {
            let middle = left + (right - left) / 2;
            let row = &self.index.directory[middle];
            match row.descriptor.content.as_slice().cmp(target.as_slice()) {
                core::cmp::Ordering::Less => left = middle + 1,
                core::cmp::Ordering::Greater => right = middle,
                core::cmp::Ordering::Equal => {
                    let end = native_coordinate(row.body_end.get());
                    let length = native_coordinate(row.descriptor.length.get());
                    let start = end - length;
                    let body_start = usize::from(self.index.index_bytes);
                    return Some(ObjectPackObject {
                        reference: reference(row),
                        body: &self.bytes[body_start + start..body_start + end],
                    });
                }
            }
        }
        None
    }
}

#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    reason = "complete view construction proved every canonical u64 coordinate is losslessly representable as usize on this target"
)]
const fn native_coordinate(value: u64) -> usize {
    value as usize
}

fn reference(row: &crate::format::DirectoryRecord) -> ObjectRef<ObjectDomain> {
    ObjectRef::from(&row.descriptor)
}
