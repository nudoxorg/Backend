//! Defines vector behavior for `server-index-graph-vector`, whose purpose is to execute typed graph and vector work through bounded leased storage.
//! This module owns the vector invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use core::ops::Deref;

use backend_semantic::ir::EntityId;
use backend_version::{ContentHasher, FixedCanonicalRecord, IndexVectorSegmentDomain};
use server_index_vocabulary::VectorSegmentId;

use crate::{MAX_PARTITIONS, Metric, MissingPartitions, PartitionId, VectorAuthority};

/// Maximum coordinates admitted by stack-resident graph/vector projection structures.
pub const MAX_VECTOR_DIMENSION: usize = 16;
const MAX_FACTS_PER_ROW: usize = 16;

/// Vector-specific stream terminal facts. This type cannot substitute for graph terminals.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VectorTerminal {
    /// Every selected vector partition completed.
    Complete {
        /// Snapshot, model, dimension, and metric authority.
        authority: VectorAuthority,
    },
    /// Cancellation won before publication.
    Cancelled {
        /// Snapshot, model, dimension, and metric authority.
        authority: VectorAuthority,
    },
}

impl VectorTerminal {
    /// Returns the complete vector authority retained by every terminal.
    #[must_use]
    pub const fn authority(self) -> VectorAuthority {
        match self {
            Self::Complete { authority } | Self::Cancelled { authority } => authority,
        }
    }
}

/// One raw borrowed exact-vector projection fact.
///
/// This is an adapter-ingress shape only: its repeated authority and partition labels must pass
/// through [`compact_vector_facts`] before a query or publication path can consume the compact
/// segment representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VectorFact<'coordinates> {
    /// Snapshot, model, dimension, and metric authority for this fact.
    pub authority: VectorAuthority,
    /// Immutable projection partition containing this fact.
    pub partition: PartitionId,
    /// Semantic entity coordinate represented by this vector.
    pub entity: EntityId,
    /// Borrowed fixed-width vector coordinates.
    pub coordinates: &'coordinates [i16],
}

impl<'coordinates> VectorFact<'coordinates> {
    /// Creates one raw ingress fact without claiming that its labels are validated.
    #[must_use]
    pub const fn new(
        authority: VectorAuthority,
        partition: PartitionId,
        entity: EntityId,
        coordinates: &'coordinates [i16],
    ) -> Self {
        Self {
            authority,
            partition,
            entity,
            coordinates,
        }
    }
}

/// One compact vector point whose authority is supplied by its segment owner.
///
/// Keeping only the semantic entity and borrowed coordinates here avoids repeating the snapshot,
/// model, dimension, metric, and partition in every point. Raw adapter ingress can be checked as a
/// [`VectorFact`] first and then converted into this segment-scoped representation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VectorPoint<'coordinates> {
    /// Semantic entity coordinate represented by this vector.
    pub entity: EntityId,
    /// Borrowed fixed-width vector coordinates.
    pub coordinates: &'coordinates [i16],
}

impl<'coordinates> VectorPoint<'coordinates> {
    /// Creates a compact point without copying its coordinate borrow.
    #[must_use]
    pub const fn new(entity: EntityId, coordinates: &'coordinates [i16]) -> Self {
        Self {
            entity,
            coordinates,
        }
    }
}

