//! Proof-backed row cursors for a reopened full semantic image.
use core::iter::FusedIterator;

use crate::ir::{
    AtomId, CoreSemanticEntity, DeclarationIdentity, DocFragment, DocId, EntityId, EntityListId,
    ExternalId, ExternalTarget, Link, LinkId, LinkOccurrence, LinkOccurrenceId, SemanticEntity,
    TypeExpr, TypeId,
};

use super::super::validate::TypedLayout;
use super::super::{
    decode,
    extensions_decode::ExtensionCounts,
    typed_decode, validate,
    wire::{FullDirectoryKind, FullImageLayout, SPARSE_BINDING_ROW_BYTES, get_u32},
};
use super::SemanticImageView;

/// A proof-backed canonical row cursor. `decode` can only fail on malformed
/// bytes, and `SemanticImageView::reopen` has already rejected those bytes.
pub struct FullRows<'bytes, T> {
    bytes: &'bytes [u8],
    layout: FullImageLayout,
    typed: TypedLayout,
    coordinate: u32,
    next: u32,
    end: u32,
    decode: fn(
        &[u8],
        FullImageLayout,
        TypedLayout,
        u32,
        u32,
    ) -> Result<T, super::super::FullSemanticImageFault>,
}

impl<T> Iterator for FullRows<'_, T> {
    type Item = T;
    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        let index = self.next;
        self.next = self.next.checked_add(1)?;
        // Admission parsed the same row grammar, bounds, and tag sequence.
        (self.decode)(self.bytes, self.layout, self.typed, self.coordinate, index).ok()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = remaining(self.end, self.next);
        (remaining, Some(remaining))
    }
}
impl<T> ExactSizeIterator for FullRows<'_, T> {}
impl<T> FusedIterator for FullRows<'_, T> {}

/// Single-pass row cursor over one typed list node.
///
/// The node's [`typed_decode::Edges`] is opened once and its grouped
/// `(role, index)` adjacency is built at most once, so iterating `n` rows
/// costs one edge-run pass plus `O(1)` (amortized) field lookups per row
/// instead of rescanning the whole edge run for every field of every row.
/// Rows decode in the same order and to the same values as per-row decoding.
pub struct FullGroupRows<'bytes, T> {
    edges: typed_decode::Edges<'bytes>,
    next: u32,
    end: u32,
    decode: fn(
        &mut typed_decode::Edges<'bytes>,
        u32,
    ) -> Result<T, super::super::FullSemanticImageFault>,
}

impl<T> Iterator for FullGroupRows<'_, T> {
    type Item = T;
    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        let index = self.next;
        self.next = self.next.checked_add(1)?;
        // Admission parsed the same row grammar, bounds, and tag sequence.
        (self.decode)(&mut self.edges, index).ok()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = remaining(self.end, self.next);
        (remaining, Some(remaining))
    }
}
impl<T> ExactSizeIterator for FullGroupRows<'_, T> {}
impl<T> FusedIterator for FullGroupRows<'_, T> {}

/// Sequential documentation cursor.  Fragment rows are variable-length
/// records in one byte range, so the cursor keeps its parse offset instead of
/// reparsing the prefix of the range for every row.
pub struct FullDocRows<'bytes> {
    value: &'bytes [u8],
    count: u32,
    next: u32,
    cursor: usize,
}

impl Iterator for FullDocRows<'_> {
    type Item = DocFragment;
    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.count {
            return None;
        }
        let fragment = doc_fragment_at(self.value, &mut self.cursor)?;
        self.next = self.next.checked_add(1)?;
        Some(fragment)
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = remaining(self.count, self.next);
        (remaining, Some(remaining))
    }
}
impl ExactSizeIterator for FullDocRows<'_> {}
impl FusedIterator for FullDocRows<'_> {}

pub struct FullCanonicalEntities<'view, 'bytes> {
    pub(super) view: &'view SemanticImageView<'bytes>,
    pub(super) next: u32,
    pub(super) end: u32,
}
pub struct FullCanonicalCoreEntities<'view, 'bytes> {
    pub(super) view: &'view SemanticImageView<'bytes>,
    pub(super) next: u32,
    pub(super) end: u32,
}
pub struct FullCanonicalTypes<'view, 'bytes> {
    pub(super) view: &'view SemanticImageView<'bytes>,
    pub(super) next: u32,
    pub(super) end: u32,
}
pub struct FullCanonicalExternals<'view, 'bytes> {
    pub(super) view: &'view SemanticImageView<'bytes>,
    pub(super) next: u32,
    pub(super) end: u32,
}
pub struct FullLinks<'view, 'bytes> {
    pub(super) view: &'view SemanticImageView<'bytes>,
    pub(super) next: u32,
    pub(super) end: u32,
}
pub struct FullOccurrences<'view, 'bytes> {
    pub(super) view: &'view SemanticImageView<'bytes>,
    pub(super) next: u32,
    pub(super) end: u32,
}

