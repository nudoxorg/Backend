use crate::SemanticError;

/// Structural work performed by occurrence-ledger updates.
///
/// The counters describe the actual bounded work of a prepared batch.  In
/// particular, `occurrence_probes` and `edge_probes` count ordered-map
/// traversals, while `occurrence_updates` and `affected_edges` count only
/// entries whose retained value changed.  A failed batch does not publish
/// counter changes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OccurrenceWorkCounters {
    /// Input delta rows inspected.
    pub input_rows: u64,
    /// Ordered occurrence-tree nodes traversed while preparing and publishing.
    pub occurrence_probes: u64,
    /// Occurrence rows inserted, changed, or removed at commit.
    pub occurrence_updates: u64,
    /// Distinct logical edges whose support changed.
    pub affected_edges: u64,
    /// Edge-tree traversals performed while preparing and publishing.
    pub edge_probes: u64,
    /// Persistent edge-tree nodes copied for the new state.
    pub edge_nodes: u64,
    /// Persistent occurrence-tree nodes copied for the new state.
    pub occurrence_nodes: u64,
}

impl OccurrenceWorkCounters {
    /// Returns the number of changed occurrence rows.
    #[must_use]
    pub const fn changed_occurrences(self) -> u64 {
        self.occurrence_updates
    }

    /// Returns the number of changed logical edges.
    #[must_use]
    pub const fn changed_edges(self) -> u64 {
        self.affected_edges
    }

    /// Returns the number of copied persistent edge-tree nodes.
    #[must_use]
    pub const fn copied_nodes(self) -> u64 {
        self.edge_nodes
    }

    /// Returns the number of copied persistent occurrence-tree nodes.
    #[must_use]
    pub const fn copied_occurrence_nodes(self) -> u64 {
        self.occurrence_nodes
    }

    pub(super) fn checked_add_assign(&mut self, other: Self) -> Result<(), SemanticError> {
        self.input_rows = self
            .input_rows
            .checked_add(other.input_rows)
            .ok_or(SemanticError::Overflow)?;
        self.occurrence_probes = self
            .occurrence_probes
            .checked_add(other.occurrence_probes)
            .ok_or(SemanticError::Overflow)?;
        self.occurrence_updates = self
            .occurrence_updates
            .checked_add(other.occurrence_updates)
            .ok_or(SemanticError::Overflow)?;
        self.affected_edges = self
            .affected_edges
            .checked_add(other.affected_edges)
            .ok_or(SemanticError::Overflow)?;
        self.edge_probes = self
            .edge_probes
            .checked_add(other.edge_probes)
            .ok_or(SemanticError::Overflow)?;
        self.edge_nodes = self
            .edge_nodes
            .checked_add(other.edge_nodes)
            .ok_or(SemanticError::Overflow)?;
        self.occurrence_nodes = self
            .occurrence_nodes
            .checked_add(other.occurrence_nodes)
            .ok_or(SemanticError::Overflow)?;
        Ok(())
    }
}
