//! Canonical cursors over a finalized `Ir`, plus the reader implementations.
//!
//! The sealed traits stay in the parent. These iterators are the exact-size
//! walks those traits return.

use core::iter::FusedIterator;
use core::slice;

use super::{
    CoreSemanticEntity, SemanticCoreReader, SemanticCursor, SemanticEntity, SemanticImageFacts,
    SemanticReader, sealed,
};
use crate::ir::{
    AtomId, AtomListId, CSharpExtension, CSharpFacts, ClangExtension, ClangFacts,
    DeclarationIdentity, DocFragment, DocId, EntityId, EntityListId, ExternalId, ExternalTarget,
    FreePredicate, FreePredicateListId, GoExtension, GoFacts, Ir, JavaExtension, JavaFacts,
    LanguageExtensionColumnView, Link, LinkId, LinkIter, LinkOccurrenceId, LinkOccurrenceIter,
    ObjectMember, ObjectMemberListId, OccurrenceAuthorityFacts, PythonExtension, PythonFacts,
    RustExtension, RustFacts, TemplatePart, TemplatePartListId, TupleElement, TupleElementListId,
    TypeExpr, TypeId, TypeListId, TypeParameter, TypeParameterBound, TypeParameterBoundListId,
    TypeParameterListId, TypeScriptExtension, TypeScriptFacts,
};

/// Exact-size canonical entity iterator backed by `Ir`'s declaration index.
pub struct IrCanonicalEntities<'image> {
    ir: &'image Ir,
    items: crate::ir::ItemIdIter<'image>,
}

/// Exact-size canonical graph cursor backed by `Ir`'s stable link index.
pub struct IrCanonicalLinks<'image> {
    ir: &'image Ir,
    ids: slice::Iter<'image, LinkId>,
}

impl Iterator for IrCanonicalLinks<'_> {
    type Item = (LinkId, Link);

    fn next(&mut self) -> Option<Self::Item> {
        let id = *self.ids.next()?;
        self.ir.link(id).map(|link| (id, link))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.ids.size_hint()
    }
}

impl ExactSizeIterator for IrCanonicalLinks<'_> {}
impl FusedIterator for IrCanonicalLinks<'_> {}

/// Exact-size canonical core iterator backed by `Ir`'s declaration index.
pub struct IrCanonicalCoreEntities<'image> {
    ir: &'image Ir,
    items: crate::ir::ItemIdIter<'image>,
}

/// Exact-size complete type census over finalized `Ir` coordinates.
pub struct IrCanonicalTypes<'image> {
    ir: &'image Ir,
    next: usize,
    end: usize,
}

/// Exact-size complete external-endpoint census over finalized `Ir`
/// coordinates.
pub struct IrCanonicalExternals<'image> {
    values: &'image [ExternalTarget],
    next: usize,
}

impl Iterator for IrCanonicalCoreEntities<'_> {
    type Item = CoreSemanticEntity;

    fn next(&mut self) -> Option<Self::Item> {
        let id = self.items.next()?.id();
        self.ir.core_semantic_entity(id)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.items.size_hint()
    }
}
impl ExactSizeIterator for IrCanonicalCoreEntities<'_> {}
impl FusedIterator for IrCanonicalCoreEntities<'_> {}

impl Iterator for IrCanonicalEntities<'_> {
    type Item = SemanticEntity;

    fn next(&mut self) -> Option<Self::Item> {
        let id = self.items.next()?.id();
        self.ir.semantic_entity(id)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.items.size_hint()
    }
}
impl ExactSizeIterator for IrCanonicalEntities<'_> {}
impl FusedIterator for IrCanonicalEntities<'_> {}

