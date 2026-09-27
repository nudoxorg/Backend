//! Resident lexical rank: one broad query, one rare query, and a full page drain.
#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "the fixed benchmark fixture is intentionally fail-fast and bounded"
)]

use backend_extension_tantivy::{
    Adapter, Authority, Binding, DocumentState, IndexRelation, Limits, Query, QueryRequest,
    ReadManifest, Recipe, TantivySource,
};
use backend_semantic::{Entity, EntityId, Source, entity_key};
use backend_version::{
    AuthorityScopeClaim, CoverageWitness, ProducerObservationClaims, ProducerObservationVerifier,
    RelationState, ScopeRoot, UntrustedProducerObservation, WorkspaceManifest,
    admit_complete_scope, admit_producer_observation,
};
use std::hint::black_box;
use std::time::Instant;

const DOCUMENTS: usize = 2048;
const SAMPLES: usize = 32;
const WARMUPS: usize = 4;

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

fn authorized_coverage(bytes: &[u8; 32]) -> CoverageWitness {
    let authority = Authority::from_value(bytes);
    let declaration = AuthorityScopeClaim::from_object_version(authority);
    let verifier = FixtureCoverageVerifier {
        producer: [9; 32],
        scope: declaration.scope_root(),
        context: [4; 32],
        evidence: vec![1, 2, 3],
    };
    let observation = UntrustedProducerObservation::new(
        verifier.producer,
        declaration.scope_root(),
        verifier.context,
        verifier.evidence.clone(),
    );
    let admitted = admit_producer_observation(observation, &verifier).expect("admitted producer");
    CoverageWitness::Complete(admit_complete_scope(declaration, admitted).expect("scope match"))
}

fn document(ordinal: u64) -> EntityId {
    let source = Source::new(7, "benches/lexical_rank.rs").expect("source identity");
    entity_key(
        &Entity::new(source, format!("declaration-{ordinal}"), None).expect("entity identity"),
    )
}

fn percentile(samples: &mut [u128], rank: usize) -> u128 {
    samples.sort_unstable();
    samples[rank.min(samples.len().saturating_sub(1))]
}

fn corpus() -> (DocumentState, Binding) {
    let documents = (0..DOCUMENTS)
        .map(|ordinal| {
            let name = format!("item_{ordinal}");
            let mut fields = vec![
                ("name".into(), name),
                (
                    "signature".into(),
                    format!("fn item_{ordinal}(widget: Widget) -> Widget"),
                ),
                (
                    "documentation".into(),
                    "A widget declaration kept in the resident lexical projection.".into(),
                ),
            ];
            fields.sort();
            (document(ordinal as u64), fields)
        })
        .collect::<Vec<_>>();
    let coverage = authorized_coverage(&[7; 32]);
    let state = RelationState::<IndexRelation>::from_entries(documents.clone(), coverage)
        .expect("relation");
    let workspace = WorkspaceManifest::from_versions(
        1,
        Vec::new(),
        Vec::new(),
        Authority::from_value(&[1; 32]),
        authorized_coverage(&[3; 32]),
    )
    .expect("workspace")
    .root();
    let binding = Binding::new(
        workspace,
        state.root(),
        Recipe::from_value(&[1; 32]),
        Authority::from_value(&[2; 32]),
        ReadManifest::from_value(b"lexical-rank"),
    );
    let state = DocumentState::new(binding, coverage, documents, Limits::default()).expect("state");
    (state, binding)
}

fn drain(adapter: &Adapter<TantivySource>, binding: Binding, query: &Query) -> usize {
    let mut cursor = None;
    let mut seen = 0usize;
    loop {
        let page = adapter
            .query(&QueryRequest {
                binding,
                query: query.clone(),
                cursor,
                limit: Limits::default().max_page,
            })
            .expect("page");
        seen = seen.saturating_add(page.hits.len());
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    seen
}

fn main() {
    let (state, binding) = corpus();
    let source = TantivySource::build(&state, Limits::default()).expect("projection");
    let postings = source.indexed_postings();
    let broad = Query::prefix(vec!["widget".into()], Limits::default()).expect("broad query");
    let rare = Query::new(vec!["item_7".into()], Limits::default()).expect("rare query");
    let broad_hits = source.search(&broad).expect("broad rank").len();
    let rare_hits = source.search(&rare).expect("rare rank").len();
    let page_source = TantivySource::build(&state, Limits::default()).expect("page projection");
    let adapter = Adapter::new(page_source, Limits::default()).expect("adapter");
    let mut broad_samples = [0_u128; SAMPLES];
    let mut rare_samples = [0_u128; SAMPLES];
    let mut drain_samples = [0_u128; SAMPLES];
    for sample in 0..(WARMUPS + SAMPLES) {
        let started = Instant::now();
        let found = source.search(&broad).expect("broad search").len();
        let elapsed = started.elapsed().as_nanos();
        assert_eq!(black_box(found), broad_hits);
        if sample >= WARMUPS {
            broad_samples[sample - WARMUPS] = elapsed;
        }
        let started = Instant::now();
        let found = source.search(&rare).expect("rare search").len();
        let elapsed = started.elapsed().as_nanos();
        assert_eq!(black_box(found), rare_hits);
        if sample >= WARMUPS {
            rare_samples[sample - WARMUPS] = elapsed;
        }
        adapter.clear_rank_cache().expect("clear rank");
        let before = adapter.rank_evaluations();
        let started = Instant::now();
        let seen = drain(&adapter, binding, &broad);
        let elapsed = started.elapsed().as_nanos();
        assert_eq!(black_box(seen), broad_hits);
        assert_eq!(adapter.rank_evaluations(), before.saturating_add(1));
        if sample >= WARMUPS {
            drain_samples[sample - WARMUPS] = elapsed;
        }
    }
    let drains = u64::try_from(WARMUPS + SAMPLES).expect("sample count");
    assert_eq!(adapter.rank_evaluations(), drains);
    println!(
        "lexical_rank documents={DOCUMENTS} postings={postings} broad_hits={broad_hits} rare_hits={rare_hits} broad_median_ns={} broad_p95_ns={} rare_median_ns={} rare_p95_ns={} drain_median_ns={} drain_p95_ns={} drain_evaluations={}",
        percentile(&mut broad_samples, SAMPLES / 2),
        percentile(&mut broad_samples, SAMPLES * 95 / 100),
        percentile(&mut rare_samples, SAMPLES / 2),
        percentile(&mut rare_samples, SAMPLES * 95 / 100),
        percentile(&mut drain_samples, SAMPLES / 2),
        percentile(&mut drain_samples, SAMPLES * 95 / 100),
        adapter.rank_evaluations() / drains,
    );
}