macro_rules! exact_cursor {
    ($name:ident, $item:ty, $call:expr) => {
        impl Iterator for $name<'_, '_> {
            type Item = $item;
            fn next(&mut self) -> Option<Self::Item> {
                if self.next >= self.end {
                    return None;
                }
                let row = self.next;
                self.next = self.next.checked_add(1)?;
                $call(self.view, row).ok()
            }
            fn size_hint(&self) -> (usize, Option<usize>) {
                let remaining = remaining(self.end, self.next);
                (remaining, Some(remaining))
            }
        }
        impl ExactSizeIterator for $name<'_, '_> {}
        impl FusedIterator for $name<'_, '_> {}
    };
}

exact_cursor!(
    FullCanonicalEntities,
    SemanticEntity,
    |view: &SemanticImageView<'_>, row| decode::entity(view.bytes, view.layout(), row)
);
exact_cursor!(
    FullCanonicalTypes,
    (TypeId, TypeExpr),
    |view: &SemanticImageView<'_>, row| typed_decode::ty(
        view.bytes,
        view.layout(),
        view.typed(),
        TypeId::new(row)
    )
    .map(|value| (TypeId::new(row), value))
);
exact_cursor!(
    FullCanonicalExternals,
    (ExternalId, ExternalTarget),
    |view: &SemanticImageView<'_>, row| decode::external(view.bytes, view.layout(), row)
        .map(|value| (ExternalId::new(row), value))
);
exact_cursor!(
    FullLinks,
    (LinkId, Link),
    |view: &SemanticImageView<'_>, row| decode::link(view.bytes, view.layout(), row)
        .map(|value| (LinkId::new(row), value))
);
exact_cursor!(
    FullOccurrences,
    (LinkOccurrenceId, LinkOccurrence),
    |view: &SemanticImageView<'_>, row| decode::occurrence(view.bytes, view.layout(), row)
        .map(|(value, _)| (LinkOccurrenceId::new(row), value))
);

impl Iterator for FullCanonicalCoreEntities<'_, '_> {
    type Item = CoreSemanticEntity;
    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        let row = self.next;
        self.next = self.next.checked_add(1)?;
        let entity = decode::entity(self.view.bytes, self.view.layout(), row).ok()?;
        Some(CoreSemanticEntity {
            id: entity.id,
            name: entity.name,
            kind: entity.kind,
            visibility: entity.visibility,
            parent: entity.parent,
            authority: entity.authority,
            source: entity.source,
            version: entity.version,
        })
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = remaining(self.end, self.next);
        (remaining, Some(remaining))
    }
}
impl ExactSizeIterator for FullCanonicalCoreEntities<'_, '_> {}
impl FusedIterator for FullCanonicalCoreEntities<'_, '_> {}

pub struct FullExtensionRows<'view, 'bytes, Facts> {
    view: &'view SemanticImageView<'bytes>,
    bindings: FullDirectoryKind,
    facts: FullDirectoryKind,
    next: u32,
    end: u32,
    decode: fn(&[u8], u32, ExtensionCounts) -> Result<Facts, super::super::FullSemanticImageFault>,
}

impl<Facts> Iterator for FullExtensionRows<'_, '_, Facts> {
    type Item = (EntityId, Facts);
    fn next(&mut self) -> Option<Self::Item> {
        if self.next >= self.end {
            return None;
        }
        let row = self.next;
        self.next = self.next.checked_add(1)?;
        let binding = binding(self.view.bytes, self.view.layout(), self.bindings, row).ok()?;
        let value = validate::extension_value(
            self.view.bytes,
            self.view.layout().entry(self.facts),
            binding.1,
        )
        .ok()?;
        // The validator decoded exactly this fact plane before construction.
        (self.decode)(value, binding.1, self.view.extension_counts())
            .ok()
            .map(|facts| (EntityId::new(binding.0), facts))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = remaining(self.end, self.next);
        (remaining, Some(remaining))
    }
}
impl<Facts> ExactSizeIterator for FullExtensionRows<'_, '_, Facts> {}
impl<Facts> FusedIterator for FullExtensionRows<'_, '_, Facts> {}

