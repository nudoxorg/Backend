//! Borrow-scoped compiler-unit row replacement and dependency invalidation.
//!
//! The owner in this module is deliberately narrower than a semantic
//! generation. It retains the last observed canonical row slabs for each
//! source unit and accepts a replacement event for one unit at a time. A
//! replacement compares only that unit's old and new sorted rows; it never
//! diffs two whole-image snapshots. Results are exact unit-contribution
//! candidates for observed families and observed dependency channels. The
//! workspace, unobserved families, embedding outputs, and dependency channels
//! without compiler events remain explicitly unproven.
//!
//! This is a replay owner after a compiler supplies one replacement
//! observation; it is not yet wired to a frontend incremental session.
//! `from_reader` runs the current canonical encoders over every row in the
//! touched reader. The avoided work starts after that capture: unchanged
//! source-unit slabs are not compared, and only reached reverse-dependency
//! buckets are traversed.
//!
//! `SemanticUnitObservation::from_reader` is the production ingestion path:
//! the seven normalized families are emitted by the existing canonical row
//! encoders over one compiler-owned `SemanticReader`. The temporary segment
//! witness is marked `Unsupported`; it is only used to validate locally
//! encoded records and cannot attest a generation or authorize reuse.

use alloc::{boxed::Box, collections::BTreeMap, vec::Vec};
use core::mem::size_of;

use backend_version::{Coverage, ScopeRoot};
use thiserror::Error;

use crate::ir::row_index::{RowFamily, RowPayload, StableRowIndexError, StableRowKey};
use crate::ir::{
    CanonicalSemanticPlaneRecordView, CoreDeclarationRows, DocumentationRows,
    EmbeddingPlaneIdentity, ImageProvenance, LanguageExtensionRows, OccurrenceRows, RelationRows,
    SemanticCoreReader, SemanticImageAuthority, SemanticInputWitness, SemanticPlaneKind,
    SemanticPlaneRecordError, SemanticReader, SourceIdentity, SourceProvenanceRows, TypesRows,
    decode_semantic_plane_segment, encode_canonical_plane_family_measured,
};
use crate::vocabulary::CompileRecipeFact;

/// Stable identity of one compiler source unit, independent of its contents.
///
/// Package scope and source path are both included. The bytes are derived
/// from captured semantic-image provenance, so an edit changes the unit's
/// rows without changing the identity used to replace them.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SourceUnitKey([u8; 32]);

impl SourceUnitKey {
    /// Returns the fixed-width unit identity.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }

    /// Captures a stable unit key from the reader's compiler provenance.
    pub fn from_reader<Reader: SemanticCoreReader + ?Sized>(
        reader: &Reader,
    ) -> Result<Self, SemanticUnitCaptureError> {
        let ImageProvenance::Captured { scope, .. } = reader.image_facts().provenance else {
            return Err(SemanticUnitCaptureError::MissingCapturedProvenance);
        };
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.semantic.ir.source-unit.v1\0");
        hash_scope_atom(reader, scope.ecosystem, 0, &mut hasher)?;
        hash_scope_atom(reader, scope.package, 1, &mut hasher)?;
        if let Some(coordinate) = scope.coordinate {
            hash_scope_atom(reader, coordinate, 2, &mut hasher)?;
        } else {
            hasher.update(&[2, 0]);
        }
        hash_scope_atom(reader, scope.path, 3, &mut hasher)?;
        Ok(Self(*hasher.finalize().as_bytes()))
    }
}

fn hash_scope_atom<Reader: SemanticCoreReader + ?Sized>(
    reader: &Reader,
    atom: crate::ir::AtomId,
    field: u8,
    hasher: &mut blake3::Hasher,
) -> Result<(), SemanticUnitCaptureError> {
    let bytes = reader
        .atom(atom)
        .ok_or(SemanticUnitCaptureError::MissingScopeAtom)?;
    let length = u64::try_from(bytes.len()).map_err(|_| SemanticUnitCaptureError::Overflow)?;
    hasher.update(&[field, 1]);
    hasher.update(&length.to_be_bytes());
    hasher.update(bytes);
    Ok(())
}

/// An independent embedding row invalidation key.
///
/// Embedding keys never enter [`StableRowKey`] or one of the seven IR row
/// families. `subject` is the embedding producer's stable key for the output
/// associated with a semantic dependency.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EmbeddingRowKey {
    plane: EmbeddingPlaneIdentity,
    subject: [u8; 32],
}

impl EmbeddingRowKey {
    /// Names one row in the exact independent embedding recipe.
    #[must_use]
    pub const fn new(plane: EmbeddingPlaneIdentity, subject: [u8; 32]) -> Self {
        Self { plane, subject }
    }

    /// Returns the output plane's complete model and recipe identity.
    #[must_use]
    pub const fn plane(self) -> EmbeddingPlaneIdentity {
        self.plane
    }

    /// Returns the producer-owned stable embedding row key.
    #[must_use]
    pub const fn subject(self) -> [u8; 32] {
        self.subject
    }
}

/// Stable key in either the seven-family IR plane or a separate embedding
/// plane.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SemanticDependencyKey {
    /// A normalized semantic IR row.
    Ir(StableRowKey),
    /// A row emitted by one exact embedding recipe.
    Embedding(EmbeddingRowKey),
}

/// A typed dependency edge whose source mutation invalidates its target.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SemanticDependencyEdge {
    channel: DependencyChannel,
    cause: SemanticDependencyKey,
    dependent: SemanticDependencyKey,
}

impl SemanticDependencyEdge {
    /// Creates an edge only when its row families match the typed channel.
    pub fn new(
        channel: DependencyChannel,
        cause: SemanticDependencyKey,
        dependent: SemanticDependencyKey,
    ) -> Result<Self, SemanticDependencyEdgeError> {
        if !channel.accepts(cause, dependent) {
            return Err(SemanticDependencyEdgeError::KeyChannelMismatch { channel });
        }
        Ok(Self {
            channel,
            cause,
            dependent,
        })
    }

    /// Returns the edge's invalidation channel.
    #[must_use]
    pub const fn channel(self) -> DependencyChannel {
        self.channel
    }

    /// Returns the key whose change triggers this edge.
    #[must_use]
    pub const fn cause(self) -> SemanticDependencyKey {
        self.cause
    }

    /// Returns the dependent key to include in the frontier.
    #[must_use]
    pub const fn dependent(self) -> SemanticDependencyKey {
        self.dependent
    }
}

/// Typed compiler dependency channels supported by the owner.
///
/// This is intentionally a closed initial set. A compiler observation may
/// mark any subset as observed; every omitted channel remains unproven.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum DependencyChannel {
    /// Reachable type edits invalidate rows that consume those stable types.
    TypeConsumer = 0,
    /// Relation edits invalidate their occurrence-site rows.
    RelationOccurrence = 1,
    /// Documentation edits invalidate their separately stored embedding row.
    DocumentationEmbedding = 2,
    /// Source-site edits invalidate their separately stored embedding row.
    SourceEmbedding = 3,
    /// Language-extension edits invalidate their separately stored embedding row.
    ExtensionEmbedding = 4,
}

impl DependencyChannel {
    /// All currently modeled dependency channels.
    pub const ALL: [Self; 5] = [
        Self::TypeConsumer,
        Self::RelationOccurrence,
        Self::DocumentationEmbedding,
        Self::SourceEmbedding,
        Self::ExtensionEmbedding,
    ];

    /// Stable bit in a channel-observation mask.
    #[must_use]
    pub const fn bit(self) -> u8 {
        1 << (self as u8)
    }

    const fn accepts(self, cause: SemanticDependencyKey, dependent: SemanticDependencyKey) -> bool {
        match (self, cause, dependent) {
            (
                Self::TypeConsumer,
                SemanticDependencyKey::Ir(source),
                SemanticDependencyKey::Ir(target),
            ) => {
                matches!(source.family(), RowFamily::Types)
                    && !matches!(target.family(), RowFamily::Types)
            }
            (
                Self::RelationOccurrence,
                SemanticDependencyKey::Ir(source),
                SemanticDependencyKey::Ir(target),
            ) => {
                matches!(source.family(), RowFamily::Relations)
                    && matches!(target.family(), RowFamily::Occurrences)
            }
            (
                Self::DocumentationEmbedding,
                SemanticDependencyKey::Ir(source),
                SemanticDependencyKey::Embedding(_),
            ) => matches!(source.family(), RowFamily::Documentation),
            (
                Self::SourceEmbedding,
                SemanticDependencyKey::Ir(source),
                SemanticDependencyKey::Embedding(_),
            ) => matches!(source.family(), RowFamily::SourceProvenance),
            (
                Self::ExtensionEmbedding,
                SemanticDependencyKey::Ir(source),
                SemanticDependencyKey::Embedding(_),
            ) => matches!(source.family(), RowFamily::LanguageExtensions),
            _ => false,
        }
    }
}

/// Invalid typed compiler dependency event.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SemanticDependencyEdgeError {
    /// The source and target families do not match the named channel.
    #[error("dependency keys do not match channel {channel:?}")]
    KeyChannelMismatch { channel: DependencyChannel },
}

/// Observed canonical row contributed by one compiler source unit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct UnitRow {
    key: StableRowKey,
    payload: RowPayload,
}

/// One borrowed-reader observation that can replace a stable source unit.
///
/// Each observed family is an exact local canonical encoding. Families
/// omitted by the image authority (currently language extensions for a
/// shared-profile image) remain unobserved; they are never represented by an
/// empty-but-complete lane.
#[derive(Clone, Debug)]
pub struct SemanticUnitObservation {
    unit: SourceUnitKey,
    authority_stamp: [u8; 32],
    rows: [Box<[UnitRow]>; 7],
    observed_families: u8,
    dependencies: Box<[SemanticDependencyEdge]>,
    observed_dependency_channels: u8,
    capture_work: SemanticCaptureWork,
}

