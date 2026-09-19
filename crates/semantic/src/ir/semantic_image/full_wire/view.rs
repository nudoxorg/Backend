//! Allocation-free borrowed full semantic-image reader.
//!
//! Construction is private to `reopen`: every iterator below relies only on
//! the structural proof recorded there and consequently never reparses into
//! an owned semantic plane or exposes native-layout slices.

use core::{iter::FusedIterator, ops::Deref};

use crate::ir::{
    AtomId, AtomListId, CSharpFacts, ClangFacts, CoreSemanticEntity, DeclarationIdentity,
    DocFragment, DocId, EntityId, EntityListId, ExternalId, ExternalTarget, FreePredicate,
    FreePredicateListId, GoFacts, JavaFacts, Link, LinkId, LinkOccurrence, LinkOccurrenceId,
    ObjectMember, ObjectMemberListId,
    OccurrenceAuthorityFacts, PythonFacts, RustFacts, SemanticCoreReader, SemanticEntity,
    SemanticImageFacts, SemanticReader, TemplatePart, TemplatePartListId, TupleElement,
    TupleElementListId, TypeExpr, TypeId, TypeListId, TypeParameter, TypeParameterBound,
    TypeParameterBoundListId, TypeParameterListId, TypeScriptFacts,
};

use super::{
    decode,
    extensions_decode::{self, ExtensionCounts},
    fault::FullSemanticImageError,
    typed_decode,
    validate::{self, TypedLayout, ValidatedFullImage},
    wire::{FullDirectoryKind, FullImageLayout, SPARSE_BINDING_ROW_BYTES, get_u32},
};

/// Fully validated borrowed `NXFI` semantic image.
///
/// It borrows exactly the caller-owned fragment/mmap bytes; all iterators
/// reuse that backing region and do not allocate or reinterpret Rust-layout
/// records. No partial image can manufacture this type because `reopen`
/// validates every complete-reader plane before returning.
pub struct SemanticImageView<'bytes> {
    bytes: &'bytes [u8],
    validated: ValidatedFullImage,
    facts: SemanticImageFacts,
}

impl<'bytes> SemanticImageView<'bytes> {
    pub fn reopen(bytes: &'bytes [u8]) -> Result<Self, FullSemanticImageError> {
        let validated = validate::reopen_full_semantic_image(bytes)?;
        Ok(Self {
            bytes,
            facts: validated.image,
            validated,
        })
    }

    fn layout(&self) -> FullImageLayout {
        self.validated.layout
    }
    fn typed(&self) -> TypedLayout {
        self.validated.typed
    }
    fn extension_counts(&self) -> ExtensionCounts {
        ExtensionCounts {
            atoms: self.layout().entry(FullDirectoryKind::Atoms).count,
            members: self.layout().entry(FullDirectoryKind::EntityLists).count,
            typed: self.typed(),
        }
    }
}

impl Deref for SemanticImageView<'_> {
    type Target = SemanticImageFacts;
    fn deref(&self) -> &Self::Target {
        &self.facts
    }
}

/// Returns the exact validated portable image bytes without copying or
/// re-encoding. Publication can therefore bind the same byte region that the
/// borrowed reader proved before it hashes or persists the generation.
impl AsRef<[u8]> for SemanticImageView<'_> {
    fn as_ref(&self) -> &[u8] {
        self.bytes
    }
}

impl crate::ir::reader::sealed::Sealed for SemanticImageView<'_> {}

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
    ) -> Result<T, super::FullSemanticImageFault>,
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
    ) -> Result<T, super::FullSemanticImageFault>,
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
    view: &'view SemanticImageView<'bytes>,
    next: u32,
    end: u32,
}
pub struct FullCanonicalCoreEntities<'view, 'bytes> {
    view: &'view SemanticImageView<'bytes>,
    next: u32,
    end: u32,
}
pub struct FullCanonicalTypes<'view, 'bytes> {
    view: &'view SemanticImageView<'bytes>,
    next: u32,
    end: u32,
}
pub struct FullCanonicalExternals<'view, 'bytes> {
    view: &'view SemanticImageView<'bytes>,
    next: u32,
    end: u32,
}
pub struct FullLinks<'view, 'bytes> {
    view: &'view SemanticImageView<'bytes>,
    next: u32,
    end: u32,
}
pub struct FullOccurrences<'view, 'bytes> {
    view: &'view SemanticImageView<'bytes>,
    next: u32,
    end: u32,
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
    decode: fn(&[u8], u32, ExtensionCounts) -> Result<Facts, super::FullSemanticImageFault>,
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

impl<'bytes> SemanticCoreReader for SemanticImageView<'bytes> {
    type CanonicalCoreEntities<'image>
        = FullCanonicalCoreEntities<'image, 'bytes>
    where
        Self: 'image;