/// Exact rejection while sealing one borrowed vector segment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VectorSegmentError {
    /// The segment authority has no supported coordinate shape.
    AuthorityDimension {
        /// Maximum supported coordinate count.
        maximum: usize,
        /// Rejected authority dimension.
        observed: usize,
    },
    /// The segment would exceed its bounded fact count.
    TooManyFacts {
        /// Maximum facts admitted in one segment.
        maximum: usize,
        /// Complete observed fact count.
        observed: usize,
    },
    /// The caller-provided compact point buffer cannot retain every raw ingress fact.
    InsufficientPointOutput {
        /// Compact points required to preserve all raw facts.
        required: usize,
        /// Caller-provided compact point slots.
        available: usize,
    },
    /// A fact belongs to a different immutable vector authority.
    WrongAuthority {
        /// Zero-based rejected fact position.
        index: usize,
        /// Authority pinned by the segment owner.
        expected: VectorAuthority,
        /// Authority carried by the rejected fact.
        observed: VectorAuthority,
    },
    /// A fact belongs to a different projection partition.
    WrongPartition {
        /// Zero-based rejected fact position.
        index: usize,
        /// Partition pinned by the segment owner.
        expected: PartitionId,
        /// Partition carried by the rejected fact.
        observed: PartitionId,
    },
    /// A fact has a coordinate count different from the authority.
    FactDimension {
        /// Zero-based rejected fact position.
        index: usize,
        /// Required coordinate count.
        expected: usize,
        /// Complete observed coordinate count.
        observed: usize,
    },
    /// Two adjacent facts carry the same entity identity.
    DuplicateEntity {
        /// First occurrence position.
        first_index: usize,
        /// Later duplicate position.
        index: usize,
        /// Repeated semantic entity.
        entity: EntityId,
    },
    /// Facts are not in canonical strictly ascending entity order.
    OutOfOrder {
        /// Later offending fact position.
        index: usize,
        /// Immediately preceding entity identity.
        previous: EntityId,
        /// Offending entity identity.
        observed: EntityId,
    },
}

/// Validates raw adapter facts and writes compact points into caller-owned storage.
///
/// This is the only vector ingress boundary that accepts repeated per-fact authority labels.
/// It validates the entire batch before writing any output, preserving the caller's point buffer
/// on rejection. Pass the returned prefix to [`ValidatedVectorSegment::try_new`] to seal its
/// authority, partition, canonical order, and identity for querying or publication.
#[allow(
    clippy::result_large_err,
    reason = "cross-authority rejection retains both complete authority facts"
)]
pub fn compact_vector_facts<'coordinates>(
    authority: VectorAuthority,
    partition: PartitionId,
    facts: &[VectorFact<'coordinates>],
    output: &mut [VectorPoint<'coordinates>],
) -> Result<usize, VectorSegmentError> {
    let dimension = checked_vector_dimension(authority)?;
    if facts.len() > MAX_FACTS_PER_ROW {
        return Err(VectorSegmentError::TooManyFacts {
            maximum: MAX_FACTS_PER_ROW,
            observed: facts.len(),
        });
    }
    if output.len() < facts.len() {
        return Err(VectorSegmentError::InsufficientPointOutput {
            required: facts.len(),
            available: output.len(),
        });
    }
    for (index, fact) in facts.iter().copied().enumerate() {
        if fact.authority != authority {
            return Err(VectorSegmentError::WrongAuthority {
                index,
                expected: authority,
                observed: fact.authority,
            });
        }
        if fact.partition != partition {
            return Err(VectorSegmentError::WrongPartition {
                index,
                expected: partition,
                observed: fact.partition,
            });
        }
        if fact.coordinates.len() != dimension {
            return Err(VectorSegmentError::FactDimension {
                index,
                expected: dimension,
                observed: fact.coordinates.len(),
            });
        }
    }
    for (slot, fact) in output.iter_mut().zip(facts.iter().copied()) {
        *slot = VectorPoint::new(fact.entity, fact.coordinates);
    }
    Ok(facts.len())
}

/// One validated immutable vector segment borrowed from caller-owned facts.
///
/// Construction validates the complete authority, partition, dimension, uniqueness, and
/// canonical entity order before streaming the same points into a typed [`VectorSegmentId`]. The
/// identity and borrowed points are private correlated state, so callers cannot re-label a segment
/// after validation or forge a proof by constructing this type directly.
///
/// ```compile_fail
/// use server_index_graph_vector::{VectorAuthority, ValidatedVectorSegment};
/// use server_index_vocabulary::VectorSegmentId;
///
/// fn forge<'facts>(
///     authority: VectorAuthority,
///     facts: &'facts [server_index_graph_vector::VectorPoint<'facts>],
///     id: VectorSegmentId,
/// ) {
///     let _ = ValidatedVectorSegment(server_index_graph_vector::VectorSegmentView {
///         authority,
///         partition: server_index_graph_vector::PartitionId { raw: 0 },
///         point_count: 0,
///         facts,
///         id,
///     });
/// }
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValidatedVectorSegment<'facts>(VectorSegmentView<'facts>);

