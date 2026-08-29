use nudox_id::{ContentId, ObjectDomain};
use nudox_object::{ObjectLength, ObjectRef};

use crate::{ObjectPackError, ObjectPackIndex};

/// A selected descriptor and its exact borrowed body in a complete pack.
pub struct ObjectPackObject<'pack> {
    pub reference: ObjectRef<ObjectDomain>,
    pub body: &'pack [u8],
}

impl ObjectPackObject<'_> {
    /// Verifies this selected body's canonical identity.
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
pub struct ObjectPackView<'pack> {
    bytes: &'pack [u8],
    index: ObjectPackIndex<'pack>,
}

impl<'pack> TryFrom<&'pack [u8]> for ObjectPackView<'pack> {
    type Error = ObjectPackError;

    fn try_from(bytes: &'pack [u8]) -> Result<Self, Self::Error> {
        let header_bytes = bytes.get(..crate::OBJECT_PACK_HEADER_BYTES).ok_or(
            ObjectPackError::HeaderTruncated {
                required: crate::ObjectPackBytes::from(crate::OBJECT_PACK_HEADER_BYTES),
                available: bytes.len().into(),
            },
        )?;
        let header = crate::ObjectPackHeader::try_from(header_bytes)?;
        let index_len = usize::from(header.index_bytes);
        let index_bytes = bytes
            .get(..index_len)
            .ok_or(ObjectPackError::DirectoryExtent {
                expected: header.index_bytes,
                actual: bytes.len().into(),
            })?;
        let index = ObjectPackIndex::try_from(index_bytes)?;
        let expected = usize::from(index.pack_bytes);
        if bytes.len() != expected {
            return Err(ObjectPackError::PackExtent {
                expected: index.pack_bytes,
                actual: bytes.len().into(),
            });
        }
        Ok(Self { bytes, index })
    }
}

impl<'pack> ObjectPackView<'pack> {
    /// Borrows the complete validated backing pack.
    pub fn bytes(&self) -> &'pack [u8] {
        self.bytes
    }

    /// Borrows the retained validated index witness.
    pub fn index(&self) -> &ObjectPackIndex<'pack> {
        &self.index
    }

    /// Finds a descriptor by raw 32-byte identity using binary search.
    pub fn get(&self, content: &ContentId<ObjectDomain>) -> Option<ObjectPackObject<'pack>> {
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
                    let end = usize::try_from(row.body_end.get()).ok()?;
                    let length =
                        usize::try_from(*ObjectLength::from(row.descriptor.length.get())).ok()?;
                    let start = end.checked_sub(length)?;
                    let body_start = usize::from(self.index.index_bytes);
                    let body = self.bytes.get(body_start + start..body_start + end)?;
                    return Some(ObjectPackObject {
                        reference: ObjectRef::try_from(row.descriptor).ok()?,
                        body,
                    });
                }
            }
        }
        None
    }

    /// Alias for [`ObjectPackView::get`].
    pub fn lookup(&self, content: &ContentId<ObjectDomain>) -> Option<ObjectPackObject<'pack>> {
        self.get(content)
    }
}
