//! Deterministic metric scoring for exact local reranking.

use crate::incremental::vector::Metric;

pub(super) fn score(metric: Metric, query: &[f32], point: &[f32]) -> f32 {
    match metric {
        Metric::EuclideanSquared => query
            .iter()
            .zip(point)
            .map(|(query, point)| {
                let difference = query - point;
                difference * difference
            })
            .sum(),
        Metric::CosineDistance => {
            let (dot, query_norm, point_norm) = query.iter().zip(point).fold(
                (0.0_f32, 0.0_f32, 0.0_f32),
                |(dot, query_norm, point_norm), (query, point)| {
                    (
                        dot + query * point,
                        query_norm + query * query,
                        point_norm + point * point,
                    )
                },
            );
            if query_norm == 0.0 || point_norm == 0.0 {
                1.0
            } else {
                1.0 - dot / (query_norm.sqrt() * point_norm.sqrt())
            }
        }
        Metric::NegativeDot => -query
            .iter()
            .zip(point)
            .map(|(query, point)| query * point)
            .sum::<f32>(),
    }
}