    fn image_facts(&self) -> SemanticImageFacts {
        self.facts
    }
    fn core_entity(&self, id: EntityId) -> Option<CoreSemanticEntity> {
        let entity = decode::entity(self.bytes, self.layout(), id.raw).ok()?;
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
    fn core_entity_by_identity(&self, identity: DeclarationIdentity) -> Option<CoreSemanticEntity> {
        binary_entity(self, identity).and_then(|id| self.core_entity(id))
    }
    fn atom(&self, id: AtomId) -> Option<&[u8]> {
        validate::atom_value(self.bytes, self.layout(), id.raw).ok()
    }
    fn text(&self, id: crate::ir::TextId) -> Option<&str> {
        core::str::from_utf8(self.atom(AtomId::new(id.raw))?).ok()
    }
    fn canonical_core_entities(&self) -> Self::CanonicalCoreEntities<'_> {
        FullCanonicalCoreEntities {
            view: self,
            next: 0,
            end: self.layout().entry(FullDirectoryKind::Entities).count,
        }
    }
}

impl<'bytes> SemanticReader for SemanticImageView<'bytes> {
    type CanonicalEntities<'image>
        = FullCanonicalEntities<'image, 'bytes>
    where
        Self: 'image;
    type CanonicalLinks<'image>
        = FullLinks<'image, 'bytes>
    where
        Self: 'image;
    type Links<'image>
        = FullLinks<'image, 'bytes>
    where
        Self: 'image;
    type Occurrences<'image>
        = FullOccurrences<'image, 'bytes>
    where
        Self: 'image;
    type TypeScriptExtensions<'image>
        = FullExtensionRows<'image, 'bytes, TypeScriptFacts>
    where
        Self: 'image;
    type CSharpExtensions<'image>
        = FullExtensionRows<'image, 'bytes, CSharpFacts>
    where
        Self: 'image;
    type GoExtensions<'image>
        = FullExtensionRows<'image, 'bytes, GoFacts>
    where
        Self: 'image;
    type RustExtensions<'image>
        = FullExtensionRows<'image, 'bytes, RustFacts>
    where
        Self: 'image;
    type PythonExtensions<'image>
        = FullExtensionRows<'image, 'bytes, PythonFacts>
    where
        Self: 'image;
    type JavaExtensions<'image>
        = FullExtensionRows<'image, 'bytes, JavaFacts>
    where
        Self: 'image;
    type ClangExtensions<'image>
        = FullExtensionRows<'image, 'bytes, ClangFacts>
    where
        Self: 'image;
    type Types<'image>
        = FullGroupRows<'image, TypeId>
    where
        Self: 'image;
    type Atoms<'image>
        = FullGroupRows<'image, AtomId>
    where
        Self: 'image;
    type Entities<'image>
        = FullRows<'image, EntityId>
    where
        Self: 'image;
    type Docs<'image>
        = FullDocRows<'image>
    where
        Self: 'image;
    type TupleElements<'image>
        = FullGroupRows<'image, TupleElement>
    where
        Self: 'image;
    type ObjectMembers<'image>
        = FullGroupRows<'image, ObjectMember>
    where
        Self: 'image;
    type TemplateParts<'image>
        = FullGroupRows<'image, TemplatePart>
    where
        Self: 'image;
    type TypeParameters<'image>
        = FullGroupRows<'image, TypeParameter>
    where
        Self: 'image;
    type TypeParameterBounds<'image>
        = FullGroupRows<'image, TypeParameterBound>
    where
        Self: 'image;
    type FreePredicates<'image>
        = FullGroupRows<'image, FreePredicate>
    where
        Self: 'image;
    type CanonicalTypes<'image>
        = FullCanonicalTypes<'image, 'bytes>
    where
        Self: 'image;
    type CanonicalExternals<'image>
        = FullCanonicalExternals<'image, 'bytes>
    where
        Self: 'image;