impl SemanticUnitObservation {
    /// Encodes the existing seven-family record grammar from one source-unit
    /// reader. The extension family is unobserved unless the reader carries a
    /// concrete language profile. Jumbo or malformed-family encoding errors
    /// abort the observation without mutating an owner.
    pub fn from_reader<Reader: SemanticReader + ?Sized>(
        reader: &Reader,
        maximum_segment_bytes: usize,
    ) -> Result<Self, SemanticUnitCaptureError> {
        let unit = SourceUnitKey::from_reader(reader)?;
        let authority_stamp = authority_stamp(reader)?;
        let input = SemanticInputWitness::claimed_state(
            [0; 32],
            ScopeRoot::from_bytes([0; 32]),
            Coverage::Unsupported,
        );
        let mut rows = core::array::from_fn(|_| Box::<[UnitRow]>::default());
        let mut observed_families = 0_u8;
        let mut capture_work = SemanticCaptureWork::default();

        capture_family(
            reader,
            &CoreDeclarationRows,
            RowFamily::Core,
            input,
            maximum_segment_bytes,
            &mut rows,
            &mut observed_families,
            &mut capture_work,
        )?;
        capture_family(
            reader,
            &TypesRows,
            RowFamily::Types,
            input,
            maximum_segment_bytes,
            &mut rows,
            &mut observed_families,
            &mut capture_work,
        )?;
        capture_family(
            reader,
            &RelationRows,
            RowFamily::Relations,
            input,
            maximum_segment_bytes,
            &mut rows,
            &mut observed_families,
            &mut capture_work,
        )?;
        capture_family(
            reader,
            &OccurrenceRows,
            RowFamily::Occurrences,
            input,
            maximum_segment_bytes,
            &mut rows,
            &mut observed_families,
            &mut capture_work,
        )?;
        capture_family(
            reader,
            &DocumentationRows,
            RowFamily::Documentation,
            input,
            maximum_segment_bytes,
            &mut rows,
            &mut observed_families,
            &mut capture_work,
        )?;
        capture_family(
            reader,
            &SourceProvenanceRows,
            RowFamily::SourceProvenance,
            input,
            maximum_segment_bytes,
            &mut rows,
            &mut observed_families,
            &mut capture_work,
        )?;
        if let SemanticImageAuthority::Language(profile) = reader.image_facts().authority {
            capture_family(
                reader,
                &LanguageExtensionRows::new(profile),
                RowFamily::LanguageExtensions,
                input,
                maximum_segment_bytes,
                &mut rows,
                &mut observed_families,
                &mut capture_work,
            )?;
        }

        Ok(Self {
            unit,
            authority_stamp,
            rows,
            observed_families,
            dependencies: Box::default(),
            observed_dependency_channels: 0,
            capture_work,
        })
    }

    /// Attaches explicit compiler dependency events for the listed channels.
    /// An observed channel with no edge means this unit emitted no dependency
    /// edges of that kind. Channels omitted from the mask stay unproven.
    ///
    /// The caller must construct these edges from the same source-unit
    /// compiler revision as this observation. This owner checks channel and
    /// dependent-row ownership, but does not rediscover references by parsing
    /// canonical payload bytes.
    pub fn with_dependency_events(
        mut self,
        observed_channels: &[DependencyChannel],
        mut edges: Vec<SemanticDependencyEdge>,
    ) -> Result<Self, SemanticUnitObservationError> {
        let mut mask = 0_u8;
        for channel in observed_channels {
            mask |= channel.bit();
        }
        edges.sort_unstable();
        for (position, edge) in edges.iter().enumerate() {
            if mask & edge.channel.bit() == 0 {
                return Err(SemanticUnitObservationError::UnobservedDependencyEdge {
                    channel: edge.channel,
                });
            }
            if position > 0 && edges[position - 1] == *edge {
                return Err(SemanticUnitObservationError::DuplicateDependencyEdge);
            }
            if let SemanticDependencyKey::Ir(dependent) = edge.dependent {
                let family = dependent.family();
                let lane = &self.rows[family_index(family)];
                if self.observed_families & family_bit(family) == 0
                    || lane
                        .binary_search_by_key(&dependent, |row| row.key)
                        .is_err()
                {
                    return Err(SemanticUnitObservationError::DependentRowNotOwned { family });
                }
            }
        }
        self.dependencies = edges.into_boxed_slice();
        self.observed_dependency_channels = mask;
        Ok(self)
    }

    /// Returns the stable source-unit key.
    #[must_use]
    pub const fn unit(&self) -> SourceUnitKey {
        self.unit
    }

    /// Returns a bit mask of families observed by the canonical encoders.
    /// Bits use the closed order [`RowFamily::ALL`].
    #[must_use]
    pub const fn observed_family_mask(&self) -> u8 {
        self.observed_families
    }

    /// Measurements from the canonical row encodings used to form this
    /// source-unit event.
    #[must_use]
    pub const fn capture_work(&self) -> SemanticCaptureWork {
        self.capture_work
    }
}

fn capture_family<Reader, Encoder>(
    reader: &Reader,
    encoder: &Encoder,
    family: RowFamily,
    input: SemanticInputWitness,
    maximum_segment_bytes: usize,
    rows: &mut [Box<[UnitRow]>; 7],
    observed_families: &mut u8,
    capture_work: &mut SemanticCaptureWork,
) -> Result<(), SemanticUnitCaptureError>
where
    Reader: SemanticReader + ?Sized,
    Encoder: crate::ir::CanonicalPlaneRowEncoder + ?Sized,
{
    let measured =
        encode_canonical_plane_family_measured(reader, encoder, input, maximum_segment_bytes)?;
    let encoding_metrics = measured.metrics();
    capture_work.rows_encoded = capture_work
        .rows_encoded
        .saturating_add(encoding_metrics.row_count());
    capture_work.row_key_index_capacity_bytes = capture_work
        .row_key_index_capacity_bytes
        .saturating_add(encoding_metrics.row_index_capacity_bytes());
    capture_work.peak_encoder_scratch_capacity_bytes = core::cmp::max(
        capture_work.peak_encoder_scratch_capacity_bytes,
        encoding_metrics.peak_tracked_scratch_upper_bound_bytes(),
    );
    let segments = measured.segments();
    let expected = match encoder.kind() {
        SemanticPlaneKind::Ir(plane) => plane,
        SemanticPlaneKind::Embeddings(_) => {
            return Err(SemanticUnitCaptureError::UnexpectedEmbeddingFamily);
        }
    };
    let mut family_rows = Vec::new();
    for segment in segments.iter() {
        if segment.kind() != SemanticPlaneKind::Ir(expected) {
            return Err(SemanticUnitCaptureError::FamilyKindMismatch { family });
        }
        let descriptor = segment.metadata()?;
        let view = decode_semantic_plane_segment(segment.kind(), &descriptor, segment.bytes())?;
        for row in view.records() {
            capture_work.rows_decoded = capture_work.rows_decoded.saturating_add(1);
            capture_work.bytes_hashed = capture_work.bytes_hashed.saturating_add(
                u64::try_from(row.payload().len().saturating_add(1)).unwrap_or(u64::MAX),
            );
            family_rows.push(indexed_row(family, row)?);
        }
    }
    validate_unit_rows(family, &family_rows)?;
    rows[family_index(family)] = family_rows.into_boxed_slice();
    *observed_families |= family_bit(family);
    Ok(())
}

fn authority_stamp<Reader: SemanticCoreReader + ?Sized>(
    reader: &Reader,
) -> Result<[u8; 32], SemanticUnitCaptureError> {
    let ImageProvenance::Captured {
        source: SourceIdentity { identity, byte_len },
        recipe,
        ..
    } = reader.image_facts().provenance
    else {
        return Err(SemanticUnitCaptureError::MissingCapturedProvenance);
    };
    let recipe: CompileRecipeFact = recipe;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.semantic.ir.source-unit-authority.v1\0");
    hasher.update(identity.as_ref());
    hasher.update(&byte_len.to_be_bytes());
    hasher.update(recipe.identity.as_ref());
    Ok(*hasher.finalize().as_bytes())
}

fn indexed_row(
    family: RowFamily,
    record: CanonicalSemanticPlaneRecordView<'_>,
) -> Result<UnitRow, SemanticUnitCaptureError> {
    let payload = RowPayload::from_tagged_bytes(record.tag(), record.payload())?;
    Ok(UnitRow {
        key: StableRowKey::new(family, record.key()),
        payload,
    })
}

fn validate_unit_rows(family: RowFamily, rows: &[UnitRow]) -> Result<(), SemanticUnitCaptureError> {
    for pair in rows.windows(2) {
        if pair[0].key >= pair[1].key {
            return Err(SemanticUnitCaptureError::UnsortedOrDuplicateRows { family });
        }
    }
    Ok(())
}

/// One replace or delete event from a compiler owner.
#[derive(Clone, Debug)]
pub enum SemanticUnitEvent {
    /// Replaces the named unit with its newly captured canonical rows.
    Replace {
        /// Monotonic compiler/session revision for this unit.
        revision: u64,
        /// Exact-family observation produced by `from_reader`.
        observation: SemanticUnitObservation,
    },
    /// Removes the named source unit. Its retained row slabs are deleted.
    Delete {
        /// Stable unit whose compiler input was removed.
        unit: SourceUnitKey,
        /// Monotonic compiler/session revision for this unit.
        revision: u64,
    },
}

impl SemanticUnitEvent {
    fn unit(&self) -> SourceUnitKey {
        match self {
            Self::Replace { observation, .. } => observation.unit,
            Self::Delete { unit, .. } => *unit,
        }
    }

    fn revision(&self) -> u64 {
        match self {
            Self::Replace { revision, .. } | Self::Delete { revision, .. } => *revision,
        }
    }
}

/// Direct row mutation or a typed dependency invalidation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrontierEntry {
    key: SemanticDependencyKey,
    cause: FrontierCause,
    owner: SourceUnitKey,
}

impl FrontierEntry {
    /// Returns this entry's row key and plane.
    #[must_use]
    pub const fn key(self) -> SemanticDependencyKey {
        self.key
    }

    /// Returns whether the key changed in a source unit or was invalidated
    /// through an observed dependency edge.
    #[must_use]
    pub const fn cause(self) -> FrontierCause {
        self.cause
    }

    /// Source-unit owner whose changed contribution or dependent slab should
    /// be revisited.
    #[must_use]
    pub const fn owner(self) -> SourceUnitKey {
        self.owner
    }
}

/// Why one key appears in a semantic frontier.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum FrontierCause {
    /// The row's unit-local stable key or tagged payload changed.
    DirectUnitMutation,
    /// An observed typed dependency edge invalidated the row or embedding.
    DependencyInvalidation,
}

/// Scope of exactness for one edit result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrontierScope {
    /// Exact within registered source-unit contributions and observed
    /// dependency events. No workspace-wide completeness is asserted.
    RegisteredUnitContributions,
}

/// Exact candidate keys emitted by one source-unit edit batch.
#[derive(Clone, Debug)]
pub struct SemanticChangeFrontier {
    entries: Box<[FrontierEntry]>,
    replay_units: Box<[SourceUnitKey]>,
    authority_generation_changes: Box<[SourceUnitKey]>,
    scope: FrontierScope,
    unproven_families: u8,
    unproven_dependency_channels: u8,
    unproven_workspace_scope: bool,
    unproven_embedding_plane: bool,
    metrics: SemanticEditWork,
}

