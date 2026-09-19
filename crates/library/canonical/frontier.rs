//! Exact branch/log/schema/root execution frontier.

use super::schema::{BranchKey, LogKey, ViewStateRoot};

/// Stable cursor frontier binding branch, log, schema, root, and sequence.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Frontier {
    /// Branch whose log is being observed.
    pub branch: BranchKey,
    /// Log whose sequence is being observed.
    pub log: LogKey,
    /// Command/view schema version.
    pub schema: u16,
    /// Root observed at this sequence.
    pub root: ViewStateRoot,
    /// Next sequence after the observed transition.
    pub sequence: u64,
}

impl Frontier {
    /// Creates a frontier from its complete logical bindings.
    #[must_use]
    pub const fn new(
        branch: BranchKey,
        log: LogKey,
        schema: u16,
        root: ViewStateRoot,
        sequence: u64,
    ) -> Self {
        Self {
            branch,
            log,
            schema,
            root,
            sequence,
        }
    }

    /// Projects this product cursor position onto the flow execution frontier
    /// used by incremental arrangements.
    ///
    /// The product sequence is an epoch boundary and therefore maps to a
    /// singleton flow time with iteration zero. The relation root remains
    /// available through [`backend_flow::BoundFrontier`] at the view seam.
    #[must_use]
    pub fn flow(&self) -> backend_flow::Frontier {
        backend_flow::Frontier::new(backend_flow::Time::new(
            backend_flow::Epoch(self.sequence),
            0,
        ))
    }
}
