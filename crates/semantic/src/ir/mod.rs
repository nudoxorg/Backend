//! Encodes, validates, maps, and borrows canonical compiler IR fragments.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(target_pointer_width = "16")]
compile_error!("backend-semantic::ir requires at least a 32-bit address space");

mod authority;
mod canonical_data;
mod columnar;
mod coordinate;
mod declaration_identity;
mod discovery;
mod docs_facts;
mod extension_pools;
mod interner;
/// Content-addressed chunked storage for jumbo documentation and source values.
pub mod jumbo_rope;
#[cfg(feature = "mmap")]
mod mapping;
mod model;
mod prepared;
mod range;
mod reader;
/// Immutable persistent index for stable-key semantic rows.
pub mod row_index;
mod semantic;
mod semantic_data_view;
mod semantic_discovery;
mod semantic_extension_section;
mod semantic_facts;
mod semantic_generation;
mod semantic_image;
/// The maintained renderer surface: prepared semantic-document, canonical-type,
/// neutral, and zero-allocation display lanes over one static reader.
pub mod semantic_render;
mod type_facts;
mod typed_plane_manifest_v2;
mod vcs;
mod versioned;
pub mod versioned_records;
mod view;
mod wire;

pub use crate::ir_vocabulary::Confidence as OccurrenceConfidence;
pub use crate::ir_vocabulary::{
    AnnotationKind, AnonRecordForm, AnonRecordFormError, ChannelDirection, ChildCountLaw,
    CvQualifiers, CvQualifiersError, DeclarationFamilyId, DeclarationIdentity, DeclarationKey,
    DeclarationKeyFault, DeclarationPathFault, ExternalCoordinate, ExternalDeclarationIdentity,
    ExternalEntityRef, ExternalFragmentId, ExternalProductRef, ExternalTypeRef,
    ForeignDeclarationId, ForeignKey, ForeignKeyFault, ForeignOrigin, FunctionVariadicForm,
    ListSpan, NominalRef, Occurrence, OccurrenceTarget, PackageLineage, PackageLineageFault,
    PackageLineageView, PooledListError, PreimageOverflow, PrimitiveShape, PrimitiveShapeError,
    Product, ProductChildRole, ProductChildRoleCodeError, ProductChildren, ProductConstructorFault,
    ProductConstructorTag, ProductId, ProductList, ProductListId, ProductRef, ReferenceKind,
    ReferenceKindCodeError, RelSpan, RelSpanFault, Resolution, SemanticAtom, SemanticProduct,
    SemanticProductChild, SemanticProductConstructor, SemanticTypeChild, SemanticTypeFault,
    SemanticTypeRecord, SemanticTypeTag, SemanticTypeTagError, StableRef, TypeCell,
    TypeChildTarget, TypeChildren, TypeFactId, TypeReason, TypeReasonError, TypeRef, TypeWidth,
    TypeWidthError, VariantAvailability, VariantFingerprint,
};
pub use crate::ir_vocabulary::{
    MappedModifier as LatticeMappedModifier, Variance as LatticeVariance,
};
pub use crate::vocabulary::{
    CSharpVersion, CStandard, CxxStandard, GoVersion, JavaRelease, Language, LanguageProfile,
    PythonVersion, RustEdition, TypeScriptSource, UnknownLanguageProfile,
};
pub use authority::{
    AuthorityFactFault, AuthorityFactPlane, EntityAuthorityColumns, EntityAuthorityFacts,
    FactAvailability, ImageProvenance, ImageProvenanceClaim, OccurrenceAuthorityColumns,
    OccurrenceAuthorityFacts, ParentageAuthority, SemanticScopeClaim, SemanticScopeFacts,
    UnrepresentedAuthorityOwner,
};
pub use canonical_data::{
    CanonicalDataError, CanonicalDataGraph, DataCanonicalization, DataCountLane, DataFacts,
    DataOutput, DataOutputLane, DataResource, DataResourceBudget, DataScratch, DataScratchLane,
    canonicalize_data_with_budget,
};
pub use coordinate::{
    AtomId, AtomSpace, DenseId, Entity, EntityId, List, ListId, Text, TextId, Type, TypeId,
};
pub use declaration_identity::{DeclarationParentage, ScopedDeclarationKey};
pub use discovery::{FragmentDiscovery, FragmentDiscoveryError, SemanticCensus};
pub use docs_facts::{
    DecodedDocFact, DocFactCursor, DocFactFault, DocFactInput, DocFragmentInput, DocLinkTarget,
    DocumentationLane,
};
pub use extension_pools::{
    DecodedFreePredicate, DecodedRefList, DecodedTypeParameter, DecodedTypeParameterBound,
    DecodedTypeParameterBoundCursor, DecodedTypeParameterBoundList, DecodedTypeParameterCursor,
    DecodedTypeParameterKind, DecodedTypeParameterList, DecodedTypeParameterSemantics,
    ExtensionFreePredicate, ExtensionPoolFault, ExtensionPoolListLane, ExtensionPoolsLane,
    ExtensionRefList, ExtensionTypeParameter, ExtensionTypeParameterBound,
    ExtensionTypeParameterBoundRange, ExtensionTypeParameterKind, ExtensionTypeParameterRange,
    ReopenedExtensionPools, ReopenedTypeParameterList, TypeParameterField, TypeParameterListBounds,
    TypeParameterTagField, reopen_extension_pools,
};
pub use interner::{
    ArenaRange, AtomInterner, AtomTable, AtomTableView, CapacityError, CapacitySpace, Interner,
    ListInterner, ListTable, ListTableView,
};
pub use jumbo_rope::{
    CheckedJumboLeaf, CheckedJumboValueDescriptor, JUMBO_ROPE_MAX_LEAF_BYTES,
    JUMBO_ROPE_MIN_LEAF_BYTES, JUMBO_ROPE_STREAM_BUFFER_BYTES, JUMBO_ROPE_TARGET_LEAF_BYTES,
    JUMBO_VALUE_DESCRIPTOR_WIRE_BYTES, JumboOperationError, JumboRopeBuildMetrics,
    JumboRopeClosure, JumboRopeError, JumboRopeLeafRef, JumboRopeLimits, JumboRopeNode,
    JumboRopeObjectId, JumboRopeObjectKind, JumboRopeObjectSink, JumboRopeObjectSource,
    JumboRopeProof, JumboRopeProofSibling, JumboRopeProofSide, JumboRopeProofSpan,
    JumboRopeStreamWriter, JumboRopeWriteReceipt, JumboValueContext, JumboValueDescriptorId,
    JumboValueEncoding, JumboValueFamily, MissingJumboLeafRanges, ROPE_NODE_WIRE_BYTES,
    UntrustedJumboValueDescriptor, VerifiedJumboRope, prove_jumbo_leaf, requires_jumbo_rope,
    write_jumbo_value, write_jumbo_value_from_reader,
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
pub use reader::{
    CoreSemanticEntity, ExternalTargetIdentity, ExternalTargetIdentityFault,
    IrCanonicalCoreEntities, IrCanonicalEntities, IrExtensionRows, ScopedExternalTargetIdentity,
    SemanticCoreReader, SemanticCursor, SemanticEntity, SemanticImageFacts, SemanticReader,
};
#[doc(hidden)]
pub use semantic::{
    ArrayShape, AtomListId, BorrowedTree, BuildError, BuiltinType, CSharpExtension, CSharpFacts,
    CSharpMemberEffects, CSharpNullability, CSharpPartialRole, CSharpReferenceKind,
    CallableElementRole, ClangExtension, ClangFacts, ClangLayout, ClangQualifiers,
    ClangStorageClass, ComputedState, ComputedType, ComputedTypeId, ConcreteState, ConcreteType,
    ConcreteTypeId, Confidence, CorePayloadCoverage, CorePayloadHash, CorePayloadPlane,
    CxxReferenceCategory, DeclarationLinkTarget, DocFragment, DocId, DocInput, EntityColumns,
    EntityListId, EntityRange, EntityVersion, External, ExternalId, ExternalTarget,
    ForeignExternalTarget, ForeignTargetOrigin, FreePredicate, FreePredicateListId, FrontendTree,
    GoExtension, GoFacts, GoSignature, GraphColumns, GuardedType, Ir, IrBuilder, Item, ItemIdIter,
    ItemKind, ItemView, JavaExtension, JavaFacts, LanguageExtensionColumnView,
    LanguageExtensionInput, LanguageExtensionViolation, LanguageExtensionsView, Link, LinkId,
    LinkIter, LinkKind, LinkOccurrence, LinkOccurrenceColumns, LinkOccurrenceId,
    LinkOccurrenceIter, LinkOccurrenceSpace, LinkSpace, LinkTarget, LiteralType, MappedModifier,
    Mutability, NativeCharacterRole, ObjectMember, ObjectMemberListId, OptionalId, PropertyKey,
    PythonExtension, PythonFacts, PythonParameterKind, QualifiedSegments, RustExtension, RustFacts,
    RustOwnership, SemanticImageAuthority, SemanticSpace, SourceColumnsView, SourceSpan,
    SparseColumnView, StorageColumns, TemplatePart, TemplatePartListId, TreeBuilder, TreeEntity,
    TreeEntityId, TreeItemInput, TreeLinkInput, TreeLinkTarget, TupleElement, TupleElementKind,
    TupleElementListId, TypeColumns, TypeExpr, TypeHeader, TypeListId, TypePairPayload,
    TypeParameter, TypeParameterBound, TypeParameterBoundListId, TypeParameterInference,
    TypeParameterKind, TypeParameterListId, TypeParameterPrimaryRequirement,
    TypeParameterRequirements, TypeQuadPayload, TypeQuery, TypeScriptExtension, TypeScriptFacts,
    TypeState, TypeTag, TypeTriplePayload, TypedTypeId, UnknownReason, UnknownState, UnknownType,
    UnknownTypeId, VariadicForm, Variance, VcsColumns, Visibility, WildcardBound,
};
pub use semantic_data_view::{
    SemanticDataAtom, SemanticDataAtomCursor, SemanticDataChild, SemanticDataCounts,
    SemanticDataEntityRoot, SemanticDataView,
};
pub use semantic_discovery::{
    AvailabilityCensus, EntityAuthorityCensus, LanguageExtensionCensus, ParentageCensus,
    SemanticDiscoveryError, SemanticDiscoveryReference, SemanticImageCensus,
    SemanticImageDiscovery,
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
pub use semantic_generation::{
    MAX_TYPED_PLANE_VERIFICATION_ADAPTER_BYTES, SemanticContentRootV2,
    SemanticGenerationFamilyCommitmentV2, SemanticGenerationFamilyRootV2,
    SemanticGenerationProofError, SemanticGenerationRootV2, SemanticTypedPlaneVerificationTierV2,
    UntrustedSemanticContentRootV2, UntrustedSemanticGenerationRootV2, VerifiedTypedPlaneContentV2,
    verify_typed_plane_content_v2, verify_typed_plane_content_v2_with_jumbo_source,
    verify_typed_plane_content_v2_with_tier,
};
pub use semantic_image::{
    CoreProvenanceFault, CoreProvenanceIdentityField, CoreSemanticImageFault,
    CoreSemanticImageField, ExtensionPlanFault, FullEntityFault, FullPlanError,
    FullSemanticImageError, FullSemanticImageFault, FullSemanticImageField,
    FullSemanticImageIdentityField, GraphPlanFault, PreparedFullSemanticImage, ScopeComponent,
    SemanticImageEncodeError, SemanticImageIdentity, SemanticImageProofOwner,
    SemanticImageReopenError, SemanticImageView, TerminalPoolDomain, TerminalPoolFault,
    encode_full_semantic_image, full_semantic_image_len, reset_semantic_image_validations,
    semantic_image_validations,
};
#[cfg(feature = "mmap")]
pub use semantic_image::{
    MappedSemanticImage, MappedSemanticImageError, MappedSemanticImageIoPhase,
    load_semantic_image_mmap, open_semantic_image_mmap,
};
pub use semantic_render::{
    CFamilySemanticDocumentDialect, CSharpSemanticDocumentDialect, CanonicalTypeRenderError,
    CanonicalTypeRenderLimits, CanonicalTypeRenderReference, GoSemanticDocumentDialect,
    JavaSemanticDocumentDialect, NeutralDialect, PreparedCanonicalType, PreparedCanonicalTypeView,
    PreparedNeutral, PreparedNeutralView, PreparedSemanticDocument, PreparedSemanticDocumentView,
    PythonSemanticDocumentDialect, RenderDialect, RenderFailure, RustSemanticDocumentDialect,
    SemanticDocumentDialect, SemanticDocumentError, SemanticDocumentFact,
    SemanticDocumentReference, SemanticImageSourceSyntaxDialect, SourceSyntaxDialect,
    SourceSyntaxError, TypeScriptSemanticDocumentDialect, UnsupportedSemanticStage,
    prepare_canonical_type, prepare_neutral, prepare_profile, prepare_semantic_document,
    prepare_source_syntax,
};
pub use type_facts::{
    DecodedTypeFact, DecodedTypeFactChild, TypeFactChildCursor, TypeFactCounts, TypeFactCursor,
    TypeFactFault, TypeFactInput, TypeFactLane, TypeFactSegment,
};
pub use typed_plane_manifest_v2::{
    MAX_TYPED_PLANE_MANIFEST_V2_BYTES, MAX_TYPED_PLANE_MANIFEST_V2_RESIDENT_BYTES,
    MAX_TYPED_PLANE_SEGMENTS_V2, SEMANTIC_TYPED_PLANE_MANIFEST_V2_SCHEMA, SemanticInputClaimV2,
    SemanticTypedPlaneFamilyDescriptorV2, SemanticTypedPlaneManifestV2,
    SemanticTypedPlaneManifestV2Error, SemanticTypedPlaneManifestV2ResourceUsage,
    SemanticTypedPlaneManifestV2Schema, SemanticTypedPlaneSegmentClaimV2,
};
pub use vcs::{
    Delta, Diff, EntityChange, EntityChanges, EntityFacetChanges, FacetChange, FacetComparison,
    FacetCoverage, GenerationId, LinkChange, LinkChangeKind, LinkChanges, SemanticDiff,
    SemanticEntityChange, SemanticEntityChanges, SemanticEntityRef, SemanticLinkChange,
    SemanticLinkChangeKind, SemanticLinkChanges, SemanticSnapshot, SemanticStableLink,
    SemanticStableLinks, Snapshot, StableLink, StableLinkKey, StableLinks,
};
pub use versioned::{
    EmbeddingNormalization, EmbeddingPlaneIdentity, MAX_SEMANTIC_SEGMENT_BYTES,
    SemanticBuildIdentity, SemanticCoverageState, SemanticDeltaAction, SemanticDeltaCursor,
    SemanticHydrationCoverage, SemanticHydrationCursor, SemanticHydrationCursorToken,
    SemanticInputWitness, SemanticIrPlane, SemanticManifestCoverageScope, SemanticManifestError,
    SemanticManifestRoot, SemanticPlane, SemanticPlaneCatalog, SemanticPlaneCatalogEntry,
    SemanticPlaneCatalogRoot, SemanticPlaneCoverageScope, SemanticPlaneImageKey, SemanticPlaneKind,
    SemanticPlaneManifest, SemanticPlaneRoot, SemanticPlaneSegment, SemanticRangeRequest,
    SemanticSegmentId, SemanticSegmentVerifier, UntrustedSemanticSegmentId,
    VERSIONED_PLANE_MANIFEST_SCHEMA, VERSIONED_PLANE_SEGMENT_SCHEMA, VersionedPlaneManifestSchema,
    VersionedPlaneSegmentSchema,
};
pub use versioned_records::{
    CanonicalPlaneEncodingMetrics, CanonicalPlaneRowEncoder, CanonicalPlaneStreamError,
    CanonicalSemanticPlaneKeySink, CanonicalSemanticPlaneRecordCursor,
    CanonicalSemanticPlaneRecordView, CanonicalSemanticPlaneRowKey,
    CanonicalSemanticPlaneSegmentPayload, CanonicalSemanticPlaneSegmentRef,
    CanonicalSemanticPlaneSegmentSink, CanonicalSemanticPlaneSegmentView,
    CheckedLanguageExtensionFamilyV2, CoreDeclarationRows, DocumentationRows,
    LanguageExtensionRows, MeasuredCanonicalPlaneEncoding, OccurrenceHandle, OccurrenceRows,
    RelationRows, SemanticPlaneRecordError, SourceProvenanceHandle, SourceProvenanceRows,
    TypedRecordPlan, TypesRows, ValidatedCanonicalSemanticPlaneSegment, VerifiedJumboPlaneClosure,
    declaration_plane_key, decode_semantic_plane_segment, encode_canonical_plane_family,
    encode_canonical_plane_family_measured, encode_declaration_planes,
    encode_language_extension_plane, stream_canonical_plane_family,
    stream_canonical_plane_family_with_jumbo, validate_language_extension_family_v2,
    verify_jumbo_plane_family_closures, verify_language_extension_plane_against_reader,
    verify_semantic_plane_family_against_reader,
};
pub use view::OccurrenceFault as OccurrenceViewFault;
pub use view::{
    Atom, AtomCursor, DirectoryFault, EntityCursor, FragmentError, FragmentView, SectionKind,
    SemanticDataFault, TypeNodeCursor, WireField,
};

pub use wire::{FRAGMENT_MAGIC, FRAGMENT_SCHEMA};