/// Immutable facts exposed by a validated segment without exposing its constructor.
///
/// [`ValidatedVectorSegment`] dereferences to this view but deliberately has no `DerefMut`.
/// Callers get direct field access while the correlated proof remains impossible to forge or edit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VectorSegmentView<'facts> {
    /// Authority proved for every point.
    pub authority: VectorAuthority,
    /// Partition proved for every point.
    pub partition: PartitionId,
    /// Canonical narrowed point count committed by `id`.
    pub point_count: u8,
    /// Canonically ordered validated point lane.
    pub facts: &'facts [VectorPoint<'facts>],
    /// Identity committing to every field in this view.
    pub id: VectorSegmentId,
}

/// Copyable immutable segment selection derived from a validated point lane.
///
/// The fields stay private because the identity commits to the correlated authority, partition,
/// point count, entity order, and coordinates. Remote adapters may select by this descriptor, but
/// the descriptor alone does not prove that a remote candidate's coordinates are members of the
/// segment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VectorSegmentDescriptor(VectorSegmentSelection);

/// Portable immutable facts exposed by a validated segment descriptor.
///
/// The descriptor dereferences to this view without `DerefMut`, so adapters can use direct field
/// access while construction remains sealed behind validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VectorSegmentSelection {
    /// Authority bound into the segment identity.
    pub authority: VectorAuthority,
    /// Partition bound into the segment identity.
    pub partition: PartitionId,
    /// Exact canonical point count.
    pub point_count: u8,
    /// Identity of the complete ordered point lane.
    pub id: VectorSegmentId,
}

impl Deref for VectorSegmentDescriptor {
    type Target = VectorSegmentSelection;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'facts> ValidatedVectorSegment<'facts> {
    /// Validates one canonical borrowed segment and computes its identity without allocation.
    #[allow(
        clippy::result_large_err,
        reason = "cross-authority rejection retains both complete authority facts"
    )]
    pub fn try_new(
        authority: VectorAuthority,
        partition: PartitionId,
        facts: &'facts [VectorPoint<'facts>],
    ) -> Result<Self, VectorSegmentError> {
        let dimension = checked_vector_dimension(authority)?;
        if facts.len() > MAX_FACTS_PER_ROW {
            return Err(VectorSegmentError::TooManyFacts {
                maximum: MAX_FACTS_PER_ROW,
                observed: facts.len(),
            });
        }
        // The bounded admission proof above makes this narrowing lossless on every target before
        // the canonical stream widens the count back to its fixed u64 record.
        let fact_count = match u8::try_from(facts.len()) {
            Ok(count) => count,
            Err(_) => {
                return Err(VectorSegmentError::TooManyFacts {
                    maximum: MAX_FACTS_PER_ROW,
                    observed: facts.len(),
                });
            }
        };
        for (index, fact) in facts.iter().enumerate() {
            if fact.coordinates.len() != dimension {
                return Err(VectorSegmentError::FactDimension {
                    index,
                    expected: dimension,
                    observed: fact.coordinates.len(),
                });
            }
        }
        for (index, pair) in facts.windows(2).enumerate() {
            match pair[0].entity.raw.cmp(&pair[1].entity.raw) {
                core::cmp::Ordering::Less => {}
                core::cmp::Ordering::Equal => {
                    return Err(VectorSegmentError::DuplicateEntity {
                        first_index: index,
                        index: index + 1,
                        entity: pair[1].entity,
                    });
                }
                core::cmp::Ordering::Greater => {
                    return Err(VectorSegmentError::OutOfOrder {
                        index: index + 1,
                        previous: pair[0].entity,
                        observed: pair[1].entity,
                    });
                }
            }
        }

        Ok(Self(VectorSegmentView {
            authority,
            partition,
            point_count: fact_count,
            facts,
            id: vector_segment_id(authority, partition, fact_count, facts),
        }))
    }

    /// Derives a portable immutable selection without lending resident coordinates.
    #[must_use]
    pub const fn descriptor(&self) -> VectorSegmentDescriptor {
        VectorSegmentDescriptor(VectorSegmentSelection {
            authority: self.0.authority,
            partition: self.0.partition,
            point_count: self.0.point_count,
            id: self.0.id,
        })
    }
}

