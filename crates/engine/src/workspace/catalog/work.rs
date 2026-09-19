//! Bounded work accounting for catalog updates and admission.

use super::super::owner::WorkspaceError;

/// Exact persistent-tree and durable-CAS work performed by one catalog
/// operation. `tree_*` is path-copy work; `nodes_*` is relation-node CAS
/// admission. Neither counter includes unrelated workspace objects.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct CatalogWork {
    /// Canonical tree nodes visited while preparing an update or lookup.
    pub tree_nodes_read: usize,
    /// Canonical tree bytes read or re-encoded by the relation kernel.
    pub tree_bytes_read: usize,
    /// Canonical tree nodes copied by the update.
    pub tree_nodes_written: usize,
    /// Canonical tree bytes emitted by the update.
    pub tree_bytes_written: usize,
    /// Relation-node objects created by the store CAS.
    pub nodes_written: usize,
    /// Bytes created by the store CAS.
    pub bytes_written: usize,
}

impl CatalogWork {
    pub(super) fn add_lazy(
        &mut self,
        work: backend_version::LazyTreeWork,
    ) -> Result<(), WorkspaceError> {
        self.tree_nodes_read = self
            .tree_nodes_read
            .checked_add(work.loaded_nodes)
            .ok_or(WorkspaceError::Bounds)?;
        self.tree_nodes_written = self
            .tree_nodes_written
            .checked_add(work.rebuilt_nodes)
            .and_then(|value| value.checked_add(work.split_nodes))
            .ok_or(WorkspaceError::Bounds)?;
        self.tree_bytes_written = self
            .tree_bytes_written
            .checked_add(work.emitted_bytes)
            .ok_or(WorkspaceError::Bounds)?;
        Ok(())
    }

    pub(super) fn add_delta(
        &mut self,
        work: backend_version::DeltaWork,
    ) -> Result<(), WorkspaceError> {
        self.tree_nodes_read = self
            .tree_nodes_read
            .checked_add(work.visited_nodes)
            .ok_or(WorkspaceError::Bounds)?;
        self.tree_nodes_written = self
            .tree_nodes_written
            .checked_add(work.copied_nodes)
            .ok_or(WorkspaceError::Bounds)?;
        self.tree_bytes_written = self
            .tree_bytes_written
            .checked_add(work.encoded_bytes)
            .ok_or(WorkspaceError::Bounds)?;
        Ok(())
    }
}
