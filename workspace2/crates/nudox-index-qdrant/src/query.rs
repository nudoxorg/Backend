//! Exact Qdrant query admission, parsing, and deterministic result publication.

use nudox_index_graph_vector::PartitionId;

use super::{
    QdrantBlockingAdapter, admission,
    contract::{QdrantError, QdrantHit, QueryHitCount, RequestPhase},
    limits::QUERY_POINTS_PATH,
    scoring,
    transport::{self, Method},
    wire,
};

impl QdrantBlockingAdapter {
    /// Queries Qdrant with full authority filters and reports exact absent selected partitions.
    pub fn query(
        &self,
        selected: &[PartitionId],
        query_coordinates: &[i16],
        requested_limit: usize,
        output: &mut [Option<QdrantHit>],
    ) -> Result<QueryHitCount, QdrantError> {
        admission::validate_query(
            self.authority,
            selected,
            query_coordinates,
            requested_limit,
            output.len(),
        )?;
        let response = self.request_json(
            RequestPhase::QueryPoints,
            Method::Post,
            &self.url(QUERY_POINTS_PATH),
            wire::QueryRequest::new(self.authority, selected, query_coordinates),
        )?;
        if !transport::is_success(response.status) {
            return Err(transport::status_error(RequestPhase::QueryPoints, response));
        }
        let mut hits =
            wire::parse_query_hits(self.authority, selected, query_coordinates, &response.body)?;
        hits.sort_by(|left, right| scoring::compare_hits(*left, *right));
        hits.truncate(requested_limit);
        let written = hits.len();
        for slot in output.iter_mut().take(requested_limit) {
            *slot = None;
        }
        for (slot, hit) in output.iter_mut().zip(hits) {
            *slot = Some(hit);
        }
        Ok(written.into())
    }
}
