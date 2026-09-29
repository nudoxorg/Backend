//! Measures the production `VectorIndex::search_embedding` path through an
//! in-memory exact candidate source. Corpus and model setup stay outside the
//! measured operation, while page validation, payload decoding, exact scoring,
//! and result ordering remain inside it.
#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "the fixed benchmark fixture is bounded and fails at its construction site"
)]

use std::{hint::black_box, mem::size_of, time::Instant};

use allocation_counter::{AllocationInfo, measure};
use backend_extension_qdrant::{
    AnnBase, Authority, Binding, CandidateId, CandidateRelation, CandidateState, DocumentVector,
    EmbeddingEncoding, EmbeddingNormalization, EmbeddingPooling, EmbeddingRecipe, Frontier,
    MemorySource, Metric, ModelVersion, QueryVector, ReadManifest, SearchQuality, TokenizerVersion,
    TreatmentVersion, VectorFacts, VectorIndex,
};
use backend_version::{
    AuthorityScopeClaim, CoverageWitness, ProducerObservationClaims, ProducerObservationVerifier,
    RelationState, ScopeRoot, UntrustedProducerObservation, WorkspaceManifest,
    admit_complete_scope, admit_producer_observation,
};

const WARMUPS: usize = 8;
const SAMPLES: usize = 128;
const DIMENSIONS: [usize; 2] = [384, 1536];
const CANDIDATE_COUNTS: [usize; 2] = [128, 512];
const METRICS: [Metric; 3] = [
    Metric::CosineDistance,
    Metric::EuclideanSquared,
    Metric::NegativeDot,
];

struct FixtureCoverageVerifier {
    producer: [u8; 32],
    scope: ScopeRoot,
    context: [u8; 32],
    evidence: Vec<u8>,
}

impl ProducerObservationVerifier for FixtureCoverageVerifier {
    type Error = ();

    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        (observation.producer_identity() == self.producer
            && observation.scope_root() == self.scope
            && observation.context() == self.context
            && observation.evidence() == self.evidence.as_slice())
        .then(|| {
            ProducerObservationClaims::new(
                self.producer,
                self.scope,
                self.context,
                *blake3::hash(&self.evidence).as_bytes(),
            )
        })
        .ok_or(())
    }
}

fn coverage(seed: u8) -> CoverageWitness {
    let authority = Authority::from_value(&[seed; 32]);
    let declaration = AuthorityScopeClaim::from_object_version(authority);
    let verifier = FixtureCoverageVerifier {
        producer: [seed.wrapping_add(1); 32],
        scope: declaration.scope_root(),
        context: [seed.wrapping_add(2); 32],
        evidence: vec![seed, seed.wrapping_add(3), seed.wrapping_add(4)],
    };
    let observation = UntrustedProducerObservation::new(
        verifier.producer,
        verifier.scope,
        verifier.context,
        verifier.evidence.clone(),
    );
    let admitted = admit_producer_observation(observation, &verifier).expect("admit fixture");
    CoverageWitness::Complete(admit_complete_scope(declaration, admitted).expect("scope match"))
}

fn recipe(dimensions: usize, metric: Metric) -> EmbeddingRecipe {
    EmbeddingRecipe {
        model: ModelVersion::from_value(&[0x31; 32]),
        tokenizer: TokenizerVersion::from_value(&[0x32; 32]),
        dimensions: u32::try_from(dimensions)
            .expect("dimension fits recipe")
            .try_into()
            .expect("nonzero dimension"),
        metric,
        pooling: EmbeddingPooling::Mean,
        normalization: EmbeddingNormalization::None,
        encoding: EmbeddingEncoding::Float32,
        query_treatment: TreatmentVersion::from_value(b"query"),
        document_treatment: TreatmentVersion::from_value(b"document"),
    }
}

fn limits() -> backend_extension_qdrant::Limits {
    backend_extension_qdrant::Limits {
        max_candidates: 4096,
        max_tombstones: 4096,
        max_payload_bytes: 16 * 1024,
        max_total_payload_bytes: 8 * 1024 * 1024,
        max_page: 256,
    }
}

