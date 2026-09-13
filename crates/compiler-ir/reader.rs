//! Static semantic-image reading shared by owned and future borrowed images.
//!
//! The contract deliberately contains semantic values and exact cursors, never
//! Rust-layout slices from `Ir`. A validated mmap reader can therefore decode
//! the same rows from canonical bytes without allocation or enum-layout
//! reinterpretation.

use core::iter::{Copied, FusedIterator};
use core::slice;

use crate::{
    AtomId, AtomListId, CSharpExtension, CSharpFacts, ClangExtension, ClangFacts,
    DeclarationIdentity, DocFragment, DocId, EntityAuthorityFacts, EntityId, EntityListId,
    EntityVersion, ExternalId, ExternalTarget, ForeignTargetOrigin, GoExtension, GoFacts,
    ImageProvenance, Ir, JavaExtension, JavaFacts, LanguageExtensionColumnView, Link, LinkId,
    LinkIter, LinkOccurrence, LinkOccurrenceId, LinkOccurrenceIter, ObjectMember,
    ObjectMemberListId, OccurrenceAuthorityFacts, PythonExtension, PythonFacts, RustExtension,
    RustFacts, SemanticImageAuthority, SourceSpan, TemplatePart, TemplatePartListId, TupleElement,
    TupleElementListId, TypeExpr, TypeId, TypeListId, TypeParameter, TypeParameterBound,
    TypeParameterBoundListId, TypeParameterListId, TypeScriptExtension, TypeScriptFacts,
    VariantAvailability,
};

/// Coordinate-free identity of one external endpoint admitted from a complete
/// semantic image.
///
/// Image-local atom and external ordinals never enter this identity. Foreign
/// origins, paths, and display values are committed as their exact bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ExternalTargetIdentity([u8; 32]);

/// Exact package-and-image scope for one compiler external endpoint.
///
/// External endpoints are not global declarations. Binding the portable
/// endpoint identity to both owners prevents identical foreign spellings in
/// different packages or image generations from collapsing into one row.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ScopedExternalTargetIdentity([u8; 32]);

impl ExternalTargetIdentity {
    /// Captures one endpoint only after resolving its typed coordinate against
    /// the supplied complete image.
    pub fn capture<Reader: SemanticReader + ?Sized>(
        reader: &Reader,
        external: ExternalId,
    ) -> Result<Self, ExternalTargetIdentityFault> {
        let target = reader
            .external(external)
            .ok_or(ExternalTargetIdentityFault::MissingExternal { external })?;
        let mut identity = blake3::Hasher::new();
        identity.update(b"compiler-ir.external-target.v1\0");
        match target {
            ExternalTarget::Stable { target } => {
                identity.update(&[0]);
                identity.update(target.fragment.as_ref());
                identity.update(target.declaration.family.as_bytes());
                identity.update(target.declaration.variant.as_bytes());
            }
            ExternalTarget::Foreign(target) => {
                identity.update(&[1]);
                identity.update(target.identity.foreign.as_bytes());
                match target.identity.variant {
                    VariantAvailability::Known(variant) => {
                        identity.update(&[1]);
                        identity.update(variant.as_bytes());
                    }
                    VariantAvailability::Unavailable => {
                        identity.update(&[0]);
                    }
                }
                match target.origin {
                    ForeignTargetOrigin::Package { ecosystem, package } => {
                        identity.update(&[0]);
                        hash_external_atom(reader, ecosystem, &mut identity)?;
                        hash_external_atom(reader, package, &mut identity)?;
                    }
                    ForeignTargetOrigin::Namespace {
                        ecosystem,
                        namespace,
                    } => {
                        identity.update(&[1]);
                        hash_external_atom(reader, ecosystem, &mut identity)?;
                        hash_external_atom(reader, namespace, &mut identity)?;
                    }
                    ForeignTargetOrigin::Universe { ecosystem } => {
                        identity.update(&[2]);
                        hash_external_atom(reader, ecosystem, &mut identity)?;
                    }
                    ForeignTargetOrigin::Unspecified { ecosystem } => {
                        identity.update(&[3]);
                        hash_external_atom(reader, ecosystem, &mut identity)?;
                    }
                }
                hash_external_atom(reader, target.path, &mut identity)?;
                hash_external_atom(reader, target.display, &mut identity)?;
                match target.kind {
                    Some(kind) => {
                        identity.update(&[1]);
                        identity.update(&u16::from(kind).to_be_bytes());
                    }
                    None => {
                        identity.update(&[0]);
                    }
                }
            }
            ExternalTarget::FragmentEntity { target, display } => {
                identity.update(&[2]);
                identity.update(target.fragment.as_ref());
                identity.update(&target.ordinal.to_be_bytes());
                hash_external_atom(reader, display, &mut identity)?;
            }
        }
        Ok(Self(*identity.finalize().as_bytes()))
    }

