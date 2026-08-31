use core::ops::Deref;

use nudox_ir_vocab::EntityId;

use crate::{GraphAuthority, MAX_PARTITIONS, MissingPartitions, PartitionId};

const MAX_EDGES_PER_ROW: usize = 16;

/// One immutable directed graph edge with complete projection authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphEdge {
    /// Snapshot and projection recipe that give this edge meaning.
    pub authority: GraphAuthority,
    /// Immutable partition containing this edge.
    pub partition: PartitionId,
    /// Entity at the edge's origin.
    pub source: EntityId,
    /// Entity reached by the edge.
    pub target: EntityId,
}

impl GraphEdge {
    /// Creates one edge fact. A graph view validates its authority before admission.
    #[must_use]
    pub const fn new(
        authority: GraphAuthority,
        partition: PartitionId,
        source: EntityId,
        target: EntityId,
    ) -> Self {
        Self {
            authority,
            partition,
            source,
            target,
        }
    }
}

/// Borrowed graph facts supplied by one immutable partition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphRow<'facts> {
    /// Partition coordinate claimed by this row.
    pub partition: PartitionId,
    /// Borrowed edge facts supplied by the partition.
    pub edges: &'facts [GraphEdge],
}

/// Exact reason a borrowed graph was rejected before entering the query adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdmissionError {
    /// Global fan-out bound; checked before duplicate or edge work.
    TooManyRows {
        /// Maximum selected partition rows.
        maximum: usize,
        /// Complete observed row count.
        observed: usize,
    },
    /// Per-row edge bound; checked before duplicate work.
    TooManyEdges {
        /// Rejected row coordinate.
        row_index: usize,
        /// Maximum facts admitted from one partition.
        maximum: usize,
        /// Complete observed fact count.
        observed: usize,
    },
    /// Two rows claim the same partition.
    DuplicatePartition {
        /// First row owning the partition.
        first_row_index: usize,
        /// Later duplicate row.
        row_index: usize,
        /// Repeated coordinate.
        partition: PartitionId,
    },
    /// A row coordinate and edge coordinate disagree.
    WrongEdgePartition {
        /// Row containing the rejected edge.
        row_index: usize,
        /// Edge coordinate inside the row.
        edge_index: usize,
        /// Row partition.
        expected: PartitionId,
        /// Edge partition.
        observed: PartitionId,
    },
    /// An edge belongs to another snapshot or graph recipe.
    WrongGraphAuthority {
        /// Row containing the rejected edge.
        row_index: usize,
        /// Edge coordinate inside the row.
        edge_index: usize,
        /// Pinned query authority.
        expected: GraphAuthority,
        /// Rejected edge authority.
        observed: GraphAuthority,
    },
}

/// Validated borrowed graph view consumed by a graph adapter.
///
/// The authority and rows remain private because they are correlated by the
/// validation performed by [`Self::try_new`]. Constructing a view by mixing a
/// row slice with an unrelated authority is therefore not part of the public
/// API.
///
/// ```compile_fail
/// use nudox_index_graph_vector::{GraphAuthority, GraphRow, ValidatedGraphView};
///
/// fn forge<'facts>(
///     authority: GraphAuthority,
///     rows: &'facts [GraphRow<'facts>],
/// ) {
///     let _ = ValidatedGraphView { authority, rows };
/// }
/// ```
#[derive(Debug, Eq, PartialEq)]
pub struct ValidatedGraphView<'facts> {
    authority: GraphAuthority,
    rows: &'facts [GraphRow<'facts>],
}

impl<'facts> ValidatedGraphView<'facts> {
    /// Validates global work bounds before scanning duplicates or facts.
    pub fn try_new(
        authority: GraphAuthority,
        rows: &'facts [GraphRow<'facts>],
    ) -> Result<Self, AdmissionError> {
        if rows.len() > MAX_PARTITIONS {
            return Err(AdmissionError::TooManyRows {
                maximum: MAX_PARTITIONS,
                observed: rows.len(),
            });
        }
        for (row_index, row) in rows.iter().enumerate() {
            if row.edges.len() > MAX_EDGES_PER_ROW {
                return Err(AdmissionError::TooManyEdges {
                    row_index,
                    maximum: MAX_EDGES_PER_ROW,
                    observed: row.edges.len(),
                });
            }
        }
        for row_index in 0..rows.len() {
            for first_row_index in 0..row_index {
                if rows[first_row_index].partition == rows[row_index].partition {
                    return Err(AdmissionError::DuplicatePartition {
                        first_row_index,
                        row_index,
                        partition: rows[row_index].partition,
                    });
                }
            }
        }
        for (row_index, row) in rows.iter().copied().enumerate() {
            for (edge_index, edge) in row.edges.iter().copied().enumerate() {
                if edge.authority != authority {
                    return Err(AdmissionError::WrongGraphAuthority {
                        row_index,
                        edge_index,
                        expected: authority,
                        observed: edge.authority,
                    });
                }
                if edge.partition != row.partition {
                    return Err(AdmissionError::WrongEdgePartition {
                        row_index,
                        edge_index,
                        expected: row.partition,
                        observed: edge.partition,
                    });
                }
            }
        }
        Ok(Self { authority, rows })
    }

    /// Returns the authority proved for every borrowed row.
    #[must_use]
    pub const fn authority(&self) -> GraphAuthority {
        self.authority
    }

    /// Returns all neighbors from selected available partitions in stable entity order.
    pub fn neighbors(
        &self,
        selected: &[PartitionId],
        source: EntityId,
        output: &mut [Option<GraphHit>],
    ) -> Result<GraphQueryOutcome, GraphQueryError> {
        validate_selection(selected)?;
        let mut required = 0_usize;
        for row in self.rows.iter().copied() {
            if selection_contains(selected, row.partition) {
                for edge in row.edges.iter().copied() {
                    if edge.source == source {
                        required += 1;
                    }
                }
            }
        }
        if output.len() < required {
            return Err(GraphQueryError::InsufficientOutput {
                required,
                available: output.len(),
            });
        }
        let mut written = 0;
        for row in self.rows.iter().copied() {
            if selection_contains(selected, row.partition) {
                for edge in row.edges.iter().copied() {
                    if edge.source == source {
                        output[written] = Some(GraphHit {
                            authority: self.authority,
                            partition: row.partition,
                            entity: edge.target,
                        });
                        written += 1;
                    }
                }
            }
        }
        insertion_sort_hits(&mut output[..written]);
        Ok(GraphQueryOutcome {
            written,
            terminal: graph_terminal(self.authority, selected, self.rows),
        })
    }
}

