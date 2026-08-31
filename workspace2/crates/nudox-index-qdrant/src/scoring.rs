//! Deterministic conversion and ordering of remote vector results.

use std::cmp::Ordering;

use nudox_index_graph_vector::Metric;

use super::contract::QdrantHit;

pub(super) fn projected_score(metric: Metric, query: &[i16], coordinates: &[f64]) -> f64 {
    match metric {
        Metric::SquaredEuclidean => query
            .iter()
            .zip(coordinates)
            .map(|(left, right)| {
                let difference = f64::from(*left) - *right;
                difference * difference
            })
            .sum(),
        Metric::NegativeDotProduct => -query
            .iter()
            .zip(coordinates)
            .map(|(left, right)| f64::from(*left) * *right)
            .sum::<f64>(),
    }
}

pub(super) fn compare_hits(left: QdrantHit, right: QdrantHit) -> Ordering {
    left.score
        .total_cmp(&right.score)
        .then_with(|| left.entity.raw.cmp(&right.entity.raw))
        .then_with(|| left.partition.raw.cmp(&right.partition.raw))
}
