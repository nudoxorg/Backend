//! Immutable query arrangements over one accepted [`ViewRoot`].
//!
//! The view owns canonical row values. This module retains only stable row
//! identities and path-copied canonical secondary relations. A one-row
//! commit therefore visits affected tree paths and postings while untouched
//! canonical subtrees remain shared.

use crate::RowId;
use backend_flow::MaterializedWork;
use backend_version::TreeError;
use std::sync::atomic::{AtomicU64, Ordering};

mod query;
mod schema;
mod update;

use schema::{
    ChildRowKey, ChildrenRelation, ChildrenTree, DocumentIndexRelation, DocumentTree,
    NameIndexRelation, NameKey, NamePostingKey, NamePostingRelation, NamePostingTree, NameTree,
    PackageIndexRelation, PackageRowKey, PackageSymbolsRelation, PackageSymbolsTree, PackageTree,
    UnscopedIndexRelation, UnscopedTree, searchable_text, trigrams,
};

/// Structural work observed by one immutable library projection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QueryWork {
    /// Rows inspected while cold-loading or applying a checked view delta.
    pub indexed_rows: u64,
    /// Canonical tree nodes visited while locating changed paths.
    pub index_probes: u64,
    /// Canonical tree nodes copied for changed paths.
    pub index_copied_nodes: u64,
    /// Canonical tree nodes retained from an earlier arrangement.
    pub index_reused_nodes: u64,
    /// Ordered arrangement lookups performed by queries.
    pub seek_probes: u64,
    /// Rows materialized into query snapshots.
    pub output_rows: u64,
    /// Rows visited by a query-wide scan.
    pub scan_rows: u64,
    /// Rows reordered by a query operation.
    pub sort_rows: u64,
}

/// Shared atomic work counters for clones of one projection.
#[derive(Debug, Default)]
pub(crate) struct WorkCounters {
    indexed_rows: AtomicU64,
    index_probes: AtomicU64,
    index_copied_nodes: AtomicU64,
    index_reused_nodes: AtomicU64,
    seek_probes: AtomicU64,
    output_rows: AtomicU64,
    scan_rows: AtomicU64,
    sort_rows: AtomicU64,
}

impl WorkCounters {
    pub(crate) fn record_indexed(&self, rows: usize) {
        self.indexed_rows.fetch_add(rows as u64, Ordering::Relaxed);
    }

    pub(crate) fn record_materialized(&self, work: MaterializedWork) {
        self.index_probes
            .fetch_add(work.visited_nodes as u64, Ordering::Relaxed);
        self.index_copied_nodes
            .fetch_add(work.copied_nodes as u64, Ordering::Relaxed);
        self.index_reused_nodes
            .fetch_add(work.reused_nodes as u64, Ordering::Relaxed);
    }

    pub(crate) fn record_seek(&self) {
        self.seek_probes.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_output(&self, rows: usize) {
        self.output_rows.fetch_add(rows as u64, Ordering::Relaxed);
    }

    pub(crate) fn snapshot(&self) -> QueryWork {
        QueryWork {
            indexed_rows: self.indexed_rows.load(Ordering::Relaxed),
            index_probes: self.index_probes.load(Ordering::Relaxed),
            index_copied_nodes: self.index_copied_nodes.load(Ordering::Relaxed),
            index_reused_nodes: self.index_reused_nodes.load(Ordering::Relaxed),
            seek_probes: self.seek_probes.load(Ordering::Relaxed),
            output_rows: self.output_rows.load(Ordering::Relaxed),
            scan_rows: self.scan_rows.load(Ordering::Relaxed),
            sort_rows: self.sort_rows.load(Ordering::Relaxed),
        }
    }
}

/// Failure while constructing or incrementally updating an arrangement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ArrangementError(TreeError);

impl From<TreeError> for ArrangementError {
    fn from(error: TreeError) -> Self {
        Self(error)
    }
}

impl std::fmt::Display for ArrangementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for ArrangementError {}

/// Immutable arrangement indexes for one view snapshot.
#[derive(Clone, Debug)]
pub(crate) struct ProjectionArrangement {
    // `None` is only the structurally infallible empty library. Cold-loads
    // and accepted transitions always install a checked tree in every slot.
    documents: Option<DocumentTree>,
    packages: Option<PackageTree>,
    unscoped_symbols: Option<UnscopedTree>,
    names: Option<NameTree>,
    name_postings: Option<NamePostingTree>,
    package_symbols: Option<PackageSymbolsTree>,
    children: Option<ChildrenTree>,
}

/// One bounded row-identity page returned directly from a retained
/// arrangement. The extra candidate is consumed only to determine whether a
/// continuation cursor is needed.
#[derive(Debug)]
pub(crate) struct ArrangementPage {
    /// Stable row identities in the requested order.
    pub ids: Vec<RowId>,
    /// Whether another bounded page follows.
    pub has_more: bool,
}
