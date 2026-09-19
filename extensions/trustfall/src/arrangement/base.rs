//! Immutable graph arrangement base rows.

use crate::contracts::GraphRow;
use crate::delta::GraphState;
use crate::{Binding, Error};
use backend_version::CoverageWitness;
use std::sync::Arc;

/// Immutable graph rows at one exact semantic relation root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphBase {
    binding: Binding,
    coverage: CoverageWitness,
    rows: Arc<[GraphRow]>,
    bytes: usize,
}

impl GraphBase {
    /// Builds an immutable arrangement base from a complete graph state.
    ///
    /// # Errors
    ///
    /// Returns [`Error::IncompleteCoverage`] for a partial state or
    /// [`Error::SizeLimit`] when row byte accounting overflows.
    pub fn from_state(state: &GraphState) -> Result<Self, Error> {
        if !matches!(
            state.coverage(),
            CoverageWitness::Complete(_) | CoverageWitness::Closed(_)
        ) {
            return Err(Error::IncompleteCoverage);
        }
        let rows = state.iter().collect::<Vec<_>>();
        let bytes = rows.iter().try_fold(0usize, |bytes, row| {
            row.values.iter().try_fold(bytes, |bytes, value| {
                bytes.checked_add(value.len()).ok_or(Error::SizeLimit)
            })
        })?;
        Ok(Self {
            binding: state.binding(),
            coverage: state.coverage(),
            rows: Arc::from(rows),
            bytes,
        })
    }

    /// Returns the exact relation binding.
    #[must_use]
    pub const fn binding(&self) -> Binding {
        self.binding
    }

    /// Returns the complete state witness.
    #[must_use]
    pub const fn coverage(&self) -> CoverageWitness {
        self.coverage
    }

    /// Returns canonical rows in key order.
    #[must_use]
    pub fn rows(&self) -> &[GraphRow] {
        &self.rows
    }

    /// Returns an O(log n) point lookup in the retained sorted base.
    #[must_use]
    pub fn get(&self, key: u64) -> Option<&GraphRow> {
        self.rows
            .binary_search_by_key(&key, |row| row.key)
            .ok()
            .map(|index| &self.rows[index])
    }

    /// Returns base bytes retained by the arrangement.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }
}
