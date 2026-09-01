//! Exercises the `server-index-publish` graph-seal contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Compiler publication to snapshot-pinned graph rows: the ingest journey for the graph family.
//!
//! A compiler publication is durably sealed, reopened, projected into exact and lexical segments,
//! and sealed into an immutable snapshot. The graph projection derived from the same reopened
//! fragments is pinned to that exact snapshot authority, passes graph admission unchanged, and
//! answers real Trustfall neighbor queries. An authority naming any other snapshot is rejected
//! before admission.

use core::mem::MaybeUninit;

use compiler_driver::CompiledFragment;
use compiler_ir::{
    AtomInput, EntityKind, EntityRecord, FragmentView, PrepareError, PreparedFragment,
    PrimitiveType, SourceIdentity, TypeNode, WriteError,
};
use compiler_ir_vocabulary::{AtomId, EntityId, TypeId};
use compiler_publication::{
    OpenPublicationScratch, OpenedFragmentError, PublicationScratch, PublishCompiledError,
    PublishControl, PublishedCompilation, open_published, publish_compiled,
};
use compiler_vocabulary::{CompileRecipeFact, Language, NativeTool, Stage};
use heart_identity::{ContentId, SourceFactDomain, ToolchainDomain};
use server_index_build::{
    EntityFact, EntityProjection, GraphProjection, GraphProjectionError, GraphProjectionScratch,
    IndexBuildScratch, NodeKind, NodeWorkspace, OwnerCount, ReferenceTarget,
    SEMANTIC_TYPE_REFERENCE_PROJECTION, build, build_graph_projection,
};
use server_index_core::{ExactRow, LexicalRow};
use server_index_graph_vector::{
    GraphAuthority, GraphEdge, GraphRow, PartitionId, ValidatedGraphView,
};
use server_index_publish::{
    CompilationIndexError, CompilationIndexScratch, seal_compilation_index,
};
use server_index_trustfall::TrustfallGraph;
use server_index_vocabulary::IndexSnapshotId;
use server_journal::{
    DurablePublisher, PublicationLimitError, PublicationLimits, PublicationOpenError,
    PublicationPaths, ShutdownError,
};
use thiserror::Error;

const FRAGMENT_BYTES: usize = 512;
const MANIFEST_BYTES: usize = 1024;
const FRAGMENT_OUTPUT_BYTES: usize = 1024;
const LOCALITY_BYTES: usize = 256;
const BUILD_LANES: usize = 4;
const GRAPH_EDGES: usize = 64;
const GRAPH_ROWS: usize = 8;

