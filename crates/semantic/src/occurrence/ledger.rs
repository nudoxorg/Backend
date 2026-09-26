use super::edge::{EdgeSet, OccurrenceRows, RowState, map_row_delta_error, map_row_state_error};
use super::work::OccurrenceWorkCounters;
use crate::{
    Edge, EdgeKey, EdgeKind, FacetCoverage, OccurrenceAddress, OccurrenceFact, SemanticError,
};
use backend_flow::Delta as FlowDelta;
use backend_version::{MapChange, prepare_delta_with_state};
use std::collections::BTreeMap;

mod apply;

type PendingRows = BTreeMap<OccurrenceAddress, (OccurrenceFact, i64)>;

/// A weighted occurrence ledger used for safe occurrence retractions.
///
/// Retained rows live in the backend-version persistent relation tree.  A
/// ledger clone therefore shares its immutable row root, while each prepared
/// batch publishes a checked path-copy target only after edge and row
/// validation have completed.
#[derive(Clone, Debug)]
pub struct OccurrenceLedger {
    coverage: FacetCoverage,
    rows: RowState,
    row_count: usize,
    edges: EdgeSet,
    work: OccurrenceWorkCounters,
}

impl PartialEq for OccurrenceLedger {
    fn eq(&self, other: &Self) -> bool {
        self.coverage == other.coverage && self.rows.root() == other.rows.root()
    }
}

impl Eq for OccurrenceLedger {}