    fn entity(&self, id: EntityId) -> Option<SemanticEntity> {
        decode::entity(self.bytes, self.layout(), id.raw).ok()
    }
    fn entity_by_identity(&self, identity: DeclarationIdentity) -> Option<SemanticEntity> {
        binary_entity(self, identity).and_then(|id| self.entity(id))
    }
    fn external(&self, id: ExternalId) -> Option<ExternalTarget> {
        decode::external(self.bytes, self.layout(), id.raw).ok()
    }
    fn link(&self, id: LinkId) -> Option<Link> {
        decode::link(self.bytes, self.layout(), id.raw).ok()
    }
    fn occurrence_authority(&self, id: LinkOccurrenceId) -> Option<OccurrenceAuthorityFacts> {
        decode::occurrence(self.bytes, self.layout(), id.raw)
            .ok()
            .map(|(_, authority)| authority)
    }

    fn ty(&self, id: TypeId) -> Option<TypeExpr> {
        typed_decode::ty(self.bytes, self.layout(), self.typed(), id).ok()
    }
    fn types(&self, id: TypeListId) -> Option<Self::Types<'_>> {
        rows(self, 1, id.raw, typed_decode::type_list_item)
    }
    fn atom_list(&self, id: AtomListId) -> Option<Self::Atoms<'_>> {
        rows(self, 5, id.raw, typed_decode::atom_list_item)
    }
    fn entity_list(&self, id: EntityListId) -> Option<Self::Entities<'_>> {
        entity_rows(self, id)
    }
    fn docs(&self, id: DocId) -> Option<Self::Docs<'_>> {
        doc_rows(self, id)
    }
    fn tuple_elements(&self, id: TupleElementListId) -> Option<Self::TupleElements<'_>> {
        rows(self, 2, id.raw, typed_decode::tuple_element)
    }
    fn object_members(&self, id: ObjectMemberListId) -> Option<Self::ObjectMembers<'_>> {
        rows(self, 3, id.raw, typed_decode::object_member)
    }
    fn template_parts(&self, id: TemplatePartListId) -> Option<Self::TemplateParts<'_>> {
        rows(self, 4, id.raw, typed_decode::template_part)
    }
    fn type_parameters(&self, id: TypeParameterListId) -> Option<Self::TypeParameters<'_>> {
        rows(self, 6, id.raw, typed_decode::type_parameter)
    }
    fn type_parameter_bounds(
        &self,
        id: TypeParameterBoundListId,
    ) -> Option<Self::TypeParameterBounds<'_>> {
        rows(self, 7, id.raw, typed_decode::type_parameter_bound)
    }
    fn free_predicates(&self, id: FreePredicateListId) -> Option<Self::FreePredicates<'_>> {
        rows(self, 8, id.raw, typed_decode::free_predicate)
    }

    fn canonical_entities(&self) -> Self::CanonicalEntities<'_> {
        FullCanonicalEntities {
            view: self,
            next: 0,
            end: self.layout().entry(FullDirectoryKind::Entities).count,
        }
    }
    fn canonical_links(&self) -> Self::CanonicalLinks<'_> {
        FullLinks {
            view: self,
            next: 0,
            end: self.layout().entry(FullDirectoryKind::Links).count,
        }
    }
    fn canonical_types(&self) -> Self::CanonicalTypes<'_> {
        FullCanonicalTypes {
            view: self,
            next: 0,
            end: self.typed().counts[0],
        }
    }
    fn canonical_externals(&self) -> Self::CanonicalExternals<'_> {
        FullCanonicalExternals {
            view: self,
            next: 0,
            end: self.layout().entry(FullDirectoryKind::Externals).count,
        }
    }
    fn links_from(&self, entity: EntityId) -> Self::Links<'_> {
        let entry = self.layout().entry(FullDirectoryKind::Links);
        let start = lower_link(self, entity.raw, entry.count);
        let mut end = start;
        while end < entry.count
            && decode::link(self.bytes, self.layout(), end)
                .ok()
                .is_some_and(|link| link.from == entity)
        {
            end += 1;
        }
        FullLinks {
            view: self,
            next: start,
            end,
        }
    }
    fn link_occurrences(&self) -> Self::Occurrences<'_> {
        FullOccurrences {
            view: self,
            next: 0,
            end: self.layout().entry(FullDirectoryKind::Occurrences).count,
        }
    }
    fn typescript_extensions(&self) -> Self::TypeScriptExtensions<'_> {
        extension_rows(
            self,
            FullDirectoryKind::TypeScriptFacts,
            FullDirectoryKind::TypeScriptBindings,
            extensions_decode::typescript,
        )
    }
    fn csharp_extensions(&self) -> Self::CSharpExtensions<'_> {
        extension_rows(
            self,
            FullDirectoryKind::CSharpFacts,
            FullDirectoryKind::CSharpBindings,
            extensions_decode::csharp,
        )
    }
    fn go_extensions(&self) -> Self::GoExtensions<'_> {
        extension_rows(
            self,
            FullDirectoryKind::GoFacts,
            FullDirectoryKind::GoBindings,
            extensions_decode::go,
        )
    }
    fn rust_extensions(&self) -> Self::RustExtensions<'_> {
        extension_rows(
            self,
            FullDirectoryKind::RustFacts,
            FullDirectoryKind::RustBindings,
            extensions_decode::rust,
        )
    }
    fn python_extensions(&self) -> Self::PythonExtensions<'_> {
        extension_rows(
            self,
            FullDirectoryKind::PythonFacts,
            FullDirectoryKind::PythonBindings,
            extensions_decode::python,
        )
    }
    fn java_extensions(&self) -> Self::JavaExtensions<'_> {
        extension_rows(
            self,
            FullDirectoryKind::JavaFacts,
            FullDirectoryKind::JavaBindings,
            extensions_decode::java,
        )
    }
    fn clang_extensions(&self) -> Self::ClangExtensions<'_> {
        extension_rows(
            self,
            FullDirectoryKind::ClangFacts,
            FullDirectoryKind::ClangBindings,
            extensions_decode::clang,
        )
    }
    fn typescript_extension(&self, entity: EntityId) -> Option<TypeScriptFacts> {
        extension(
            self,
            FullDirectoryKind::TypeScriptFacts,
            FullDirectoryKind::TypeScriptBindings,
            entity,
            extensions_decode::typescript,
        )
    }
    fn csharp_extension(&self, entity: EntityId) -> Option<CSharpFacts> {
        extension(
            self,
            FullDirectoryKind::CSharpFacts,
            FullDirectoryKind::CSharpBindings,
            entity,
            extensions_decode::csharp,
        )
    }
    fn go_extension(&self, entity: EntityId) -> Option<GoFacts> {
        extension(
            self,
            FullDirectoryKind::GoFacts,
            FullDirectoryKind::GoBindings,
            entity,
            extensions_decode::go,
        )
    }
    fn rust_extension(&self, entity: EntityId) -> Option<RustFacts> {
        extension(
            self,
            FullDirectoryKind::RustFacts,
            FullDirectoryKind::RustBindings,
            entity,
            extensions_decode::rust,
        )
    }
    fn python_extension(&self, entity: EntityId) -> Option<PythonFacts> {
        extension(
            self,
            FullDirectoryKind::PythonFacts,
            FullDirectoryKind::PythonBindings,
            entity,
            extensions_decode::python,
        )
    }
    fn java_extension(&self, entity: EntityId) -> Option<JavaFacts> {
        extension(
            self,
            FullDirectoryKind::JavaFacts,
            FullDirectoryKind::JavaBindings,
            entity,
            extensions_decode::java,
        )
    }
    fn clang_extension(&self, entity: EntityId) -> Option<ClangFacts> {
        extension(
            self,
            FullDirectoryKind::ClangFacts,
            FullDirectoryKind::ClangBindings,
            entity,
            extensions_decode::clang,
        )
    }
}

