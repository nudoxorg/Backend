use nudox_ir_vocab::EntityId;

use crate::{MAX_PARTITIONS, Metric, PartitionId, VectorAuthority};

const MAX_VECTOR_DIMENSION: usize = 16;
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

/// One borrowed exact-vector projection fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VectorFact<'coordinates> {
    authority: VectorAuthority,
    partition: PartitionId,
    entity: EntityId,
    coordinates: &'coordinates [i16],
}

impl<'coordinates> VectorFact<'coordinates> {
    /// Creates a fact that is revalidated against query authority before ranking.
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

/// Borrowed vector facts supplied by one immutable partition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VectorRow<'facts, 'coordinates> {
    partition: PartitionId,
    facts: &'facts [VectorFact<'coordinates>],
}

impl<'facts, 'coordinates> VectorRow<'facts, 'coordinates> {
    /// Binds a complete borrowed fact slice to one partition coordinate.
    #[must_use]
    pub const fn new(partition: PartitionId, facts: &'facts [VectorFact<'coordinates>]) -> Self {
        Self { partition, facts }
    }
}

/// Stable exact-vector result with complete model/metric provenance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VectorHit {
    authority: VectorAuthority,
    partition: PartitionId,
    entity: EntityId,
    score: i64,
}

impl VectorHit {
    /// Returns snapshot, model, dimension, and metric authority.
    #[must_use]
    pub const fn authority(self) -> VectorAuthority {
        self.authority
    }

    /// Returns the source partition.
    #[must_use]
    pub const fn partition(self) -> PartitionId {
        self.partition
    }

    /// Returns the semantic entity.
    #[must_use]
    pub const fn entity(self) -> EntityId {
        self.entity
    }

    /// Returns the metric-specific deterministic score; smaller ranks first.
    #[must_use]
    pub const fn score(self) -> i64 {
        self.score
    }
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
    /// Selected or available fan-out exceeds its global bound.
    TooManyRows {
        /// Maximum fan-out.
        maximum: usize,
        /// Complete observed fan-out.
        observed: usize,
    },
    /// Per-row fact bound; checked before duplicate work.
    TooManyFacts {
        /// Rejected row.
        row_index: usize,
        /// Maximum facts in one row.
        maximum: usize,
        /// Complete observed fact count.
        observed: usize,
    },
    /// A row coordinate was repeated.
    DuplicateRow {
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
    /// A fact belongs to another snapshot.
    WrongSnapshot {
        /// Rejected row.
        row_index: usize,
        /// Rejected fact.
        fact_index: usize,
        /// Pinned query snapshot.
        expected: nudox_index_vocab::IndexSnapshotId,
        /// Rejected fact snapshot.
        observed: nudox_index_vocab::IndexSnapshotId,
    },
    /// A fact belongs to another embedding model.
    WrongModel {
        /// Rejected row.
        row_index: usize,
        /// Rejected fact.
        fact_index: usize,
        /// Query model.
        expected: crate::ModelId,
        /// Rejected model.
        observed: crate::ModelId,
    },
    /// A fact belongs to another model dimension.
    WrongModelDimension {
        /// Rejected row.
        row_index: usize,
        /// Rejected fact.
        fact_index: usize,
        /// Query dimension.
        expected: u16,
        /// Rejected dimension.
        observed: u16,
    },
    /// A fact belongs to another distance metric.
    WrongMetric {
        /// Rejected row.
        row_index: usize,
        /// Rejected fact.
        fact_index: usize,
        /// Query metric.
        expected: Metric,
        /// Rejected metric.
        observed: Metric,
    },
    /// A row coordinate and fact coordinate disagree.
    WrongPartition {
        /// Rejected row.
        row_index: usize,
        /// Rejected fact.
        fact_index: usize,
        /// Row coordinate.
        expected: PartitionId,
        /// Fact coordinate.
        observed: PartitionId,
    },
    /// A fact has the wrong coordinate count.
    FactDimension {
        /// Rejected row.
        row_index: usize,
        /// Rejected fact.
        fact_index: usize,
        /// Required coordinate count.
        expected: usize,
        /// Complete observed coordinate count.
        observed: usize,
    },
}

/// Snapshot/model/metric-pinned complete or partial terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VectorQueryTerminal {
    authority: VectorAuthority,
    missing: [Option<PartitionId>; MAX_PARTITIONS],
    missing_len: usize,
}

impl VectorQueryTerminal {
    /// Returns the complete vector authority.
    #[must_use]
    pub const fn authority(self) -> VectorAuthority {
        self.authority
    }

    /// Returns true when at least one selected partition was unavailable.
    #[must_use]
    pub const fn is_partial(self) -> bool {
        self.missing_len != 0
    }

    /// Returns the exact missing count.
    #[must_use]
    pub const fn missing_len(self) -> usize {
        self.missing_len
    }

    /// Returns one exact missing coordinate by semantic position.
    #[must_use]
    pub const fn missing_at(self, index: usize) -> Option<PartitionId> {
        if index < self.missing_len {
            self.missing[index]
        } else {
            None
        }
    }
}

