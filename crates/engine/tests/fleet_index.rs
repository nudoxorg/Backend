//! Deterministic 200-fragment fleet lifecycle over compiler publication to immutable retrieval.
//!
//! The journey exercises only existing public boundaries: compact fragment
//! preparation, durable compiler publication, reopen, compiler-to-index build,
//! compiler-index seal, and exact plus lexical manifest queries. All inputs are
//! synthetic yet realistic with distinct sources, entity kinds, type
//! coordinates, atom names, and package documents.

mod build_support;

use core::mem::MaybeUninit;
use std::fs;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use backend_engine::driver::CompiledFragment;
use backend_engine::index_build::{
    BuildAdmissionError, BuildDerivationError, BuildError, BuildRegion, EntityFact,
    EntityProjection, IndexBuildCapacity, IndexBuildScratch, build, preflight,
};
use backend_engine::index_publish::{
    CompilationIndexError, CompilationIndexScratch, seal_compilation_index,
};
use backend_engine::publication::binding::COMPILATION_BINDING_BYTES;
use backend_engine::publication::manifest::StoredFragmentFacts;
use backend_engine::publication::{
    OpenPublicationScratch, OpenPublishedError, PublicationScratch, PublishCompiledError,
    PublishedCompilation, PublishControl, UncommittedPublication, open_published, publish_compiled,
};
use backend_engine::queue::{BoundedQueue, QueueBudget, QueueError};
use backend_semantic::index_core::{
    EntityDocumentId, ExactManifest, ExactManifestError, ExactOperation, ExactResolution,
    ExactRow, ExactSegment, ExactSegmentError, ExactSegmentId, IndexSnapshot, IndexSnapshotError,
    LexicalManifest, LexicalManifestError, LexicalOperation, LexicalQueryError, LexicalRow,
    LexicalSegment, LexicalSegmentError, LexicalSegmentId, LexicalSnapshotHit, LexicalTopK,
};
use backend_semantic::ir::{
    Atom, AtomId, AtomInput, EntityKind, EntityRecord, FragmentView, PreparedFragment,
    PrimitiveType, SourceIdentity, TypeId, TypeNode,
};
use backend_semantic::vocabulary::{
    CompileRecipeFact, LanguageProfile, NativeTool, RustEdition, Stage,
};
use backend_store::journal::{DurablePublisher, PublicationLimits, PublicationPaths};
use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

const FRAGMENT_COUNT: usize = 200;
const SHARED_TERM: &[u8] = b"fleet-shared-token";
const TOOLCHAIN_BYTES: &[u8] = b"fleet-test-toolchain";
const UNIQUE_PREFIX: &str = "fleet-entity-";
const SOURCE_PREFIX: &str = "fleet-source-";