    /// Returns the canonical fixed-width endpoint commitment.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Binds this endpoint to the exact package and immutable image that
    /// supplied it.
    #[must_use]
    pub fn in_scope(self, package: [u8; 32], image: [u8; 32]) -> ScopedExternalTargetIdentity {
        let mut scoped = blake3::Hasher::new();
        scoped.update(b"compiler-ir.scoped-external-target.v1\0");
        scoped.update(&package);
        scoped.update(&image);
        scoped.update(self.as_bytes());
        ScopedExternalTargetIdentity(*scoped.finalize().as_bytes())
    }
}

impl ScopedExternalTargetIdentity {
    /// Returns the fixed-width package, image, and endpoint commitment.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Failure to resolve a claimed external endpoint within its exact image.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ExternalTargetIdentityFault {
    /// The image has no external row at the claimed typed coordinate.
    #[error("semantic image has no external endpoint at {external:?}")]
    MissingExternal { external: ExternalId },
    /// An admitted external row refers to an absent atom.
    #[error("semantic external endpoint refers to missing atom {atom:?}")]
    MissingAtom { atom: AtomId },
}

fn hash_external_atom<Reader: SemanticCoreReader + ?Sized>(
    reader: &Reader,
    atom: AtomId,
    identity: &mut blake3::Hasher,
) -> Result<(), ExternalTargetIdentityFault> {
    let bytes = reader
        .atom(atom)
        .ok_or(ExternalTargetIdentityFault::MissingAtom { atom })?;
    identity.update(&(bytes.len() as u64).to_be_bytes());
    identity.update(bytes);
    Ok(())
}

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

/// Immutable declaration facts available in every portable core semantic
/// image.  It deliberately omits all pooled coordinates: a core image cannot
/// accidentally expose a type/list/document ID whose backing plane was not
/// admitted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CoreSemanticEntity {
    pub id: EntityId,
    pub name: AtomId,
    pub kind: crate::ItemKind,
    pub visibility: crate::Visibility,
    /// Canonical local containment coordinate when the image retained one.
    /// The authority row separately proves whether this relationship is root,
    /// bound, unrepresented, or unavailable.
    pub parent: Option<EntityId>,
    pub authority: EntityAuthorityFacts,
    pub source: Option<SourceSpan>,
    pub version: EntityVersion,
}

/// Sealed, allocation-free access to one finalized semantic image.
///
/// All traversal is static through GAT cursors. `StorageColumns` remains an
/// `Ir`-specific performance view; it is intentionally not required here
/// because a durable reader must not expose native enum padding or pointers.
/// The portable, allocation-free core of a finalized semantic image.
///
/// A core reader covers only facts whose representation is present in the
/// portable core directory.  It is intentionally a separate capability from
/// [`SemanticReader`]: callers cannot obtain dangling pooled coordinates from
/// a partial image and must ask for the full capability before traversing
/// types, lists, externals, graph rows, or language extensions.
pub trait SemanticCoreReader: sealed::Sealed {
    type CanonicalCoreEntities<'image>: ExactSizeIterator<Item = CoreSemanticEntity> + FusedIterator
    where
        Self: 'image;

    fn image_facts(&self) -> SemanticImageFacts;
    fn core_entity(&self, id: EntityId) -> Option<CoreSemanticEntity>;
    fn core_entity_by_identity(&self, identity: DeclarationIdentity) -> Option<CoreSemanticEntity>;
    fn atom(&self, id: AtomId) -> Option<&[u8]>;
    fn text(&self, id: crate::TextId) -> Option<&str>;
    fn canonical_core_entities(&self) -> Self::CanonicalCoreEntities<'_>;
}