#[allow(
    clippy::result_large_err,
    reason = "dimension rejection preserves the complete authority evidence"
)]
fn checked_vector_dimension(authority: VectorAuthority) -> Result<usize, VectorSegmentError> {
    let dimension = usize::from(authority.dimension);
    if dimension == 0 || dimension > MAX_VECTOR_DIMENSION {
        return Err(VectorSegmentError::AuthorityDimension {
            maximum: MAX_VECTOR_DIMENSION,
            observed: dimension,
        });
    }
    Ok(dimension)
}

impl<'facts> AsRef<[VectorPoint<'facts>]> for ValidatedVectorSegment<'facts> {
    fn as_ref(&self) -> &[VectorPoint<'facts>] {
        self.facts
    }
}

impl<'facts> Deref for ValidatedVectorSegment<'facts> {
    type Target = VectorSegmentView<'facts>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// V2 deliberately moves this unpublished derived ID onto the restored foundation namespace.
// The version bump prevents an old Heart-era v1 value from being mistaken for this grammar.
const VECTOR_SEGMENT_VERSION: [u8; 23] = *b"nudox.vector.segment.v2";

struct CanonicalRecord<const BYTES: usize>([u8; BYTES]);

impl<const BYTES: usize> FixedCanonicalRecord<BYTES> for CanonicalRecord<BYTES> {
    fn canonical_bytes(&self) -> &[u8; BYTES] {
        &self.0
    }
}

fn vector_segment_id(
    authority: VectorAuthority,
    partition: PartitionId,
    fact_count: u8,
    facts: &[VectorPoint<'_>],
) -> VectorSegmentId {
    let mut hasher = ContentHasher::<IndexVectorSegmentDomain>::new();
    hasher.write_record(&CanonicalRecord(VECTOR_SEGMENT_VERSION));
    hasher.write_record(&CanonicalRecord(*authority.snapshot.as_ref()));
    hasher.write_record(&CanonicalRecord(*authority.model.as_ref()));
    hasher.write_record(&CanonicalRecord(authority.dimension.to_le_bytes()));
    hasher.write_record(&CanonicalRecord([authority.metric as u8]));
    hasher.write_record(&CanonicalRecord(partition.raw.to_le_bytes()));
    hasher.write_record(&CanonicalRecord((u64::from(fact_count)).to_le_bytes()));
    for fact in facts {
        hasher.write_record(&CanonicalRecord(fact.entity.raw.to_le_bytes()));
        for coordinate in fact.coordinates.iter().copied() {
            hasher.write_record(&CanonicalRecord(coordinate.to_le_bytes()));
        }
    }
    hasher.finalize()
}

/// Stable exact-vector result with complete model/metric provenance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VectorHit {
    /// Snapshot, model, dimension, and metric authority.
    pub authority: VectorAuthority,
    /// Source partition.
    pub partition: PartitionId,
    /// Semantic entity.
    pub entity: EntityId,
    /// Metric-specific deterministic score; smaller ranks first.
    pub score: i64,
}

/// Exact rejection from bounded vector admission or validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VectorQueryError {
    /// Authority shape exceeds the fixed scalar control.
    AuthorityDimension {
        /// Maximum supported dimension.
        maximum: usize,
        /// Rejected dimension.
        observed: usize,
    },
    /// Query coordinates do not match model authority.
    QueryDimension {
        /// Required coordinate count.
        expected: usize,
        /// Complete observed coordinate count.
        observed: usize,
    },
    /// Requested top-k cannot fit caller output; checked before facts.
    InsufficientOutput {
        /// Requested top-k.
        required: usize,
        /// Caller output slots.
        available: usize,
    },
    /// Selected partition fan-out exceeds its global bound.
    TooManySelected {
        /// Maximum fan-out.
        maximum: usize,
        /// Complete observed fan-out.
        observed: usize,
    },
    /// Supplied validated-segment fan-out exceeds its global bound.
    TooManySegments {
        /// Maximum segment count.
        maximum: usize,
        /// Complete observed segment count.
        observed: usize,
    },
    /// A sealed segment is pinned to a different vector authority.
    WrongSegmentAuthority {
        /// Zero-based sealed segment position.
        segment_index: usize,
        /// Query authority.
        expected: VectorAuthority,
        /// Authority sealed into the segment.
        observed: VectorAuthority,
    },
    /// A sealed segment partition was repeated.
    DuplicateSegment {
        /// First occurrence.
        first_index: usize,
        /// Later occurrence.
        index: usize,
        /// Repeated coordinate.
        partition: PartitionId,
    },
    /// A selected coordinate was repeated.
    DuplicateSelected {
        /// First occurrence.
        first_index: usize,
        /// Later occurrence.
        index: usize,
        /// Repeated coordinate.
        partition: PartitionId,
    },
}