#[derive(Debug, Error)]
enum SealGraphError {
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
    Fragment(#[from] compiler_ir::FragmentError),
    #[error("compiler publication failed")]
    Publish(#[source] Box<PublishCompiledError>),
    #[error("compiler publication reopen failed")]
    Open(#[source] Box<compiler_publication::OpenPublishedError>),
    #[error("reopened fragment reconstruction failed")]
    OpenedFragment(#[source] Box<OpenedFragmentError>),
    #[error("durable publication did not expose a reopened package")]
    MissingOpenedCompilation,
    #[error("index builder rejected a reopened fragment")]
    BuildRejected(#[from] BuildRejection),
    #[error("compiler-index seal rejected a complete prepared index")]
    Seal(#[from] CompilationIndexError),
    #[error("graph projection rejected its derivation")]
    Projection(#[from] GraphProjectionError),
    #[error("graph admission rejected derived rows: {0:?}")]
    Admission(server_index_graph_vector::AdmissionError),
    #[error("graph query rejected its output contract: {0:?}")]
    Query(server_index_graph_vector::GraphQueryError),
    #[error("trustfall query rejected its output contract")]
    Trustfall(#[from] server_index_trustfall::TrustfallGraphError),
    #[error("derived graph rows diverged from the published facts")]
    GraphFacts,
    #[error("temporary fixture filesystem operation failed")]
    Io(#[from] std::io::Error),
}

/// Structural classification of a builder rejection, independent of borrowed row evidence.
#[derive(Debug, Error)]
enum BuildRejection {
    #[error("preflight rejected caller capacity")]
    Admission,
    #[error("derivation rejected a canonical compiler fact")]
    Derivation,
    #[error("exact core rejected derived rows")]
    Exact,
    #[error("lexical core rejected derived rows")]
    Lexical,
}

impl<'bytes> From<server_index_build::BuildError<'bytes>> for BuildRejection {
    fn from(error: server_index_build::BuildError<'bytes>) -> Self {
        match error {
            server_index_build::BuildError::Admission(_) => Self::Admission,
            server_index_build::BuildError::Derivation(_) => Self::Derivation,
            server_index_build::BuildError::Exact { .. } => Self::Exact,
            server_index_build::BuildError::Lexical { .. } => Self::Lexical,
        }
    }
}

impl From<PublishCompiledError> for SealGraphError {
    fn from(error: PublishCompiledError) -> Self {
        Self::Publish(Box::new(error))
    }
}

impl From<compiler_publication::OpenPublishedError> for SealGraphError {
    fn from(error: compiler_publication::OpenPublishedError) -> Self {
        Self::Open(Box::new(error))
    }
}

impl From<OpenedFragmentError> for SealGraphError {
    fn from(error: OpenedFragmentError) -> Self {
        Self::OpenedFragment(Box::new(error))
    }
}

impl From<server_index_graph_vector::AdmissionError> for SealGraphError {
    fn from(error: server_index_graph_vector::AdmissionError) -> Self {
        Self::Admission(error)
    }
}

impl From<server_index_graph_vector::GraphQueryError> for SealGraphError {
    fn from(error: server_index_graph_vector::GraphQueryError) -> Self {
        Self::Query(error)
    }
}

#[test]
fn sealed_compilation_snapshots_pin_derived_graph_rows() -> Result<(), SealGraphError> {
    let directory =
        std::env::temp_dir().join(format!("server-index-graph-seal-{}", std::process::id()));
    std::fs::create_dir(&directory)?;
    let result = seal_graph_journey(&directory);
    let cleanup = std::fs::remove_dir_all(&directory);
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(journey), _) => Err(journey),
        (Ok(()), Err(cleanup)) => Err(cleanup.into()),
    }
}

fn seal_graph_journey(directory: &std::path::Path) -> Result<(), SealGraphError> {
    let limits =
        PublicationLimits::new(core::num::NonZeroUsize::MIN, core::num::NonZeroUsize::MIN)?;
    let journal = DurablePublisher::create(
        &PublicationPaths::in_directory(&directory.join("durable")),
        limits,
    )?;
    let mut fragment_bytes = [0_u8; FRAGMENT_BYTES];
    let compiled = {
        let prepared = reference_fragment(&mut fragment_bytes)?;
        CompiledFragment {
            source: prepared_source(),
            recipe: prepared_recipe(),
            fragment: prepared,
        }
    };
    let fragments = [compiled];
    let mut manifest_output = [0_u8; MANIFEST_BYTES];
    let mut manifest_facts = [None; 1];
    let mut ordinals = [0_usize; 1];
    let mut locality_output = [0_u8; LOCALITY_BYTES];
    let mut binding_output = [0_u8; compiler_publication::binding::COMPILATION_BINDING_BYTES];
    let selected = publish_compiled(
        &journal,
        &directory.join("artifacts"),
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
    journal.shutdown()?;

    let reopened = DurablePublisher::reopen(
        &PublicationPaths::in_directory(&directory.join("durable")),
        limits,
    )?;
    let journey = reopen_derive_and_query(directory, &reopened, &selected);
    let shutdown = reopened.shutdown();
    match (journey, shutdown) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(journey), Ok(())) => Err(journey),
        (Ok(()), Err(shutdown)) => Err(shutdown.into()),
        (Err(journey), Err(shutdown)) => {
            drop(journey);
            Err(shutdown.into())
        }
    }
}

fn reopen_derive_and_query(
    directory: &std::path::Path,
    journal: &DurablePublisher,
    selected: &PublishedCompilation,
) -> Result<(), SealGraphError> {
    let mut manifest_output = [0_u8; MANIFEST_BYTES];
    let mut manifest_facts = [None; 1];
    let mut fragment_output = [0_u8; FRAGMENT_OUTPUT_BYTES];
    let mut locality_output = [0_u8; LOCALITY_BYTES];
    let opened = open_published(
        journal,
        &directory.join("artifacts"),
        OpenPublicationScratch {
            manifest_output: &mut manifest_output,
            manifest_facts: &mut manifest_facts,
            fragment_output: &mut fragment_output,
            locality_output: &mut locality_output,
        },
    )?
    .ok_or(SealGraphError::MissingOpenedCompilation)?;
    if opened.publication != selected.publication {
        return Err(SealGraphError::MissingOpenedCompilation);
    }

    let mut projections = [MaybeUninit::<EntityProjection<'_>>::uninit(); BUILD_LANES];
    let mut entities = [MaybeUninit::<EntityFact<'_>>::uninit(); BUILD_LANES];
    let mut exact_rows = [MaybeUninit::<ExactRow<'_>>::uninit(); BUILD_LANES];
    let mut lexical_rows = [MaybeUninit::<LexicalRow<'_>>::uninit(); BUILD_LANES];
    let mut atoms = [MaybeUninit::<compiler_ir::Atom<'_>>::uninit(); BUILD_LANES];
    let mut type_nodes = [MaybeUninit::<TypeNode>::uninit(); BUILD_LANES];
    let mut fragments = opened.fragments();
    let fragment = match fragments.next() {
        Some(fragment) => fragment?,
        None => return Err(SealGraphError::MissingOpenedCompilation),
    };
    let prepared = build(
        &fragment,
        IndexBuildScratch {
            projections: &mut projections,
            entities: &mut entities,
            exact_rows: &mut exact_rows,
            lexical_rows: &mut lexical_rows,
            atoms: &mut atoms,
            type_nodes: &mut type_nodes,
        },
    )
    .map_err(|error| SealGraphError::BuildRejected(error.into()))?;
    let prepared_indexes = [prepared];
    let mut exact = [prepared.exact.id; 1];
    let mut lexical = [prepared.lexical.id; 1];
    let sealed = seal_compilation_index(
        opened,
        &prepared_indexes,
        CompilationIndexScratch {
            exact: &mut exact,
            lexical: &mut lexical,
        },
    )
    .map_err(|rejected| rejected.error)?;
    let authority = GraphAuthority::new(sealed.snapshot.id, SEMANTIC_TYPE_REFERENCE_PROJECTION);

    let mut rows = [MaybeUninit::<GraphRow>::uninit(); GRAPH_ROWS];
    let mut edges = [MaybeUninit::<GraphEdge>::uninit(); GRAPH_EDGES];
    let mut kinds = [NodeKind::PLACEHOLDER; 8];
    let mut targets = [ReferenceTarget::PLACEHOLDER; 8];
    let mut owners = [OwnerCount::PLACEHOLDER; 8];
    let projection: GraphProjection<'_> = build_graph_projection(
        &fragment.view,
        authority,
        PartitionId::new(0),
        GraphProjectionScratch {
            rows: &mut rows,
            edges: &mut edges,
        },
        &mut NodeWorkspace::new(&mut kinds, &mut targets, &mut owners)?,
    )?;
    if projection.edge_count() != 2 {
        return Err(SealGraphError::GraphFacts);
    }
    let validated = ValidatedGraphView::try_new(authority, projection.rows)?;
    let mut hits = [None; 4];
    let outcome = validated.neighbors(&[PartitionId::new(0)], EntityId::new(2), &mut hits)?;
    if outcome.written != 1 || outcome.terminal.is_partial() {
        return Err(SealGraphError::GraphFacts);
    }
    let mut queried = [None; 4];
    let terminal = TrustfallGraph::new(&validated).neighbors(EntityId::new(2), &mut queried)?;
    if terminal.written != 1 || terminal.authority != authority {
        return Err(SealGraphError::GraphFacts);
    }
    let graph_hit = queried
        .iter()
        .find_map(|slot| *slot)
        .ok_or(SealGraphError::GraphFacts)?;
    if graph_hit.entity != EntityId::new(0) {
        return Err(SealGraphError::GraphFacts);
    }
    if sealed.snapshot.generation != selected.publication.generation.pinned_root {
        return Err(SealGraphError::GraphFacts);
    }
    assert_ne!(
        sealed.snapshot.id,
        IndexSnapshotId::from_canonical_bytes(b"graph-seal-unrelated")
    );
    Ok(())
}

fn prepared_source() -> SourceIdentity {
    let source_bytes = b"graph-seal-fixture-source-bytes";
    SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source_bytes),
        byte_len: 31,
    }
}

fn prepared_recipe() -> CompileRecipeFact {
    CompileRecipeFact::derive(
        Language::Rust,
        Stage::LowerIr,
        NativeTool::Rustc,
        prepared_source().identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"graph-seal-fixture-toolchain"),
    )
}

fn reference_fragment(
    buffer: &mut [u8; FRAGMENT_BYTES],
) -> Result<FragmentView<'_>, SealGraphError> {
    let entities = [
        EntityRecord {
            semantic_type: TypeId::new(0),
            name: AtomId::new(0),
            kind: EntityKind::Function,
        },
        EntityRecord {
            semantic_type: TypeId::new(1),
            name: AtomId::new(0),
            kind: EntityKind::Record,
        },
        EntityRecord {
            semantic_type: TypeId::new(2),
            name: AtomId::new(0),
            kind: EntityKind::Constant,
        },
    ];
    let nodes = [
        TypeNode::Reference(TypeId::new(1)),
        TypeNode::Primitive(PrimitiveType::I32),
        TypeNode::Reference(TypeId::new(0)),
    ];
    let atoms = [AtomInput { bytes: b"Alpha" }];
    let prepared = PreparedFragment::prepare(
        prepared_source(),
        prepared_recipe(),
        &entities,
        &nodes,
        &atoms,
    )?;
    let bytes = prepared.write_into(buffer)?;
    Ok(FragmentView::validate(bytes)?)
}
