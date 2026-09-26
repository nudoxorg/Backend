//! Allocation-free borrowed full semantic-image reader.
//!
//! Construction is private to `reopen`: every iterator below relies only on
//! the structural proof recorded there and consequently never reparses into
//! an owned semantic plane or exposes native-layout slices.

use core::ops::Deref;

use crate::ir::{
    AtomId, AtomListId, CSharpFacts, ClangFacts, CoreSemanticEntity, DeclarationIdentity,
    DocId, EntityId, EntityListId, ExternalId, ExternalTarget, FreePredicate,
    FreePredicateListId, GoFacts, JavaFacts, Link, LinkId, LinkOccurrenceId,
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
    wire::{FullDirectoryKind, FullImageLayout},
};

mod cursor;

use cursor::{
    FullCanonicalCoreEntities, FullCanonicalEntities, FullCanonicalExternals, FullCanonicalTypes,
    FullDocRows, FullExtensionRows, FullGroupRows, FullLinks, FullOccurrences, FullRows,
    binary_entity, doc_rows, entity_rows, extension, extension_rows, lower_link, rows,
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

