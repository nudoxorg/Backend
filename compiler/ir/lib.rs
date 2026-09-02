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
mod docs_facts;
mod extension_pools;
mod interner;
#[cfg(feature = "mmap")]
mod mapping;
mod model;
mod prepared;
mod range;
mod render;
mod semantic;
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
pub use compiler_ir_vocabulary::{
    AnonRecordForm, AnonRecordFormError, ChildCountLaw, DeclarationKey, DeclarationKeyFault,
    DeclarationPathFault, Disambiguator, ExternalCoordinate, ExternalEntityRef, ExternalFragmentId,
    ExternalProductRef, ExternalTypeRef, ForeignKey, ForeignKeyFault, ForeignOrigin, ListSpan,
    NominalRef, Occurrence, OccurrenceTarget, PackageLineage, PackageLineageFault, PooledListError,
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
pub use extension_pools::{
    DecodedRefList, DecodedTypeParameter, ExtensionPoolFault, ExtensionPoolsLane, ExtensionRefList,
    ExtensionTypeParameter, ReopenedExtensionPools,
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
    AtomListId, BorrowedTree, BuildError, BuiltinType, CSharpExtension, CSharpFacts,
    CSharpMemberEffects, CSharpNullability, CSharpPartialRole, CSharpReferenceKind, ClangExtension,
    ClangFacts, ClangLayout, ClangQualifiers, ClangStorageClass, ComputedState, ComputedType,
    ComputedTypeId, ConcreteState, ConcreteType, ConcreteTypeId, Confidence, DocFragment, DocId,
    DocInput, EntityColumns, EntityListId, EntityRange, EntityVersion, External, ExternalId,
    ExternalTarget, FrontendTree, GoExtension, GoFacts, GoSignature, GraphColumns, GuardedType, Ir,
    IrBuilder, Item, ItemIdIter, ItemKind, ItemView, JavaExtension, JavaFacts,
    LanguageExtensionColumnView, LanguageExtensionInput, LanguageExtensionViolation,
    LanguageExtensionsView, Link, LinkId, LinkIter, LinkKind, LinkSpace, LinkTarget, LiteralType,
    MappedModifier, Mutability, ObjectMember, ObjectMemberListId, OptionalId, PayloadHash,
    PropertyKey, PythonExtension, PythonFacts, PythonParameterKind, RustExtension, RustFacts,
    RustOwnership, SemanticImageAuthority, SemanticSpace, SourceColumnsView, SourceSpan,
    SparseColumnView, StableEntityId, StorageColumns, TemplatePart, TemplatePartListId,
    TreeBuilder, TreeEntity, TreeEntityId, TreeItemInput, TreeLinkInput, TreeLinkTarget,
    TupleElement, TupleElementKind, TupleElementListId, TypeColumns, TypeExpr, TypeHeader,
    TypeListId, TypePairPayload, TypeParameter, TypeParameterListId, TypeQuadPayload, TypeQuery,
    TypeScriptExtension, TypeScriptFacts, TypeState, TypeTag, TypeTriplePayload, TypedTypeId,
    UnknownState, UnknownType, UnknownTypeId, Variance, VcsColumns, Visibility,
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
pub use type_facts::{DecodedTypeFact, TypeFactCursor, TypeFactFault, TypeFactInput, TypeFactLane};
pub use vcs::{
    Diff, EntityChange, EntityChangeKind, EntityChanges, GenerationId, LinkChange, LinkChangeKind,
    LinkChanges, Snapshot, StableLink, StableLinkKey, StableLinks,
};
pub use view::OccurrenceFault as OccurrenceViewFault;
pub use view::{
    Atom, AtomCursor, DirectoryFault, EntityCursor, FragmentError, FragmentView, SectionKind,
    SemanticDataFault, TypeNodeCursor, WireField,
};

pub use wire::{FRAGMENT_MAGIC, FRAGMENT_SCHEMA};
