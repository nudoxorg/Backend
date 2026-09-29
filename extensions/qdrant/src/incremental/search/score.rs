//! Deterministic metric scoring for exact local reranking.

use crate::Error;
use crate::incremental::vector::{AdmittedEncodedPoint, Metric};

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

/// Scores a point whose immutable facts were fully admitted before the search
/// began. The caller has checked that query dimensions match the facts; the
/// proof wrapper guarantees exact shape and finite coordinates, so this loop
/// only performs endian-safe decode and the scalar metric math.
pub(super) fn score_admitted(
    metric: Metric,
    query: &[f32],
    point: &AdmittedEncodedPoint<'_>,
) -> f32 {
    let coordinates = point.coordinates();
    match metric {
        Metric::EuclideanSquared => query
            .iter()
            .zip(coordinates)
            .map(|(query, point)| {
                let difference = query - point;
                difference * difference
            })
            .sum(),
        Metric::CosineDistance => {
            let (dot, query_norm, point_norm) = query.iter().zip(coordinates).fold(
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
            .zip(coordinates)
            .map(|(query, point)| query * point)
            .sum::<f32>(),
    }
}

/// Scores one admitted canonical point payload without materializing a
/// coordinate vector. The four-byte dimension and each coordinate are read
/// as big-endian integers, so the payload may begin at any byte alignment and
/// behaves identically on every host endian.
pub(super) fn score_encoded(metric: Metric, query: &[f32], payload: &[u8]) -> Result<f32, Error> {
    let dimension_bytes: [u8; 4] = payload
        .get(..4)
        .ok_or(Error::MalformedInput)?
        .try_into()
        .map_err(|_| Error::MalformedInput)?;
    let dimension =
        usize::try_from(u32::from_be_bytes(dimension_bytes)).map_err(|_| Error::SizeLimit)?;
    if dimension == 0 {
        return Err(Error::MalformedInput);
    }
    let expected_bytes = dimension
        .checked_mul(std::mem::size_of::<u32>())
        .and_then(|bytes| bytes.checked_add(4))
        .ok_or(Error::SizeLimit)?;
    if expected_bytes != payload.len() {
        return Err(Error::MalformedInput);
    }
    if dimension != query.len() {
        return Err(Error::DimensionMismatch);
    }
    let coordinates = payload.get(4..).ok_or(Error::MalformedInput)?;
    let mut coordinate_bytes = coordinates.chunks_exact(4);

    match metric {
        Metric::EuclideanSquared => {
            let mut total = 0.0_f32;
            for (query, bytes) in query.iter().zip(coordinate_bytes.by_ref()) {
                let point = decode_coordinate(bytes)?;
                let difference = query - point;
                total += difference * difference;
            }
            Ok(total)
        }
        Metric::CosineDistance => {
            let (mut dot, mut query_norm, mut point_norm) = (0.0_f32, 0.0_f32, 0.0_f32);
            for (query, bytes) in query.iter().zip(coordinate_bytes.by_ref()) {
                let point = decode_coordinate(bytes)?;
                dot += query * point;
                query_norm += query * query;
                point_norm += point * point;
            }
            if query_norm == 0.0 || point_norm == 0.0 {
                Ok(1.0)
            } else {
                Ok(1.0 - dot / (query_norm.sqrt() * point_norm.sqrt()))
            }
        }
        Metric::NegativeDot => {
            let mut dot = 0.0_f32;
            for (query, bytes) in query.iter().zip(coordinate_bytes.by_ref()) {
                dot += query * decode_coordinate(bytes)?;
            }
            Ok(-dot)
        }
    }
}

fn decode_coordinate(bytes: &[u8]) -> Result<f32, Error> {
    let bits = u32::from_be_bytes(bytes.try_into().map_err(|_| Error::MalformedInput)?);
    let value = f32::from_bits(bits);
    value
        .is_finite()
        .then_some(value)
        .ok_or(Error::MalformedInput)
}

#[cfg(test)]
mod tests {
    use super::{score, score_encoded};
    use crate::{
        EmbeddingEncoding, EmbeddingNormalization, EmbeddingPooling, EmbeddingRecipe, Error,
        Metric, ModelVersion, QueryVector, TokenizerVersion, TreatmentVersion, VectorPoint,
    };

    fn scalar_oracle(metric: Metric, query: &[f32], point: &[f32]) -> f32 {
        match metric {
            Metric::EuclideanSquared => {
                let mut total = 0.0_f32;
                for (query, point) in query.iter().zip(point) {
                    let difference = query - point;
                    total += difference * difference;
                }
                total
            }
            Metric::CosineDistance => {
                let mut dot = 0.0_f32;
                let mut query_norm = 0.0_f32;
                let mut point_norm = 0.0_f32;
                for (query, point) in query.iter().zip(point) {
                    dot += query * point;
                    query_norm += query * query;
                    point_norm += point * point;
                }
                if query_norm == 0.0 || point_norm == 0.0 {
                    1.0
                } else {
                    1.0 - dot / (query_norm.sqrt() * point_norm.sqrt())
                }
            }
            Metric::NegativeDot => {
                let mut dot = 0.0_f32;
                for (query, point) in query.iter().zip(point) {
                    dot += query * point;
                }
                -dot
            }
        }
    }

    #[test]
    fn scorer_preserves_ordered_f32_results_for_unaligned_tails_and_near_ties() {
        let dimensions = [
            1, 2, 3, 4, 7, 15, 16, 17, 31, 32, 33, 127, 384, 385, 1536, 1537,
        ];
        for dimension in dimensions {
            // Offset by one scalar so SIMD-width alignment cannot be assumed.
            let query_storage = (0..=dimension)
                .map(|index| coordinate(index, 7))
                .collect::<Vec<_>>();
            let point_storage = (0..=dimension)
                .map(|index| coordinate(index, 11))
                .collect::<Vec<_>>();
            let near_tie_storage = (0..=dimension)
                .map(|index| coordinate(index, 11).next_up())
                .collect::<Vec<_>>();
            let query = &query_storage[1..];
            let point = &point_storage[1..];
            let near_tie = &near_tie_storage[1..];

            for metric in [
                Metric::CosineDistance,
                Metric::EuclideanSquared,
                Metric::NegativeDot,
            ] {
                let expected = scalar_oracle(metric, query, point);
                let observed = score(metric, query, point);
                assert_eq!(
                    observed.to_bits(),
                    expected.to_bits(),
                    "{metric:?} d={dimension}"
                );

                let expected_near_tie = scalar_oracle(metric, query, near_tie);
                let observed_near_tie = score(metric, query, near_tie);
                assert_eq!(
                    observed_near_tie.to_bits(),
                    expected_near_tie.to_bits(),
                    "{metric:?} near-tie d={dimension}"
                );
            }
        }
    }

    #[test]
    fn encoded_scorer_is_bit_exact_for_random_unaligned_payloads_and_tails() {
        let dimensions = [
            1, 2, 3, 7, 15, 16, 17, 31, 32, 33, 127, 384, 385, 1536, 1537,
        ];
        let mut random = 0x8f31_1c47_7a2d_b609_u64;
        for dimension in dimensions {
            let query = (0..dimension)
                .map(|_| finite_random(&mut random))
                .collect::<Vec<_>>();
            let point = (0..dimension)
                .map(|_| finite_random(&mut random))
                .collect::<Vec<_>>();
            let near_tie = point
                .iter()
                .map(|value| value.next_up())
                .collect::<Vec<_>>();
            let payload = encode_unaligned(&point);
            let near_tie_payload = encode_unaligned(&near_tie);

            for metric in [
                Metric::CosineDistance,
                Metric::EuclideanSquared,
                Metric::NegativeDot,
            ] {
                let expected = scalar_oracle(metric, &query, &point);
                let materialized = score(metric, &query, &point);
                assert_eq!(
                    materialized.to_bits(),
                    expected.to_bits(),
                    "materialized {metric:?} random d={dimension}"
                );
                let observed = score_encoded(metric, &query, &payload[1..])
                    .expect("admitted point payload scores");
                assert_eq!(
                    observed.to_bits(),
                    expected.to_bits(),
                    "{metric:?} random d={dimension}"
                );

                let expected_near_tie = scalar_oracle(metric, &query, &near_tie);
                let materialized_near_tie = score(metric, &query, &near_tie);
                assert_eq!(
                    materialized_near_tie.to_bits(),
                    expected_near_tie.to_bits(),
                    "materialized {metric:?} near-tie d={dimension}"
                );
                let observed_near_tie = score_encoded(metric, &query, &near_tie_payload[1..])
                    .expect("admitted near-tie payload scores");
                assert_eq!(
                    observed_near_tie.to_bits(),
                    expected_near_tie.to_bits(),
                    "{metric:?} near-tie d={dimension}"
                );
            }
        }
    }

    #[test]
    fn encoded_scorer_rejects_bad_bounds_dimensions_and_non_finite_values() {
        assert_eq!(
            score_encoded(Metric::NegativeDot, &[1.0], &[]),
            Err(Error::MalformedInput)
        );
        assert!(matches!(
            score_encoded(Metric::NegativeDot, &[1.0], &[0xff, 0xff, 0xff, 0xff]),
            Err(Error::MalformedInput | Error::SizeLimit)
        ));
        assert_eq!(
            score_encoded(Metric::NegativeDot, &[1.0], &[0, 0, 0, 2, 0, 0, 0, 0]),
            Err(Error::MalformedInput)
        );
        assert_eq!(
            score_encoded(Metric::NegativeDot, &[1.0], &[0, 0, 0, 0]),
            Err(Error::MalformedInput)
        );
        assert_eq!(
            score_encoded(
                Metric::NegativeDot,
                &[1.0],
                &[0, 0, 0, 2, 0x3f, 0x80, 0, 0, 0x40, 0, 0, 0],
            ),
            Err(Error::DimensionMismatch)
        );
        for non_finite in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let payload = encode_unaligned(&[non_finite]);
            assert_eq!(
                score_encoded(Metric::NegativeDot, &[1.0], &payload[1..]),
                Err(Error::MalformedInput)
            );
        }
    }

    #[test]
    fn finite_extremes_keep_overflow_and_zero_norm_behavior_explicit() {
        assert!(score(Metric::EuclideanSquared, &[f32::MAX], &[-f32::MAX]).is_infinite());
        assert!(score(Metric::NegativeDot, &[f32::MAX], &[f32::MAX]).is_infinite());
        assert!(score(Metric::CosineDistance, &[f32::MAX], &[f32::MAX]).is_nan());
        assert_eq!(
            score(Metric::CosineDistance, &[0.0, -0.0], &[3.0, 4.0]),
            1.0
        );
        assert_eq!(
            score(Metric::CosineDistance, &[3.0, 4.0], &[0.0, -0.0]),
            1.0
        );
    }

    #[test]
    fn coordinate_admission_rejects_nan_before_scoring() {
        let recipe = EmbeddingRecipe {
            model: ModelVersion::from_value(&[1; 32]),
            tokenizer: TokenizerVersion::from_value(&[2; 32]),
            dimensions: 1.try_into().expect("nonzero dimensions"),
            metric: Metric::CosineDistance,
            pooling: EmbeddingPooling::Mean,
            normalization: EmbeddingNormalization::None,
            encoding: EmbeddingEncoding::Float32,
            query_treatment: TreatmentVersion::from_value(b"query"),
            document_treatment: TreatmentVersion::from_value(b"document"),
        };
        assert_eq!(
            VectorPoint::new(crate::CandidateId(1), vec![f32::NAN]).map(|_| ()),
            Err(Error::MalformedInput)
        );
        assert_eq!(
            QueryVector::new(recipe, vec![f32::NAN]).map(|_| ()),
            Err(Error::DimensionMismatch)
        );
    }

    fn coordinate(index: usize, salt: usize) -> f32 {
        let mixed = index.wrapping_mul(0x9e37).wrapping_add(salt * 0x51);
        let centered = (mixed % 4093) as i32 - 2046;
        centered as f32 / 4093.0
    }

    fn finite_random(state: &mut u64) -> f32 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        let [a, b, c, d, e, f, g, h] = (*state).to_be_bytes();
        let exponent_bits = u32::from_be_bytes([a, b, c, d]);
        let coordinate_bits = u32::from_be_bytes([e, f, g, h]);
        let sign = coordinate_bits & 0x8000_0000;
        let exponent = 96 + exponent_bits % 64;
        let mantissa = coordinate_bits & 0x007f_ffff;
        f32::from_bits(sign | (exponent << 23) | mantissa)
    }

    fn encode_unaligned(values: &[f32]) -> Vec<u8> {
        let dimension = u32::try_from(values.len()).expect("test dimension fits payload");
        let mut payload = Vec::with_capacity(1 + 4 + values.len() * 4);
        payload.push(0x5a);
        payload.extend_from_slice(&dimension.to_be_bytes());
        for value in values {
            payload.extend_from_slice(&value.to_bits().to_be_bytes());
        }
        payload
    }
}