impl Iterator for IrCanonicalTypes<'_> {
    type Item = (TypeId, TypeExpr);

    fn next(&mut self) -> Option<Self::Item> {
        let raw = self.next;
        self.next = self.next.checked_add(1)?;
        if raw >= self.end {
            return None;
        }
        let id = TypeId::new(u32::try_from(raw).ok()?);
        Some((id, self.ir.ty(id)?))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.end.saturating_sub(self.next);
        (remaining, Some(remaining))
    }
}
impl ExactSizeIterator for IrCanonicalTypes<'_> {}
impl FusedIterator for IrCanonicalTypes<'_> {}

impl Iterator for IrCanonicalExternals<'_> {
    type Item = (ExternalId, ExternalTarget);

    fn next(&mut self) -> Option<Self::Item> {
        let raw = self.next;
        self.next = self.next.checked_add(1)?;
        let value = *self.values.get(raw)?;
        Some((ExternalId::new(u32::try_from(raw).ok()?), value))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.values.len().saturating_sub(self.next);
        (remaining, Some(remaining))
    }
}
impl ExactSizeIterator for IrCanonicalExternals<'_> {}
impl FusedIterator for IrCanonicalExternals<'_> {}

/// Exact-size iterator over one present-only language-extension sparse plane.
/// It retains no union payload and never walks the other six planes.
pub struct IrExtensionRows<'image, Facts, Space> {
    plane: LanguageExtensionColumnView<'image, Facts, Space>,
    next: u32,
    end: u32,
    remaining: usize,
}

impl<'image, Facts, Space> IrExtensionRows<'image, Facts, Space> {
    fn new(plane: LanguageExtensionColumnView<'image, Facts, Space>) -> Self {
        let remaining = plane.ids.values().len();
        let end = match u32::try_from(plane.ids.row_count()) {
            Ok(value) => value,
            // Entity coordinates reserve `u32::MAX` for absence, so a
            // finalized `Ir` cannot carry a larger aligned extension lane.
            Err(_) => u32::MAX,
        };
        Self {
            plane,
            next: 0,
            end,
            remaining,
        }
    }
}

impl<Facts: Copy, Space> Iterator for IrExtensionRows<'_, Facts, Space> {
    type Item = (EntityId, Facts);

    fn next(&mut self) -> Option<Self::Item> {
        while self.next < self.end {
            let entity = EntityId::new(self.next);
            self.next += 1;
            if let Some(facts) = self.plane.get(entity).copied() {
                self.remaining = self.remaining.saturating_sub(1);
                return Some((entity, facts));
            }
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}
impl<Facts: Copy, Space> ExactSizeIterator for IrExtensionRows<'_, Facts, Space> {}
impl<Facts: Copy, Space> FusedIterator for IrExtensionRows<'_, Facts, Space> {}

impl sealed::Sealed for Ir {}

impl SemanticCoreReader for Ir {
    type CanonicalCoreEntities<'image> = IrCanonicalCoreEntities<'image>;

    fn image_facts(&self) -> SemanticImageFacts {
        SemanticImageFacts {
            authority: self.storage_columns().authority,
            provenance: self.image_provenance(),
        }
    }

    fn core_entity(&self, id: EntityId) -> Option<CoreSemanticEntity> {
        Ir::core_semantic_entity(self, id)
    }

    fn core_entity_by_identity(&self, identity: DeclarationIdentity) -> Option<CoreSemanticEntity> {
        Ir::find_declaration(self, identity)
            .and_then(|item| Ir::core_semantic_entity(self, item.id()))
    }

    fn atom(&self, id: AtomId) -> Option<&[u8]> {
        Ir::atom(self, id)
    }
    fn text(&self, id: crate::ir::TextId) -> Option<&str> {
        Ir::text(self, id)
    }
    fn canonical_core_entities(&self) -> Self::CanonicalCoreEntities<'_> {
        IrCanonicalCoreEntities {
            ir: self,
            items: Ir::canonical_items(self),
        }
    }
}

