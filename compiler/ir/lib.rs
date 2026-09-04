//! The `compiler-ir` crate exists to encode, validate, map, and borrow canonical compiler IR fragments.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
#![no_std]
#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

extern crate alloc;

#[cfg(feature = "mmap")]
extern crate std;

#[cfg(target_pointer_width = "16")]
compile_error!("compiler-ir requires at least a 32-bit address space");

mod canonical_data;
mod columnar;
mod coordinate;
mod declaration_identity;
mod docs_facts;
mod discovery;
mod extension_pools;
mod interner;
#[cfg(feature = "mmap")]
mod mapping;
mod model;
mod prepared;
mod range;
mod render;
mod semantic;
mod semantic_data_view;
mod semantic_extension_section;
mod semantic_facts;
mod type_facts;
mod vcs;
mod view;
mod wire;

pub use canonical_data::{
    CanonicalDataError, CanonicalDataGraph, DataCanonicalization, DataCountLane, DataFacts,
    DataOutput, DataOutputLane, DataResource, DataResourceBudget, DataScratch, DataScratchLane,
    canonicalize_data_with_budget,
};
pub use declaration_identity::{DeclarationParentage, ScopedDeclarationKey};
pub use compiler_ir_vocabulary::Confidence as OccurrenceConfidence;
pub use compiler_ir_vocabulary::{
    AnnotationKind, AnonRecordForm, AnonRecordFormError, ChildCountLaw, CvQualifiers,
    CvQualifiersError, DeclarationKey, DeclarationKeyFault, DeclarationPathFault,
    ExternalCoordinate, ExternalEntityRef, ExternalFragmentId,
    ExternalProductRef, ExternalTypeRef, ForeignKey, ForeignKeyFault, ForeignOrigin,
    ChannelDirection, FunctionVariadicForm, ListSpan,
    NativeCharacterRole, NominalRef, Occurrence, OccurrenceTarget, PackageLineage, PackageLineageView,
    PackageLineageFault, PooledListError,
    PreimageOverflow, PrimitiveShape, PrimitiveShapeError, Product, ProductChildRole,
    ProductChildRoleCodeError, ProductChildren, ProductConstructorFault, ProductConstructorTag,
    ProductId, ProductList, ProductListId, ProductRef, ReferenceKind, ReferenceKindCodeError,
    RelSpan, RelSpanFault, Resolution, SemanticAtom, SemanticProduct, SemanticProductChild,
    SemanticProductConstructor, SemanticTypeChild, SemanticTypeFault, SemanticTypeRecord,
    SemanticTypeTag, SemanticTypeTagError, StableRef, TypeCell, TypeChildTarget, TypeChildren,
    TypeFactId, TypeReason, TypeReasonError, TypeRef, TypeWidth, TypeWidthError,
};
pub use compiler_ir_vocabulary::{
    MappedModifier as LatticeMappedModifier, Variance as LatticeVariance,
};
pub use compiler_vocabulary::{
    CSharpVersion, CStandard, CxxStandard, GoVersion, JavaRelease, Language, LanguageProfile,
    PythonVersion, RustEdition, TypeScriptSource, UnknownLanguageProfile,
};
pub use coordinate::{
    AtomId, AtomSpace, DenseId, Entity, EntityId, List, ListId, Text, TextId, Type, TypeId,
};
pub use docs_facts::{
    DecodedDocFact, DocFactCursor, DocFactFault, DocFactInput, DocFragmentInput, DocLinkTarget,
    DocumentationLane,
};
pub use discovery::{FragmentDiscovery, FragmentDiscoveryError, SemanticCensus};
pub use extension_pools::{
    DecodedRefList, DecodedTypeParameter, DecodedTypeParameterBound,
    DecodedTypeParameterBoundCursor, DecodedTypeParameterBoundList, DecodedTypeParameterCursor,
    DecodedTypeParameterKind, DecodedTypeParameterList, DecodedTypeParameterSemantics,
    ExtensionPoolFault,
    ExtensionPoolListLane, ExtensionPoolsLane, ExtensionRefList, ExtensionTypeParameter,
    ExtensionTypeParameterBound, ExtensionTypeParameterBoundRange, ExtensionTypeParameterKind,
    ExtensionTypeParameterRange, ReopenedExtensionPools, ReopenedTypeParameterList,
    TypeParameterField, TypeParameterListBounds, TypeParameterTagField, reopen_extension_pools,
};
pub use interner::{
    ArenaRange, AtomInterner, AtomTable, AtomTableView, CapacityError, CapacitySpace, Interner,
    ListInterner, ListTable, ListTableView,
};
#[cfg(feature = "mmap")]
pub use mapping::{
    MappedFragment, MappedFragmentError, MappedFragmentIoPhase, MappedFragmentView,
    open_fragment_mmap,
};
pub use model::{
    AtomFault, AtomInput, EntityFault, EntityKind, EntityKindCodeError, EntityNameFault,
    EntityRecord, EntityRecordFault, EntityType, PrimitiveType, RecipeFact, RecipeFactFault,
    SourceIdentity, SourceIdentityFault, TypeNode, TypeNodeFault,
};
pub use prepared::{FragmentSemantics, LayoutStep, PrepareError, PreparedFragment, WriteError};
pub use range::{
    FragmentRange, FragmentRangeManifest, FragmentRangeManifestError, FragmentRangeManifestView,
    FragmentRangeRequest, FragmentRangeVerifyError, VerifiedFragmentRange,
    VerifiedFragmentRangeView,
};
pub use render::{DocsDisplay, EmbeddingDisplay, EmbeddingProfile, SignatureDisplay, TypeDisplay};
pub use semantic::{
    ArrayShape, AtomListId, BorrowedTree, BuildError, BuiltinType, CSharpExtension, CSharpFacts,
    CallableElementRole, CxxReferenceCategory,
    CSharpMemberEffects, CSharpNullability, CSharpPartialRole, CSharpReferenceKind, ClangExtension,
    ClangFacts, ClangLayout, ClangQualifiers, ClangStorageClass, ComputedState, ComputedType,
    ComputedTypeId, ConcreteState, ConcreteType, ConcreteTypeId, Confidence, DocFragment, DocId,
    DeclarationFamilyId, DocInput, EntityColumns, EntityListId, EntityRange, EntityVersion,
    External, ExternalDeclarationIdentity, ExternalId, ExternalTarget, FrontendTree, GoExtension,
    GoFacts, GoSignature, GraphColumns, GuardedType, Ir,
    IrBuilder, Item, ItemIdIter, ItemKind, ItemView, JavaExtension, JavaFacts,
    LanguageExtensionColumnView, LanguageExtensionInput, LanguageExtensionViolation,
    LanguageExtensionsView, Link, LinkId, LinkIter, LinkKind, LinkOccurrence,
    LinkOccurrenceColumns, LinkOccurrenceId, LinkOccurrenceIter, LinkOccurrenceSpace, LinkSpace,
    LinkTarget, LiteralType,
    CorePayloadCoverage, CorePayloadHash, CorePayloadPlane, DeclarationIdentity,
    DeclarationLinkTarget, MappedModifier,
    Mutability, ObjectMember, ObjectMemberListId, OptionalId,
    PropertyKey, PythonExtension, PythonFacts, PythonParameterKind, QualifiedSegments, RustExtension, RustFacts,
    RustOwnership, SemanticImageAuthority, SemanticSpace, SourceColumnsView, SourceSpan,
    SparseColumnView, StorageColumns, TemplatePart, TemplatePartListId,
    TreeBuilder, TreeEntity, TreeEntityId, TreeItemInput, TreeLinkInput, TreeLinkTarget,
    TupleElement, TupleElementKind, TupleElementListId, TypeColumns, TypeExpr, TypeHeader,
    TypeListId, TypePairPayload, TypeParameter, TypeParameterBound, TypeParameterBoundListId,
    TypeParameterInference, TypeParameterKind, TypeParameterListId, TypeParameterPrimaryRequirement,
    TypeParameterRequirements, TypeQuadPayload, TypeQuery, VariadicForm,
    TypeScriptExtension, TypeScriptFacts, TypeState, TypeTag, TypeTriplePayload, TypedTypeId,
    UnknownReason, UnknownState, UnknownType, UnknownTypeId, Variance, VariantAvailability,
    VariantFingerprint, VcsColumns, Visibility,
    WildcardBound,
};
pub use semantic_data_view::{
    SemanticDataAtom, SemanticDataAtomCursor, SemanticDataChild, SemanticDataCounts,
    SemanticDataEntityRoot, SemanticDataView,
};
pub use semantic_extension_section::{
    ExtensionSectionInput, ExtensionSectionPlane, ExtensionSectionSource,
    LanguageExtensionCommonBounds, LanguageExtensionDirectoryKind, LanguageExtensionEncodeError,
    LanguageExtensionReopenError, LanguageExtensionWireFact, ReopenedLanguageExtensionColumn,
    ReopenedLanguageExtensionSection, SECTION_NONE, ValidatedLanguageExtensionCommonBounds,
    encode_fragment_extension_section, encode_language_extension_section,
    fragment_extension_section_len, language_extension_section_len,
    reopen_language_extension_section,
};
pub use semantic_facts::{
    DecodedOccurrence, OccurrenceCursor, OccurrenceFault, OccurrenceInput, OccurrenceLane,
};
pub use type_facts::{
    DecodedTypeFact, DecodedTypeFactChild, TypeFactChildCursor, TypeFactCounts, TypeFactCursor,
    TypeFactFault, TypeFactInput, TypeFactLane, TypeFactSegment,
};
pub use vcs::{
    Delta, Diff, EntityChange, EntityChanges, GenerationId, LinkChange, LinkChangeKind,
    LinkChanges, Snapshot, StableLink, StableLinkKey, StableLinks,
};
pub use view::OccurrenceFault as OccurrenceViewFault;
pub use view::{
    Atom, AtomCursor, DirectoryFault, EntityCursor, FragmentError, FragmentView, SectionKind,
    SemanticDataFault, TypeNodeCursor, WireField,
};

pub use wire::{FRAGMENT_MAGIC, FRAGMENT_SCHEMA};
