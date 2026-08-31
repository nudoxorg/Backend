#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
//! Compact compiler-to-publication-to-index ownership journey.

use std::{
    fs, io,
    mem::MaybeUninit,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use nudox_compile_driver::CompiledFragment;
use nudox_compile_publication::{
    OpenPublicationScratch, OpenPublishedError, OpenedCompilation, OpenedFragmentError,
    PublicationScratch, PublishCompiledError, PublishControl, PublishedCompilation,
    binding::COMPILATION_BINDING_BYTES, open_published, publish_compiled,
};
use nudox_compile_vocab::{CompileRecipeFact, Language, NativeTool, Stage};
use nudox_durable_journal::{
    DurablePublisher, PublicationLimitError, PublicationLimits, PublicationOpenError,
    PublicationPaths, ShutdownError,
};
use nudox_id::{ContentId, SourceFactDomain, ToolchainDomain};
use nudox_index_build::{EntityFact, EntityProjection, IndexBuildScratch, PreparedIndex, build};
use nudox_index_core::{ExactRow, LexicalRow};
use nudox_index_publish::{CompilationIndexError, CompilationIndexScratch, seal_compilation_index};
use nudox_ir_format::{
    Atom, AtomInput, EntityKind, EntityRecord, FragmentError, FragmentView, PrepareError,
    PreparedFragment, PrimitiveType, SourceIdentity, TypeNode, WriteError,
};
use nudox_ir_vocab::{AtomId, TypeId};
use thiserror::Error;

const FRAGMENT_BYTES: usize = 512;
const MANIFEST_BYTES: usize = 1_024;
const FRAGMENT_OUTPUT_BYTES: usize = 1_024;
const LOCALITY_BYTES: usize = 256;
const FRAGMENT_SLOTS: usize = 1;

static JOURNEY_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Error)]
pub(super) enum TestError {
    #[error("could not create the compiler snapshot fixture")]
    CreateFixture(#[source] io::Error),
    #[error("compiler snapshot journey failed and fixture cleanup also failed")]
    JourneyAndCleanup {
        journey: Box<JourneyError>,
        cleanup: io::Error,
    },
    #[error("compiler snapshot fixture cleanup failed")]
    Cleanup(#[source] io::Error),
    #[error(transparent)]
    Journey(Box<JourneyError>),
}

impl From<JourneyError> for TestError {
    fn from(error: JourneyError) -> Self {
        Self::Journey(Box::new(error))
    }
}

#[derive(Debug, Error)]
pub(super) enum JourneyError {
    #[error("could not configure durable publication")]
    Limits(#[from] PublicationLimitError),
    #[error("could not create or reopen durable publication")]
    Publisher(#[from] PublicationOpenError),
    #[error("could not shut down durable publication")]
    Shutdown(#[from] ShutdownError),
    #[error("could not prepare compact IR")]
    Prepare(#[from] PrepareError),
    #[error("could not write compact IR")]
    Write(#[from] WriteError),
    #[error("could not validate compact IR")]
    Fragment(#[from] FragmentError),
    #[error("compiler publication failed")]
    Publish(#[source] Box<PublishCompiledError>),
    #[error("published compiler package and binding disagree")]
    PublishedFactsMismatch,
    #[error(transparent)]
    Reopen(Box<ReopenError>),
    #[error("source length does not fit the compact source fact")]
    SourceLength(#[from] std::num::TryFromIntError),
    #[error("reopened operation failed and shutdown also failed")]
    ReopenAndShutdown {
        operation: Box<ReopenError>,
        shutdown: ShutdownError,
    },
}

impl From<PublishCompiledError> for JourneyError {
    fn from(error: PublishCompiledError) -> Self {
        Self::Publish(Box::new(error))
    }
}

impl From<ReopenError> for JourneyError {
    fn from(error: ReopenError) -> Self {
        Self::Reopen(Box::new(error))
    }
}

#[derive(Debug, Error)]
pub(super) enum ReopenError {
    #[error("compiler publication reopen failed")]
    Open(#[source] Box<OpenPublishedError>),
    #[error("durable publication did not expose a reopened package")]
    MissingOpenedCompilation,
    #[error("manifest-selected compiler fragment could not be reopened")]
    Fragment(#[source] Box<OpenedFragmentError>),
    #[error("reopened package and binding disagree")]
    ReopenedFactsMismatch,
    #[error("reopened compiler publication did not expose its one fragment")]
    MissingFragment,
    #[error("reopened compiler publication exposed more than one fragment")]
    SurplusFragment,
    #[error("compiler-authoritative index construction unexpectedly rejected the fixture")]
    BuildRejected,
    #[error("incomplete prepared-index coverage was accepted")]
    IncompleteCoverageAccepted,
    #[error("incomplete coverage did not retain the exact prepared-count cause")]
    IncompleteCoverageCause,
    #[error("incomplete coverage rejection did not retain the opened publication")]
    OpenedPublicationLost,
    #[error("compiler-index seal rejected a complete prepared index")]
    Seal(#[from] CompilationIndexError),
}

impl From<OpenedFragmentError> for ReopenError {
    fn from(error: OpenedFragmentError) -> Self {
        Self::Fragment(Box::new(error))
    }
}

impl From<OpenPublishedError> for ReopenError {
    fn from(error: OpenPublishedError) -> Self {
        Self::Open(Box::new(error))
    }
}

struct Fixture {
    directory: PathBuf,
}

impl Fixture {
    fn create() -> Result<Self, io::Error> {
        let sequence = JOURNEY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "nudox-index-compiler-snapshot-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&directory)?;
        Ok(Self { directory })
    }

    fn journal(&self) -> PublicationPaths {
        PublicationPaths::in_directory(&self.directory.join("durable"))
    }

    fn artifacts(&self) -> PathBuf {
        self.directory.join("artifacts")
    }

    fn remove(self) -> Result<(), io::Error> {
        fs::remove_dir_all(self.directory)
    }
}

pub(super) fn run() -> Result<(), TestError> {
    let fixture = Fixture::create().map_err(TestError::CreateFixture)?;
    let journey = run_journey(&fixture);
    let cleanup = fixture.remove();
    match (journey, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(journey), Ok(())) => Err(TestError::Journey(Box::new(journey))),
        (Ok(()), Err(cleanup)) => Err(TestError::Cleanup(cleanup)),
        (Err(journey), Err(cleanup)) => Err(TestError::JourneyAndCleanup {
            journey: Box::new(journey),
            cleanup,
        }),
    }
}

fn run_journey(fixture: &Fixture) -> Result<(), JourneyError> {
    let limits = PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)?;
    let journal = DurablePublisher::create(&fixture.journal(), limits)?;
    let mut fragment_bytes = [0_u8; FRAGMENT_BYTES];
    {
        let fragment = compact_fragment(&mut fragment_bytes)?;
        let fragments = [fragment];
        let mut manifest_output = [0_u8; MANIFEST_BYTES];
        let mut manifest_facts = [None; FRAGMENT_SLOTS];
        let mut ordinals = [0_usize; FRAGMENT_SLOTS];
        let mut locality_output = [0_u8; LOCALITY_BYTES];
        let mut binding_output = [0_u8; COMPILATION_BINDING_BYTES];
        let published = publish_compiled(
            &journal,
            &fixture.artifacts(),
            &fragments,
            PublishControl::Continue,
            PublicationScratch {
                manifest_output: &mut manifest_output,
                manifest_facts: &mut manifest_facts,
                ordinals: &mut ordinals,
                locality_output: &mut locality_output,
                binding_output: &mut binding_output,
            },
        )?;
        if published.publication.generation != published.binding.generation {
            return Err(JourneyError::PublishedFactsMismatch);
        }
        journal.shutdown()?;
        let reopened = DurablePublisher::reopen(&fixture.journal(), limits)?;
        let operation = reopen_and_seal(&reopened, &fixture.artifacts(), &published);
        let shutdown = reopened.shutdown();
        match (operation, shutdown) {
            (Ok(()), Ok(())) => {}
            (Err(operation), Ok(())) => return Err(operation.into()),
            (Ok(()), Err(shutdown)) => return Err(JourneyError::Shutdown(shutdown)),
            (Err(operation), Err(shutdown)) => {
                return Err(JourneyError::ReopenAndShutdown {
                    operation: Box::new(operation),
                    shutdown,
                });
            }
        }
    }
    Ok(())
}

fn compact_fragment(
    output: &mut [u8; FRAGMENT_BYTES],
) -> Result<CompiledFragment<'_>, JourneyError> {
    let source_bytes = b"pub const answer: bool = true;";
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source_bytes),
        byte_len: u32::try_from(source_bytes.len())?,
    };
    let recipe = CompileRecipeFact::derive(
        Language::Rust,
        Stage::LowerIr,
        NativeTool::Rustc,
        source.identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"rustc-test-toolchain"),
    );
    let entities = [EntityRecord {
        semantic_type: TypeId::new(0),
        name: AtomId::new(0),
        kind: EntityKind::Constant,
    }];
    let type_nodes = [TypeNode::Primitive(PrimitiveType::Bool)];
    let atoms = [AtomInput { bytes: b"answer" }];
    let prepared = PreparedFragment::prepare(source, recipe, &entities, &type_nodes, &atoms)?;
    let bytes = prepared.write_into(output)?;
    let fragment = FragmentView::validate(bytes)?;
    Ok(CompiledFragment {
        source,
        recipe,
        fragment,
    })
}

fn reopen_and_seal(
    journal: &DurablePublisher,
    artifacts: &Path,
    selected: &PublishedCompilation,
) -> Result<(), ReopenError> {
    let mut manifest_output = [0_u8; MANIFEST_BYTES];
    let mut manifest_facts = [None; FRAGMENT_SLOTS];
    let mut fragment_output = [0_u8; FRAGMENT_OUTPUT_BYTES];
    let mut locality_output = [0_u8; LOCALITY_BYTES];
    let opened = open_published(
        journal,
        artifacts,
        OpenPublicationScratch {
            manifest_output: &mut manifest_output,
            manifest_facts: &mut manifest_facts,
            fragment_output: &mut fragment_output,
            locality_output: &mut locality_output,
        },
    )?
    .ok_or(ReopenError::MissingOpenedCompilation)?;
    if opened.publication != selected.publication || opened.binding != selected.binding {
        return Err(ReopenError::ReopenedFactsMismatch);
    }

    let mut projections = [MaybeUninit::<EntityProjection<'_>>::uninit(); FRAGMENT_SLOTS];
    let mut entities = [MaybeUninit::<EntityFact<'_>>::uninit(); FRAGMENT_SLOTS];
    let mut exact_rows = [MaybeUninit::<ExactRow<'_>>::uninit(); FRAGMENT_SLOTS];
    let mut lexical_rows = [MaybeUninit::<LexicalRow<'_>>::uninit(); FRAGMENT_SLOTS];
    let mut atoms = [MaybeUninit::<Atom<'_>>::uninit(); FRAGMENT_SLOTS];
    let mut type_nodes = [MaybeUninit::<TypeNode>::uninit(); FRAGMENT_SLOTS];
    let prepared = build_index(
        &opened,
        IndexBuildScratch {
            projections: &mut projections,
            entities: &mut entities,
            exact_rows: &mut exact_rows,
            lexical_rows: &mut lexical_rows,
            atoms: &mut atoms,
            type_nodes: &mut type_nodes,
        },
    )?;
    let prepared_indexes = [prepared];
    let mut exact = [prepared.exact.id; FRAGMENT_SLOTS];
    let mut lexical = [prepared.lexical.id; FRAGMENT_SLOTS];
    let opened = reject_incomplete_coverage(opened, selected, &mut exact, &mut lexical)?;
    let sealed = seal_compilation_index(
        opened,
        &prepared_indexes,
        CompilationIndexScratch {
            exact: &mut exact,
            lexical: &mut lexical,
        },
    )
    .map_err(|rejected| rejected.error)?;
    if sealed.opened.publication != selected.publication
        || sealed.snapshot.generation != selected.publication.generation.pinned_root
        || sealed.snapshot.exact != [prepared.exact.id].as_slice()
        || sealed.snapshot.lexical != [prepared.lexical.id].as_slice()
    {
        return Err(ReopenError::ReopenedFactsMismatch);
    }
    Ok(())
}

fn build_index<'fragment: 'scratch, 'scratch>(
    opened: &OpenedCompilation<'fragment, '_>,
    scratch: IndexBuildScratch<'scratch>,
) -> Result<PreparedIndex<'scratch>, ReopenError> {
    let mut fragments = opened.fragments();
    let fragment = fragments.next().ok_or(ReopenError::MissingFragment)??;
    if fragments.next().is_some() {
        return Err(ReopenError::SurplusFragment);
    }
    build(&fragment, scratch).map_err(|_cause| ReopenError::BuildRejected)
}

fn reject_incomplete_coverage<'manifest, 'facts>(
    opened: OpenedCompilation<'manifest, 'facts>,
    selected: &PublishedCompilation,
    exact: &mut [nudox_index_core::ExactSegmentId],
    lexical: &mut [nudox_index_core::LexicalSegmentId],
) -> Result<OpenedCompilation<'manifest, 'facts>, ReopenError> {
    let empty = [];
    let rejected =
        match seal_compilation_index(opened, &empty, CompilationIndexScratch { exact, lexical }) {
            Ok(_sealed) => return Err(ReopenError::IncompleteCoverageAccepted),
            Err(rejected) => rejected,
        };
    if !matches!(
        rejected.error,
        CompilationIndexError::PreparedCount {
            expected: FRAGMENT_SLOTS,
            observed: 0,
        }
    ) {
        return Err(ReopenError::IncompleteCoverageCause);
    }
    if rejected.opened.publication != selected.publication {
        return Err(ReopenError::OpenedPublicationLost);
    }
    Ok(rejected.opened)
}