/// Snapshot/model/metric-pinned complete or partial terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VectorQueryTerminal {
    /// Every selected partition was available, including an available empty segment.
    Complete {
        /// Snapshot, model, dimension, and metric authority.
        authority: VectorAuthority,
    },
    /// At least one selected partition was unavailable.
    Partial {
        /// Snapshot, model, dimension, and metric authority.
        authority: VectorAuthority,
        /// Exact missing coordinates in selection order.
        missing: MissingPartitions,
    },
}

impl VectorQueryTerminal {
    /// Returns the complete vector authority.
    #[must_use]
    pub const fn authority(self) -> VectorAuthority {
        match self {
            Self::Complete { authority } | Self::Partial { authority, .. } => authority,
        }
    }

    /// Returns true when at least one selected partition was unavailable.
    #[must_use]
    pub const fn is_partial(self) -> bool {
        matches!(self, Self::Partial { .. })
    }

    /// Borrows exact missing coordinates in selection order.
    #[must_use]
    pub fn missing(&self) -> &[PartitionId] {
        match self {
            Self::Complete { .. } => &[],
            Self::Partial { missing, .. } => missing,
        }
    }
}

/// Bounded exact-vector result and its typed terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VectorQueryOutcome {
    /// Number of initialized top-k slots.
    pub written: usize,
    /// Complete/partial typed terminal.
    pub terminal: VectorQueryTerminal,
}

