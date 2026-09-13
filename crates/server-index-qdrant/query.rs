//! Defines query behavior for `server-index-qdrant`, whose purpose is to adapt typed vector authorities and queries to the Qdrant service.
//! This module owns the query invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Exact Qdrant query admission, parsing, and deterministic result publication.

use server_index_graph_vector::VectorSegmentDescriptor;

use super::{
    QdrantBlockingAdapter, admission,
    contract::{QdrantCandidate, QdrantError, QueryCandidateCount, RequestPhase},
    limits::QUERY_POINTS_PATH,
    scoring,
    transport::{self, Method},
    wire,
};

impl QdrantBlockingAdapter {
    /// Queries Qdrant within the caller's exact immutable segment selection.
    pub fn query(
        &self,
        selected: &[VectorSegmentDescriptor],
        query_coordinates: &[i16],
        requested_limit: usize,
        output: &mut [Option<QdrantCandidate>],
    ) -> Result<QueryCandidateCount, QdrantError> {
        admission::validate_query(
            self.authority,
            selected,
            query_coordinates,
            requested_limit,
            output.len(),
        )?;
        for slot in output.iter_mut().take(requested_limit) {
            *slot = None;
        }
        if selected.is_empty() {
            return Ok(0.into());
        }
        let response = self.request_json(
            RequestPhase::QueryPoints,
            Method::Post,
            &self.url(QUERY_POINTS_PATH),
            wire::QueryRequest::new(self.authority, selected, query_coordinates),
        )?;
        if !transport::is_success(response.status) {
            return Err(transport::status_error(RequestPhase::QueryPoints, response));
        }
        let mut hits = wire::parse_query_candidates(
            self.authority,
            selected,
            query_coordinates,
            &response.body,
        )?;
        hits.sort_by(|left, right| scoring::compare_candidates(*left, *right));
        hits.truncate(requested_limit);
        let written = hits.len();
        for (slot, hit) in output.iter_mut().zip(hits) {
            *slot = Some(hit);
        }
        Ok(written.into())
    }
}

#[cfg(test)]
mod tests {
    use compiler_ir::EntityId;
    use server_index_graph_vector::{Metric, ModelId, PartitionId, VectorAuthority};
    use server_index_vocabulary::{IndexSnapshotId, VectorSegmentId};

    use super::*;
    use crate::{PhysicalPointId, QdrantCandidate};

    fn authority() -> VectorAuthority {
        VectorAuthority::new(
            IndexSnapshotId::from_canonical_bytes(b"qdrant empty selection snapshot"),
            ModelId::new([3; 16]),
            2,
            Metric::SquaredEuclidean,
        )
    }

    #[test]
    fn empty_selection_clears_requested_output_without_transport() -> Result<(), QdrantError> {
        let authority = authority();
        let adapter = QdrantBlockingAdapter::new(
            "http://127.0.0.1:1",
            "empty-selection-must-not-connect",
            authority,
        )?;
        let stale = QdrantCandidate {
            authority,
            segment: VectorSegmentId::from_canonical_bytes(b"stale remote candidate"),
            partition: PartitionId::new(1),
            entity: EntityId::new(2),
            score: 3.0,
            physical_id: PhysicalPointId(4),
        };
        let mut output = [Some(stale)];

        let written = adapter.query(&[], &[0, 0], output.len(), &mut output)?;

        assert_eq!(written.count, 0);
        assert_eq!(output, [None]);
        Ok(())
    }
}
