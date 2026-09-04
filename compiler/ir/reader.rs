//! Static semantic-image reading shared by owned and future borrowed images.
//!
//! The contract deliberately contains semantic values and exact cursors, never
//! Rust-layout slices from `Ir`. A validated mmap reader can therefore decode
//! the same rows from canonical bytes without allocation or enum-layout
//! reinterpretation.

use core::iter::{Copied, FusedIterator};
use core::slice;

use crate::{
    AtomId, AtomListId, CSharpFacts, ClangFacts, DeclarationIdentity, DocFragment, DocId,
    EntityAuthorityFacts, EntityId, EntityListId, EntityVersion, ExternalId, ExternalTarget,
    GoExtension, GoFacts, ImageProvenance, Ir, JavaExtension, JavaFacts,
    LanguageExtensionColumnView, Link, LinkId, LinkIter, LinkOccurrence, LinkOccurrenceId,
    LinkOccurrenceIter, ObjectMember, ObjectMemberListId, OccurrenceAuthorityFacts,
    PythonExtension, PythonFacts, RustExtension, RustFacts, SemanticImageAuthority, SourceSpan,
    TemplatePart, TemplatePartListId, TupleElement, TupleElementListId, TypeExpr, TypeId,
    TypeListId, TypeParameter, TypeParameterBound, TypeParameterBoundListId,
    TypeParameterListId, TypeScriptExtension, TypeScriptFacts, CSharpExtension, ClangExtension,
};

pub(crate) mod sealed {
    pub trait Sealed {}
}

/// Copying exact-size cursor used for canonical pooled semantic values.
pub type SemanticCursor<'image, T> = Copied<slice::Iter<'image, T>>;

/// Immutable, aligned declaration facts for one finalized entity row.
///
/// This is a row view, not a second entity model. Keeping item, source,
/// authority, and version facts together preserves captured-empty truth when
/// readers cross the owned/borrowed boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticEntity {
    pub id: EntityId,
    pub name: AtomId,
    pub kind: crate::ItemKind,
    pub visibility: crate::Visibility,
    pub parent: Option<EntityId>,
    pub semantic_type: Option<TypeId>,
    pub members: EntityListId,
    pub docs: DocId,
    pub attributes: AtomListId,
    pub source: Option<SourceSpan>,
    pub authority: EntityAuthorityFacts,
    pub version: EntityVersion,
}

/// Immutable image-level authority facts shared by every semantic row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticImageFacts {
    pub authority: SemanticImageAuthority,
    pub provenance: ImageProvenance,
}

/// Sealed, allocation-free access to one finalized semantic image.
///
/// All traversal is static through GAT cursors. `StorageColumns` remains an
/// `Ir`-specific performance view; it is intentionally not required here
/// because a durable reader must not expose native enum padding or pointers.
pub trait SemanticReader: sealed::Sealed {
    type CanonicalEntities<'image>: ExactSizeIterator<Item = SemanticEntity> + FusedIterator
    where
        Self: 'image;
    type Links<'image>: ExactSizeIterator<Item = (LinkId, Link)> + FusedIterator
    where
        Self: 'image;
    type Occurrences<'image>: ExactSizeIterator<Item = (LinkOccurrenceId, LinkOccurrence)>
        + FusedIterator
    where
        Self: 'image;
    type TypeScriptExtensions<'image>: ExactSizeIterator<Item = (EntityId, TypeScriptFacts)> + FusedIterator
    where
        Self: 'image;
    type CSharpExtensions<'image>: ExactSizeIterator<Item = (EntityId, CSharpFacts)> + FusedIterator
    where
        Self: 'image;
    type GoExtensions<'image>: ExactSizeIterator<Item = (EntityId, GoFacts)> + FusedIterator
    where
        Self: 'image;
    type RustExtensions<'image>: ExactSizeIterator<Item = (EntityId, RustFacts)> + FusedIterator
    where
        Self: 'image;
    type PythonExtensions<'image>: ExactSizeIterator<Item = (EntityId, PythonFacts)> + FusedIterator
    where
        Self: 'image;
    type JavaExtensions<'image>: ExactSizeIterator<Item = (EntityId, JavaFacts)> + FusedIterator
    where
        Self: 'image;
    type ClangExtensions<'image>: ExactSizeIterator<Item = (EntityId, ClangFacts)> + FusedIterator
    where
        Self: 'image;
    type Types<'image>: ExactSizeIterator<Item = TypeId> + FusedIterator
    where
        Self: 'image;
    type Atoms<'image>: ExactSizeIterator<Item = AtomId> + FusedIterator
    where
        Self: 'image;
    type Entities<'image>: ExactSizeIterator<Item = EntityId> + FusedIterator
    where
        Self: 'image;
    type Docs<'image>: ExactSizeIterator<Item = DocFragment> + FusedIterator
    where
        Self: 'image;
    type TupleElements<'image>: ExactSizeIterator<Item = TupleElement> + FusedIterator
    where
        Self: 'image;
    type ObjectMembers<'image>: ExactSizeIterator<Item = ObjectMember> + FusedIterator
    where
        Self: 'image;
    type TemplateParts<'image>: ExactSizeIterator<Item = TemplatePart> + FusedIterator
    where
        Self: 'image;
    type TypeParameters<'image>: ExactSizeIterator<Item = TypeParameter> + FusedIterator
    where
        Self: 'image;
    type TypeParameterBounds<'image>: ExactSizeIterator<Item = TypeParameterBound> + FusedIterator
    where
        Self: 'image;