/// Bounded exact-vector result and its typed terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VectorQueryOutcome {
    written: usize,
    terminal: VectorQueryTerminal,
}

impl VectorQueryOutcome {
    /// Returns the number of initialized top-k slots.
    #[must_use]
    pub const fn written(self) -> usize {
        self.written
    }

    /// Returns the complete/partial typed terminal.
    #[must_use]
    pub const fn terminal(self) -> VectorQueryTerminal {
        self.terminal
    }
}

/// Executes the scalar local oracle over borrowed immutable vector facts.
pub fn exact_vector_query(
    authority: VectorAuthority,
    selected: &[PartitionId],
    rows: &[VectorRow<'_, '_>],
    query: &[i16],
    requested_limit: usize,
    output: &mut [Option<VectorHit>],
) -> Result<VectorQueryOutcome, VectorQueryError> {
    let dimension = usize::from(authority.dimension());
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
    validate_vector_rows(authority, selected, rows, dimension)?;
    for slot in output.iter_mut().take(requested_limit) {
        *slot = None;
    }
    let mut written = 0;
    for row in rows.iter().copied() {
        if selected.contains(&row.partition) {
            for fact in row.facts.iter().copied() {
                let candidate = VectorHit {
                    authority,
                    partition: row.partition,
                    entity: fact.entity,
                    score: score(authority.metric(), query, fact.coordinates),
                };
                insert_top_k(output, requested_limit, &mut written, candidate);
            }
        }
    }
    Ok(VectorQueryOutcome {
        written,
        terminal: vector_terminal(authority, selected, rows),
    })
}

fn validate_vector_rows(
    authority: VectorAuthority,
    selected: &[PartitionId],
    rows: &[VectorRow<'_, '_>],
    dimension: usize,
) -> Result<(), VectorQueryError> {
    if selected.len() > MAX_PARTITIONS {
        return Err(VectorQueryError::TooManyRows {
            maximum: MAX_PARTITIONS,
            observed: selected.len(),
        });
    }
    if rows.len() > MAX_PARTITIONS {
        return Err(VectorQueryError::TooManyRows {
            maximum: MAX_PARTITIONS,
            observed: rows.len(),
        });
    }
    for (row_index, row) in rows.iter().enumerate() {
        if row.facts.len() > MAX_FACTS_PER_ROW {
            return Err(VectorQueryError::TooManyFacts {
                row_index,
                maximum: MAX_FACTS_PER_ROW,
                observed: row.facts.len(),
            });
        }
    }
    reject_duplicate_partitions(selected, rows)?;
    for (row_index, row) in rows.iter().enumerate() {
        for (fact_index, fact) in row.facts.iter().enumerate() {
            if fact.authority.snapshot() != authority.snapshot() {
                return Err(VectorQueryError::WrongSnapshot {
                    row_index,
                    fact_index,
                    expected: authority.snapshot(),
                    observed: fact.authority.snapshot(),
                });
            }
            if fact.authority.model() != authority.model() {
                return Err(VectorQueryError::WrongModel {
                    row_index,
                    fact_index,
                    expected: authority.model(),
                    observed: fact.authority.model(),
                });
            }
            if fact.authority.dimension() != authority.dimension() {
                return Err(VectorQueryError::WrongModelDimension {
                    row_index,
                    fact_index,
                    expected: authority.dimension(),
                    observed: fact.authority.dimension(),
                });
            }
            if fact.authority.metric() != authority.metric() {
                return Err(VectorQueryError::WrongMetric {
                    row_index,
                    fact_index,
                    expected: authority.metric(),
                    observed: fact.authority.metric(),
                });
            }
            if fact.partition != row.partition {
                return Err(VectorQueryError::WrongPartition {
                    row_index,
                    fact_index,
                    expected: row.partition,
                    observed: fact.partition,
                });
            }
            if fact.coordinates.len() != dimension {
                return Err(VectorQueryError::FactDimension {
                    row_index,
                    fact_index,
                    expected: dimension,
                    observed: fact.coordinates.len(),
                });
            }
        }
    }
    Ok(())
}

fn reject_duplicate_partitions(
    selected: &[PartitionId],
    rows: &[VectorRow<'_, '_>],
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
    for index in 0..rows.len() {
        for first_index in 0..index {
            if rows[first_index].partition == rows[index].partition {
                return Err(VectorQueryError::DuplicateRow {
                    first_index,
                    index,
                    partition: rows[index].partition,
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
    rows: &[VectorRow<'_, '_>],
) -> VectorQueryTerminal {
    let mut missing = [None; MAX_PARTITIONS];
    let mut missing_len = 0;
    for partition in selected.iter().copied() {
        if !rows.iter().any(|row| row.partition == partition) {
            missing[missing_len] = Some(partition);
            missing_len += 1;
        }
    }
    VectorQueryTerminal {
        authority,
        missing,
        missing_len,
    }
}
