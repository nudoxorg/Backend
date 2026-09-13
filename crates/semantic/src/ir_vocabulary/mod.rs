//! Dense, type-separated coordinates for canonical entities, types, and atoms inside one IR fragment.
//! Recursive semantic products, pooled child lists, and external fragment authorities share the
//! same dense discipline. The occurrence plane (reference fidelity lattice, reference categories,
//! owner-relative spans), the frozen type-expression lattice, and cross-fragment reference and
//! declaration-identity records share the crate. This crate contains no format policy, allowing
//! producers and index consumers to share it cheaply.

mod coordinates;
mod entity;
mod identity;
mod occurrence;
mod products;
mod type_lattice;

pub use self::coordinates::{
    Atom, AtomId, DenseId, Entity, EntityId, ExternalCoordinate, ExternalEntityRef,
    ExternalFragmentId, ExternalProductRef, ExternalTypeRef, ListId, ListSpan, PooledListError,
    Product, ProductChildren, ProductId, ProductListId, SemanticAtom, Type, TypeId,
};
pub use self::entity::{EntityKind, EntityKindCodeError};
pub use self::identity::{
    DeclarationFamilyId, DeclarationIdentity, DeclarationKey, DeclarationKeyFault,
    DeclarationPathFault, ExternalDeclarationIdentity, ForeignDeclarationId, ForeignKey,
    ForeignKeyFault, ForeignOrigin, Occurrence, OccurrenceTarget, PackageLineage,
    PackageLineageFault, PackageLineageView, PreimageOverflow, Resolution, StableRef,
    VariantAvailability, VariantFingerprint,
};
pub use self::occurrence::{
    Confidence, ConfidenceCodeError, ReferenceKind, ReferenceKindCodeError, RelSpan, RelSpanFault,
};
pub use self::products::{
    ProductChildRole, ProductChildRoleCodeError, ProductConstructorFault, ProductConstructorTag,
    ProductList, ProductRef, SemanticProduct, SemanticProductChild, SemanticProductConstructor,
};
pub use self::type_lattice::{
    AnnotationKind, AnonRecordForm, AnonRecordFormError, ChannelDirection, ChildCountLaw,
    CvQualifiers, CvQualifiersError, FunctionVariadicForm, MappedModifier, MappedModifierError,
    NominalRef, PrimitiveShape, PrimitiveShapeError, SemanticTypeChild, SemanticTypeFault,
    SemanticTypeRecord, SemanticTypeTag, SemanticTypeTagError, TypeCell, TypeChildTarget,
    TypeChildren, TypeFactId, TypeReason, TypeReasonError, TypeRef, TypeWidth, TypeWidthError,
    Variance, VarianceError,
};
