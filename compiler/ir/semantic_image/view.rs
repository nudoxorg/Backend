//! Borrowed allocation-free core semantic-image reader.

use core::{
    iter::FusedIterator,
    ops::Deref,
};

use crate::{
    AtomId, CoreSemanticEntity, DeclarationIdentity, EntityId, SemanticCoreReader,
    SemanticImageFacts, TextId,
};

use super::model::{
    decode_entity, get_u32, CoreImageLayout, CoreSemanticImageField, ATOM_ROW_BYTES,
};

/// Immutable facts proven by validation and directly inspectable on a
/// borrowed core image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CoreSemanticImageFacts {
    pub image: SemanticImageFacts,
    pub atom_count: u32,
    pub entity_count: u32,
}

/// Validated borrowed portable core semantic image.
///
/// It retains only the original byte slice plus checked offsets. Atom/text
/// views borrow those exact bytes; repeated reads neither allocate nor parse
/// another owned image. The type implements `SemanticCoreReader`, never the
/// complete `SemanticReader` capability.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CoreSemanticImageView<'bytes> {
    bytes: &'bytes [u8],
    layout: CoreImageLayout,
    facts: CoreSemanticImageFacts,
}

impl<'bytes> CoreSemanticImageView<'bytes> {
    pub(super) fn from_validated(
        bytes: &'bytes [u8],
        layout: CoreImageLayout,
        image: SemanticImageFacts,
        atom_count: u32,
        entity_count: u32,
    ) -> Self {
        Self {
            bytes,
            layout,
            facts: CoreSemanticImageFacts {
                image,
                atom_count,
                entity_count,
            },
        }
    }

    fn atom_bytes(self, atom: AtomId) -> Option<&'bytes [u8]> {
        let row = atom.index();
        if row >= self.layout.atom_rows {
            return None;
        }
        let offset = self.layout.atoms.checked_add(row.checked_mul(ATOM_ROW_BYTES)?)?;
        let start = usize::try_from(get_u32(self.bytes, offset, CoreSemanticImageField::AtomRange).ok()?).ok()?;
        let length = usize::try_from(get_u32(self.bytes, offset + 4, CoreSemanticImageField::AtomRange).ok()?).ok()?;
        let start = self.layout.bytes.checked_add(start)?;
        let end = start.checked_add(length)?;
        self.bytes.get(start..end)
    }

    fn entity_row(self, row: u32) -> Option<CoreSemanticEntity> {
        // `reopen_core_semantic_image` validates this exact row, every
        // discriminant, all cross-references, and canonical ordering before
        // constructing the view. `ok()` is therefore a defensive boundary,
        // not a recovery path for unvalidated bytes.
        decode_entity(self.bytes, self.layout, row).ok()
    }
}

impl Deref for CoreSemanticImageView<'_> {
    type Target = CoreSemanticImageFacts;

    fn deref(&self) -> &Self::Target { &self.facts }
}

/// Exact-size canonical cursor over already validated entity rows.
pub(crate) struct CoreSemanticImageEntities<'bytes> {
    image: CoreSemanticImageView<'bytes>,
    next: u32,
    end: u32,
    remaining: usize,
}

impl Iterator for CoreSemanticImageEntities<'_> {
    type Item = CoreSemanticEntity;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        let row = self.next;
        self.next += 1;
        let entity = self.image.entity_row(row)?;
        self.remaining -= 1;
        Some(entity)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}
impl ExactSizeIterator for CoreSemanticImageEntities<'_> {}
impl FusedIterator for CoreSemanticImageEntities<'_> {}

impl crate::reader::sealed::Sealed for CoreSemanticImageView<'_> {}

impl SemanticCoreReader for CoreSemanticImageView<'_> {
    type CanonicalCoreEntities<'image> = CoreSemanticImageEntities<'image> where Self: 'image;

    fn image_facts(&self) -> SemanticImageFacts { self.facts.image }

    fn core_entity(&self, id: EntityId) -> Option<CoreSemanticEntity> {
        self.entity_row(u32::try_from(id.index()).ok()?)
    }

    fn core_entity_by_identity(&self, identity: DeclarationIdentity) -> Option<CoreSemanticEntity> {
        let mut lower = 0_u32;
        let mut upper = self.facts.entity_count;
        while lower < upper {
            let middle = lower + (upper - lower) / 2;
            let entity = self.entity_row(middle)?;
            match entity.version.identity().cmp(&identity) {
                core::cmp::Ordering::Less => lower = middle + 1,
                core::cmp::Ordering::Greater => upper = middle,
                core::cmp::Ordering::Equal => return Some(entity),
            }
        }
        None
    }

    fn atom(&self, id: AtomId) -> Option<&[u8]> { self.atom_bytes(id) }

    fn text(&self, id: TextId) -> Option<&str> {
        let raw = u32::try_from(id.index()).ok()?;
        core::str::from_utf8(self.atom_bytes(AtomId::new(raw))?).ok()
    }

    fn canonical_core_entities(&self) -> Self::CanonicalCoreEntities<'_> {
        CoreSemanticImageEntities {
            image: *self,
            next: 0,
            end: self.facts.entity_count,
            remaining: self.layout.entity_rows,
        }
    }
}