impl SemanticReader for Ir {
    type CanonicalEntities<'image> = IrCanonicalEntities<'image>;
    type CanonicalLinks<'image> = IrCanonicalLinks<'image>;
    type Links<'image> = LinkIter<'image>;
    type Occurrences<'image> = LinkOccurrenceIter<'image>;
    type TypeScriptExtensions<'image> =
        IrExtensionRows<'image, TypeScriptFacts, TypeScriptExtension>;
    type CSharpExtensions<'image> = IrExtensionRows<'image, CSharpFacts, CSharpExtension>;
    type GoExtensions<'image> = IrExtensionRows<'image, GoFacts, GoExtension>;
    type RustExtensions<'image> = IrExtensionRows<'image, RustFacts, RustExtension>;
    type PythonExtensions<'image> = IrExtensionRows<'image, PythonFacts, PythonExtension>;
    type JavaExtensions<'image> = IrExtensionRows<'image, JavaFacts, JavaExtension>;
    type ClangExtensions<'image> = IrExtensionRows<'image, ClangFacts, ClangExtension>;
    type Types<'image> = SemanticCursor<'image, TypeId>;
    type Atoms<'image> = SemanticCursor<'image, AtomId>;
    type Entities<'image> = SemanticCursor<'image, EntityId>;
    type Docs<'image> = SemanticCursor<'image, DocFragment>;
    type TupleElements<'image> = SemanticCursor<'image, TupleElement>;
    type ObjectMembers<'image> = SemanticCursor<'image, ObjectMember>;
    type TemplateParts<'image> = SemanticCursor<'image, TemplatePart>;
    type TypeParameters<'image> = SemanticCursor<'image, TypeParameter>;
    type TypeParameterBounds<'image> = SemanticCursor<'image, TypeParameterBound>;
    type FreePredicates<'image> = SemanticCursor<'image, FreePredicate>;
    type CanonicalTypes<'image> = IrCanonicalTypes<'image>;
    type CanonicalExternals<'image> = IrCanonicalExternals<'image>;

    fn entity(&self, id: EntityId) -> Option<SemanticEntity> {
        Ir::semantic_entity(self, id)
    }
    fn entity_by_identity(&self, identity: DeclarationIdentity) -> Option<SemanticEntity> {
        Ir::find_declaration(self, identity).and_then(|item| Ir::semantic_entity(self, item.id()))
    }
    fn external(&self, id: ExternalId) -> Option<ExternalTarget> {
        Ir::external(self, id).copied()
    }
    fn link(&self, id: LinkId) -> Option<Link> {
        Ir::link(self, id)
    }
    fn occurrence_authority(&self, id: LinkOccurrenceId) -> Option<OccurrenceAuthorityFacts> {
        self.occurrence_authority_columns()
            .source
            .get(id.index())
            .copied()
            .map(|source| OccurrenceAuthorityFacts { source })
    }