impl SemanticChangeFrontier {
    /// Sorted direct and dependent candidates. Embedding candidates remain
    /// tagged as `SemanticDependencyKey::Embedding` and never enter the IR
    /// family key space.
    #[must_use]
    pub fn entries(&self) -> &[FrontierEntry] {
        &self.entries
    }

    /// Sorted unique source units whose retained semantic slabs changed or
    /// whose rows were invalidated through an observed dependency edge.
    #[must_use]
    pub fn replay_units(&self) -> &[SourceUnitKey] {
        &self.replay_units
    }

    /// Sorted units whose source or compile-recipe authority changed even if
    /// all seven observed row payloads remained byte-identical.
    #[must_use]
    pub fn authority_generation_changes(&self) -> &[SourceUnitKey] {
        &self.authority_generation_changes
    }

    /// Exactness scope carried by this candidate result.
    #[must_use]
    pub const fn scope(&self) -> FrontierScope {
        self.scope
    }

    /// Families for which at least one affected registered unit lacks an
    /// exact current observation. Bits follow [`RowFamily::ALL`].
    #[must_use]
    pub const fn unproven_families(&self) -> u8 {
        self.unproven_families
    }

    /// Dependency channels for which at least one registered active unit
    /// lacks an exact current compiler event, plus channels omitted by this
    /// batch. Bits follow [`DependencyChannel::ALL`].
    #[must_use]
    pub const fn unproven_dependency_channels(&self) -> u8 {
        self.unproven_dependency_channels
    }

    /// Whether units outside this owner were not enumerated by this event
    /// stream. This is always true for this source-only foundation.
    #[must_use]
    pub const fn has_unproven_workspace_scope(&self) -> bool {
        self.unproven_workspace_scope
    }

    /// Whether an independent embedding producer's current rows were not
    /// observed. Invalidation candidates may still be present.
    #[must_use]
    pub const fn has_unproven_embedding_plane(&self) -> bool {
        self.unproven_embedding_plane
    }

    /// Honest row, dependency, and owned-slab work for this edit batch.
    #[must_use]
    pub const fn metrics(&self) -> SemanticEditWork {
        self.metrics
    }
}

/// Work counters for unit-local edits and indexed dependency closure.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SemanticEditWork {
    /// Number of events received before same-unit coalescing.
    pub events_received: u64,
    /// Number of lower-revision or coalesced events not applied.
    pub events_skipped: u64,
    /// Number of source units actually replaced or deleted.
    pub units_changed: u64,
    /// Canonical records decoded while building the source-unit observations.
    pub rows_decoded: u64,
    /// Canonical row records whose stable key and tagged payload were equal
    /// before and after a touched unit replacement.
    pub rows_reused: u64,
    /// Directly changed or dependency-invalidated keys emitted by the batch.
    pub rows_invalidated: u64,
    /// Canonical tag-plus-payload bytes hashed into the owner's row payload
    /// identities while observations were captured.
    pub bytes_hashed: u64,
    /// Canonical rows encoded by the existing family encoders for the input
    /// observations. This does not count every reference visited while plans
    /// are built.
    pub rows_encoded: u64,
    /// Encoder-reported full-family key-index capacity used while capturing
    /// observations.
    pub row_key_index_capacity_bytes: u64,
    /// Maximum encoder-reported bounded row/segment scratch for one family.
    pub peak_encoder_scratch_capacity_bytes: u64,
    /// Unit-row slots present in every observation passed to this batch before
    /// same-unit event coalescing.
    pub input_observation_row_slots: u64,
    /// Exact `UnitRow` byte footprint of those boxed observation slices.
    pub input_observation_row_slab_bytes: u64,
    /// Number of row-pair comparisons in touched unit slabs.
    pub row_comparisons: u64,
    /// Number of source-unit row contributions inserted, deleted, or replaced.
    pub direct_row_changes: u64,
    /// Current or previously observed dependency edges visited by closure.
    pub dependency_edges_visited: u64,
    /// Number of edges added to or removed from an observed source-unit
    /// dependency set. Each changed edge invalidates its dependent key.
    pub dependency_edges_changed: u64,
    /// Upper bound on retained owner rows plus all incoming boxed observations
    /// and one in-flight replacement row copy, in `UnitRow` elements. This
    /// excludes B-tree metadata and temporary key entries.
    pub peak_temporary_row_slots: u64,
    /// Corresponding row-slab byte upper bound. It includes retained owner
    /// capacity, all incoming boxed observations, and one in-flight row copy;
    /// it excludes allocator headers and B-tree metadata.
    pub peak_unit_row_slab_bytes: u64,
    /// Current retained `UnitRow` element slots owned by registered units.
    pub retained_row_slots: u64,
    /// Current allocated capacity bytes of retained row slabs. This excludes
    /// B-tree nodes and edge-vector allocations and is not a total heap claim.
    pub retained_row_slab_capacity_bytes: u64,
    /// Peak owned candidate, work-queue, and unit-merge vector capacity bytes
    /// during this batch. It excludes B-tree nodes and allocator headers.
    pub peak_tracked_scratch_capacity_bytes: u64,
}

#[derive(Debug)]
struct StoredUnit {
    revision: u64,
    authority_stamp: Option<[u8; 32]>,
    active: bool,
    rows: [Vec<UnitRow>; 7],
    observed_families: u8,
    observed_dependency_channels: u8,
    dependencies: Vec<SemanticDependencyEdge>,
}

impl StoredUnit {
    fn tombstone(revision: u64) -> Self {
        Self {
            revision,
            authority_stamp: None,
            active: false,
            rows: core::array::from_fn(|_| Vec::new()),
            observed_families: 0,
            observed_dependency_channels: 0,
            dependencies: Vec::new(),
        }
    }
}

/// Measurements from producing one source-unit observation through existing
/// canonical encoders.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SemanticCaptureWork {
    /// Canonical records emitted by family encoders.
    pub rows_encoded: u64,
    /// Canonical records borrowed from encoded segments and indexed.
    pub rows_decoded: u64,
    /// Tag and payload bytes passed to the row-payload identity hasher.
    pub bytes_hashed: u64,
    /// Full-family key-index capacity reported by existing encoders.
    pub row_key_index_capacity_bytes: u64,
    /// Maximum encoder-reported scratch capacity for one family.
    pub peak_encoder_scratch_capacity_bytes: u64,
}

