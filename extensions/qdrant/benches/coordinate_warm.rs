//! Warm Qdrant upsert of a wide batch: retrieve matching keys and write nothing.
//!
//! Document construction and the cold upload stay outside the timer. Each sample
//! hashes every coordinate, builds every payload, and reads the resident keys back.
#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "the fixed benchmark fixture is intentionally fail-fast and bounded"
)]

use std::{
    collections::HashMap,
    io::{Read, Write},
    net::TcpListener,
    num::{NonZeroU8, NonZeroU32},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use backend_extension_qdrant::{
    Authority, Binding, CandidateId, CandidateRelation, DocumentVector, EmbeddingEncoding,
    EmbeddingNormalization, EmbeddingPooling, EmbeddingRecipe, Frontier, Metric, ModelVersion,
    QdrantHttpClient, QdrantHttpConfig, QdrantMutationReceipt, ReadManifest, TokenizerVersion,
    TreatmentVersion,
};
use backend_version::{
    AuthorityScopeClaim, CoverageWitness, ProducerObservationClaims, ProducerObservationVerifier,
    RelationState, ScopeRoot, UntrustedProducerObservation, WorkspaceManifest,
    admit_complete_scope, admit_producer_observation,
};
use std::hint::black_box;

const DOCUMENTS: usize = 128;
const DIMENSIONS: usize = 384;
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

fn recipe() -> EmbeddingRecipe {
    EmbeddingRecipe {
        model: ModelVersion::from_value(&[1; 32]),
        tokenizer: TokenizerVersion::from_value(&[2; 32]),
        dimensions: NonZeroU32::new(u32::try_from(DIMENSIONS).expect("dimensions"))
            .expect("dimension"),
        metric: Metric::CosineDistance,
        pooling: EmbeddingPooling::Mean,
        normalization: EmbeddingNormalization::None,
        encoding: EmbeddingEncoding::Float32,
        query_treatment: TreatmentVersion::from_value(b"query"),
        document_treatment: TreatmentVersion::from_value(b"document"),
    }
}

fn binding(recipe: EmbeddingRecipe) -> Binding {
    let witnessed = coverage();
    let root = RelationState::<CandidateRelation>::empty(witnessed).root();
    let workspace = WorkspaceManifest::from_versions(
        1,
        Vec::new(),
        Vec::new(),
        Authority::from_value(&[1; 32]),
        witnessed,
    )
    .expect("workspace")
    .root();
    Binding::new(
        workspace,
        root,
        recipe.version(),
        Authority::from_value(&[2; 32]),
        ReadManifest::from_value(b"reads"),
    )
    .with_frontier(Frontier::from_value(&[3; 32]))
}

fn documents(recipe: EmbeddingRecipe) -> Vec<DocumentVector> {
    (0..DOCUMENTS)
        .map(|offset| {
            let id =
                CandidateId::new(u64::try_from(offset).expect("offset") + 1).expect("candidate");
            let values = (0..DIMENSIONS)
                .map(|dimension| {
                    f32::from(u16::try_from((offset + dimension) % 251 + 1).expect("coordinate"))
                        / 251.0
                })
                .collect();
            DocumentVector::new(recipe, id, values).expect("document vector")
        })
        .collect()
}

fn percentile(samples: &mut [u128], rank: usize) -> u128 {
    samples.sort_unstable();
    samples[rank.min(samples.len().saturating_sub(1))]
}

fn read_http(stream: &mut std::net::TcpStream) -> (String, Vec<u8>) {
    let mut request = Vec::new();
    let mut byte = [0_u8; 1];
    while !request.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).expect("request byte");
        request.push(byte[0]);
    }
    let header = String::from_utf8(request).expect("HTTP header");
    let content_length = header
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then_some(value.trim())
        })
        .and_then(|length| length.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0_u8; content_length];
    stream.read_exact(&mut body).expect("request body");
    (header, body)
}

fn serve(retrieves: Arc<Mutex<usize>>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let address = listener.local_addr().expect("address");
    thread::spawn(move || {
        let mut stored = HashMap::<String, serde_json::Value>::new();
        loop {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let (header, body) = read_http(&mut stream);
            let first = header.lines().next().expect("request line");
            let response = if first.starts_with("POST ") {
                let request: serde_json::Value =
                    serde_json::from_slice(&body).expect("retrieve JSON");
                let with_vector = request["with_vector"].as_bool().unwrap_or(true);
                let points = request["ids"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|id| id.as_str())
                    .filter_map(|id| stored.get(id).cloned())
                    .map(|mut point| {
                        if !with_vector && let Some(object) = point.as_object_mut() {
                            object.remove("vector");
                        }
                        point
                    })
                    .collect::<Vec<_>>();
                *retrieves.lock().expect("retrieves") += 1;
                serde_json::json!({"result": points}).to_string()
            } else if first.starts_with("PUT ") {
                let request: serde_json::Value =
                    serde_json::from_slice(&body).expect("upsert JSON");
                let points = request["points"].as_array().cloned().unwrap_or_default();
                for point in points {
                    let Some(id) = point["id"].as_str().map(str::to_owned) else {
                        continue;
                    };
                    stored.insert(id, point);
                }
                r#"{"result":{"status":"completed"}}"#.to_owned()
            } else {
                panic!("unexpected Qdrant request: {first}");
            };
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                response.len()
            )
            .expect("response");
        }
    });
    format!("http://{address}")
}

fn main() {
    let recipe = recipe();
    let binding = binding(recipe);
    let retrieves = Arc::new(Mutex::new(0_usize));
    let endpoint = serve(Arc::clone(&retrieves));
    let client = QdrantHttpClient::new(
        QdrantHttpConfig {
            endpoint,
            collection: "vectors".to_owned(),
            api_key: None,
            connect_deadline: Duration::from_secs(2),
            read_deadline: Duration::from_secs(2),
            attempts: NonZeroU8::new(1).expect("attempts"),
            max_response_bytes: 8 * 1024 * 1024,
            max_request_bytes: 16 * 1024 * 1024,
            max_batch_points: DOCUMENTS,
        },
        recipe,
    )
    .expect("client");
    let batch = documents(recipe);
    let cold = client.upsert(binding, &batch).expect("cold upsert");
    assert_eq!(
        cold,
        QdrantMutationReceipt {
            points: DOCUMENTS,
            batches: 1,
        }
    );
    let mut samples = [0_u128; SAMPLES];
    for sample in 0..(WARMUPS + SAMPLES) {
        let before = *retrieves.lock().expect("retrieves");
        let started = Instant::now();
        let warm = client.upsert(binding, &batch).expect("warm upsert");
        let elapsed = started.elapsed().as_nanos();
        assert_eq!(black_box(warm.points), 0);
        assert_eq!(warm.batches, 0);
        assert_eq!(*retrieves.lock().expect("retrieves"), before + 1);
        if sample >= WARMUPS {
            samples[sample - WARMUPS] = elapsed;
        }
    }
    println!(
        "coordinate_warm documents={DOCUMENTS} dimensions={DIMENSIONS} warm_median_ns={} warm_p95_ns={}",
        percentile(&mut samples, SAMPLES / 2),
        percentile(&mut samples, SAMPLES * 95 / 100),
    );
}