#[derive(Debug, thiserror::Error)]
enum FleetError {
    #[error("filesystem operation failed")]
    Io(#[from] std::io::Error),
    #[error("publication limit configuration failed")]
    Limits(#[from] backend_store::journal::PublicationLimitError),
    #[error("durable publisher open failed")]
    PublisherOpen(#[from] backend_store::journal::PublicationOpenError),
    #[error("durable publisher shutdown failed")]
    PublisherShutdown(#[from] backend_store::journal::ShutdownError),
    #[error("fragment preparation failed")]
    Prepare(#[from] backend_semantic::ir::PrepareError),
    #[error("fragment encoding failed")]
    Write(#[from] backend_semantic::ir::WriteError),
    #[error("fragment validation failed")]
    Fragment(#[from] backend_semantic::ir::FragmentError),
    #[error("fragment range manifest failed")]
    Ranges(#[from] backend_semantic::ir::FragmentRangeManifestError),
    #[error("compiler publication failed")]
    Publish(#[from] PublishCompiledError),
    #[error("compiler publication reopen failed")]
    Open(#[from] OpenPublishedError),
    #[error("reopened fragment reconstruction failed")]
    OpenedFragment(#[from] backend_engine::publication::OpenedFragmentError),
    #[error("index build admission rejected")]
    BuildAdmission(#[from] BuildAdmissionError),
    #[error("index build derivation failed")]
    BuildDerivation(#[from] BuildDerivationError),
    #[error("index build exact core rejected rows")]
    BuildExactCore { fault: CoreFault },
    #[error("index build lexical core rejected rows")]
    BuildLexicalCore { fault: CoreFault },
    #[error("compiler index seal failed")]
    Seal(#[from] CompilationIndexError),
    #[error("index snapshot seal failed")]
    Snapshot(#[from] IndexSnapshotError),
    #[error("exact manifest construction failed: {0:?}")]
    ExactManifest(ExactManifestError),
    #[error("lexical manifest construction failed: {0:?}")]
    LexicalManifest(LexicalManifestError),
    #[error("lexical query failed: {0:?}")]
    LexicalQuery(LexicalQueryError),
    #[error("lexical top-k bound rejected: {0:?}")]
    TopK(backend_semantic::index_core::LexicalTopKError),
    #[error("durable queue admission failed")]
    Queue(#[from] QueueError),
    #[error("source length does not fit compact facts")]
    SourceLength(#[from] core::num::TryFromIntError),
    #[error("durable queue unexpectedly reported full")]
    UnexpectedQueueFull,
    #[error("expected {expected}, observed {observed}")]
    Mismatch {
        expected: &'static str,
        observed: &'static str,
    },
    #[error("count mismatch for {what}: expected {expected}, observed {observed}")]
    Count {
        what: &'static str,
        expected: usize,
        observed: usize,
    },
}

impl<'bytes> From<BuildError<'bytes>> for FleetError {
    fn from(error: BuildError<'bytes>) -> Self {
        match error {
            BuildError::Admission(cause) => Self::BuildAdmission(cause),
            BuildError::Derivation(cause) => Self::BuildDerivation(cause),
            BuildError::Exact { cause } => Self::BuildExactCore {
                fault: exact_fault(cause),
            },
            BuildError::Lexical { cause } => Self::BuildLexicalCore {
                fault: lexical_fault(cause),
            },
        }
    }
}

impl From<ExactManifestError> for FleetError {
    fn from(cause: ExactManifestError) -> Self {
        Self::ExactManifest(cause)
    }
}

impl From<LexicalManifestError> for FleetError {
    fn from(cause: LexicalManifestError) -> Self {
        Self::LexicalManifest(cause)
    }
}

impl From<LexicalQueryError> for FleetError {
    fn from(cause: LexicalQueryError) -> Self {
        Self::LexicalQuery(cause)
    }
}

impl From<backend_semantic::index_core::LexicalTopKError> for FleetError {
    fn from(cause: backend_semantic::index_core::LexicalTopKError) -> Self {
        Self::TopK(cause)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CoreFault {
    Admission,
    Payload,
    CanonicalOrder,
    StreamCount,
}

const fn exact_fault(error: ExactSegmentError<'_>) -> CoreFault {
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

const fn lexical_fault(error: LexicalSegmentError<'_>) -> CoreFault {
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct FleetOutcome {
    generation_pinned_root: backend_version::GenerationId,
    manifest_bytes: Vec<u8>,
    snapshot_id: backend_semantic::index_core::IndexSnapshotId,
    exact_segments: usize,
    lexical_segments: usize,
    shared_hits: usize,
    distinct_hit_segments: usize,
}

fn nonzero(value: usize) -> Result<NonZeroUsize, FleetError> {
    match NonZeroUsize::new(value) {
        Some(found) => Ok(found),
        None => Err(FleetError::Mismatch {
            expected: "nonzero queue capacity",
            observed: "zero capacity",
        }),
    }
}

fn fixture_base(label: &str) -> Result<PathBuf, FleetError> {
    let ordinal = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let directory = std::env::temp_dir().join(format!(
        "fleet-index-{label}-{}-{ordinal}",
        std::process::id()
    ));
    fs::create_dir_all(&directory)?;
    Ok(directory)
}

fn source_bytes_for(index: usize) -> Vec<u8> {
    format!("{SOURCE_PREFIX}{index:03}").into_bytes()
}

fn unique_name_for(index: usize) -> Vec<u8> {
    format!("{UNIQUE_PREFIX}{index:03}").into_bytes()
}

const fn kind_for(index: usize) -> EntityKind {
    match index % 15 {
        0 => EntityKind::Function,
        1 => EntityKind::Constant,
        2 => EntityKind::Record,
        3 => EntityKind::Module,
        4 => EntityKind::Field,
        5 => EntityKind::Alias,
        6 => EntityKind::Trait,
        7 => EntityKind::Implementation,
        8 => EntityKind::Enum,
        9 => EntityKind::Variant,
        10 => EntityKind::Static,
        11 => EntityKind::Reexport,
        12 => EntityKind::Parameter,
        13 => EntityKind::Macro,
        _ => EntityKind::Namespace,
    }
}

const fn primitive_for(index: usize) -> PrimitiveType {
    match index % 3 {
        0 => PrimitiveType::Bool,
        1 => PrimitiveType::I32,
        _ => PrimitiveType::String,
    }
}

fn owned_fragment_bytes(index: usize) -> Result<Vec<u8>, FleetError> {
    let source_raw = source_bytes_for(index);
    let unique_raw = unique_name_for(index);
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(&source_raw),
        byte_len: u32::try_from(source_raw.len())?,
    };
    let recipe = CompileRecipeFact::derive(
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        NativeTool::Rustc,
        source.identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(TOOLCHAIN_BYTES),
    );
    let entities = [
        EntityRecord {
            semantic_type: TypeId::new((index as u32) % 3),
            name: AtomId::new(0),
            kind: kind_for(index),
        },
        EntityRecord {
            semantic_type: TypeId::new(0),
            name: AtomId::new(1),
            kind: EntityKind::Function,
        },
    ];
    let types = [
        TypeNode::Primitive(PrimitiveType::Bool),
        TypeNode::Primitive(PrimitiveType::I32),
        TypeNode::Primitive(PrimitiveType::String),
    ];
    let atoms = [
        AtomInput {
            bytes: &unique_raw,
        },
        AtomInput { bytes: SHARED_TERM },
    ];
    let prepared = PreparedFragment::prepare(source, recipe, &entities, &types, &atoms)?;
    let mut output = [0_u8; 1024];
    let written = prepared.write_into(&mut output)?;
    let mut owned = Vec::with_capacity(written.len());
    owned.extend_from_slice(written);
    let _ = primitive_for(index);
    Ok(owned)
}

fn manifest_file_bytes(artifacts: &std::path::Path) -> Result<Vec<u8>, FleetError> {
    let directory = artifacts.join("manifests");
    let mut entries = fs::read_dir(&directory)?;
    let mut found: Option<PathBuf> = None;
    let mut count = 0_usize;
    for entry in entries.by_ref() {
        let path = entry?.path();
        count += 1;
        if found.is_none() {
            found = Some(path);
        }
    }
    if count != 1 {
        return Err(FleetError::Count {
            what: "stored compiler manifests",
            expected: 1,
            observed: count,
        });
    }
    match found {
        Some(path) => Ok(fs::read(&path)?),
        None => Err(FleetError::Mismatch {
            expected: "one stored compiler manifest",
            observed: "no stored compiler manifest",
        }),
    }
}

#[allow(clippy::too_many_lines, reason = "one deterministic fleet journey retains every public boundary")]
fn run_once(label: &str) -> Result<FleetOutcome, FleetError> {
    let _ = build_support::limits()?;
    let base = fixture_base(label)?;
    let journal_dir = base.join("journal");
    let artifacts = base.join("artifacts");
    let limits = PublicationLimits::new(nonzero(8)?, nonzero(8)?)?;
    let paths = PublicationPaths::in_directory(&journal_dir);
    let publisher = DurablePublisher::create(&paths, limits)?;

    let mut owned: Vec<Vec<u8>> = Vec::with_capacity(FRAGMENT_COUNT);
    for index in 0..FRAGMENT_COUNT {
        owned.push(owned_fragment_bytes(index)?);
    }
    if owned.len() != FRAGMENT_COUNT {
        return Err(FleetError::Count {
            what: "synthetic fleet fragments",
            expected: FRAGMENT_COUNT,
            observed: owned.len(),
        });
    }
    let mut compiled: Vec<CompiledFragment<'_>> = Vec::with_capacity(FRAGMENT_COUNT);
    for bytes in &owned {
        let view = FragmentView::validate(bytes)?;
        compiled.push(CompiledFragment {
            source: view.source,
            recipe: view.recipe,
            fragment: view,
        });
    }

    let manifest_len = 16 + FRAGMENT_COUNT * 404 + 64;
    let mut manifest_output = vec![0_u8; manifest_len];
    let mut manifest_facts = vec![None::<StoredFragmentFacts>; FRAGMENT_COUNT];
    let mut ordinals = vec![0_usize; FRAGMENT_COUNT];
    let mut locality_output = vec![0_u8; 1 << 20];
    let mut binding_output = vec![0_u8; COMPILATION_BINDING_BYTES];
    let published: PublishedCompilation = match publish_compiled(
        &publisher,
        &artifacts,
        &compiled,
        PublishControl::Continue,
        PublicationScratch {
            manifest_output: &mut manifest_output,
            manifest_facts: &mut manifest_facts,
            ordinals: &mut ordinals,
            locality_output: &mut locality_output,
            binding_output: &mut binding_output,
        },
    ) {
        Ok(value) => value,
        Err(PublishCompiledError::Uncommitted(UncommittedPublication::Full { .. })) => {
            return Err(FleetError::UnexpectedQueueFull);
        }
        Err(other) => return Err(FleetError::Publish(other)),
    };
    let manifest_bytes = manifest_file_bytes(&artifacts)?;

    let mut reopen_manifest = vec![0_u8; manifest_len];
    let mut reopen_facts = vec![None::<StoredFragmentFacts>; FRAGMENT_COUNT];
    let mut fragment_output = vec![0_u8; 512 * 1024];
    let mut reopen_locality = vec![0_u8; 1 << 20];
    let opened = match open_published(
        &publisher,
        &artifacts,
        OpenPublicationScratch {
            manifest_output: &mut reopen_manifest,
            manifest_facts: &mut reopen_facts,
            fragment_output: &mut fragment_output,
            locality_output: &mut reopen_locality,
        },
    )? {
        Some(value) => value,
        None => {
            return Err(FleetError::Mismatch {
                expected: "one selected durable compiler package",
                observed: "no selected compiler package",
            });
        }
    };
    if opened.publication != published.publication {
        return Err(FleetError::Mismatch {
            expected: "reopen selects the published generation",
            observed: "reopen selected another generation",
        });
    }
    let fragment_total = usize::try_from(opened.manifest.fragment_count).map_err(|_| {
        FleetError::Mismatch {
            expected: "addressable fragment count",
            observed: "fragment count overflow",
        }
    })?;
    if fragment_total != FRAGMENT_COUNT {
        return Err(FleetError::Count {
            what: "reopened fleet fragments",
            expected: FRAGMENT_COUNT,
            observed: fragment_total,
        });
    }

    let mut cursor = opened.fragments();
    let mut fragments = Vec::with_capacity(FRAGMENT_COUNT);
    for _ in 0..FRAGMENT_COUNT {
        match cursor.next() {
            Some(Ok(fragment)) => fragments.push(fragment),
            Some(Err(cause)) => return Err(FleetError::OpenedFragment(cause)),
            None => {
                return Err(FleetError::Mismatch {
                    expected: "manifest-named reopened fragment",
                    observed: "truncated reopened cursor",
                });
            }
        }
    }
    if fragments.len() != FRAGMENT_COUNT {
        return Err(FleetError::Count {
            what: "collected reopened fragments",
            expected: FRAGMENT_COUNT,
            observed: fragments.len(),
        });
    }

    let mut indexes = Vec::with_capacity(FRAGMENT_COUNT);
    for fragment in &fragments {
        let projections_box: Box<[MaybeUninit<EntityProjection<'_>>]> = (0..4)
            .map(|_| MaybeUninit::<EntityProjection<'_>>::uninit())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let entities_box: Box<[MaybeUninit<EntityFact<'_>>]> = (0..4)
            .map(|_| MaybeUninit::<EntityFact<'_>>::uninit())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let exact_box: Box<[MaybeUninit<ExactRow<'_>>]> = (0..4)
            .map(|_| MaybeUninit::<ExactRow<'_>>::uninit())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let lexical_box: Box<[MaybeUninit<LexicalRow<'_>>]> = (0..4)
            .map(|_| MaybeUninit::<LexicalRow<'_>>::uninit())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let atoms_box: Box<[MaybeUninit<Atom<'_>>]> = (0..4)
            .map(|_| MaybeUninit::<Atom<'_>>::uninit())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let types_box: Box<[MaybeUninit<TypeNode>]> = (0..4)
            .map(|_| MaybeUninit::<TypeNode>::uninit())
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let projections_leaked: &mut [MaybeUninit<EntityProjection<'_>>] =
            Box::leak(projections_box);
        let entities_leaked: &mut [MaybeUninit<EntityFact<'_>>] = Box::leak(entities_box);
        let exact_leaked: &mut [MaybeUninit<ExactRow<'_>>] = Box::leak(exact_box);
        let lexical_leaked: &mut [MaybeUninit<LexicalRow<'_>>] = Box::leak(lexical_box);
        let atoms_leaked: &mut [MaybeUninit<Atom<'_>>] = Box::leak(atoms_box);
        let types_leaked: &mut [MaybeUninit<TypeNode>] = Box::leak(types_box);
        let prepared = build(
            fragment,
            IndexBuildScratch {
                projections: projections_leaked,
                entities: entities_leaked,
                exact_rows: exact_leaked,
                lexical_rows: lexical_leaked,
                atoms: atoms_leaked,
                type_nodes: types_leaked,
            },
        )?;
        if prepared.entities.len() != 2 {
            return Err(FleetError::Count {
                what: "indexed entities per fleet fragment",
                expected: 2,
                observed: prepared.entities.len(),
            });
        }
        indexes.push(prepared);
    }
    if indexes.len() != FRAGMENT_COUNT {
        return Err(FleetError::Count {
            what: "prepared fleet indexes",
            expected: FRAGMENT_COUNT,
            observed: indexes.len(),
        });
    }

    let mut exact_ids = vec![ExactSegmentId::from_canonical_bytes(b"fleet-placeholder-exact"); FRAGMENT_COUNT];
    let mut lexical_ids =
        vec![LexicalSegmentId::from_canonical_bytes(b"fleet-placeholder-lex"); FRAGMENT_COUNT];
    let sealed = seal_compilation_index(
        opened,
        &indexes,
        CompilationIndexScratch {
            exact: &mut exact_ids,
            lexical: &mut lexical_ids,
        },
    )
    .map_err(|rejected| FleetError::Seal(rejected.error))?;
    let snapshot = sealed.snapshot;

    let mut exact_segments: Vec<ExactSegment<'_>> = Vec::with_capacity(FRAGMENT_COUNT);
    let mut lexical_segments: Vec<LexicalSegment<'_>> = Vec::with_capacity(FRAGMENT_COUNT);
    for prepared in &indexes {
        exact_segments.push(prepared.exact);
        lexical_segments.push(prepared.lexical);
    }
    exact_segments.sort_by(|left, right| left.id.cmp(&right.id));
    lexical_segments.sort_by(|left, right| left.id.cmp(&right.id));

    let target_unique = unique_name_for(7);
    let mut target_key: Option<backend_engine::index_build::ExactEntityKey> = None;
    for prepared in &indexes {
        for fact in prepared.entities {
            if fact.name == target_unique.as_slice() {
                target_key = Some(fact.exact_key);
            }
        }
    }
    let target_key = match target_key {
        Some(key) => key,
        None => {
            return Err(FleetError::Mismatch {
                expected: "indexed unique fleet entity",
                observed: "missing unique entity key",
            });
        }
    };
    let exact_manifest = ExactManifest::new(snapshot, &exact_segments, &[])?;
    let exact_terminal = exact_manifest.execute(ExactOperation::new(target_key.as_ref()));
    match exact_terminal {
        backend_semantic::index_core::ExactTerminal::Complete {
            resolution:
                ExactResolution::Present { value: _, segment: _ },
            ..
        } => {}
        _ => {
            return Err(FleetError::Mismatch {
                expected: "complete exact present resolution",
                observed: "another exact terminal",
            });
        }
    }

    let lexical_manifest = LexicalManifest::new(snapshot, &lexical_segments, &[])?;
    let top_k = LexicalTopK::new(FRAGMENT_COUNT)?;
    let mut seen: Vec<Option<EntityDocumentId>> = vec![None; FRAGMENT_COUNT];
    let placeholder_segment = match lexical_segments.first() {
        Some(segment) => segment.id,
        None => {
            return Err(FleetError::Mismatch {
                expected: "at least one lexical segment",
                observed: "no lexical segments",
            });
        }
    };
    let placeholder_document = match indexes.first() {
        Some(prepared) => match prepared.lexical.rows.first() {
            Some(row) => row.document,
            None => {
                return Err(FleetError::Mismatch {
                    expected: "at least one lexical row",
                    observed: "no lexical rows",
                });
            }
        },
        None => {
            return Err(FleetError::Mismatch {
                expected: "at least one prepared index",
                observed: "no prepared indexes",
            });
        }
    };
    let placeholder = LexicalSnapshotHit::new(
        placeholder_segment,
        SHARED_TERM,
        placeholder_document,
        backend_semantic::index_core::LexicalScore::from(1),
    );
    let mut lexical_output = vec![placeholder; FRAGMENT_COUNT];
    let terminal = lexical_manifest.execute(
        LexicalOperation::new(SHARED_TERM),
        top_k,
        &mut seen,
        &mut lexical_output,
    )?;
    let hit_slice: &[LexicalSnapshotHit<'_>] = match terminal {
        backend_semantic::index_core::LexicalTerminal::Complete { hits, .. } => hits,
        _ => {
            return Err(FleetError::Mismatch {
                expected: "complete lexical terminal",
                observed: "another lexical terminal",
            });
        }
    };
    let shared_hits = hit_slice.len();
    if shared_hits != FRAGMENT_COUNT {
        return Err(FleetError::Count {
            what: "shared-term lexical hits",
            expected: FRAGMENT_COUNT,
            observed: shared_hits,
        });
    }
    let mut distinct_segments: Vec<LexicalSegmentId> = Vec::with_capacity(shared_hits);
    for hit in hit_slice {
        if hit.term != SHARED_TERM {
            return Err(FleetError::Mismatch {
                expected: "shared-term lexical hit",
                observed: "another lexical term",
            });
        }
        let mut known = false;
        for known_id in &distinct_segments {
            if *known_id == hit.segment {
                known = true;
            }
        }
        if !known {
            distinct_segments.push(hit.segment);
        }
    }
    if distinct_segments.len() < 2 {
        return Err(FleetError::Count {
            what: "distinct cross-package hit segments",
            expected: 2,
            observed: distinct_segments.len(),
        });
    }
    let mut distinct_artifacts = 1_usize;
    let first_document = match hit_slice.first() {
        Some(hit) => hit.document,
        None => {
            return Err(FleetError::Mismatch {
                expected: "at least one shared hit",
                observed: "no shared hits",
            });
        }
    };
    for hit in hit_slice.iter().skip(1) {
        if hit.document != first_document {
            distinct_artifacts += 1;
        }
    }
    if distinct_artifacts < 2 {
        return Err(FleetError::Count {
            what: "distinct cross-package hit documents",
            expected: 2,
            observed: distinct_artifacts,
        });
    }

    let queue: BoundedQueue<Vec<u8>> = BoundedQueue::new(QueueBudget::new(256, 1 << 20));
    for index in 0..FRAGMENT_COUNT {
        let payload = unique_name_for(index);
        if queue.try_push(payload).is_err() {
            return Err(FleetError::Mismatch {
                expected: "durable queue admission",
                observed: "queue unexpectedly full",
            });
        }
    }
    let mut drained = 0_usize;
    while queue.try_pop().is_some() {
        drained += 1;
    }
    if drained != FRAGMENT_COUNT {
        return Err(FleetError::Count {
            what: "drained durable queue payloads",
            expected: FRAGMENT_COUNT,
            observed: drained,
        });
    }

    publisher.shutdown()?;
    let outcome = FleetOutcome {
        generation_pinned_root: published.publication.generation.pinned_root,
        manifest_bytes,
        snapshot_id: snapshot.id,
        exact_segments: exact_segments.len(),
        lexical_segments: lexical_segments.len(),
        shared_hits,
        distinct_hit_segments: distinct_segments.len(),
    };
    fs::remove_dir_all(&base)?;
    Ok(outcome)
}

#[test]
fn fleet_200_fragment_lifecycle() -> Result<(), FleetError> {
    let outcome = run_once("lifecycle")?;
    if outcome.exact_segments != FRAGMENT_COUNT {
        return Err(FleetError::Count {
            what: "sealed exact segments",
            expected: FRAGMENT_COUNT,
            observed: outcome.exact_segments,
        });
    }
    if outcome.lexical_segments != FRAGMENT_COUNT {
        return Err(FleetError::Count {
            what: "sealed lexical segments",
            expected: FRAGMENT_COUNT,
            observed: outcome.lexical_segments,
        });
    }
    if outcome.shared_hits != FRAGMENT_COUNT {
        return Err(FleetError::Count {
            what: "fleet shared-term hits",
            expected: FRAGMENT_COUNT,
            observed: outcome.shared_hits,
        });
    }
    if outcome.distinct_hit_segments < 2 {
        return Err(FleetError::Count {
            what: "fleet cross-package segments",
            expected: 2,
            observed: outcome.distinct_hit_segments,
        });
    }
    Ok(())
}

#[test]
fn fleet_published_root_byte_identical() -> Result<(), FleetError> {
    let first = run_once("determinism-first")?;
    let second = run_once("determinism-second")?;
    if first.generation_pinned_root != second.generation_pinned_root {
        return Err(FleetError::Mismatch {
            expected: "byte-identical published root",
            observed: "deterministic runs diverged",
        });
    }
    if first.manifest_bytes != second.manifest_bytes {
        return Err(FleetError::Mismatch {
            expected: "byte-identical manifest bytes",
            observed: "deterministic manifests diverged",
        });
    }
    if first.snapshot_id != second.snapshot_id {
        return Err(FleetError::Mismatch {
            expected: "identical sealed snapshot identity",
            observed: "deterministic snapshots diverged",
        });
    }
    Ok(())
}

#[test]
fn fleet_durable_queue_without_full() -> Result<(), FleetError> {
    let queue: BoundedQueue<Vec<u8>> = BoundedQueue::new(QueueBudget::new(256, 1 << 20));
    for index in 0..FRAGMENT_COUNT {
        match queue.try_push(unique_name_for(index)) {
            Ok(()) => {}
            Err(QueueError::Count | QueueError::Bytes) => {
                return Err(FleetError::UnexpectedQueueFull);
            }
            Err(other) => return Err(FleetError::Queue(other)),
        }
    }
    if queue.usage().count != FRAGMENT_COUNT {
        return Err(FleetError::Count {
            what: "queued durable payloads",
            expected: FRAGMENT_COUNT,
            observed: queue.usage().count,
        });
    }
    let mut drained = 0_usize;
    while queue.try_pop().is_some() {
        drained += 1;
    }
    if drained != FRAGMENT_COUNT {
        return Err(FleetError::Count {
            what: "dequeued durable payloads",
            expected: FRAGMENT_COUNT,
            observed: drained,
        });
    }
    let outcome = run_once("queue-admission")?;
    if outcome.shared_hits != FRAGMENT_COUNT {
        return Err(FleetError::Count {
            what: "queue-admission shared hits",
            expected: FRAGMENT_COUNT,
            observed: outcome.shared_hits,
        });
    }
    Ok(())
}

#[allow(clippy::too_many_lines, reason = "typed terminal preservation checks every public failure family")]
#[test]
fn fleet_typed_terminals_preserved() -> Result<(), FleetError> {
    let base = fixture_base("terminals")?;
    let journal_dir = base.join("journal");
    let artifacts = base.join("artifacts");
    let limits = PublicationLimits::new(nonzero(4)?, nonzero(4)?)?;
    let publisher = DurablePublisher::create(&PublicationPaths::in_directory(&journal_dir), limits)?;
    let single = owned_fragment_bytes(0)?;
    let view = FragmentView::validate(&single)?;
    let compiled_single = CompiledFragment {
        source: view.source,
        recipe: view.recipe,
        fragment: view,
    };

    let mut manifest_output = vec![0_u8; 4096];
    let mut manifest_facts = vec![None::<StoredFragmentFacts>; 1];
    let mut ordinals = vec![0_usize; 1];
    let mut locality_output = vec![0_u8; 65536];
    let mut binding_output = vec![0_u8; COMPILATION_BINDING_BYTES];
    match publish_compiled(
        &publisher,
        &artifacts,
        std::slice::from_ref(&compiled_single),
        PublishControl::CancelBeforeStorage,
        PublicationScratch {
            manifest_output: &mut manifest_output,
            manifest_facts: &mut manifest_facts,
            ordinals: &mut ordinals,
            locality_output: &mut locality_output,
            binding_output: &mut binding_output,
        },
    ) {
        Err(PublishCompiledError::CancelledBeforeStorage) => {}
        Err(other) => return Err(FleetError::Publish(other)),
        Ok(_) => {
            return Err(FleetError::Mismatch {
                expected: "typed cancelled-before-storage terminal",
                observed: "unexpected publication success",
            });
        }
    }

    let mut manifest_output = vec![0_u8; 4096];
    let mut manifest_facts = vec![None::<StoredFragmentFacts>; 1];
    let mut ordinals = vec![0_usize; 1];
    let mut locality_output = vec![0_u8; 65536];
    let mut binding_output = vec![0_u8; COMPILATION_BINDING_BYTES];
    let published = match publish_compiled(
        &publisher,
        &artifacts,
        std::slice::from_ref(&compiled_single),
        PublishControl::Continue,
        PublicationScratch {
            manifest_output: &mut manifest_output,
            manifest_facts: &mut manifest_facts,
            ordinals: &mut ordinals,
            locality_output: &mut locality_output,
            binding_output: &mut binding_output,
        },
    ) {
        Ok(value) => value,
        Err(PublishCompiledError::Uncommitted(UncommittedPublication::Full { .. })) => {
            return Err(FleetError::UnexpectedQueueFull);
        }
        Err(other) => return Err(FleetError::Publish(other)),
    };

    let mut reopen_manifest = vec![0_u8; 4096];
    let mut reopen_facts = vec![None::<StoredFragmentFacts>; 1];
    let mut fragment_output = vec![0_u8; 8192];
    let mut reopen_locality = vec![0_u8; 65536];
    let opened = match open_published(
        &publisher,
        &artifacts,
        OpenPublicationScratch {
            manifest_output: &mut reopen_manifest,
            manifest_facts: &mut reopen_facts,
            fragment_output: &mut fragment_output,
            locality_output: &mut reopen_locality,
        },
    )? {
        Some(value) => value,
        None => {
            return Err(FleetError::Mismatch {
                expected: "one selected terminal package",
                observed: "no selected package",
            });
        }
    };
    if opened.publication != published.publication {
        return Err(FleetError::Mismatch {
            expected: "terminal reopen selects publication",
            observed: "terminal reopen diverged",
        });
    }
    let mut cursor = opened.fragments();
    let fragment = match cursor.next() {
        Some(Ok(value)) => value,
        Some(Err(cause)) => return Err(FleetError::OpenedFragment(cause)),
        None => {
            return Err(FleetError::Mismatch {
                expected: "one terminal fragment",
                observed: "no terminal fragment",
            });
        }
    };

    match preflight(
        &fragment,
        IndexBuildCapacity {
            projections: 0,
            entities: 0,
            exact_rows: 0,
            lexical_rows: 0,
            atoms: 0,
            type_nodes: 0,
        },
    ) {
        Err(BuildAdmissionError::OutputTooSmall { .. }) => {}
        Err(other) => return Err(FleetError::BuildAdmission(other)),
        Ok(()) => {
            return Err(FleetError::Mismatch {
                expected: "typed output-too-small terminal",
                observed: "unexpected preflight success",
            });
        }
    }

    let projections_box: Box<[MaybeUninit<EntityProjection<'_>>]> = (0..2)
        .map(|_| MaybeUninit::<EntityProjection<'_>>::uninit())
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let entities_box: Box<[MaybeUninit<EntityFact<'_>>]> = (0..2)
        .map(|_| MaybeUninit::<EntityFact<'_>>::uninit())
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let exact_box: Box<[MaybeUninit<ExactRow<'_>>]> = (0..2)
        .map(|_| MaybeUninit::<ExactRow<'_>>::uninit())
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let lexical_box: Box<[MaybeUninit<LexicalRow<'_>>]> = (0..2)
        .map(|_| MaybeUninit::<LexicalRow<'_>>::uninit())
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let atoms_box: Box<[MaybeUninit<Atom<'_>>]> = (0..2)
        .map(|_| MaybeUninit::<Atom<'_>>::uninit())
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let types_box: Box<[MaybeUninit<TypeNode>]> = (0..3)
        .map(|_| MaybeUninit::<TypeNode>::uninit())
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let projections_leaked: &mut [MaybeUninit<EntityProjection<'_>>] = Box::leak(projections_box);
    let entities_leaked: &mut [MaybeUninit<EntityFact<'_>>] = Box::leak(entities_box);
    let exact_leaked: &mut [MaybeUninit<ExactRow<'_>>] = Box::leak(exact_box);
    let lexical_leaked: &mut [MaybeUninit<LexicalRow<'_>>] = Box::leak(lexical_box);
    let atoms_leaked: &mut [MaybeUninit<Atom<'_>>] = Box::leak(atoms_box);
    let types_leaked: &mut [MaybeUninit<TypeNode>] = Box::leak(types_box);
    let prepared = build(
        &fragment,
        IndexBuildScratch {
            projections: projections_leaked,
            entities: entities_leaked,
            exact_rows: exact_leaked,
            lexical_rows: lexical_leaked,
            atoms: atoms_leaked,
            type_nodes: types_leaked,
        },
    )?;

    let mut exact_scratch = vec![ExactSegmentId::from_canonical_bytes(b"fleet-terminal-exact")];
    let mut lexical_scratch = vec![LexicalSegmentId::from_canonical_bytes(b"fleet-terminal-lex")];
    match seal_compilation_index(
        opened,
        &[],
        CompilationIndexScratch {
            exact: &mut exact_scratch,
            lexical: &mut lexical_scratch,
        },
    ) {
        Err(rejected) => match rejected.error {
            CompilationIndexError::PreparedCount { expected, observed } => {
                if expected != 1 || observed != 0 {
                    return Err(FleetError::Count {
                        what: "seal prepared-count observed",
                        expected: 0,
                        observed,
                    });
                }
            }
            other => return Err(FleetError::Seal(other)),
        },
        Ok(_) => {
            return Err(FleetError::Mismatch {
                expected: "typed prepared-count terminal",
                observed: "unexpected seal success",
            });
        }
    }

    let exact_ids = [prepared.exact.id];
    let lexical_ids = [prepared.lexical.id];
    let snapshot_value =
        IndexSnapshot::new(published.publication.generation.pinned_root, &exact_ids, &lexical_ids)?;
    let exact_segments = [prepared.exact];
    let lexical_segments = [prepared.lexical];
    match ExactManifest::new(snapshot_value, &[], &[]) {
        Err(ExactManifestError::SnapshotSelectionWidth { .. }) => {}
        Err(other) => return Err(FleetError::ExactManifest(other)),
        Ok(_) => {
            return Err(FleetError::Mismatch {
                expected: "typed exact selection-width terminal",
                observed: "unexpected exact manifest success",
            });
        }
    }
    let lexical_manifest = LexicalManifest::new(snapshot_value, &lexical_segments, &[])?;
    let top_k = LexicalTopK::new(1)?;
    let mut seen: Vec<Option<EntityDocumentId>> = vec![];
    let terminal_document = match prepared.lexical.rows.first() {
        Some(row) => row.document,
        None => {
            return Err(FleetError::Mismatch {
                expected: "at least one terminal lexical row",
                observed: "no terminal lexical rows",
            });
        }
    };
    let placeholder = LexicalSnapshotHit::new(
        prepared.lexical.id,
        SHARED_TERM,
        terminal_document,
        backend_semantic::index_core::LexicalScore::from(1),
    );
    let mut lexical_output = vec![placeholder; 1];
    match lexical_manifest.execute(
        LexicalOperation::new(SHARED_TERM),
        top_k,
        &mut seen,
        &mut lexical_output,
    ) {
        Err(LexicalQueryError::ScratchCapacity { .. }) => {}
        Err(other) => return Err(FleetError::LexicalQuery(other)),
        Ok(_) => {
            return Err(FleetError::Mismatch {
                expected: "typed lexical scratch-capacity terminal",
                observed: "unexpected lexical success",
            });
        }
    }
    let _ = exact_segments;
    let _ = BuildRegion::Projections;

    publisher.shutdown()?;
    fs::remove_dir_all(&base)?;
    Ok(())
}