/// Complete static semantic image access.
///
/// This capability extends [`SemanticCoreReader`] only once every typed pool,
/// external target, graph row, occurrence fact, and sparse extension plane is
/// available.  A partial portable image therefore cannot silently substitute
/// an empty lane for unavailable semantic truth.
pub trait SemanticReader: SemanticCoreReader {
    type CanonicalEntities<'image>: ExactSizeIterator<Item = SemanticEntity> + FusedIterator
    where
        Self: 'image;
    type CanonicalLinks<'image>: ExactSizeIterator<Item = (LinkId, Link)> + FusedIterator
    where
        Self: 'image;
    type Links<'image>: ExactSizeIterator<Item = (LinkId, Link)> + FusedIterator
    where
        Self: 'image;
    type Occurrences<'image>: ExactSizeIterator<Item = (LinkOccurrenceId, LinkOccurrence)>
        + FusedIterator
    where
        Self: 'image;
    type TypeScriptExtensions<'image>: ExactSizeIterator<Item = (EntityId, TypeScriptFacts)>
        + FusedIterator
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
    type CanonicalTypes<'image>: ExactSizeIterator<Item = (TypeId, TypeExpr)> + FusedIterator
    where
        Self: 'image;
    type CanonicalExternals<'image>: ExactSizeIterator<Item = (ExternalId, ExternalTarget)>
        + FusedIterator
    where
        Self: 'image;

    fn entity(&self, id: EntityId) -> Option<SemanticEntity>;
    fn entity_by_identity(&self, identity: DeclarationIdentity) -> Option<SemanticEntity>;
    fn external(&self, id: ExternalId) -> Option<ExternalTarget>;
    fn link(&self, id: LinkId) -> Option<Link>;
    fn occurrence_authority(&self, id: LinkOccurrenceId) -> Option<OccurrenceAuthorityFacts>;

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
    /// Enumerates every graph relation exactly once in canonical stable-key
    /// order. This is the graph counterpart to [`Self::canonical_entities`]
    /// and lets durable borrowed images enter IR-VCS without reconstruction.
    fn canonical_links(&self) -> Self::CanonicalLinks<'_>;
    /// Enumerates every stored type coordinate exactly once.  A fully
    /// validated semantic image emits these coordinates in canonical typed
    /// order; owned `Ir` exposes its finalized image coordinates without
    /// making native storage layout part of the trait.
    fn canonical_types(&self) -> Self::CanonicalTypes<'_>;
    /// Enumerates every external endpoint exactly once in this image's
    /// validated coordinate order.
    fn canonical_externals(&self) -> Self::CanonicalExternals<'_>;
    fn links_from(&self, entity: EntityId) -> Self::Links<'_>;
    fn link_occurrences(&self) -> Self::Occurrences<'_>;
    fn typescript_extensions(&self) -> Self::TypeScriptExtensions<'_>;
    fn csharp_extensions(&self) -> Self::CSharpExtensions<'_>;
    fn go_extensions(&self) -> Self::GoExtensions<'_>;
    fn rust_extensions(&self) -> Self::RustExtensions<'_>;
    fn python_extensions(&self) -> Self::PythonExtensions<'_>;
    fn java_extensions(&self) -> Self::JavaExtensions<'_>;
    fn clang_extensions(&self) -> Self::ClangExtensions<'_>;

    /// Looks up one declaration's TypeScript facts without scanning the
    /// sparse plane.  Named scans above remain available for whole-image
    /// traversal; this accessor is the rendering/indexing hot path.
    fn typescript_extension(&self, entity: EntityId) -> Option<TypeScriptFacts>;
    fn csharp_extension(&self, entity: EntityId) -> Option<CSharpFacts>;
    fn go_extension(&self, entity: EntityId) -> Option<GoFacts>;
    fn rust_extension(&self, entity: EntityId) -> Option<RustFacts>;
    fn python_extension(&self, entity: EntityId) -> Option<PythonFacts>;
    fn java_extension(&self, entity: EntityId) -> Option<JavaFacts>;
    fn clang_extension(&self, entity: EntityId) -> Option<ClangFacts>;
}

/// Exact-size canonical entity iterator backed by `Ir`'s declaration index.
pub struct IrCanonicalEntities<'image> {
    ir: &'image Ir,
    items: crate::ItemIdIter<'image>,
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
    items: crate::ItemIdIter<'image>,
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
    fn text(&self, id: crate::TextId) -> Option<&str> {
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