pub(super) fn rows<'view, 'bytes, T>(
    view: &'view SemanticImageView<'bytes>,
    domain: u8,
    coordinate: u32,
    decode: fn(
        &mut typed_decode::Edges<'bytes>,
        u32,
    ) -> Result<T, super::super::FullSemanticImageFault>,
) -> Option<FullGroupRows<'bytes, T>> {
    let mut edges =
        typed_decode::Edges::for_node(view.bytes, view.layout(), view.typed(), domain, coordinate)
            .ok()?;
    let end = typed_decode::logical_count(&mut edges, domain).ok()?;
    Some(FullGroupRows {
        edges,
        next: 0,
        end,
        decode,
    })
}

pub(super) fn entity_rows<'view, 'bytes>(
    view: &'view SemanticImageView<'bytes>,
    id: EntityListId,
) -> Option<FullRows<'view, EntityId>> {
    let value = validate::range_value(
        view.bytes,
        view.layout().entry(FullDirectoryKind::EntityLists),
        view.layout().entry(FullDirectoryKind::EntityListBytes),
        id.raw,
        super::super::FullSemanticImageField::EntityLists,
    )
    .ok()?;
    let end = read_u32(value, 1)?;
    Some(FullRows {
        bytes: view.bytes,
        layout: view.layout(),
        typed: view.typed(),
        coordinate: id.raw,
        next: 0,
        end,
        decode: entity_list_row,
    })
}
pub(super) fn doc_rows<'view, 'bytes>(
    view: &'view SemanticImageView<'bytes>,
    id: DocId,
) -> Option<FullDocRows<'bytes>> {
    let value = validate::range_value(
        view.bytes,
        view.layout().entry(FullDirectoryKind::Documentation),
        view.layout().entry(FullDirectoryKind::DocumentationBytes),
        id.raw,
        super::super::FullSemanticImageField::Documentation,
    )
    .ok()?;
    let count = read_u32(value, 1)?;
    Some(FullDocRows {
        value,
        count,
        next: 0,
        cursor: 5,
    })
}

fn entity_list_row(
    bytes: &[u8],
    layout: FullImageLayout,
    _: TypedLayout,
    coordinate: u32,
    index: u32,
) -> Result<EntityId, super::super::FullSemanticImageFault> {
    let value = validate::range_value(
        bytes,
        layout.entry(FullDirectoryKind::EntityLists),
        layout.entry(FullDirectoryKind::EntityListBytes),
        coordinate,
        super::super::FullSemanticImageField::EntityLists,
    )?;
    let offset = usize::try_from(index)
        .map_err(|_| super::super::FullSemanticImageFault::LengthOverflow {
            field: super::super::FullSemanticImageField::EntityLists,
        })?
        .checked_mul(4)
        .and_then(|offset| offset.checked_add(5))
        .ok_or(super::super::FullSemanticImageFault::LengthOverflow {
            field: super::super::FullSemanticImageField::EntityLists,
        })?;
    Ok(EntityId::new(read_u32(value, offset).ok_or(
        super::super::FullSemanticImageFault::Truncated {
            field: super::super::FullSemanticImageField::EntityLists,
            offset,
        },
    )?))
}

/// Decodes one documentation fragment at `cursor`, advancing it past the
/// record.  Returns `None` on any truncation or discriminant fault, exactly
/// like the row decoder it replaces.
fn doc_fragment_at(value: &[u8], cursor: &mut usize) -> Option<DocFragment> {
    let tag = *value.get(*cursor)?;
    *cursor = cursor.checked_add(1)?;
    let fragment = match tag {
        0 => {
            let atom = AtomId::new(read_u32(value, *cursor)?);
            *cursor += 4;
            DocFragment::Text(crate::ir::TextId::new(atom.raw))
        }
        1 => {
            let atom = AtomId::new(read_u32(value, *cursor)?);
            *cursor += 4;
            DocFragment::Code(crate::ir::TextId::new(atom.raw))
        }
        2 => {
            let label = crate::ir::TextId::new(read_u32(value, *cursor)?);
            let target_tag = *value.get(cursor.checked_add(4)?)?;
            let target = read_u32(value, cursor.checked_add(5)?)?;
            *cursor += 9;
            let target = match target_tag {
                0 => crate::ir::LinkTarget::Local(EntityId::new(target)),
                1 => crate::ir::LinkTarget::External(ExternalId::new(target)),
                _ => return None,
            };
            DocFragment::Link { label, target }
        }
        3 => DocFragment::SoftBreak,
        4 => DocFragment::HardBreak,
        _ => return None,
    };
    Some(fragment)
}