    fn ty(&self, id: TypeId) -> Option<TypeExpr> {
        Ir::ty(self, id)
    }
    fn types(&self, id: TypeListId) -> Option<Self::Types<'_>> {
        Ir::types(self, id).map(|rows| rows.iter().copied())
    }
    fn atom_list(&self, id: AtomListId) -> Option<Self::Atoms<'_>> {
        Ir::atom_list(self, id).map(|rows| rows.iter().copied())
    }
    fn entity_list(&self, id: EntityListId) -> Option<Self::Entities<'_>> {
        Ir::entity_list(self, id).map(|rows| rows.iter().copied())
    }
    fn docs(&self, id: DocId) -> Option<Self::Docs<'_>> {
        Ir::documentation(self, id).map(|rows| rows.iter().copied())
    }
    fn tuple_elements(&self, id: TupleElementListId) -> Option<Self::TupleElements<'_>> {
        Ir::tuple_elements(self, id).map(|rows| rows.iter().copied())
    }
    fn object_members(&self, id: ObjectMemberListId) -> Option<Self::ObjectMembers<'_>> {
        Ir::object_members(self, id).map(|rows| rows.iter().copied())
    }
    fn template_parts(&self, id: TemplatePartListId) -> Option<Self::TemplateParts<'_>> {
        Ir::template_parts(self, id).map(|rows| rows.iter().copied())
    }
    fn type_parameters(&self, id: TypeParameterListId) -> Option<Self::TypeParameters<'_>> {
        Ir::type_parameters(self, id).map(|rows| rows.iter().copied())
    }
    fn type_parameter_bounds(
        &self,
        id: TypeParameterBoundListId,
    ) -> Option<Self::TypeParameterBounds<'_>> {
        Ir::type_parameter_bounds(self, id).map(|rows| rows.iter().copied())
    }
    fn free_predicates(&self, id: FreePredicateListId) -> Option<Self::FreePredicates<'_>> {
        Ir::free_predicates(self, id).map(|rows| rows.iter().copied())
    }

    fn canonical_entities(&self) -> Self::CanonicalEntities<'_> {
        IrCanonicalEntities {
            ir: self,
            items: Ir::canonical_items(self),
        }
    }
    fn canonical_links(&self) -> Self::CanonicalLinks<'_> {
        IrCanonicalLinks {
            ir: self,
            ids: self.canonical_link_ids().iter(),
        }
    }
    fn canonical_types(&self) -> Self::CanonicalTypes<'_> {
        IrCanonicalTypes {
            ir: self,
            next: 0,
            end: self.storage_columns().types.headers.len(),
        }
    }
    fn canonical_externals(&self) -> Self::CanonicalExternals<'_> {
        IrCanonicalExternals {
            values: self.storage_columns().externals,
            next: 0,
        }
    }
    fn links_from(&self, entity: EntityId) -> Self::Links<'_> {
        Ir::links_from(self, entity)
    }
    fn link_occurrences(&self) -> Self::Occurrences<'_> {
        Ir::link_occurrences(self)
    }
    fn typescript_extensions(&self) -> Self::TypeScriptExtensions<'_> {
        let plane = self.language_extensions().typescript;
        IrExtensionRows::new(plane)
    }
    fn csharp_extensions(&self) -> Self::CSharpExtensions<'_> {
        let plane = self.language_extensions().csharp;
        IrExtensionRows::new(plane)
    }
    fn go_extensions(&self) -> Self::GoExtensions<'_> {
        let plane = self.language_extensions().go;
        IrExtensionRows::new(plane)
    }
    fn rust_extensions(&self) -> Self::RustExtensions<'_> {
        let plane = self.language_extensions().rust;
        IrExtensionRows::new(plane)
    }
    fn python_extensions(&self) -> Self::PythonExtensions<'_> {
        let plane = self.language_extensions().python;
        IrExtensionRows::new(plane)
    }
    fn java_extensions(&self) -> Self::JavaExtensions<'_> {
        let plane = self.language_extensions().java;
        IrExtensionRows::new(plane)
    }
    fn clang_extensions(&self) -> Self::ClangExtensions<'_> {
        let plane = self.language_extensions().clang;
        IrExtensionRows::new(plane)
    }
    fn typescript_extension(&self, entity: EntityId) -> Option<TypeScriptFacts> {
        self.language_extensions().typescript.get(entity).copied()
    }
    fn csharp_extension(&self, entity: EntityId) -> Option<CSharpFacts> {
        self.language_extensions().csharp.get(entity).copied()
    }
    fn go_extension(&self, entity: EntityId) -> Option<GoFacts> {
        self.language_extensions().go.get(entity).copied()
    }
    fn rust_extension(&self, entity: EntityId) -> Option<RustFacts> {
        self.language_extensions().rust.get(entity).copied()
    }
    fn python_extension(&self, entity: EntityId) -> Option<PythonFacts> {
        self.language_extensions().python.get(entity).copied()
    }
    fn java_extension(&self, entity: EntityId) -> Option<JavaFacts> {
        self.language_extensions().java.get(entity).copied()
    }
    fn clang_extension(&self, entity: EntityId) -> Option<ClangFacts> {
        self.language_extensions().clang.get(entity).copied()
    }
}
