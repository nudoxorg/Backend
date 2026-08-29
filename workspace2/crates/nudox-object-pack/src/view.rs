use nudox_id::{ContentId, ObjectDomain};
use nudox_object::{ObjectLength, ObjectRef};
use nudox_schema::SchemaId;

use crate::{ObjectPackError, ObjectPackIndex};

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
pub struct ObjectPackView<'pack> {
    /// Complete backing bytes, validated once with `index`.
    pub bytes: &'pack [u8],
    /// Retained typed index witness for the complete backing bytes.
    pub index: ObjectPackIndex<'pack>,
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
                    let length =
                        native_coordinate(*ObjectLength::from(row.descriptor.length.get()));
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

fn native_coordinate(value: u64) -> usize {
    match usize::try_from(value) {
        Ok(value) => value,
        Err(_) => unreachable!("validated object-pack coordinate fits the target address space"),
    }
}

fn reference(row: &crate::format::DirectoryRecord) -> ObjectRef<ObjectDomain> {
    let Ok(schema) = SchemaId::try_from(row.descriptor.schema.get()) else {
        unreachable!("validated object-pack descriptor has a known schema");
    };
    ObjectRef {
        content: ContentId::from(row.descriptor.content),
        length: ObjectLength::from(row.descriptor.length.get()),
        schema,
        kind: row.descriptor.kind.get().into(),
    }
}
