use nudox_ir_vocab::EntityId;

use crate::{GraphAuthority, MAX_PARTITIONS, PartitionId};

const MAX_EDGES_PER_ROW: usize = 16;

/// One immutable directed graph edge with complete projection authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphEdge {
    authority: GraphAuthority,
    partition: PartitionId,
    source: EntityId,
    target: EntityId,
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

    /// Returns the complete immutable graph authority.
    #[must_use]
    pub const fn authority(self) -> GraphAuthority {
        self.authority
    }

    /// Returns the owning partition.
    #[must_use]
    pub const fn partition(self) -> PartitionId {
        self.partition
    }

    /// Returns the source entity.
    #[must_use]
    pub const fn source(self) -> EntityId {
        self.source
    }

    /// Returns the target entity.
    #[must_use]
    pub const fn target(self) -> EntityId {
        self.target
    }
}

/// Borrowed graph facts supplied by one immutable partition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphRow<'facts> {
    partition: PartitionId,
    edges: &'facts [GraphEdge],
}

impl<'facts> GraphRow<'facts> {
    /// Binds a complete borrowed edge slice to its partition coordinate.
    #[must_use]
    pub const fn new(partition: PartitionId, edges: &'facts [GraphEdge]) -> Self {
        Self { partition, edges }
    }

    /// Returns the partition coordinate.
    #[must_use]
    pub const fn partition(self) -> PartitionId {
        self.partition
    }

    /// Lends the complete immutable edge slice.
    #[must_use]
    pub const fn edges(self) -> &'facts [GraphEdge] {
        self.edges
    }
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

/// Validated borrowed graph view consumed by the nested Trustfall adapter.
#[derive(Debug, Eq, PartialEq)]
pub struct TrustfallGraph<'facts> {
    authority: GraphAuthority,
    rows: &'facts [GraphRow<'facts>],
}

impl<'facts> TrustfallGraph<'facts> {
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

    /// Returns the graph authority pinned by validation.
    #[must_use]
    pub const fn authority(&self) -> GraphAuthority {
        self.authority
    }

    /// Lends validated partition rows without copying their edges.
    #[must_use]
    pub const fn rows(&self) -> &'facts [GraphRow<'facts>] {
        self.rows
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

/// Stable graph result carrying all authority required for interpretation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphHit {
    authority: GraphAuthority,
    partition: PartitionId,
    entity: EntityId,
}

impl GraphHit {
    /// Returns the complete graph authority.
    #[must_use]
    pub const fn authority(self) -> GraphAuthority {
        self.authority
    }

    /// Returns the source partition for provenance.
    #[must_use]
    pub const fn partition(self) -> PartitionId {
        self.partition
    }

    /// Returns the neighboring entity.
    #[must_use]
    pub const fn entity(self) -> EntityId {
        self.entity
    }
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
pub struct GraphQueryTerminal {
    authority: GraphAuthority,
    missing: [Option<PartitionId>; MAX_PARTITIONS],
    missing_len: usize,
}

impl GraphQueryTerminal {
    /// Returns the complete graph authority.
    #[must_use]
    pub const fn authority(self) -> GraphAuthority {
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

/// Bounded graph result and its exact terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphQueryOutcome {
    written: usize,
    terminal: GraphQueryTerminal,
}

impl GraphQueryOutcome {
    /// Returns the number of initialized result slots.
    #[must_use]
    pub const fn written(self) -> usize {
        self.written
    }

    /// Returns the pinned complete/partial terminal.
    #[must_use]
    pub const fn terminal(self) -> GraphQueryTerminal {
        self.terminal
    }
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
    let mut missing = [None; MAX_PARTITIONS];
    let mut missing_len = 0;
    for partition in selected.iter().copied() {
        if !rows.iter().any(|row| row.partition == partition) {
            missing[missing_len] = Some(partition);
            missing_len += 1;
        }
    }
    GraphQueryTerminal {
        authority,
        missing,
        missing_len,
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