fn add_capture_work(target: &mut SemanticCaptureWork, source: SemanticCaptureWork) {
    target.rows_encoded = target.rows_encoded.saturating_add(source.rows_encoded);
    target.rows_decoded = target.rows_decoded.saturating_add(source.rows_decoded);
    target.bytes_hashed = target.bytes_hashed.saturating_add(source.bytes_hashed);
    target.row_key_index_capacity_bytes = target
        .row_key_index_capacity_bytes
        .saturating_add(source.row_key_index_capacity_bytes);
    target.peak_encoder_scratch_capacity_bytes = core::cmp::max(
        target.peak_encoder_scratch_capacity_bytes,
        source.peak_encoder_scratch_capacity_bytes,
    );
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct IndexedDependency {
    owner: SourceUnitKey,
    edge: SemanticDependencyEdge,
}

/// Persistent in-memory owner for source-unit semantic row contributions.
///
/// Its sorted family slabs survive between edit batches. The edit path touches
/// only the replaced unit's old/new rows, dependency events for that unit, and
/// the indexed closure reached from its changed keys. It does not own the
/// frontend compiler session or claim that every package input was observed.
#[derive(Debug, Default)]
pub struct SemanticEditOwner {
    units: BTreeMap<SourceUnitKey, StoredUnit>,
    reverse_dependencies: BTreeMap<SemanticDependencyKey, Vec<IndexedDependency>>,
    unobserved_family_units: [u64; 7],
    unobserved_dependency_units: [u64; 5],
    retained_row_slots: u64,
    retained_row_slab_capacity_bytes: u64,
}

impl SemanticEditOwner {
    /// Creates an empty source-unit owner. Empty means no source events have
    /// arrived; it does not prove an empty workspace.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Applies a source event batch in stable unit/revision order.
    ///
    /// Only the highest revision per unit in this batch is applied. Lower
    /// revisions that arrive in a later batch are ignored using the retained
    /// per-unit watermark. Same-unit, same-revision duplicate events are
    /// rejected before state mutation because their order has no authority.
    /// Each exact candidate is relative to this owner's retained unit slabs;
    /// callers must submit a `Delete` for removed units and both delete and
    /// replace events for a path rename. Missing workspace units remain
    /// unproven.
    pub fn apply_events(
        &mut self,
        mut events: Vec<SemanticUnitEvent>,
    ) -> Result<SemanticChangeFrontier, SemanticEditOwnerError> {
        let received = events.len();
        let mut capture_work = SemanticCaptureWork::default();
        let mut input_observation_row_slots = 0_u64;
        for event in &events {
            if let SemanticUnitEvent::Replace { observation, .. } = event {
                add_capture_work(&mut capture_work, observation.capture_work);
                let rows = observation
                    .rows
                    .iter()
                    .map(|family| family.len())
                    .sum::<usize>();
                input_observation_row_slots = input_observation_row_slots
                    .saturating_add(u64::try_from(rows).unwrap_or(u64::MAX));
            }
        }
        events.sort_unstable_by_key(|event| (event.unit(), event.revision()));
        let mut newest = Vec::with_capacity(events.len());
        let mut cursor = events.into_iter().peekable();
        while let Some(first) = cursor.next() {
            let unit = first.unit();
            let mut winner = first;
            while cursor.peek().is_some_and(|event| event.unit() == unit) {
                let event = cursor.next().expect("peeked event is present");
                if event.revision() == winner.revision() {
                    return Err(SemanticEditOwnerError::DuplicateUnitRevision {
                        unit,
                        revision: winner.revision(),
                    });
                }
                winner = event;
            }
            newest.push(winner);
        }

        let mut work = SemanticEditWork {
            events_received: u64::try_from(received).unwrap_or(u64::MAX),
            events_skipped: u64::try_from(received.saturating_sub(newest.len()))
                .unwrap_or(u64::MAX),
            rows_encoded: capture_work.rows_encoded,
            rows_decoded: capture_work.rows_decoded,
            bytes_hashed: capture_work.bytes_hashed,
            row_key_index_capacity_bytes: capture_work.row_key_index_capacity_bytes,
            peak_encoder_scratch_capacity_bytes: capture_work.peak_encoder_scratch_capacity_bytes,
            input_observation_row_slots,
            input_observation_row_slab_bytes: input_observation_row_slots
                .saturating_mul(u64::try_from(size_of::<UnitRow>()).unwrap_or(u64::MAX)),
            ..SemanticEditWork::default()
        };
        work.peak_temporary_row_slots = self
            .retained_row_slots
            .saturating_add(work.input_observation_row_slots);
        work.peak_unit_row_slab_bytes = self
            .retained_row_slab_capacity_bytes
            .saturating_add(work.input_observation_row_slab_bytes);
        let mut direct = Vec::new();
        let mut replay_units = Vec::new();
        let mut authority_generation_changes = Vec::new();
        let mut removed_dependency_edges = Vec::new();
        let mut event_unproven_families = 0_u8;
        let mut event_unproven_channels = 0_u8;

        for event in newest {
            let unit = event.unit();
            let revision = event.revision();
            if self
                .units
                .get(&unit)
                .is_some_and(|stored| revision <= stored.revision)
            {
                work.events_skipped = work.events_skipped.saturating_add(1);
                continue;
            }

            match event {
                SemanticUnitEvent::Replace { observation, .. } => {
                    self.apply_replace(
                        unit,
                        revision,
                        observation,
                        &mut direct,
                        &mut replay_units,
                        &mut authority_generation_changes,
                        &mut removed_dependency_edges,
                        &mut event_unproven_families,
                        &mut event_unproven_channels,
                        &mut work,
                    )?;
                }
                SemanticUnitEvent::Delete { .. } => {
                    self.apply_delete(
                        unit,
                        revision,
                        &mut direct,
                        &mut replay_units,
                        &mut removed_dependency_edges,
                        &mut event_unproven_families,
                        &mut event_unproven_channels,
                        &mut work,
                    )?;
                }
            }
            work.units_changed = work.units_changed.saturating_add(1);
        }

        direct.sort_unstable_by_key(|entry| (entry.key, entry.owner));
        direct.dedup_by_key(|entry| entry.key);
        let mut visited: Vec<SemanticDependencyKey> =
            direct.iter().map(|entry| entry.key).collect();
        visited.sort_unstable();
        visited.dedup();
        let mut entries = direct;
        let mut queue = visited.clone();
        let mut removed_reverse_dependencies =
            BTreeMap::<SemanticDependencyKey, Vec<IndexedDependency>>::new();
        for indexed in removed_dependency_edges {
            removed_reverse_dependencies
                .entry(indexed.edge.cause)
                .or_default()
                .push(indexed);
        }
        for edges in removed_reverse_dependencies.values_mut() {
            edges.sort_unstable();
            edges.dedup();
        }
        while !queue.is_empty() {
            queue.sort_unstable();
            queue.dedup();
            let mut next = Vec::new();
            for cause in queue.drain(..) {
                if let Some(edges) = self.reverse_dependencies.get(&cause) {
                    for indexed in edges {
                        let Some(owner) = self.units.get(&indexed.owner) else {
                            continue;
                        };
                        if !owner.active
                            || owner.observed_dependency_channels & indexed.edge.channel.bit() == 0
                        {
                            continue;
                        }
                        visit_dependency(
                            *indexed,
                            &mut visited,
                            &mut next,
                            &mut entries,
                            &mut replay_units,
                            &mut work,
                        );
                    }
                }
                if let Some(edges) = removed_reverse_dependencies.get(&cause) {
                    for indexed in edges {
                        visit_dependency(
                            *indexed,
                            &mut visited,
                            &mut next,
                            &mut entries,
                            &mut replay_units,
                            &mut work,
                        );
                    }
                }
            }
            next.sort_unstable();
            next.dedup();
            let next_capacity_bytes = capacity_bytes(&next);
            let mut fresh = Vec::with_capacity(next.len());
            for key in next {
                if visited.binary_search(&key).is_err() {
                    fresh.push(key);
                }
            }
            visited.extend(fresh.iter().copied());
            visited.sort_unstable();
            work.peak_tracked_scratch_capacity_bytes =
                work.peak_tracked_scratch_capacity_bytes.max(
                    next_capacity_bytes
                        .saturating_add(capacity_bytes(&fresh))
                        .saturating_add(capacity_bytes(&queue))
                        .saturating_add(capacity_bytes(&visited))
                        .saturating_add(capacity_bytes(&entries)),
                );
            queue = fresh;
        }
        entries.sort_unstable_by_key(|entry| (entry.key, entry.cause, entry.owner));
        entries.dedup_by_key(|entry| entry.key);
        replay_units.sort_unstable();
        replay_units.dedup();
        authority_generation_changes.sort_unstable();
        authority_generation_changes.dedup();
        work.rows_invalidated = u64::try_from(entries.len()).unwrap_or(u64::MAX);
        work.peak_tracked_scratch_capacity_bytes = work.peak_tracked_scratch_capacity_bytes.max(
            capacity_bytes(&entries)
                .saturating_add(capacity_bytes(&visited))
                .saturating_add(capacity_bytes(&queue))
                .saturating_add(capacity_bytes(&replay_units))
                .saturating_add(capacity_bytes(&authority_generation_changes))
                .saturating_add(
                    removed_reverse_dependencies
                        .values()
                        .fold(0_u64, |total, edges| {
                            total.saturating_add(capacity_bytes(edges))
                        }),
                ),
        );

        work.retained_row_slots = self.retained_row_slots;
        work.retained_row_slab_capacity_bytes = self.retained_row_slab_capacity_bytes;

        let unproven_families = event_unproven_families
            | self
                .unobserved_family_units
                .iter()
                .enumerate()
                .fold(0_u8, |mask, (index, count)| {
                    if *count > 0 {
                        mask | (1 << index)
                    } else {
                        mask
                    }
                });
        let unproven_dependency_channels = event_unproven_channels
            | self.unobserved_dependency_units.iter().enumerate().fold(
                0_u8,
                |mask, (index, count)| {
                    if *count > 0 {
                        mask | (1 << index)
                    } else {
                        mask
                    }
                },
            );

        Ok(SemanticChangeFrontier {
            entries: entries.into_boxed_slice(),
            replay_units: replay_units.into_boxed_slice(),
            authority_generation_changes: authority_generation_changes.into_boxed_slice(),
            scope: FrontierScope::RegisteredUnitContributions,
            unproven_families,
            unproven_dependency_channels,
            unproven_workspace_scope: true,
            unproven_embedding_plane: true,
            metrics: work,
        })
    }

    fn apply_replace(
        &mut self,
        unit: SourceUnitKey,
        revision: u64,
        observation: SemanticUnitObservation,
        direct: &mut Vec<FrontierEntry>,
        replay_units: &mut Vec<SourceUnitKey>,
        authority_generation_changes: &mut Vec<SourceUnitKey>,
        removed_dependency_edges: &mut Vec<IndexedDependency>,
        event_unproven_families: &mut u8,
        event_unproven_channels: &mut u8,
        work: &mut SemanticEditWork,
    ) -> Result<(), SemanticEditOwnerError> {
        let was_active = self.units.get(&unit).is_some_and(|stored| stored.active);
        let mut stored = self
            .units
            .remove(&unit)
            .unwrap_or_else(|| StoredUnit::tombstone(0));
        let previous_dependencies = stored.dependencies.clone();
        if stored
            .authority_stamp
            .is_some_and(|previous| previous != observation.authority_stamp)
        {
            authority_generation_changes.push(unit);
            replay_units.push(unit);
        }
        if was_active {
            self.change_unknown_counts(
                stored.observed_families,
                stored.observed_dependency_channels,
                false,
            );
        }

        for family in RowFamily::ALL {
            let family_index = family_index(family);
            let bit = family_bit(family);
            if observation.observed_families & bit == 0 {
                stored.observed_families &= !bit;
                *event_unproven_families |= bit;
                continue;
            }
            let new_rows = observation.rows[family_index].as_ref();
            let old_rows = if was_active {
                stored.rows[family_index].as_slice()
            } else {
                &[]
            };
            let diff = diff_unit_rows(family, old_rows, new_rows, work)?;
            work.peak_tracked_scratch_capacity_bytes = work
                .peak_tracked_scratch_capacity_bytes
                .max(capacity_bytes(&diff).saturating_add(capacity_bytes(direct)));
            let changed_family = !diff.is_empty();
            direct.extend(diff.into_iter().map(|key| FrontierEntry {
                key,
                cause: FrontierCause::DirectUnitMutation,
                owner: unit,
            }));
            if changed_family {
                replay_units.push(unit);
            }
            let old_capacity = stored.rows[family_index]
                .capacity()
                .saturating_mul(size_of::<UnitRow>());
            let old_len = stored.rows[family_index].len();
            let in_flight_copy_bytes = new_rows.len().saturating_mul(size_of::<UnitRow>());
            work.peak_temporary_row_slots = work.peak_temporary_row_slots.max(
                self.retained_row_slots
                    .saturating_add(work.input_observation_row_slots)
                    .saturating_add(u64::try_from(new_rows.len()).unwrap_or(u64::MAX)),
            );
            work.peak_unit_row_slab_bytes = work.peak_unit_row_slab_bytes.max(
                self.retained_row_slab_capacity_bytes
                    .saturating_add(work.input_observation_row_slab_bytes)
                    .saturating_add(u64::try_from(in_flight_copy_bytes).unwrap_or(u64::MAX)),
            );
            self.retained_row_slots = self
                .retained_row_slots
                .saturating_sub(u64::try_from(old_len).unwrap_or(u64::MAX));
            self.retained_row_slab_capacity_bytes = self
                .retained_row_slab_capacity_bytes
                .saturating_sub(u64::try_from(old_capacity).unwrap_or(u64::MAX));
            stored.rows[family_index] = new_rows.to_vec();
            self.retained_row_slots = self
                .retained_row_slots
                .saturating_add(u64::try_from(stored.rows[family_index].len()).unwrap_or(u64::MAX));
            self.retained_row_slab_capacity_bytes =
                self.retained_row_slab_capacity_bytes.saturating_add(
                    u64::try_from(
                        stored.rows[family_index]
                            .capacity()
                            .saturating_mul(size_of::<UnitRow>()),
                    )
                    .unwrap_or(u64::MAX),
                );
            work.peak_temporary_row_slots = work.peak_temporary_row_slots.max(
                self.retained_row_slots
                    .saturating_add(work.input_observation_row_slots),
            );
            work.peak_unit_row_slab_bytes = work.peak_unit_row_slab_bytes.max(
                self.retained_row_slab_capacity_bytes
                    .saturating_add(work.input_observation_row_slab_bytes),
            );
            stored.observed_families |= bit;
        }
        for family in RowFamily::ALL {
            if observation.observed_families & family_bit(family) == 0 {
                *event_unproven_families |= family_bit(family);
            }
        }

        let observed_channels = observation.observed_dependency_channels;
        for channel in DependencyChannel::ALL {
            let bit = channel.bit();
            if observed_channels & bit != 0 {
                let old_edges: Vec<_> = previous_dependencies
                    .iter()
                    .copied()
                    .filter(|edge| edge.channel.bit() & bit != 0)
                    .collect();
                let new_edges: Vec<_> = observation
                    .dependencies
                    .iter()
                    .copied()
                    .filter(|edge| edge.channel.bit() & bit != 0)
                    .collect();
                let (removed, added) = dependency_edge_changes(&old_edges, &new_edges);
                work.peak_tracked_scratch_capacity_bytes =
                    work.peak_tracked_scratch_capacity_bytes.max(
                        capacity_bytes(&old_edges)
                            .saturating_add(capacity_bytes(&new_edges))
                            .saturating_add(capacity_bytes(&removed))
                            .saturating_add(capacity_bytes(&added)),
                    );
                for edge in removed.iter().chain(&added).copied() {
                    direct.push(FrontierEntry {
                        key: edge.dependent,
                        cause: FrontierCause::DependencyInvalidation,
                        owner: unit,
                    });
                    replay_units.push(unit);
                    work.dependency_edges_changed = work.dependency_edges_changed.saturating_add(1);
                }
                removed_dependency_edges.extend(
                    removed
                        .into_iter()
                        .map(|edge| IndexedDependency { owner: unit, edge }),
                );
                self.remove_unit_edges(unit, bit, &stored.dependencies);
                stored
                    .dependencies
                    .retain(|edge| edge.channel.bit() & bit == 0);
                for edge in observation.dependencies.iter().copied() {
                    if edge.channel.bit() & bit != 0 {
                        self.insert_unit_edge(unit, edge)?;
                        stored.dependencies.push(edge);
                    }
                }
                stored.observed_dependency_channels |= bit;
            } else {
                removed_dependency_edges.extend(
                    stored
                        .dependencies
                        .iter()
                        .copied()
                        .filter(|edge| edge.channel.bit() & bit != 0)
                        .map(|edge| IndexedDependency { owner: unit, edge }),
                );
                stored.observed_dependency_channels &= !bit;
                *event_unproven_channels |= bit;
            }
        }
        stored.dependencies.sort_unstable();
        stored.active = true;
        stored.revision = revision;
        stored.authority_stamp = Some(observation.authority_stamp);
        self.change_unknown_counts(
            stored.observed_families,
            stored.observed_dependency_channels,
            true,
        );
        self.units.insert(unit, stored);
        Ok(())
    }

    fn apply_delete(
        &mut self,
        unit: SourceUnitKey,
        revision: u64,
        direct: &mut Vec<FrontierEntry>,
        replay_units: &mut Vec<SourceUnitKey>,
        removed_dependency_edges: &mut Vec<IndexedDependency>,
        event_unproven_families: &mut u8,
        event_unproven_channels: &mut u8,
        work: &mut SemanticEditWork,
    ) -> Result<(), SemanticEditOwnerError> {
        let Some(mut stored) = self.units.remove(&unit) else {
            self.units.insert(unit, StoredUnit::tombstone(revision));
            return Ok(());
        };
        if stored.active {
            let direct_changes_before = work.direct_row_changes;
            self.change_unknown_counts(
                stored.observed_families,
                stored.observed_dependency_channels,
                false,
            );
            for family in RowFamily::ALL {
                let family_index = family_index(family);
                let bit = family_bit(family);
                if stored.observed_families & bit == 0 {
                    *event_unproven_families |= bit;
                }
                let old_rows = &stored.rows[family_index];
                for row in old_rows {
                    direct.push(FrontierEntry {
                        key: SemanticDependencyKey::Ir(row.key),
                        cause: FrontierCause::DirectUnitMutation,
                        owner: unit,
                    });
                    work.direct_row_changes = work.direct_row_changes.saturating_add(1);
                }
                self.retained_row_slots = self
                    .retained_row_slots
                    .saturating_sub(u64::try_from(old_rows.len()).unwrap_or(u64::MAX));
                self.retained_row_slab_capacity_bytes =
                    self.retained_row_slab_capacity_bytes.saturating_sub(
                        u64::try_from(old_rows.capacity().saturating_mul(size_of::<UnitRow>()))
                            .unwrap_or(u64::MAX),
                    );
                stored.rows[family_index] = Vec::new();
                stored.observed_families |= bit;
            }
            for channel in DependencyChannel::ALL {
                if stored.observed_dependency_channels & channel.bit() == 0 {
                    *event_unproven_channels |= channel.bit();
                }
            }
            for edge in stored.dependencies.iter().copied() {
                direct.push(FrontierEntry {
                    key: edge.dependent,
                    cause: FrontierCause::DependencyInvalidation,
                    owner: unit,
                });
                removed_dependency_edges.push(IndexedDependency { owner: unit, edge });
                work.dependency_edges_changed = work.dependency_edges_changed.saturating_add(1);
            }
            self.remove_unit_edges(unit, u8::MAX, &stored.dependencies);
            stored.dependencies.clear();
            if work.direct_row_changes > direct_changes_before {
                replay_units.push(unit);
            }
        } else {
            *event_unproven_families |= 0x7f;
            *event_unproven_channels |= 0x1f;
        }
        stored.active = false;
        stored.revision = revision;
        stored.observed_families = 0;
        stored.observed_dependency_channels = 0;
        self.units.insert(unit, stored);
        Ok(())
    }

    fn insert_unit_edge(
        &mut self,
        unit: SourceUnitKey,
        edge: SemanticDependencyEdge,
    ) -> Result<(), SemanticEditOwnerError> {
        let key = edge.cause;
        let indexed = IndexedDependency { owner: unit, edge };
        let bucket = self.reverse_dependencies.entry(key).or_default();
        match bucket.binary_search(&indexed) {
            Ok(_) => return Err(SemanticEditOwnerError::DuplicateDependencyEdge),
            Err(position) => bucket.insert(position, indexed),
        }
        Ok(())
    }

    fn remove_unit_edges(
        &mut self,
        unit: SourceUnitKey,
        channel_mask: u8,
        dependencies: &[SemanticDependencyEdge],
    ) {
        for edge in dependencies
            .iter()
            .copied()
            .filter(|edge| edge.channel.bit() & channel_mask != 0)
        {
            let key = edge.cause;
            let Some(bucket) = self.reverse_dependencies.get_mut(&key) else {
                continue;
            };
            let indexed = IndexedDependency { owner: unit, edge };
            if let Ok(position) = bucket.binary_search(&indexed) {
                bucket.remove(position);
            }
            let empty = bucket.is_empty();
            if empty {
                self.reverse_dependencies.remove(&key);
            }
        }
    }

    fn change_unknown_counts(&mut self, families: u8, channels: u8, active: bool) {
        let delta = if active { 1_i8 } else { -1_i8 };
        for (index, family) in RowFamily::ALL.iter().enumerate() {
            if families & family_bit(*family) == 0 {
                add_signed(&mut self.unobserved_family_units[index], delta);
            }
        }
        for channel in DependencyChannel::ALL {
            if channels & channel.bit() == 0 {
                add_signed(
                    &mut self.unobserved_dependency_units[channel as usize],
                    delta,
                );
            }
        }
    }
}

fn visit_dependency(
    indexed: IndexedDependency,
    visited: &mut Vec<SemanticDependencyKey>,
    next: &mut Vec<SemanticDependencyKey>,
    entries: &mut Vec<FrontierEntry>,
    replay_units: &mut Vec<SourceUnitKey>,
    work: &mut SemanticEditWork,
) {
    work.dependency_edges_visited = work.dependency_edges_visited.saturating_add(1);
    let dependent = indexed.edge.dependent;
    replay_units.push(indexed.owner);
    if visited.binary_search(&dependent).is_err() {
        next.push(dependent);
        entries.push(FrontierEntry {
            key: dependent,
            cause: FrontierCause::DependencyInvalidation,
            owner: indexed.owner,
        });
    }
}

fn dependency_edge_changes(
    old: &[SemanticDependencyEdge],
    new: &[SemanticDependencyEdge],
) -> (Vec<SemanticDependencyEdge>, Vec<SemanticDependencyEdge>) {
    let mut removed = Vec::new();
    let mut added = Vec::new();
    let (mut left, mut right) = (0_usize, 0_usize);
    while left < old.len() || right < new.len() {
        match (old.get(left), new.get(right)) {
            (Some(before), Some(after)) => match before.cmp(after) {
                core::cmp::Ordering::Less => {
                    removed.push(*before);
                    left += 1;
                }
                core::cmp::Ordering::Greater => {
                    added.push(*after);
                    right += 1;
                }
                core::cmp::Ordering::Equal => {
                    left += 1;
                    right += 1;
                }
            },
            (Some(before), None) => {
                removed.push(*before);
                left += 1;
            }
            (None, Some(after)) => {
                added.push(*after);
                right += 1;
            }
            (None, None) => break,
        }
    }
    (removed, added)
}

fn add_signed(target: &mut u64, delta: i8) {
    if delta >= 0 {
        *target = target.saturating_add(delta as u64);
    } else {
        *target = target.saturating_sub(delta.unsigned_abs() as u64);
    }
}

fn diff_unit_rows(
    family: RowFamily,
    old: &[UnitRow],
    new: &[UnitRow],
    work: &mut SemanticEditWork,
) -> Result<Vec<SemanticDependencyKey>, SemanticEditOwnerError> {
    validate_unit_rows(family, old)?;
    validate_unit_rows(family, new)?;
    let mut changed = Vec::new();
    changed.reserve(old.len().saturating_add(new.len()));
    let (mut left, mut right) = (0_usize, 0_usize);
    while left < old.len() || right < new.len() {
        match (old.get(left), new.get(right)) {
            (Some(before), Some(after)) => {
                work.row_comparisons = work.row_comparisons.saturating_add(1);
                match before.key.cmp(&after.key) {
                    core::cmp::Ordering::Less => {
                        changed.push(SemanticDependencyKey::Ir(before.key));
                        work.direct_row_changes = work.direct_row_changes.saturating_add(1);
                        left += 1;
                    }
                    core::cmp::Ordering::Greater => {
                        changed.push(SemanticDependencyKey::Ir(after.key));
                        work.direct_row_changes = work.direct_row_changes.saturating_add(1);
                        right += 1;
                    }
                    core::cmp::Ordering::Equal => {
                        if before.payload != after.payload {
                            changed.push(SemanticDependencyKey::Ir(after.key));
                            work.direct_row_changes = work.direct_row_changes.saturating_add(1);
                        } else {
                            work.rows_reused = work.rows_reused.saturating_add(1);
                        }
                        left += 1;
                        right += 1;
                    }
                }
            }
            (Some(before), None) => {
                changed.push(SemanticDependencyKey::Ir(before.key));
                work.direct_row_changes = work.direct_row_changes.saturating_add(1);
                left += 1;
            }
            (None, Some(after)) => {
                changed.push(SemanticDependencyKey::Ir(after.key));
                work.direct_row_changes = work.direct_row_changes.saturating_add(1);
                right += 1;
            }
            (None, None) => break,
        }
    }
    Ok(changed)
}

fn family_index(family: RowFamily) -> usize {
    family.code() as usize - 1
}

const fn family_bit(family: RowFamily) -> u8 {
    1 << (family.code() - 1)
}

// This counter measures allocated slots, including unused vector capacity.
#[allow(clippy::ptr_arg)]
fn capacity_bytes<T>(values: &Vec<T>) -> u64 {
    u64::try_from(values.capacity().saturating_mul(size_of::<T>())).unwrap_or(u64::MAX)
}

/// Source-unit identity or canonical-family capture failed before an owner
/// event could be created.
#[derive(Debug, Error)]
pub enum SemanticUnitCaptureError {
    /// A compiler-captured package/file scope is required for stable unit
    /// replacement.
    #[error("semantic image has no captured source-unit provenance")]
    MissingCapturedProvenance,
    /// A captured package/file atom was absent from the semantic arena.
    #[error("captured source-unit scope atom is unavailable")]
    MissingScopeAtom,
    /// A row segment named a different family from its canonical encoder.
    #[error("canonical {family:?} encoder emitted a different plane")]
    FamilyKindMismatch { family: RowFamily },
    /// The unit observation attempted to store an embedding as an IR family.
    #[error("embedding output cannot be captured as an IR source-unit family")]
    UnexpectedEmbeddingFamily,
    /// Row keys emitted for one unit family were not strictly increasing.
    #[error("canonical {family:?} unit rows are not strictly ordered")]
    UnsortedOrDuplicateRows { family: RowFamily },
    /// Checked identity, length, or counter arithmetic overflowed.
    #[error("source-unit identity or row payload size overflow")]
    Overflow,
    /// A canonical row family failed local encoding or segment validation.
    #[error(transparent)]
    Record(#[from] SemanticPlaneRecordError),
    /// A canonical row payload could not be assigned a stable payload ID.
    #[error(transparent)]
    RowIndex(#[from] StableRowIndexError),
}

/// Invalid source-unit observation supplied to the edit owner.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SemanticUnitObservationError {
    /// An edge was supplied while its channel was declared unobserved.
    #[error("dependency edge supplied for unobserved channel {channel:?}")]
    UnobservedDependencyEdge { channel: DependencyChannel },
    /// The same exact dependency edge appeared twice in one unit event.
    #[error("duplicate dependency edge in source-unit observation")]
    DuplicateDependencyEdge,
    /// An IR dependent key must belong to the same unit observation as its
    /// dependency event so replay can identify the affected owner slab.
    #[error("dependency event's {family:?} dependent row is not owned by this unit")]
    DependentRowNotOwned { family: RowFamily },
}

/// Edit-session rejection before a semantic frontier is returned.
#[derive(Debug, Error)]
pub enum SemanticEditOwnerError {
    /// A source unit was assigned two conflicting events with one revision.
    #[error("source unit {unit:?} has conflicting events at revision {revision}")]
    DuplicateUnitRevision { unit: SourceUnitKey, revision: u64 },
    /// A dependency edge was already present in the reverse owner index.
    #[error("duplicate dependency edge in source-unit owner index")]
    DuplicateDependencyEdge,
    /// A unit row slab was invalid or payload accounting failed.
    #[error(transparent)]
    Capture(#[from] SemanticUnitCaptureError),
}

#[cfg(test)]
mod tests {
    use alloc::{
        collections::{BTreeMap, BTreeSet},
        vec,
        vec::Vec,
    };

    use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};

    use crate::{
        ir::{
            CorePayloadHash, DeclarationFamilyId, EntityAuthorityFacts, EntityVersion,
            FactAvailability, IrBuilder, Item, ItemKind, LanguageProfile, PackageLineage,
            ParentageAuthority, RustEdition, SemanticDependencyKey, SemanticUnitEvent,
            SemanticUnitObservation, SourceIdentity, VariantFingerprint, Visibility,
        },
        vocabulary::{CompileRecipeFact, NativeTool, Stage},
    };

    use super::*;

    const ALL_FAMILY_BITS: u8 = 0x7f;
    const ALL_DEPENDENCY_BITS: u8 = 0x1f;

    fn unit(id: u8) -> SourceUnitKey {
        SourceUnitKey([id; 32])
    }

    fn row(family: RowFamily, key: u8, tag: u8, payload: &[u8]) -> UnitRow {
        UnitRow {
            key: StableRowKey::new(family, [key; 32]),
            payload: RowPayload::from_tagged_bytes(tag, payload).expect("payload hashes"),
        }
    }

    fn observation(
        unit: SourceUnitKey,
        stamp: u8,
        rows: impl IntoIterator<Item = UnitRow>,
    ) -> SemanticUnitObservation {
        let mut lanes: [Vec<UnitRow>; 7] = core::array::from_fn(|_| Vec::new());
        for row in rows {
            lanes[family_index(row.key.family())].push(row);
        }
        for (index, lane) in lanes.iter_mut().enumerate() {
            lane.sort_unstable_by_key(|row| row.key);
            validate_unit_rows(RowFamily::ALL[index], lane).expect("test rows are ordered");
        }
        SemanticUnitObservation {
            unit,
            authority_stamp: [stamp; 32],
            rows: lanes.map(Vec::into_boxed_slice),
            observed_families: ALL_FAMILY_BITS,
            dependencies: Box::default(),
            observed_dependency_channels: 0,
            capture_work: SemanticCaptureWork::default(),
        }
    }

    fn full_rows(unit: SourceUnitKey, base: u8, stamp: u8) -> SemanticUnitObservation {
        let rows = RowFamily::ALL
            .into_iter()
            .enumerate()
            .map(|(index, family)| {
                let key = base.wrapping_add(index as u8);
                let mut identity = blake3::Hasher::new();
                identity.update(b"edit-session-test-row.v1\0");
                identity.update(&unit.as_bytes());
                identity.update(&[key, family.code()]);
                UnitRow {
                    key: StableRowKey::new(family, *identity.finalize().as_bytes()),
                    payload: RowPayload::from_tagged_bytes(family.code(), &[key, family.code()])
                        .expect("payload hashes"),
                }
            });
        observation(unit, stamp, rows)
    }

    fn all_channels(
        observation: SemanticUnitObservation,
        edges: Vec<SemanticDependencyEdge>,
    ) -> SemanticUnitObservation {
        observation
            .with_dependency_events(&DependencyChannel::ALL, edges)
            .expect("test dependency observations are typed")
    }

    fn edge(
        channel: DependencyChannel,
        cause: SemanticDependencyKey,
        dependent: SemanticDependencyKey,
    ) -> SemanticDependencyEdge {
        SemanticDependencyEdge::new(channel, cause, dependent).expect("typed dependency edge")
    }

    fn ir_key(family: RowFamily, key: u8) -> SemanticDependencyKey {
        SemanticDependencyKey::Ir(StableRowKey::new(family, [key; 32]))
    }

    fn embedding_key(subject: u8) -> SemanticDependencyKey {
        let plane = EmbeddingPlaneIdentity::new(
            [1; 32],
            [2; 32],
            [3; 32],
            8,
            crate::ir::EmbeddingNormalization::None,
            [4; 32],
            [5; 32],
        )
        .expect("valid embedding identity");
        SemanticDependencyKey::Embedding(EmbeddingRowKey::new(plane, [subject; 32]))
    }

    fn replace(revision: u64, observation: SemanticUnitObservation) -> SemanticUnitEvent {
        SemanticUnitEvent::Replace {
            revision,
            observation,
        }
    }

    fn delete(revision: u64, unit: SourceUnitKey) -> SemanticUnitEvent {
        SemanticUnitEvent::Delete { unit, revision }
    }

    #[derive(Clone, Debug, Default)]
    struct OracleUnit {
        active: bool,
        rows: [BTreeMap<StableRowKey, RowPayload>; 7],
        observed_families: u8,
        dependencies: Vec<SemanticDependencyEdge>,
        observed_dependency_channels: u8,
        revision: u64,
    }

    type OracleState = BTreeMap<SourceUnitKey, OracleUnit>;

    fn oracle_apply(state: &mut OracleState, events: &[SemanticUnitEvent]) {
        let mut sorted: Vec<&SemanticUnitEvent> = events.iter().collect();
        sorted.sort_unstable_by_key(|event| (event.unit(), event.revision()));
        let mut newest: Vec<&SemanticUnitEvent> = Vec::new();
        for event in sorted {
            if let Some(last) = newest.last_mut()
                && last.unit() == event.unit()
            {
                if event.revision() > last.revision() {
                    *last = event;
                }
            } else {
                newest.push(event);
            }
        }
        for event in newest {
            let unit_key = event.unit();
            let revision = event.revision();
            if state
                .get(&unit_key)
                .is_some_and(|current| revision <= current.revision)
            {
                continue;
            }
            match event {
                SemanticUnitEvent::Replace { observation, .. } => {
                    let previous = state.get(&unit_key).cloned().unwrap_or_default();
                    let mut next = previous;
                    next.active = true;
                    next.revision = revision;
                    next.observed_families = observation.observed_families;
                    for (index, _) in RowFamily::ALL.iter().enumerate() {
                        if observation.observed_families & (1 << index) != 0 {
                            next.rows[index] = observation.rows[index]
                                .iter()
                                .map(|row| (row.key, row.payload))
                                .collect();
                        }
                    }
                    for channel in DependencyChannel::ALL {
                        if observation.observed_dependency_channels & channel.bit() != 0 {
                            next.dependencies.retain(|edge| edge.channel != channel);
                            next.dependencies.extend(
                                observation
                                    .dependencies
                                    .iter()
                                    .copied()
                                    .filter(|edge| edge.channel == channel),
                            );
                        }
                    }
                    next.observed_dependency_channels = observation.observed_dependency_channels;
                    next.dependencies.sort_unstable();
                    state.insert(unit_key, next);
                }
                SemanticUnitEvent::Delete { .. } => {
                    let mut next = state.get(&unit_key).cloned().unwrap_or_default();
                    next.active = false;
                    next.revision = revision;
                    next.rows = core::array::from_fn(|_| BTreeMap::new());
                    next.dependencies.clear();
                    next.observed_families = 0;
                    next.observed_dependency_channels = 0;
                    state.insert(unit_key, next);
                }
            }
        }
    }

    fn oracle_frontier(
        before: &OracleState,
        after: &OracleState,
    ) -> (
        BTreeMap<SemanticDependencyKey, FrontierCause>,
        BTreeSet<SourceUnitKey>,
    ) {
        let units: BTreeSet<SourceUnitKey> = before.keys().chain(after.keys()).copied().collect();
        let mut entries = BTreeMap::new();
        let mut replay = BTreeSet::new();

        // Deliberate full scan: visit every family row in every old and new
        // unit, independently of the production owner's edited-unit merge.
        for unit_key in units {
            let old = before.get(&unit_key).filter(|unit| unit.active);
            let new = after.get(&unit_key).filter(|unit| unit.active);
            for family_index in 0..7 {
                let old_rows = old.map(|unit| &unit.rows[family_index]);
                let new_rows = new.map(|unit| &unit.rows[family_index]);
                let row_keys: BTreeSet<StableRowKey> = old_rows
                    .into_iter()
                    .flat_map(|rows| rows.keys().copied())
                    .chain(new_rows.into_iter().flat_map(|rows| rows.keys().copied()))
                    .collect();
                for key in row_keys {
                    let old_value = old_rows.and_then(|rows| rows.get(&key));
                    let new_value = new_rows.and_then(|rows| rows.get(&key));
                    if old_value != new_value {
                        entries.insert(
                            SemanticDependencyKey::Ir(key),
                            FrontierCause::DirectUnitMutation,
                        );
                        replay.insert(unit_key);
                    }
                }
            }
        }

        let dependency_units: BTreeSet<SourceUnitKey> =
            before.keys().chain(after.keys()).copied().collect();
        for unit_key in dependency_units {
            let old = before.get(&unit_key).filter(|unit| unit.active);
            let new = after.get(&unit_key).filter(|unit| unit.active);
            let old_edges: BTreeSet<_> = old
                .into_iter()
                .flat_map(|unit| unit.dependencies.iter().copied())
                .collect();
            let new_edges: BTreeSet<_> = new
                .into_iter()
                .flat_map(|unit| unit.dependencies.iter().copied())
                .collect();
            for edge in old_edges.symmetric_difference(&new_edges) {
                entries
                    .entry(edge.dependent)
                    .or_insert(FrontierCause::DependencyInvalidation);
                replay.insert(unit_key);
            }
        }

        let mut queue: Vec<SemanticDependencyKey> = entries.keys().copied().collect();
        let mut visited: BTreeSet<SemanticDependencyKey> = queue.iter().copied().collect();
        while let Some(cause) = queue.pop() {
            for state in [before, after] {
                for (owner, unit) in state.iter().filter(|(_, unit)| unit.active) {
                    for edge in &unit.dependencies {
                        if edge.cause != cause
                            || unit.observed_dependency_channels & edge.channel.bit() == 0
                        {
                            continue;
                        }
                        replay.insert(*owner);
                        if visited.insert(edge.dependent) {
                            entries.insert(edge.dependent, FrontierCause::DependencyInvalidation);
                            queue.push(edge.dependent);
                        }
                    }
                }
            }
        }
        (entries, replay)
    }

    fn oracle_full_scan_row_visits(before: &OracleState, after: &OracleState) -> usize {
        before
            .values()
            .chain(after.values())
            .filter(|unit| unit.active)
            .flat_map(|unit| unit.rows.iter())
            .map(BTreeMap::len)
            .sum()
    }

    fn assert_matches_oracle(
        frontier: &SemanticChangeFrontier,
        before: &OracleState,
        after: &OracleState,
    ) {
        let (expected, replay) = oracle_frontier(before, after);
        let actual: BTreeMap<_, _> = frontier
            .entries()
            .iter()
            .map(|entry| (entry.key(), entry.cause()))
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(
            frontier
                .replay_units()
                .iter()
                .copied()
                .collect::<BTreeSet<_>>(),
            replay
        );
    }

    fn captured_image(source: &[u8], path: &str, toolchain: &[u8]) -> crate::ir::Ir {
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let source_identity = ContentId::<SourceFactDomain>::from_canonical_bytes(source);
        let toolchain = ContentId::<ToolchainDomain>::from_canonical_bytes(toolchain);
        let source_fact = SourceIdentity {
            identity: source_identity,
            byte_len: u32::try_from(source.len()).expect("source length fits"),
        };
        let recipe = CompileRecipeFact::derive(
            profile,
            Stage::LowerIr,
            NativeTool::Rustc,
            source_identity,
            toolchain,
        );
        let mut builder = IrBuilder::new();
        builder
            .set_language_profile(profile)
            .expect("language profile is admitted");
        builder
            .set_image_provenance(
                source_fact,
                recipe,
                PackageLineage::new("cargo", "frontier-test").expect("valid package lineage"),
                path,
            )
            .expect("captured source scope is admitted");
        let item = Item {
            name: builder.intern_atom(b"same declaration").expect("name atom"),
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            parent: None,
            semantic_type: None,
            members: builder.intern_members(&[]).expect("empty members"),
            docs: builder.intern_docs(&[]).expect("empty docs"),
            attributes: builder.intern_attributes(&[]).expect("empty attributes"),
            source: None,
        };
        builder
            .add_item(
                EntityVersion {
                    family: DeclarationFamilyId::from_raw([1; 16]),
                    variant: VariantFingerprint::from_raw([2; 16]),
                    core_payload: CorePayloadHash::from_raw([3; 16]),
                },
                item,
                None,
                EntityAuthorityFacts {
                    parentage: ParentageAuthority::Root,
                    visibility: FactAvailability::Captured,
                    ..EntityAuthorityFacts::default()
                },
            )
            .expect("declaration row is admitted");
        builder.finish().expect("captured test IR is valid")
    }

    #[test]
    fn sparse_unit_edit_matches_full_scan_oracle_and_invalidates_each_typed_channel() {
        let a = unit(1);
        let b = unit(2);
        let mut base_rows = vec![
            row(RowFamily::Core, 10, 1, &[10]),
            row(RowFamily::Types, 20, 2, &[20]),
            row(RowFamily::Relations, 30, 3, &[30]),
            row(RowFamily::Occurrences, 40, 4, &[40]),
            row(RowFamily::Documentation, 50, 5, &[50]),
            row(RowFamily::SourceProvenance, 60, 6, &[60]),
            row(RowFamily::LanguageExtensions, 70, 7, &[70]),
        ];
        base_rows.sort_unstable_by_key(|row| row.key);
        let mut target_rows = vec![
            row(RowFamily::Occurrences, 141, 4, &[141]),
            row(RowFamily::LanguageExtensions, 171, 7, &[171]),
            row(RowFamily::Core, 111, 1, &[111]),
            row(RowFamily::Types, 121, 2, &[121]),
            row(RowFamily::Relations, 131, 3, &[131]),
            row(RowFamily::Documentation, 151, 5, &[151]),
            row(RowFamily::SourceProvenance, 161, 6, &[161]),
        ];
        target_rows.sort_unstable_by_key(|row| row.key);
        let embedding_doc = embedding_key(201);
        let embedding_source = embedding_key(202);
        let embedding_extension = embedding_key(203);
        let dependencies = vec![
            edge(
                DependencyChannel::TypeConsumer,
                ir_key(RowFamily::Types, 20),
                ir_key(RowFamily::LanguageExtensions, 171),
            ),
            edge(
                DependencyChannel::RelationOccurrence,
                ir_key(RowFamily::Relations, 30),
                ir_key(RowFamily::Occurrences, 141),
            ),
            edge(
                DependencyChannel::DocumentationEmbedding,
                ir_key(RowFamily::Documentation, 50),
                embedding_doc,
            ),
            edge(
                DependencyChannel::SourceEmbedding,
                ir_key(RowFamily::SourceProvenance, 60),
                embedding_source,
            ),
            edge(
                DependencyChannel::ExtensionEmbedding,
                ir_key(RowFamily::LanguageExtensions, 70),
                embedding_extension,
            ),
        ];

        let mut owner = SemanticEditOwner::new();
        let mut oracle = OracleState::new();
        let base_a = all_channels(observation(a, 1, base_rows), Vec::new());
        let base_b = all_channels(observation(b, 2, target_rows), dependencies);
        let mut initial_events = vec![replace(1, base_a.clone()), replace(1, base_b.clone())];
        for index in 0..96_u8 {
            let unrelated = unit(index.wrapping_add(16));
            initial_events.push(replace(
                1,
                all_channels(
                    full_rows(unrelated, index.wrapping_add(80), index),
                    Vec::new(),
                ),
            ));
        }
        let before_events = initial_events.clone();
        let initial_frontier = owner
            .apply_events(initial_events)
            .expect("initial units apply");
        oracle_apply(&mut oracle, &before_events);
        assert!(initial_frontier.has_unproven_workspace_scope());

        let old_state = oracle.clone();
        let edited = all_channels(
            observation(
                a,
                3,
                vec![
                    row(RowFamily::Core, 10, 1, &[110]),
                    row(RowFamily::Types, 20, 2, &[220]),
                    row(RowFamily::Relations, 31, 3, &[31]),
                    row(RowFamily::Occurrences, 40, 4, &[40]),
                    row(RowFamily::Documentation, 50, 5, &[550]),
                    row(RowFamily::SourceProvenance, 60, 6, &[660]),
                    row(RowFamily::LanguageExtensions, 70, 7, &[770]),
                ],
            ),
            Vec::new(),
        );
        let edit = replace(2, edited);
        let after_events = vec![edit.clone()];
        let frontier = owner.apply_events(vec![edit]).expect("sparse edit applies");
        oracle_apply(&mut oracle, &after_events);

        assert_matches_oracle(&frontier, &old_state, &oracle);
        assert_eq!(frontier.unproven_families(), 0);
        assert_eq!(frontier.unproven_dependency_channels(), 0);
        assert!(frontier.has_unproven_embedding_plane());
        assert_eq!(frontier.scope(), FrontierScope::RegisteredUnitContributions);
        assert!(
            frontier
                .entries()
                .iter()
                .any(|entry| entry.key() == embedding_doc)
        );
        assert!(
            frontier
                .entries()
                .iter()
                .any(|entry| entry.key() == embedding_source)
        );
        assert!(
            frontier
                .entries()
                .iter()
                .any(|entry| entry.key() == embedding_extension)
        );
        assert!(frontier.entries().iter().any(|entry| {
            entry.key() == ir_key(RowFamily::Occurrences, 141)
                && entry.cause() == FrontierCause::DependencyInvalidation
        }));
        assert!(frontier.entries().iter().any(|entry| {
            entry.key() == ir_key(RowFamily::LanguageExtensions, 171)
                && entry.cause() == FrontierCause::DependencyInvalidation
        }));

        let work = frontier.metrics();
        let full_scan_rows = oracle_full_scan_row_visits(&old_state, &oracle);
        assert!(usize::try_from(work.row_comparisons).expect("small test count") < full_scan_rows);
        assert!(
            usize::try_from(work.peak_temporary_row_slots).expect("small test count")
                < full_scan_rows
        );
        assert!(
            usize::try_from(work.peak_unit_row_slab_bytes).expect("small test bytes")
                < full_scan_rows.saturating_mul(size_of::<UnitRow>())
        );
        assert!(
            usize::try_from(work.peak_tracked_scratch_capacity_bytes).expect("small test bytes")
                < full_scan_rows.saturating_mul(size_of::<UnitRow>())
        );
        assert_eq!(work.rows_reused, 1);
        assert_eq!(work.direct_row_changes, 7);
        assert_eq!(work.dependency_edges_visited, 5);
        assert_eq!(work.rows_invalidated as usize, frontier.entries().len());
        assert!(work.retained_row_slab_capacity_bytes > 0);
    }

    #[test]
    fn no_op_insert_delete_and_rename_are_explicit_unit_events() {
        let old = unit(30);
        let new = unit(31);
        let first = all_channels(full_rows(old, 1, 9), Vec::new());
        let mut owner = SemanticEditOwner::new();
        let mut oracle = OracleState::new();
        let initial = vec![replace(1, first.clone())];
        owner
            .apply_events(initial.clone())
            .expect("initial unit applies");
        oracle_apply(&mut oracle, &initial);

        let before_noop = oracle.clone();
        let noop = replace(2, first.clone());
        let noop_frontier = owner
            .apply_events(vec![noop.clone()])
            .expect("no-op revision applies");
        oracle_apply(&mut oracle, &[noop]);
        assert_matches_oracle(&noop_frontier, &before_noop, &oracle);
        assert!(noop_frontier.entries().is_empty());
        assert_eq!(noop_frontier.metrics().rows_reused, 7);

        let before_insert = oracle.clone();
        let inserted = all_channels(full_rows(unit(32), 41, 10), Vec::new());
        let insert = replace(1, inserted.clone());
        let insert_frontier = owner
            .apply_events(vec![insert.clone()])
            .expect("insert applies");
        oracle_apply(&mut oracle, &[insert]);
        assert_matches_oracle(&insert_frontier, &before_insert, &oracle);
        assert_eq!(insert_frontier.metrics().direct_row_changes, 7);

        let before_delete = oracle.clone();
        let delete_unit = inserted.unit();
        let deletion = delete(2, delete_unit);
        let delete_frontier = owner
            .apply_events(vec![deletion.clone()])
            .expect("delete applies");
        oracle_apply(&mut oracle, &[deletion]);
        assert_matches_oracle(&delete_frontier, &before_delete, &oracle);
        assert_eq!(delete_frontier.metrics().direct_row_changes, 7);

        let before_rename = oracle.clone();
        let renamed = all_channels(full_rows(new, 1, 9), Vec::new());
        let rename = vec![delete(3, old), replace(1, renamed.clone())];
        let rename_frontier = owner
            .apply_events(rename.clone())
            .expect("rename is explicit delete plus insert");
        oracle_apply(&mut oracle, &rename);
        assert_matches_oracle(&rename_frontier, &before_rename, &oracle);
        assert_eq!(rename_frontier.entries().len(), 14);
        assert!(rename_frontier.replay_units().contains(&old));
        assert!(rename_frontier.replay_units().contains(&new));
    }

    #[test]
    fn adding_and_removing_observed_edges_invalidates_the_dependent_owner() {
        let source = unit(41);
        let consumer = unit(42);
        let relation = row(RowFamily::Relations, 1, 3, &[1]);
        let occurrence = row(RowFamily::Occurrences, 2, 4, &[2]);
        let dependency = edge(
            DependencyChannel::RelationOccurrence,
            SemanticDependencyKey::Ir(relation.key),
            SemanticDependencyKey::Ir(occurrence.key),
        );
        let source_observation = all_channels(observation(source, 1, vec![relation]), Vec::new());
        let consumer_with_edge =
            all_channels(observation(consumer, 2, vec![occurrence]), vec![dependency]);
        let mut owner = SemanticEditOwner::new();
        let mut oracle = OracleState::new();
        let initial = vec![
            replace(1, source_observation),
            replace(1, consumer_with_edge.clone()),
        ];
        owner
            .apply_events(initial.clone())
            .expect("both source units apply");
        oracle_apply(&mut oracle, &initial);

        // The edge is removed without changing either endpoint row. The
        // dependent occurrence must still be invalidated by the graph edit.
        let before_remove = oracle.clone();
        let consumer_without_edge =
            all_channels(observation(consumer, 3, vec![occurrence]), Vec::new());
        let removal = replace(2, consumer_without_edge);
        let removed = owner
            .apply_events(vec![removal.clone()])
            .expect("dependency removal applies");
        oracle_apply(&mut oracle, &[removal]);
        assert_matches_oracle(&removed, &before_remove, &oracle);
        assert_eq!(removed.metrics().dependency_edges_changed, 1);
        assert!(removed.entries().iter().any(|entry| {
            entry.key() == SemanticDependencyKey::Ir(occurrence.key)
                && entry.cause() == FrontierCause::DependencyInvalidation
        }));
        assert!(removed.replay_units().contains(&consumer));

        // Re-adding the edge is also a graph mutation, even though all row
        // payloads remain byte-identical.
        let before_add = oracle.clone();
        let addition = replace(3, consumer_with_edge);
        let added = owner
            .apply_events(vec![addition.clone()])
            .expect("dependency addition applies");
        oracle_apply(&mut oracle, &[addition]);
        assert_matches_oracle(&added, &before_add, &oracle);
        assert_eq!(added.metrics().dependency_edges_changed, 1);
        assert!(added.replay_units().contains(&consumer));
    }

    #[test]
    fn out_of_order_and_coalesced_events_keep_only_the_latest_unit_revision() {
        let source = unit(55);
        let mut owner = SemanticEditOwner::new();
        let base = replace(1, all_channels(full_rows(source, 1, 1), Vec::new()));
        owner.apply_events(vec![base]).expect("base applies");

        let rev2 = replace(2, all_channels(full_rows(source, 20, 2), Vec::new()));
        let rev3 = replace(3, all_channels(full_rows(source, 40, 3), Vec::new()));
        let coalesced = owner
            .apply_events(vec![rev3.clone(), rev2.clone()])
            .expect("newest event wins independent of arrival order");
        assert_eq!(coalesced.metrics().units_changed, 1);
        assert_eq!(coalesced.metrics().events_skipped, 1);
        assert_eq!(coalesced.entries().len(), 14);

        let stale = owner
            .apply_events(vec![rev2])
            .expect("late lower revision is ignored");
        assert!(stale.entries().is_empty());
        assert_eq!(stale.metrics().events_skipped, 1);

        let duplicate = owner.apply_events(vec![rev3.clone(), rev3]);
        assert!(matches!(
            duplicate,
            Err(SemanticEditOwnerError::DuplicateUnitRevision { revision: 3, .. })
        ));
    }

    #[test]
    fn unobserved_family_and_dependency_channels_remain_unproven() {
        let source = unit(71);
        let mut partial = full_rows(source, 90, 1);
        partial.observed_families &= !family_bit(RowFamily::LanguageExtensions);
        partial.rows[family_index(RowFamily::LanguageExtensions)] = Box::default();
        partial.observed_dependency_channels = DependencyChannel::TypeConsumer.bit();

        let mut owner = SemanticEditOwner::new();
        let frontier = owner
            .apply_events(vec![replace(1, partial)])
            .expect("partial source-unit event remains usable");
        assert_ne!(
            frontier.unproven_families() & family_bit(RowFamily::LanguageExtensions),
            0
        );
        assert_eq!(
            frontier.unproven_dependency_channels() & ALL_DEPENDENCY_BITS,
            ALL_DEPENDENCY_BITS & !DependencyChannel::TypeConsumer.bit()
        );
        assert!(frontier.has_unproven_workspace_scope());
        assert!(frontier.has_unproven_embedding_plane());
        assert_eq!(frontier.scope(), FrontierScope::RegisteredUnitContributions);
    }

    #[test]
    fn identical_semantic_rows_with_new_source_authority_do_not_look_like_a_noop_generation() {
        let first_ir = captured_image(b"same source", "src/lib.rs", b"toolchain one");
        let second_ir = captured_image(b"same source", "src/lib.rs", b"toolchain two");
        let first =
            SemanticUnitObservation::from_reader(&first_ir, crate::ir::MAX_SEMANTIC_SEGMENT_BYTES)
                .expect("first canonical source image captures");
        let second =
            SemanticUnitObservation::from_reader(&second_ir, crate::ir::MAX_SEMANTIC_SEGMENT_BYTES)
                .expect("second canonical source image captures");
        assert_eq!(first.unit(), second.unit());
        assert_eq!(first.observed_family_mask(), ALL_FAMILY_BITS);
        assert!(first.capture_work().rows_encoded > 0);
        assert!(first.capture_work().rows_decoded > 0);
        assert!(first.capture_work().bytes_hashed > 0);

        let source_unit = first.unit();
        let mut owner = SemanticEditOwner::new();
        owner
            .apply_events(vec![replace(1, first)])
            .expect("first source generation applies");
        let next = owner
            .apply_events(vec![replace(2, second)])
            .expect("new source authority applies");
        assert!(next.entries().is_empty());
        assert_eq!(next.authority_generation_changes(), &[source_unit]);
        assert!(next.replay_units().contains(&source_unit));
        assert!(next.has_unproven_workspace_scope());
        assert!(next.has_unproven_embedding_plane());
    }

    #[test]
    fn dependency_channels_enforce_ir_and_embedding_plane_separation() {
        let type_row = ir_key(RowFamily::Types, 1);
        let extension_row = ir_key(RowFamily::LanguageExtensions, 2);
        let relation_row = ir_key(RowFamily::Relations, 3);
        let occurrence_row = ir_key(RowFamily::Occurrences, 4);
        let embedding = embedding_key(5);
        assert!(
            SemanticDependencyEdge::new(DependencyChannel::TypeConsumer, type_row, extension_row,)
                .is_ok()
        );
        assert!(
            SemanticDependencyEdge::new(
                DependencyChannel::RelationOccurrence,
                relation_row,
                occurrence_row,
            )
            .is_ok()
        );
        assert!(
            SemanticDependencyEdge::new(
                DependencyChannel::DocumentationEmbedding,
                ir_key(RowFamily::Documentation, 6),
                embedding,
            )
            .is_ok()
        );
        assert!(
            SemanticDependencyEdge::new(DependencyChannel::TypeConsumer, type_row, embedding,)
                .is_err()
        );
        assert!(
            SemanticDependencyEdge::new(
                DependencyChannel::RelationOccurrence,
                relation_row,
                ir_key(RowFamily::Documentation, 7),
            )
            .is_err()
        );
    }
}