    fn image_facts(&self) -> SemanticImageFacts;
    fn entity(&self, id: EntityId) -> Option<SemanticEntity>;
    fn entity_by_identity(&self, identity: DeclarationIdentity) -> Option<SemanticEntity>;
    fn external(&self, id: ExternalId) -> Option<ExternalTarget>;
    fn link(&self, id: LinkId) -> Option<Link>;
    fn occurrence_authority(&self, id: LinkOccurrenceId) -> Option<OccurrenceAuthorityFacts>;

    fn atom(&self, id: AtomId) -> Option<&[u8]>;
    fn text(&self, id: crate::TextId) -> Option<&str>;
    fn ty(&self, id: TypeId) -> Option<TypeExpr>;
    fn types(&self, id: TypeListId) -> Option<Self::Types<'_>>;
    fn atom_list(&self, id: AtomListId) -> Option<Self::Atoms<'_>>;
    fn entity_list(&self, id: EntityListId) -> Option<Self::Entities<'_>>;
    fn docs(&self, id: DocId) -> Option<Self::Docs<'_>>;
    fn tuple_elements(&self, id: TupleElementListId) -> Option<Self::TupleElements<'_>>;
    fn object_members(&self, id: ObjectMemberListId) -> Option<Self::ObjectMembers<'_>>;
    fn template_parts(&self, id: TemplatePartListId) -> Option<Self::TemplateParts<'_>>;
    fn type_parameters(&self, id: TypeParameterListId) -> Option<Self::TypeParameters<'_>>;
    fn type_parameter_bounds(
        &self,
        id: TypeParameterBoundListId,
    ) -> Option<Self::TypeParameterBounds<'_>>;

    fn canonical_entities(&self) -> Self::CanonicalEntities<'_>;
    fn links_from(&self, entity: EntityId) -> Self::Links<'_>;
    fn link_occurrences(&self) -> Self::Occurrences<'_>;
    fn typescript_extensions(&self) -> Self::TypeScriptExtensions<'_>;
    fn csharp_extensions(&self) -> Self::CSharpExtensions<'_>;
    fn go_extensions(&self) -> Self::GoExtensions<'_>;
    fn rust_extensions(&self) -> Self::RustExtensions<'_>;
    fn python_extensions(&self) -> Self::PythonExtensions<'_>;
    fn java_extensions(&self) -> Self::JavaExtensions<'_>;
    fn clang_extensions(&self) -> Self::ClangExtensions<'_>;
}

/// Exact-size canonical entity iterator backed by `Ir`'s declaration index.
pub struct IrCanonicalEntities<'image> {
    ir: &'image Ir,
    items: crate::ItemIdIter<'image>,
}

