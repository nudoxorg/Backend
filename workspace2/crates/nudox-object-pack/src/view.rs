use nudox_id::{ContentId, ObjectDomain};
use nudox_object::ObjectRef;

use crate::{ObjectPackError, ObjectPackIndex};

/// A selected descriptor and its exact borrowed body in a complete pack.
pub struct ObjectPackObject<'pack> {
    /// Exact validated descriptor reconstructed from the selected directory row.
    pub reference: ObjectRef<ObjectDomain>,
    /// Exact body range borrowed from the complete pack backing bytes.
    pub body: &'pack [u8],
}

impl<'pack> ObjectPackObject<'pack> {
    /// Verifies this selected body's canonical identity.
    ///
    /// # Errors
    ///
    /// Returns [`ObjectPackError::ObjectContent`] with both identities when this body differs.
    pub fn verify(&self) -> Result<&'pack [u8], ObjectPackError> {
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
/// The validated directory and its exact body region remain private because
/// their pairing is the invariant that makes lookup projection infallible.
///
/// ```compile_fail,E0451
/// use nudox_object_pack::{ObjectPackError, ObjectPackView};
///
/// fn forge<'bytes>(bytes: &'bytes [u8]) -> Result<(), ObjectPackError> {
///     let _forged = ObjectPackView { directory: &[], bodies: bytes };
///     Ok(())
/// }
/// ```
pub struct ObjectPackView<'pack> {
    directory: &'pack [crate::format::DirectoryRecord],
    bodies: &'pack [u8],
}

impl<'pack> TryFrom<&'pack [u8]> for ObjectPackView<'pack> {
    type Error = ObjectPackError;

    fn try_from(bytes: &'pack [u8]) -> Result<Self, Self::Error> {
        let index = ObjectPackIndex::complete(bytes)?;
        let body_start = usize::from(index.index_bytes);
        let bodies = bytes.get(body_start..).ok_or(ObjectPackError::PackExtent {
            expected: index.pack_bytes,
            actual: bytes.len().into(),
        })?;
        Ok(Self {
            directory: index.directory,
            bodies,
        })
    }
}

impl<'pack> ObjectPackView<'pack> {
    /// Finds a descriptor by its checked typed identity using binary search.
    #[must_use]
    #[allow(
        clippy::indexing_slicing,
        reason = "complete construction proves every retained directory end and descriptor length indexes the complete backing bytes"
    )]
    pub fn lookup(&self, content: &ContentId<ObjectDomain>) -> Option<ObjectPackObject<'pack>> {
        let target = content.as_ref();
        let mut left = 0;
        let mut right = self.directory.len();
        while left < right {
            let middle = left + (right - left) / 2;
            let row = &self.directory[middle];
            match row.descriptor.content.as_slice().cmp(target.as_slice()) {
                core::cmp::Ordering::Less => left = middle + 1,
                core::cmp::Ordering::Greater => right = middle,
                core::cmp::Ordering::Equal => {
                    let end = native_coordinate(row.body_end.get());
                    let length = native_coordinate(row.descriptor.length.get());
                    let start = end - length;
                    return Some(ObjectPackObject {
                        reference: reference(row, content),
                        body: &self.bodies[start..end],
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

fn reference(
    row: &crate::format::DirectoryRecord,
    content: &ContentId<ObjectDomain>,
) -> ObjectRef<ObjectDomain> {
    ObjectRef {
        content: *content,
        length: nudox_object::ObjectLength::from(row.descriptor.length.get()),
        schema: row.descriptor.schema.get(),
        kind: nudox_object::ObjectKind::from(row.descriptor.kind.get()),
    }
}
