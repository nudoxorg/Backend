//! Rich, compact semantic IR and its borrowed compiler-ingestion path.
//!
//! The immutable result is a set of flat arenas. Variable-length data is held
//! in typed interned list pools, text and binary symbols share one atom pool,
//! and graph adjacency is compressed into forward/reverse CSR indices. Nothing
//! in an entity, type, document, or link owns a box or string.

mod builder;
mod columns;
mod error;
mod ids;
mod image;
mod language_facts;
mod packed_types;
mod relations;
mod tree;
mod type_model;

pub use builder::{IrBuilder, TreeBuilder};
pub use columns::{
    EntityColumns, GraphColumns, LanguageExtensionColumnView, LanguageExtensionsView,
    LinkOccurrenceColumns, OptionalId, SourceColumnsView, SparseColumnView, StorageColumns,
    VcsColumns,
};
pub use error::{BuildError, EntityRange, LanguageExtensionViolation, SemanticSpace};
pub use ids::{
    AtomListId, DocId, EntityListId, External, ExternalId, FreePredicateListId, ItemKind, LinkId,
    LinkOccurrenceId, LinkOccurrenceSpace, LinkSpace, ObjectMemberListId, TemplatePartListId,
    TreeEntity, TreeEntityId, TupleElementListId, TypeListId, TypeParameterBoundListId,
    TypeParameterListId,
};
pub use image::{Ir, ItemIdIter, ItemView, LinkIter, LinkOccurrenceIter};
pub use language_facts::{
    CSharpExtension, CSharpFacts, CSharpMemberEffects, CSharpNullability, CSharpPartialRole,
    CSharpReferenceKind, ClangExtension, ClangFacts, ClangLayout, ClangQualifiers,
    ClangStorageClass, GoExtension, GoFacts, GoSignature, JavaExtension, JavaFacts,
    LanguageExtensionInput, PythonExtension, PythonFacts, PythonParameterKind, RustExtension,
    RustFacts, RustOwnership, SemanticImageAuthority, TypeScriptExtension, TypeScriptFacts,
};
pub use packed_types::{
    ArrayShape, CallableElementRole, ComputedType, ConcreteType, FreePredicate, LiteralType,
    MappedModifier, ObjectMember, PropertyKey, QualifiedSegments, TemplatePart, TupleElement,
    TupleElementKind, TypeParameter, TypeParameterBound, TypeParameterInference, TypeParameterKind,
    TypeParameterPrimaryRequirement, TypeParameterRequirements, TypeQuery, VariadicForm, Variance,
    WildcardBound,
};
pub use relations::{
    Confidence, CorePayloadCoverage, CorePayloadHash, CorePayloadPlane, DeclarationLinkTarget,
    DocFragment, DocInput, EntityVersion, ExternalTarget, ForeignExternalTarget,
    ForeignTargetOrigin, Item, Link, LinkKind, LinkOccurrence, LinkTarget, SourceSpan,
};
pub use tree::{BorrowedTree, FrontendTree, TreeItemInput, TreeLinkInput, TreeLinkTarget};
pub use type_model::{
    BuiltinType, ComputedState, ComputedTypeId, ConcreteState, ConcreteTypeId,
    CxxReferenceCategory, GuardedType, Mutability, NativeCharacterRole, TypeColumns, TypeExpr,
    TypeHeader, TypePairPayload, TypeQuadPayload, TypeState, TypeTag, TypeTriplePayload,
    TypedTypeId, UnknownReason, UnknownState, UnknownType, UnknownTypeId, Visibility,
};