impl<'facts> AsRef<[GraphRow<'facts>]> for ValidatedGraphView<'facts> {
    fn as_ref(&self) -> &[GraphRow<'facts>] {
        self.rows
    }
}

impl<'facts> Deref for ValidatedGraphView<'facts> {
    type Target = [GraphRow<'facts>];

    fn deref(&self) -> &Self::Target {
        self.rows
    }
}

/// Stable graph result carrying all authority required for interpretation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphHit {
    /// Snapshot and projection recipe that give this hit meaning.
    pub authority: GraphAuthority,
    /// Source partition for result provenance.
    pub partition: PartitionId,
    /// Neighboring entity returned by the query.
    pub entity: EntityId,
}

/// Exact graph-query validation or output rejection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphQueryError {
    /// Selected fan-out exceeds the global bound.
    TooManySelected {
        /// Maximum fan-out.
        maximum: usize,
        /// Complete observed fan-out.
        observed: usize,
    },
    /// Two selected coordinates are identical.
    DuplicateSelected {
        /// First occurrence.
        first_index: usize,
        /// Later occurrence.
        index: usize,
        /// Repeated partition.
        partition: PartitionId,
    },
    /// Caller output cannot retain the complete result.
    InsufficientOutput {
        /// Complete result count.
        required: usize,
        /// Caller-provided slots.
        available: usize,
    },
}

/// Snapshot-pinned complete or exact-partial graph terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphQueryTerminal {
    /// Every selected partition was available, including an available empty row.
    Complete {
        /// Snapshot and projection recipe for the result.
        authority: GraphAuthority,
    },
    /// At least one selected partition was unavailable.
    Partial {
        /// Snapshot and projection recipe for the partial result.
        authority: GraphAuthority,
        /// Exact missing coordinates in the caller's selection order.
        missing: MissingPartitions,
    },
}

impl GraphQueryTerminal {
    /// Returns the complete graph authority.
    #[must_use]
    pub const fn authority(self) -> GraphAuthority {
        match self {
            Self::Complete { authority } | Self::Partial { authority, .. } => authority,
        }
    }

    /// Returns true when at least one selected partition was unavailable.
    #[must_use]
    pub const fn is_partial(self) -> bool {
        matches!(self, Self::Partial { .. })
    }

    /// Borrows the exact missing coordinates in selection order.
    #[must_use]
    pub fn missing(&self) -> &[PartitionId] {
        match self {
            Self::Complete { .. } => &[],
            Self::Partial { missing, .. } => missing,
        }
    }
}

/// Bounded graph result and its exact terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphQueryOutcome {
    /// Number of initialized result slots.
    pub written: usize,
    /// Pinned complete/partial terminal.
    pub terminal: GraphQueryTerminal,
}

fn validate_selection(selected: &[PartitionId]) -> Result<(), GraphQueryError> {
    if selected.len() > MAX_PARTITIONS {
        return Err(GraphQueryError::TooManySelected {
            maximum: MAX_PARTITIONS,
            observed: selected.len(),
        });
    }
    for index in 0..selected.len() {
        for first_index in 0..index {
            if selected[first_index] == selected[index] {
                return Err(GraphQueryError::DuplicateSelected {
                    first_index,
                    index,
                    partition: selected[index],
                });
            }
        }
    }
    Ok(())
}

fn selection_contains(selected: &[PartitionId], partition: PartitionId) -> bool {
    selected.contains(&partition)
}

fn graph_terminal(
    authority: GraphAuthority,
    selected: &[PartitionId],
    rows: &[GraphRow<'_>],
) -> GraphQueryTerminal {
    let mut missing = [PartitionId::new(0); MAX_PARTITIONS];
    let mut missing_len = 0;
    for partition in selected.iter().copied() {
        if !rows.iter().any(|row| row.partition == partition) {
            missing[missing_len] = partition;
            missing_len += 1;
        }
    }
    match MissingPartitions::from_prefix(missing, missing_len) {
        Some(missing) => GraphQueryTerminal::Partial { authority, missing },
        None => GraphQueryTerminal::Complete { authority },
    }
}

fn insertion_sort_hits(hits: &mut [Option<GraphHit>]) {
    for index in 1..hits.len() {
        let Some(candidate) = hits[index] else {
            continue;
        };
        let mut insertion = index;
        while insertion > 0 {
            let Some(previous) = hits[insertion - 1] else {
                break;
            };
            if graph_hit_precedes(previous, candidate) {
                break;
            }
            hits[insertion] = Some(previous);
            insertion -= 1;
        }
        hits[insertion] = Some(candidate);
    }
}

fn graph_hit_precedes(left: GraphHit, right: GraphHit) -> bool {
    (left.entity.raw, left.partition.raw) <= (right.entity.raw, right.partition.raw)
}
