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

mod columnar;
mod coordinate;
mod interner;
#[cfg(feature = "mmap")]
mod mapping;
mod model;
mod prepared;
mod range;
mod render;
mod semantic;
mod semantic_extension_section;
mod vcs;
mod view;
mod wire;

pub use compiler_vocabulary::{
    CSharpVersion, CStandard, CxxStandard, GoVersion, JavaRelease, Language, LanguageProfile,
    PythonVersion, RustEdition, TypeScriptSource,
};
pub use coordinate::{
    AtomId, AtomSpace, DenseId, Entity, EntityId, List, ListId, Text, TextId, Type, TypeId,
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
    AtomFault, AtomInput, EntityFault, EntityKind, EntityNameFault, EntityRecord,
    EntityRecordFault, EntityType, PrimitiveType, RecipeFact, RecipeFactFault, SourceIdentity,
    SourceIdentityFault, TypeNode, TypeNodeFault,
};
pub use prepared::{LayoutStep, PrepareError, PreparedFragment, WriteError};
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
    LanguageExtensionCommonBounds, LanguageExtensionDirectoryKind, LanguageExtensionEncodeError,
    LanguageExtensionReopenError, LanguageExtensionWireFact, ReopenedLanguageExtensionColumn,
    ReopenedLanguageExtensionSection, ValidatedLanguageExtensionCommonBounds,
    encode_language_extension_section, language_extension_section_len,
    reopen_language_extension_section,
};
pub use vcs::{
    Diff, EntityChange, EntityChangeKind, EntityChanges, GenerationId, LinkChange, LinkChangeKind,
    LinkChanges, Snapshot, StableLink, StableLinkKey, StableLinks,
};
pub use view::{
    Atom, AtomCursor, DirectoryFault, EntityCursor, FragmentError, FragmentView, SectionKind,
    TypeNodeCursor, WireField,
};

pub const FRAGMENT_MAGIC: [u8; 4] = *b"NXIR";
pub const FRAGMENT_SCHEMA: u16 = 1;