fn rows<'view, 'bytes, T>(
    view: &'view SemanticImageView<'bytes>,
    domain: u8,
    coordinate: u32,
    decode: fn(
        &mut typed_decode::Edges<'bytes>,
        u32,
    ) -> Result<T, super::FullSemanticImageFault>,
) -> Option<FullGroupRows<'bytes, T>> {
    let mut edges = typed_decode::Edges::for_node(
        view.bytes,
        view.layout(),
        view.typed(),
        domain,
        coordinate,
    )
    .ok()?;
    let end = typed_decode::logical_count(&mut edges, domain).ok()?;
    Some(FullGroupRows {
        edges,
        next: 0,
        end,
        decode,
    })
}

fn entity_rows<'view, 'bytes>(
    view: &'view SemanticImageView<'bytes>,
    id: EntityListId,
) -> Option<FullRows<'view, EntityId>> {
    let value = validate::range_value(
        view.bytes,
        view.layout().entry(FullDirectoryKind::EntityLists),
        view.layout().entry(FullDirectoryKind::EntityListBytes),
        id.raw,
        super::FullSemanticImageField::EntityLists,
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
fn doc_rows<'view, 'bytes>(
    view: &'view SemanticImageView<'bytes>,
    id: DocId,
) -> Option<FullDocRows<'bytes>> {
    let value = validate::range_value(
        view.bytes,
        view.layout().entry(FullDirectoryKind::Documentation),
        view.layout().entry(FullDirectoryKind::DocumentationBytes),
        id.raw,
        super::FullSemanticImageField::Documentation,
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
) -> Result<EntityId, super::FullSemanticImageFault> {
    let value = validate::range_value(
        bytes,
        layout.entry(FullDirectoryKind::EntityLists),
        layout.entry(FullDirectoryKind::EntityListBytes),
        coordinate,
        super::FullSemanticImageField::EntityLists,
    )?;
    let offset = usize::try_from(index)
        .map_err(|_| super::FullSemanticImageFault::LengthOverflow {
            field: super::FullSemanticImageField::EntityLists,
        })?
        .checked_mul(4)
        .and_then(|offset| offset.checked_add(5))
        .ok_or(super::FullSemanticImageFault::LengthOverflow {
            field: super::FullSemanticImageField::EntityLists,
        })?;
    Ok(EntityId::new(read_u32(value, offset).ok_or(
        super::FullSemanticImageFault::Truncated {
            field: super::FullSemanticImageField::EntityLists,
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

fn binary_entity(view: &SemanticImageView<'_>, identity: DeclarationIdentity) -> Option<EntityId> {
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

fn lower_link(view: &SemanticImageView<'_>, entity: u32, count: u32) -> u32 {
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

fn extension_rows<'view, 'bytes, Facts>(
    view: &'view SemanticImageView<'bytes>,
    facts: FullDirectoryKind,
    bindings: FullDirectoryKind,
    decode: fn(&[u8], u32, ExtensionCounts) -> Result<Facts, super::FullSemanticImageFault>,
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

fn extension<Facts>(
    view: &SemanticImageView<'_>,
    facts: FullDirectoryKind,
    bindings: FullDirectoryKind,
    entity: EntityId,
    decode: fn(&[u8], u32, ExtensionCounts) -> Result<Facts, super::FullSemanticImageFault>,
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
) -> Result<(u32, u32), super::FullSemanticImageFault> {
    let entry = layout.entry(kind);
    if row >= entry.count {
        return Err(super::FullSemanticImageFault::Reference {
            field: super::FullSemanticImageField::ExtensionBindings,
            row,
            expected: entry.count,
            observed: row,
        });
    }
    let offset = entry
        .offset
        .checked_add(
            usize::try_from(row)
                .map_err(|_| super::FullSemanticImageFault::LengthOverflow {
                    field: super::FullSemanticImageField::ExtensionBindings,
                })?
                .checked_mul(SPARSE_BINDING_ROW_BYTES)
                .ok_or(super::FullSemanticImageFault::LengthOverflow {
                    field: super::FullSemanticImageField::ExtensionBindings,
                })?,
        )
        .ok_or(super::FullSemanticImageFault::LengthOverflow {
            field: super::FullSemanticImageField::ExtensionBindings,
        })?;
    Ok((
        get_u32(
            bytes,
            offset,
            super::FullSemanticImageField::ExtensionBindings,
        )?,
        get_u32(
            bytes,
            offset + 4,
            super::FullSemanticImageField::ExtensionBindings,
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