fn fixture(
    dimensions: usize,
    candidate_count: usize,
    metric: Metric,
) -> (
    VectorIndex<MemorySource>,
    QueryVector,
    backend_extension_qdrant::Limits,
) {
    let recipe = recipe(dimensions, metric);
    let coverage = coverage(7);
    let mut candidates = Vec::with_capacity(candidate_count);
    for row in 0..candidate_count {
        let id = CandidateId::new(u64::try_from(row + 1).expect("candidate id"))
            .expect("positive candidate id");
        let values = (0..dimensions)
            .map(|column| {
                let lane =
                    (row.wrapping_mul(37).wrapping_add(column.wrapping_mul(13)) % 997) as i32 - 498;
                lane as f32 / 997.0
            })
            .collect();
        let document = DocumentVector::new(recipe, id, values).expect("document vector");
        candidates.push((id, document.point().to_payload()));
    }

    let root_entries = candidates
        .iter()
        .map(|(id, payload)| (id.0, payload.clone()))
        .collect::<Vec<_>>();
    let relation = RelationState::<CandidateRelation>::from_entries(root_entries, coverage)
        .expect("candidate relation");
    let workspace = WorkspaceManifest::from_versions(
        1,
        Vec::new(),
        Vec::new(),
        Authority::from_value(&[0x33; 32]),
        coverage,
    )
    .expect("workspace manifest")
    .root();
    let binding = Binding::new(
        workspace,
        relation.root(),
        recipe.version(),
        Authority::from_value(&[0x34; 32]),
        ReadManifest::from_value(b"rerank-benchmark"),
    )
    .with_frontier(Frontier::from_value(&[0x35; 32]));
    let state =
        CandidateState::new(binding, coverage, candidates, limits()).expect("candidate state");
    let facts = VectorFacts::from_recipe(state, recipe).expect("vector facts");
    let base = AnnBase::from_facts(&facts, SearchQuality::Exact, limits()).expect("ANN base");
    let source = MemorySource::new(
        binding,
        coverage,
        (1..=candidate_count)
            .map(|id| CandidateId::new(u64::try_from(id).expect("candidate id")))
            .collect::<Result<Vec<_>, _>>()
            .expect("candidate IDs"),
        limits(),
    )
    .expect("in-memory candidate source");
    let query_values = (0..dimensions)
        .map(|index| {
            let lane = (index.wrapping_mul(17) % 251) as i32 - 125;
            lane as f32 / 251.0
        })
        .collect();
    let query = QueryVector::new(recipe, query_values).expect("query vector");
    (
        VectorIndex::new(base, facts, source).expect("vector index"),
        query,
        limits(),
    )
}

fn percentile(samples: &mut [u128], numerator: usize, denominator: usize) -> u128 {
    samples.sort_unstable();
    let last = samples.len().saturating_sub(1);
    let rank = last.saturating_mul(numerator) / denominator;
    samples[rank]
}

fn allocation_percentile(samples: &[AllocationInfo], get: impl Fn(AllocationInfo) -> u64) -> u64 {
    let mut values = samples.iter().copied().map(get).collect::<Vec<_>>();
    values.sort_unstable();
    values[(values.len().saturating_sub(1) * 50) / 100]
}

fn main() {
    for dimensions in DIMENSIONS {
        for candidate_count in CANDIDATE_COUNTS {
            for metric in METRICS {
                measure_case(dimensions, candidate_count, metric);
            }
        }
    }
}

fn measure_case(dimensions: usize, candidate_count: usize, metric: Metric) {
    let (index, query, limits) = fixture(dimensions, candidate_count, metric);
    let result_limit = candidate_count / 4;
    for _ in 0..WARMUPS {
        let result = index
            .search_embedding(&query, result_limit, limits)
            .expect("warmup search");
        black_box(result.candidates.len());
    }

    let mut elapsed_ns = Vec::with_capacity(SAMPLES);
    let mut allocations = Vec::with_capacity(SAMPLES);
    let mut last_result = None;
    for _ in 0..SAMPLES {
        let mut result = None;
        let start = Instant::now();
        let allocation = measure(|| {
            result = Some(index.search_embedding(&query, result_limit, limits));
        });
        elapsed_ns.push(start.elapsed().as_nanos());
        allocations.push(allocation);
        let result = result
            .expect("measurement ran")
            .expect("rerank query succeeded");
        black_box(result.candidates.first());
        last_result = Some(result);
    }

    let scored_bytes = u64::try_from(candidate_count)
        .expect("candidate byte count")
        .saturating_mul(u64::try_from(dimensions).expect("dimension bytes"))
        .saturating_mul(u64::try_from(size_of::<f32>()).expect("coordinate width"));
    let query_bytes = u64::try_from(dimensions)
        .expect("query dimension bytes")
        .saturating_mul(u64::try_from(size_of::<f32>()).expect("coordinate width"));
    let result_bytes = last_result.as_ref().map_or(0, |result| {
        (result.candidates.len() * size_of::<backend_extension_qdrant::ScoredCandidate>()) as u64
    });
    let p50_ns = percentile(&mut elapsed_ns, 50, 100);
    let p95_ns = percentile(&mut elapsed_ns, 95, 100);
    let candidates_per_second = u128::try_from(candidate_count).expect("candidate throughput")
        * 1_000_000_000
        / p50_ns.max(1);
    let coordinate_ops_per_second = u128::try_from(candidate_count)
        .expect("candidate throughput")
        .saturating_mul(u128::try_from(dimensions).expect("dimension throughput"))
        .saturating_mul(1_000_000_000)
        / p50_ns.max(1);
    let coordinate_bytes_per_second =
        u128::from(scored_bytes).saturating_mul(1_000_000_000) / p50_ns.max(1);
    println!(
        "qdrant_rerank metric={metric:?} dimensions={dimensions} candidates={candidate_count} top_k={result_limit} samples={SAMPLES} p50_ns={p50_ns} p95_ns={p95_ns} candidates_per_second={candidates_per_second} coordinate_ops_per_second={coordinate_ops_per_second} coordinate_bytes_per_second={coordinate_bytes_per_second} scored_coordinate_bytes={scored_bytes} query_bytes={query_bytes} result_bytes={result_bytes} allocations_p50={} allocated_bytes_p50={}",
        allocation_percentile(&allocations, |sample| sample.count_total),
        allocation_percentile(&allocations, |sample| sample.bytes_total),
    );
}
