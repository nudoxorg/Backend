//! Cold build versus resident rebind and one-document revision.
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "the fixed benchmark fixture is intentionally fail-fast and bounded"
)]

use backend_extension_tantivy::{
    Authority, Binding, DocumentState, Frontier, Limits, MaintainOutcome, OverlayLimits,
    ProjectionKind, Query, TantivySource,
};
use backend_semantic::{Entity, EntityId, Source, entity_key};
use backend_version::{
    AuthorityScopeClaim, CoverageWitness, ProducerObservationClaims, ProducerObservationVerifier,
    RelationState, ScopeRoot, UntrustedProducerObservation, WorkspaceManifest,
    admit_complete_scope, admit_producer_observation,
};
use std::time::Instant;

const SAMPLES: usize = 5;
const WARMUPS: usize = 1;

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

fn coverage() -> CoverageWitness {
    let authority = Authority::from_value(&[7; 32]);
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

fn workspace() -> backend_version::WorkspaceRoot {
    WorkspaceManifest::from_versions(
        1,
        Vec::new(),
        Vec::new(),
        Authority::from_value(&[1; 32]),
        coverage(),
    )
    .expect("workspace")
    .root()
}

fn document(ordinal: u32) -> EntityId {
    let source = Source::new(7, "benches/projection_revision.rs").expect("source");
    entity_key(&Entity::new(source, format!("declaration-{ordinal}"), None).expect("entity"))
}

fn documents(size: usize, replaced: Option<&str>) -> Vec<(EntityId, Vec<(String, String)>)> {
    (0..size)
        .map(|ordinal| {
            let text = if ordinal == 0 {
                replaced.unwrap_or("sym0").to_owned()
            } else {
                format!("sym{ordinal}")
            };
            (
                document(u32::try_from(ordinal).expect("ordinal")),
                vec![("name".to_owned(), text)],
            )
        })
        .collect()
}

fn state(documents: Vec<(EntityId, Vec<(String, String)>)>, frontier: [u8; 32]) -> DocumentState {
    let witnessed = coverage();
    let root = RelationState::<backend_extension_tantivy::IndexRelation>::from_entries(
        documents.iter().cloned(),
        witnessed,
    )
    .expect("relation")
    .root();
    let binding = Binding::new(
        workspace(),
        root,
        backend_extension_tantivy::Recipe::from_value(&[1; 32]),
        Authority::from_value(&[2; 32]),
        backend_extension_tantivy::ReadManifest::from_value(b"reads".as_slice()),
    )
    .with_frontier(Frontier::from_value(&frontier));
    DocumentState::new(binding, witnessed, documents, Limits::default()).expect("state")
}

fn percentile(samples: &mut [u128], rank: usize) -> u128 {
    samples.sort_unstable();
    samples[rank.min(samples.len().saturating_sub(1))]
}

fn run(size: usize) {
    let mut cold = [0_u128; SAMPLES];
    let mut rebound = [0_u128; SAMPLES];
    let mut revised = [0_u128; SAMPLES];
    let mut rebuilt = [0_u128; SAMPLES];
    let mut rebound_postings = 0_u64;
    let mut revised_documents = 0_usize;
    for sample in 0..(WARMUPS + SAMPLES) {
        let original = state(documents(size, None), [1; 32]);
        let rebound_state = state(documents(size, None), [2; 32]);
        let revised_state = state(documents(size, Some("zephyr0")), [3; 32]);
        let started = Instant::now();
        let mut source = TantivySource::build(&original, Limits::default()).expect("cold build");
        let cold_elapsed = started.elapsed().as_nanos();
        let postings = source.indexed_postings();
        let started = Instant::now();
        let rebound_outcome = source
            .maintain(&rebound_state, OverlayLimits::default())
            .expect("rebind");
        let rebound_elapsed = started.elapsed().as_nanos();
        assert_eq!(
            source.indexed_postings(),
            postings,
            "rebind rewrote postings"
        );
        let started = Instant::now();
        let revised_outcome = source
            .maintain(&revised_state, OverlayLimits::default())
            .expect("revise");
        let revised_elapsed = started.elapsed().as_nanos();
        let hits = source
            .search(&Query::new(vec!["zephyr0".into()], Limits::default()).expect("query"))
            .expect("search revised");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].document, document(0));
        let neighbor = format!("sym{}", size / 2);
        let neighbor_hits = source
            .search(&Query::new(vec![neighbor], Limits::default()).expect("query"))
            .expect("search neighbor");
        assert_eq!(neighbor_hits.len(), 1);
        let retired = source
            .search(&Query::new(vec!["sym0".into()], Limits::default()).expect("query"))
            .expect("search retired");
        assert!(retired.is_empty(), "retired token survived the revision");
        let started = Instant::now();
        let _rebuilt =
            TantivySource::build(&revised_state, Limits::default()).expect("full rebuild");
        let rebuilt_elapsed = started.elapsed().as_nanos();
        if sample >= WARMUPS {
            let index = sample - WARMUPS;
            cold[index] = cold_elapsed;
            rebound[index] = rebound_elapsed;
            revised[index] = revised_elapsed;
            rebuilt[index] = rebuilt_elapsed;
            rebound_postings = postings;
            if let MaintainOutcome::Applied(revision) = revised_outcome {
                revised_documents = revision.rewritten_documents;
                assert_eq!(revision.kind, ProjectionKind::Revised);
            } else {
                panic!("one-document edit required a rebuild");
            }
            assert!(matches!(
                rebound_outcome,
                MaintainOutcome::Applied(revision) if revision.kind == ProjectionKind::Rebound
            ));
        }
    }
    println!(
        "projection_revision size={size} samples={SAMPLES} warmups={WARMUPS} postings={rebound_postings} revised_documents={revised_documents} cold_build median={}ns p95={}ns rebind median={}ns p95={}ns one_document median={}ns p95={}ns full_rebuild median={}ns p95={}ns",
        percentile(&mut cold, SAMPLES / 2),
        percentile(&mut cold, SAMPLES * 95 / 100),
        percentile(&mut rebound, SAMPLES / 2),
        percentile(&mut rebound, SAMPLES * 95 / 100),
        percentile(&mut revised, SAMPLES / 2),
        percentile(&mut revised, SAMPLES * 95 / 100),
        percentile(&mut rebuilt, SAMPLES / 2),
        percentile(&mut rebuilt, SAMPLES * 95 / 100)
    );
}

fn main() {
    for size in [128, 512, 1024] {
        run(size);
    }
}