/// Executes the scalar local oracle over authority-sealed borrowed vector segments.
///
/// The query never accepts an unproved point row: every point is reached only through a
/// [`ValidatedVectorSegment`], whose private owner pins the authority and partition used for the
/// returned hit. Segment authority is preflighted before the caller's output is cleared.
#[allow(
    clippy::result_large_err,
    reason = "query rejection preserves complete authority evidence"
)]
pub fn exact_vector_query(
    authority: VectorAuthority,
    selected: &[PartitionId],
    segments: &[ValidatedVectorSegment<'_>],
    query: &[i16],
    requested_limit: usize,
    output: &mut [Option<VectorHit>],
) -> Result<VectorQueryOutcome, VectorQueryError> {
    let dimension = usize::from(authority.dimension);
    if dimension == 0 || dimension > MAX_VECTOR_DIMENSION {
        return Err(VectorQueryError::AuthorityDimension {
            maximum: MAX_VECTOR_DIMENSION,
            observed: dimension,
        });
    }
    if query.len() != dimension {
        return Err(VectorQueryError::QueryDimension {
            expected: dimension,
            observed: query.len(),
        });
    }
    if requested_limit > output.len() {
        return Err(VectorQueryError::InsufficientOutput {
            required: requested_limit,
            available: output.len(),
        });
    }
    validate_vector_segments(authority, selected, segments)?;
    for slot in output.iter_mut().take(requested_limit) {
        *slot = None;
    }
    let mut written = 0;
    for segment in segments {
        if selected.contains(&segment.partition) {
            for fact in segment.facts.iter().copied() {
                let candidate = VectorHit {
                    authority,
                    partition: segment.partition,
                    entity: fact.entity,
                    score: score(authority.metric, query, fact.coordinates),
                };
                insert_top_k(output, requested_limit, &mut written, candidate);
            }
        }
    }
    Ok(VectorQueryOutcome {
        written,
        terminal: vector_terminal(authority, selected, segments),
    })
}

#[allow(
    clippy::result_large_err,
    reason = "segment validation rejection preserves complete authority evidence"
)]
fn validate_vector_segments(
    authority: VectorAuthority,
    selected: &[PartitionId],
    segments: &[ValidatedVectorSegment<'_>],
) -> Result<(), VectorQueryError> {
    if selected.len() > MAX_PARTITIONS {
        return Err(VectorQueryError::TooManySelected {
            maximum: MAX_PARTITIONS,
            observed: selected.len(),
        });
    }
    if segments.len() > MAX_PARTITIONS {
        return Err(VectorQueryError::TooManySegments {
            maximum: MAX_PARTITIONS,
            observed: segments.len(),
        });
    }
    for (segment_index, segment) in segments.iter().enumerate() {
        if segment.authority != authority {
            return Err(VectorQueryError::WrongSegmentAuthority {
                segment_index,
                expected: authority,
                observed: segment.authority,
            });
        }
    }
    reject_duplicate_partitions(selected, segments)?;
    Ok(())
}

#[allow(
    clippy::result_large_err,
    reason = "duplicate rejection uses the same exact query error taxonomy"
)]
fn reject_duplicate_partitions(
    selected: &[PartitionId],
    segments: &[ValidatedVectorSegment<'_>],
) -> Result<(), VectorQueryError> {
    for index in 0..selected.len() {
        for first_index in 0..index {
            if selected[first_index] == selected[index] {
                return Err(VectorQueryError::DuplicateSelected {
                    first_index,
                    index,
                    partition: selected[index],
                });
            }
        }
    }
    for index in 0..segments.len() {
        for first_index in 0..index {
            if segments[first_index].partition == segments[index].partition {
                return Err(VectorQueryError::DuplicateSegment {
                    first_index,
                    index,
                    partition: segments[index].partition,
                });
            }
        }
    }
    Ok(())
}

fn score(metric: Metric, query: &[i16], coordinates: &[i16]) -> i64 {
    query
        .iter()
        .zip(coordinates)
        .map(|(query_coordinate, fact_coordinate)| match metric {
            Metric::SquaredEuclidean => {
                let difference = i64::from(*query_coordinate) - i64::from(*fact_coordinate);
                difference * difference
            }
            Metric::NegativeDotProduct => {
                -(i64::from(*query_coordinate) * i64::from(*fact_coordinate))
            }
        })
        .sum()
}

fn insert_top_k(
    output: &mut [Option<VectorHit>],
    limit: usize,
    written: &mut usize,
    candidate: VectorHit,
) {
    if limit == 0 {
        return;
    }
    let occupied = (*written).min(limit);
    let mut insertion = occupied;
    for (index, current) in output.iter().copied().take(occupied).enumerate() {
        if current.is_some_and(|current| vector_hit_precedes(candidate, current)) {
            insertion = index;
            break;
        }
    }
    if insertion == limit {
        return;
    }
    let new_written = (occupied + 1).min(limit);
    for index in (insertion + 1..new_written).rev() {
        output[index] = output[index - 1];
    }
    output[insertion] = Some(candidate);
    *written = new_written;
}

fn vector_hit_precedes(left: VectorHit, right: VectorHit) -> bool {
    (left.score, left.entity.raw, left.partition.raw)
        < (right.score, right.entity.raw, right.partition.raw)
}

fn vector_terminal(
    authority: VectorAuthority,
    selected: &[PartitionId],
    segments: &[ValidatedVectorSegment<'_>],
) -> VectorQueryTerminal {
    let mut missing = [PartitionId::new(0); MAX_PARTITIONS];
    let mut missing_len = 0;
    for partition in selected.iter().copied() {
        if !segments
            .iter()
            .any(|segment| segment.partition == partition)
        {
            missing[missing_len] = partition;
            missing_len += 1;
        }
    }
    match MissingPartitions::from_prefix(missing, missing_len) {
        Some(missing) => VectorQueryTerminal::Partial { authority, missing },
        None => VectorQueryTerminal::Complete { authority },
    }
}