impl Iterator for IrCanonicalEntities<'_> {
    type Item = SemanticEntity;

    fn next(&mut self) -> Option<Self::Item> {
        let id = self.items.next()?.id();
        self.ir.semantic_entity(id)
    }

    fn size_hint(&self) -> (usize, Option<usize>) { self.items.size_hint() }
}
impl ExactSizeIterator for IrCanonicalEntities<'_> {}
impl FusedIterator for IrCanonicalEntities<'_> {}

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
        Self { plane, next: 0, end, remaining }
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

impl SemanticReader for Ir {
    type CanonicalEntities<'image> = IrCanonicalEntities<'image>;
    type Links<'image> = LinkIter<'image>;
    type Occurrences<'image> = LinkOccurrenceIter<'image>;
    type TypeScriptExtensions<'image> = IrExtensionRows<'image, TypeScriptFacts, TypeScriptExtension>;
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

    fn image_facts(&self) -> SemanticImageFacts {
        SemanticImageFacts {
            authority: self.storage_columns().authority,
            provenance: self.image_provenance(),
        }
    }
    fn entity(&self, id: EntityId) -> Option<SemanticEntity> { Ir::semantic_entity(self, id) }
    fn entity_by_identity(&self, identity: DeclarationIdentity) -> Option<SemanticEntity> {
        Ir::find_declaration(self, identity).and_then(|item| Ir::semantic_entity(self, item.id()))
    }
    fn external(&self, id: ExternalId) -> Option<ExternalTarget> { Ir::external(self, id).copied() }
    fn link(&self, id: LinkId) -> Option<Link> { Ir::link(self, id) }
    fn occurrence_authority(&self, id: LinkOccurrenceId) -> Option<OccurrenceAuthorityFacts> {
        self.occurrence_authority_columns().source.get(id.index()).copied()
            .map(|source| OccurrenceAuthorityFacts { source })
    }

    fn atom(&self, id: AtomId) -> Option<&[u8]> { Ir::atom(self, id) }
    fn text(&self, id: crate::TextId) -> Option<&str> { Ir::text(self, id) }
    fn ty(&self, id: TypeId) -> Option<TypeExpr> { Ir::ty(self, id) }
    fn types(&self, id: TypeListId) -> Option<Self::Types<'_>> { Ir::types(self, id).map(|rows| rows.iter().copied()) }
    fn atom_list(&self, id: AtomListId) -> Option<Self::Atoms<'_>> { Ir::atom_list(self, id).map(|rows| rows.iter().copied()) }
    fn entity_list(&self, id: EntityListId) -> Option<Self::Entities<'_>> { Ir::entity_list(self, id).map(|rows| rows.iter().copied()) }
    fn docs(&self, id: DocId) -> Option<Self::Docs<'_>> { Ir::documentation(self, id).map(|rows| rows.iter().copied()) }
    fn tuple_elements(&self, id: TupleElementListId) -> Option<Self::TupleElements<'_>> { Ir::tuple_elements(self, id).map(|rows| rows.iter().copied()) }
    fn object_members(&self, id: ObjectMemberListId) -> Option<Self::ObjectMembers<'_>> { Ir::object_members(self, id).map(|rows| rows.iter().copied()) }
    fn template_parts(&self, id: TemplatePartListId) -> Option<Self::TemplateParts<'_>> { Ir::template_parts(self, id).map(|rows| rows.iter().copied()) }
    fn type_parameters(&self, id: TypeParameterListId) -> Option<Self::TypeParameters<'_>> { Ir::type_parameters(self, id).map(|rows| rows.iter().copied()) }
    fn type_parameter_bounds(&self, id: TypeParameterBoundListId) -> Option<Self::TypeParameterBounds<'_>> { Ir::type_parameter_bounds(self, id).map(|rows| rows.iter().copied()) }

    fn canonical_entities(&self) -> Self::CanonicalEntities<'_> { IrCanonicalEntities { ir: self, items: Ir::canonical_items(self) } }
    fn links_from(&self, entity: EntityId) -> Self::Links<'_> { Ir::links_from(self, entity) }
    fn link_occurrences(&self) -> Self::Occurrences<'_> { Ir::link_occurrences(self) }
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
}
