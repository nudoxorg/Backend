//! Exercises the `server-index-build` tests support contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
#![allow(
    dead_code,
    unreachable_pub,
    reason = "the shared typed fixture compiles independently into several focused integration targets"
)]

use core::mem::MaybeUninit;
use std::{
    fs, io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use compiler_driver::CompiledFragment;
use backend_semantic::ir::{
    Atom, AtomInput, EntityRecord, FragmentView, PreparedFragment, SourceIdentity, TypeNode,
};
use compiler_publication::{
    OpenPublicationScratch, OpenedCompilation, OpenedFragment, OpenedFragmentCursor,
    PublicationScratch, PublishControl, open_published, publish_compiled,
};
use backend_semantic::vocabulary::{CompileRecipeFact, LanguageProfile, NativeTool, RustEdition, Stage};
use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
use server_index_build::{
    BuildAdmissionError, BuildDerivationError, BuildError, EntityFact, EntityProjection,
    IndexBuildScratch, PreparedIndex, build,
};
use backend_semantic::index_core::{ExactRow, ExactSegmentError, LexicalRow, LexicalSegmentError};
use server_journal::{DurablePublisher, PublicationLimits, PublicationPaths};
use thiserror::Error;

pub(crate) const CAPACITY: usize = 4;
pub(crate) const LARGE_FRAGMENT_BYTES: usize = 8_192;

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Error)]
pub(crate) enum TestError {
    #[error("filesystem fixture failed")]
    Io(#[from] io::Error),
    #[error("durable publisher limit configuration failed")]
    Limits(#[from] server_journal::PublicationLimitError),
    #[error("durable publisher open failed")]
    Publisher(#[from] server_journal::PublicationOpenError),
    #[error("durable publisher shutdown failed")]
    Shutdown(#[from] server_journal::ShutdownError),
    #[error("compact fragment preparation failed")]
    Prepare(#[from] backend_semantic::ir::PrepareError),
    #[error("compact fragment encoding failed")]
    Write(#[from] backend_semantic::ir::WriteError),
    #[error("compact fragment validation failed")]
    Fragment(#[from] backend_semantic::ir::FragmentError),
    #[error("persisted exact entity value was malformed")]
    ExactEntityValue(#[from] server_index_build::ExactEntityValueError),
    #[error("compact fragment range manifest failed")]
    Ranges(#[from] backend_semantic::ir::FragmentRangeManifestError),
    #[error("immutable fragment artifact operation failed")]
    Artifact {
        #[source]
        source: Box<compiler_publication::immutable::ImmutableArtifactError>,
    },
    #[error("compiler publication failed")]
    Publish {
        #[source]
        source: Box<compiler_publication::PublishCompiledError>,
    },
    #[error("compiler publication reopen failed")]
    Open {
        #[source]
        source: Box<compiler_publication::OpenPublishedError>,
    },
    #[error("reopened fragment reconstruction failed")]
    Opened {
        #[source]
        source: Box<compiler_publication::OpenedFragmentError>,
    },
    #[error("source length cannot fit compact source facts")]
    SourceLength(#[from] core::num::TryFromIntError),
    #[error("fragment writer reported {requested} bytes for a {available}-byte output")]
    EncodedRange { requested: usize, available: usize },
    #[error("index builder rejected a reopened compiler fragment")]
    Build(#[from] BuildTerminal),
    #[error(transparent)]
    BuildProof(#[from] BuildProofError),
    #[error("opened package lacked fragment {ordinal}")]
    MissingFragment { ordinal: usize },
    #[error("opened package had no selected publication")]
    MissingPublication,
}

#[derive(Debug, Error)]
pub(crate) enum BuildProofError {
    #[error("typed entity facts and existing-core rows diverged")]
    EntityRowMismatch,
    #[error("distinct fragments reused an immutable segment identity")]
    ReusedIdentity,
    #[error("warm compiler-to-index build allocated")]
    WarmAllocation,
    #[error("allocation measurement did not execute the builder")]
    MeasurementSkipped,
    #[error("a raw declaration-order change reused immutable global entity authority")]
    SourceOrderChanged,
    #[error("compiler type coordinates were treated as structural identity")]
    TypeCoordinatesCollapsed,
    #[error("valid overloaded names were rejected by the exact plane")]
    DuplicateNameRejected {
        #[source]
        cause: BuildTerminal,
    },
    #[error("exact entity key did not retrieve its corresponding typed value")]
    ExactEntityLookupMismatch,
    #[error("an empty reopened fragment did not project to empty existing-core segments")]
    EmptyFragmentProjectionMismatch,
    #[error("preflight wrote a caller region before rejecting capacity")]
    PreflightMutatedOutput,
    #[error("undersized caller region {region:?} was accepted")]
    UndersizedOutputAccepted {
        region: server_index_build::BuildRegion,
    },
    #[error("maximal admitted entity count was rejected")]
    MaximumEntityCountRejected,
    #[error("one-over-max entity count did not reject before writing caller output")]
    EntityLimitPreflightFailed,
    #[error("equal declarations from distinct reopened fragments shared a global exact entity key")]
    FragmentNamespaceCollapsed,
    #[error("equal declarations from distinct reopened fragments shared lexical authority")]
    LexicalAuthorityCollapsed,
    #[error("one reopened fragment did not project an indexed entity")]
    MissingIndexedEntity,
    #[error("the immutable snapshot rejected distinct reopened projections: {cause:?}")]
    SnapshotRejected {
        cause: backend_semantic::index_core::IndexSnapshotError,
    },
    #[error("changing one fragment atom changed another fragment's segment proof")]
    AtomChangeEscapedFragment,
    #[error("truncated immutable fragment reached the builder boundary")]
    TruncatedArtifactAccepted,
    #[error("substituted immutable fragment reached the builder boundary")]
    SubstitutedArtifactAccepted,
    #[error("immutable reopen reported an unexpected terminal")]
    UnexpectedReopenTerminal {
        #[source]
        cause: Box<compiler_publication::OpenPublishedError>,
    },
}

/// A lifetime-free structural classification of a builder terminal from one test journey.
#[derive(Debug, Error)]
pub(crate) enum BuildTerminal {
    #[error("semantic index input omitted its complete image authority")]
    MissingSemanticImage,
    #[error("canonical entity ordinal did not fit compiler identity")]
    EntityOrdinalAddressSpace,
    #[error("entity bound rejected build: observed {observed}, maximum {maximum}")]
    EntityLimit { maximum: usize, observed: usize },
    #[error("caller region {region:?} was short: required {required}, available {available}")]
    OutputTooSmall {
        region: server_index_build::BuildRegion,
        required: usize,
        available: usize,
    },
    #[error("initialization diverged in {region:?}: required {required}, available {available}")]
    ScratchInitialization {
        region: server_index_build::BuildRegion,
        required: usize,
        available: usize,
    },
    #[error("entity atom coordinate did not fit the target")]
    AtomAddressSpace,
    #[error("entity type coordinate did not fit the target")]
    TypeAddressSpace,
    #[error("validated entity name atom was absent from collected atoms")]
    MissingAtom,
    #[error("validated entity type node was absent from collected types")]
    MissingTypeNode,
    #[error("{plane:?} core rejected canonical rows as {fault:?}")]
    Core { plane: CorePlane, fault: CoreFault },
}

/// Existing immutable-core plane that rejected the builder's derived rows.
#[derive(Debug)]
pub(crate) enum CorePlane {
    Exact,
    Lexical,
}

/// Structural category of an existing-core row rejection.
#[derive(Debug)]
pub(crate) enum CoreFault {
    Admission,
    Payload,
    CanonicalOrder,
    StreamCount,
}

impl From<BuildError<'_>> for BuildTerminal {
    fn from(error: BuildError<'_>) -> Self {
        match error {
            BuildError::Admission(cause) => Self::from(cause),
            BuildError::Derivation(cause) => Self::from(cause),
            BuildError::Exact { cause } => Self::Core {
                plane: CorePlane::Exact,
                fault: exact_core_fault(cause),
            },
            BuildError::Lexical { cause } => Self::Core {
                plane: CorePlane::Lexical,
                fault: lexical_core_fault(cause),
            },
        }
    }
}

impl From<BuildDerivationError> for BuildTerminal {
    fn from(error: BuildDerivationError) -> Self {
        match error {
            BuildDerivationError::MissingSemanticImage => Self::MissingSemanticImage,
            BuildDerivationError::EntityOrdinalAddressSpace { .. } => {
                Self::EntityOrdinalAddressSpace
            }
            BuildDerivationError::ScratchInitialization {
                region,
                required,
                available,
            } => Self::ScratchInitialization {
                region,
                required,
                available,
            },
            BuildDerivationError::AtomAddressSpace { .. } => Self::AtomAddressSpace,
            BuildDerivationError::TypeAddressSpace { .. } => Self::TypeAddressSpace,
            BuildDerivationError::MissingAtom { .. } => Self::MissingAtom,
            BuildDerivationError::MissingTypeNode { .. } => Self::MissingTypeNode,
        }
    }
}

impl From<BuildAdmissionError> for BuildTerminal {
    fn from(error: BuildAdmissionError) -> Self {
        match error {
            BuildAdmissionError::EntityLimit { maximum, observed } => {
                Self::EntityLimit { maximum, observed }
            }
            BuildAdmissionError::OutputTooSmall {
                region,
                required,
                available,
            } => Self::OutputTooSmall {
                region,
                required,
                available,
            },
        }
    }
}

impl From<BuildError<'_>> for TestError {
    fn from(error: BuildError<'_>) -> Self {
        Self::Build(error.into())
    }
}

impl From<compiler_publication::immutable::ImmutableArtifactError> for TestError {
    fn from(source: compiler_publication::immutable::ImmutableArtifactError) -> Self {
        Self::Artifact {
            source: Box::new(source),
        }
    }
}

impl From<compiler_publication::PublishCompiledError> for TestError {
    fn from(source: compiler_publication::PublishCompiledError) -> Self {
        Self::Publish {
            source: Box::new(source),
        }
    }
}

impl From<compiler_publication::OpenPublishedError> for TestError {
    fn from(source: compiler_publication::OpenPublishedError) -> Self {
        Self::Open {
            source: Box::new(source),
        }
    }
}

impl From<compiler_publication::OpenedFragmentError> for TestError {
    fn from(source: compiler_publication::OpenedFragmentError) -> Self {
        Self::Opened {
            source: Box::new(source),
        }
    }
}

impl From<BuildAdmissionError> for TestError {
    fn from(error: BuildAdmissionError) -> Self {
        Self::Build(error.into())
    }
}

const fn exact_core_fault(error: ExactSegmentError<'_>) -> CoreFault {
    match error {
        ExactSegmentError::TooManyRows { .. } => CoreFault::Admission,
        ExactSegmentError::PayloadBytesLimit { .. }
        | ExactSegmentError::PayloadBytesOverflow { .. } => CoreFault::Payload,
        ExactSegmentError::OutOfOrder { .. } | ExactSegmentError::DuplicateKey { .. } => {
            CoreFault::CanonicalOrder
        }
        ExactSegmentError::RowCount { .. } => CoreFault::StreamCount,
    }
}

const fn lexical_core_fault(error: LexicalSegmentError<'_>) -> CoreFault {
    match error {
        LexicalSegmentError::TooManyRows { .. } => CoreFault::Admission,
        LexicalSegmentError::PayloadBytesLimit { .. }
        | LexicalSegmentError::PayloadBytesOverflow { .. } => CoreFault::Payload,
        LexicalSegmentError::OutOfOrder { .. } | LexicalSegmentError::DuplicateRow { .. } => {
            CoreFault::CanonicalOrder
        }
        LexicalSegmentError::RowCount { .. } => CoreFault::StreamCount,
    }
}

pub struct Fixture {
    directory: PathBuf,
}

impl Fixture {
    pub fn new(label: &str) -> Result<Self, io::Error> {
        let ordinal = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "server-index-build-{label}-{}-{ordinal}",
            std::process::id()
        ));
        fs::create_dir(&directory)?;
        Ok(Self { directory })
    }

    pub fn publisher(&self) -> Result<DurablePublisher, TestError> {
        Ok(DurablePublisher::create(
            &PublicationPaths::in_directory(&self.directory.join("journal")),
            limits()?,
        )?)
    }

    pub fn artifacts(&self) -> PathBuf {
        self.directory.join("artifacts")
    }

    pub fn remove(self) -> Result<(), io::Error> {
        fs::remove_dir_all(self.directory)
    }
}

pub struct OpenBuffers {
    pub(crate) manifest: [u8; 1024],
    pub(crate) facts: [Option<compiler_publication::manifest::StoredFragmentFacts>; 2],
    pub(crate) fragments: [u8; LARGE_FRAGMENT_BYTES],
    pub(crate) locality: [u8; 1024],
}

impl OpenBuffers {
    pub const fn new() -> Self {
        Self {
            manifest: [0; 1024],
            facts: [None; 2],
            fragments: [0; LARGE_FRAGMENT_BYTES],
            locality: [0; 1024],
        }
    }

    pub fn open<'bytes>(
        &'bytes mut self,
        publisher: &DurablePublisher,
        artifacts: &Path,
    ) -> Result<OpenedCompilation<'bytes, 'bytes>, TestError> {
        open_published(
            publisher,
            artifacts,
            OpenPublicationScratch {
                manifest_output: &mut self.manifest,
                manifest_facts: &mut self.facts,
                fragment_output: &mut self.fragments,
                locality_output: &mut self.locality,
            },
        )?
        .ok_or(TestError::MissingPublication)
    }
}

pub struct BuildBuffers<'bytes> {
    projections: [MaybeUninit<EntityProjection<'bytes>>; CAPACITY],
    entities: [MaybeUninit<EntityFact<'bytes>>; CAPACITY],
    exact: [MaybeUninit<ExactRow<'bytes>>; CAPACITY],
    lexical: [MaybeUninit<LexicalRow<'bytes>>; CAPACITY],
    atoms: [MaybeUninit<Atom<'bytes>>; CAPACITY],
    types: [MaybeUninit<TypeNode>; CAPACITY],
}

impl<'bytes> BuildBuffers<'bytes> {
    pub const fn new() -> Self {
        Self {
            projections: [MaybeUninit::uninit(); CAPACITY],
            entities: [MaybeUninit::uninit(); CAPACITY],
            exact: [MaybeUninit::uninit(); CAPACITY],
            lexical: [MaybeUninit::uninit(); CAPACITY],
            atoms: [MaybeUninit::uninit(); CAPACITY],
            types: [MaybeUninit::uninit(); CAPACITY],
        }
    }

    pub fn build(
        &'bytes mut self,
        fragment: &'bytes OpenedFragment<'bytes>,
    ) -> Result<PreparedIndex<'bytes>, TestError> {
        build(
            fragment,
            IndexBuildScratch {
                projections: &mut self.projections,
                entities: &mut self.entities,
                exact_rows: &mut self.exact,
                lexical_rows: &mut self.lexical,
                atoms: &mut self.atoms,
                type_nodes: &mut self.types,
            },
        )
        .map_err(build_error)
    }
}

pub fn publish(
    publisher: &DurablePublisher,
    artifacts: &Path,
    fragments: &[CompiledFragment<'_>],
) -> Result<compiler_publication::PublishedCompilation, TestError> {
    let mut manifest = [0_u8; 1024];
    let mut facts = [None; 2];
    let mut ordinals = [0_usize; 2];
    let mut locality = [0_u8; 1024];
    let mut binding = [0_u8; compiler_publication::binding::COMPILATION_BINDING_BYTES];
    Ok(publish_compiled(
        publisher,
        artifacts,
        fragments,
        PublishControl::Continue,
        PublicationScratch {
            manifest_output: &mut manifest,
            manifest_facts: &mut facts,
            ordinals: &mut ordinals,
            locality_output: &mut locality,
            binding_output: &mut binding,
        },
    )?)
}

pub(crate) fn write_fragment<const BYTES: usize>(
    output: &mut [u8; BYTES],
    source_bytes: &[u8],
    entities: &[EntityRecord],
    types: &[TypeNode],
    atoms: &[AtomInput<'_>],
) -> Result<usize, TestError> {
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source_bytes),
        byte_len: u32::try_from(source_bytes.len())?,
    };
    let recipe = CompileRecipeFact::derive(
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        NativeTool::Rustc,
        source.identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"index-build-test-toolchain"),
    );
    Ok(
        PreparedFragment::prepare(source, recipe, entities, types, atoms)?
            .write_into(output)?
            .len(),
    )
}

pub fn compiled(bytes: &[u8]) -> Result<CompiledFragment<'_>, TestError> {
    let fragment = FragmentView::validate(bytes)?;
    Ok(CompiledFragment {
        source: fragment.source,
        recipe: fragment.recipe,
        fragment,
    })
}

pub fn written(bytes: &[u8], length: usize) -> Result<&[u8], TestError> {
    bytes.get(..length).ok_or(TestError::EncodedRange {
        requested: length,
        available: bytes.len(),
    })
}

pub fn next_fragment<'fragment>(
    cursor: &mut OpenedFragmentCursor<'_, 'fragment>,
    ordinal: usize,
) -> Result<OpenedFragment<'fragment>, TestError> {
    match cursor.next() {
        Some(fragment) => fragment.map_err(TestError::from),
        None => Err(TestError::MissingFragment { ordinal }),
    }
}

pub fn limits() -> Result<PublicationLimits, server_journal::PublicationLimitError> {
    PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)
}

fn build_error(error: BuildError<'_>) -> TestError {
    TestError::Build(error.into())
}