pub(super) fn binary_entity(
    view: &SemanticImageView<'_>,
    identity: DeclarationIdentity,
) -> Option<EntityId> {
    let mut low = 0_u32;
    let mut high = view.layout().entry(FullDirectoryKind::Entities).count;
    while low < high {
        let middle = low + (high - low) / 2;
        let current = decode::entity(view.bytes, view.layout(), middle)
            .ok()?
            .version
            .identity();
        if current < identity {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    let entity = decode::entity(view.bytes, view.layout(), low).ok()?;
    (entity.version.identity() == identity).then_some(EntityId::new(low))
}

pub(super) fn lower_link(view: &SemanticImageView<'_>, entity: u32, count: u32) -> u32 {
    let mut low = 0_u32;
    let mut high = count;
    while low < high {
        let middle = low + (high - low) / 2;
        match decode::link(view.bytes, view.layout(), middle) {
            Ok(link) if link.from.raw < entity => low = middle + 1,
            Ok(_) => high = middle,
            Err(_) => return count,
        }
    }
    low
}

pub(super) fn extension_rows<'view, 'bytes, Facts>(
    view: &'view SemanticImageView<'bytes>,
    facts: FullDirectoryKind,
    bindings: FullDirectoryKind,
    decode: fn(&[u8], u32, ExtensionCounts) -> Result<Facts, super::super::FullSemanticImageFault>,
) -> FullExtensionRows<'view, 'bytes, Facts> {
    FullExtensionRows {
        view,
        bindings,
        facts,
        next: 0,
        end: view.layout().entry(bindings).count,
        decode,
    }
}

pub(super) fn extension<Facts>(
    view: &SemanticImageView<'_>,
    facts: FullDirectoryKind,
    bindings: FullDirectoryKind,
    entity: EntityId,
    decode: fn(&[u8], u32, ExtensionCounts) -> Result<Facts, super::super::FullSemanticImageFault>,
) -> Option<Facts> {
    let entry = view.layout().entry(bindings);
    let mut low = 0_u32;
    let mut high = entry.count;
    while low < high {
        let middle = low + (high - low) / 2;
        let (observed, _) = binding(view.bytes, view.layout(), bindings, middle).ok()?;
        if observed < entity.raw {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    let (observed, fact) = binding(view.bytes, view.layout(), bindings, low).ok()?;
    if observed != entity.raw {
        return None;
    }
    let value = validate::extension_value(view.bytes, view.layout().entry(facts), fact).ok()?;
    decode(value, fact, view.extension_counts()).ok()
}

fn binding(
    bytes: &[u8],
    layout: FullImageLayout,
    kind: FullDirectoryKind,
    row: u32,
) -> Result<(u32, u32), super::super::FullSemanticImageFault> {
    let entry = layout.entry(kind);
    if row >= entry.count {
        return Err(super::super::FullSemanticImageFault::Reference {
            field: super::super::FullSemanticImageField::ExtensionBindings,
            row,
            expected: entry.count,
            observed: row,
        });
    }
    let offset = entry
        .offset
        .checked_add(
            usize::try_from(row)
                .map_err(|_| super::super::FullSemanticImageFault::LengthOverflow {
                    field: super::super::FullSemanticImageField::ExtensionBindings,
                })?
                .checked_mul(SPARSE_BINDING_ROW_BYTES)
                .ok_or(super::super::FullSemanticImageFault::LengthOverflow {
                    field: super::super::FullSemanticImageField::ExtensionBindings,
                })?,
        )
        .ok_or(super::super::FullSemanticImageFault::LengthOverflow {
            field: super::super::FullSemanticImageField::ExtensionBindings,
        })?;
    Ok((
        get_u32(
            bytes,
            offset,
            super::super::FullSemanticImageField::ExtensionBindings,
        )?,
        get_u32(
            bytes,
            offset + 4,
            super::super::FullSemanticImageField::ExtensionBindings,
        )?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let value = bytes.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes(<[u8; 4]>::try_from(value).ok()?))
}

fn remaining(end: u32, next: u32) -> usize {
    match end
        .checked_sub(next)
        .and_then(|value| usize::try_from(value).ok())
    {
        Some(value) => value,
        // Construction validates all directory counts on a supported
        // 32-or-larger-bit target, so this branch is unreachable for a view.
        None => 0,
    }
}
