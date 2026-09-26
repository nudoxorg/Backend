//! Loopback benchmark for delta-aware Qdrant upserts.
//!
//! A cold batch misses every coordinate and pays for a retrieve plus a full put.
//! A warm batch retrieves matching keys and writes nothing. A one-point delta
//! retrieves the resident batch and puts only the missing point.
//!
//! Retrieve honors `with_vector`. The production client asks for payload only,
//! so a warm readback is the payload, not a copy of the stored vectors.
#![allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::large_stack_arrays,
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

const SAMPLES: usize = 12;
const WARMUPS: usize = 2;
const DIMENSIONS: usize = 32;

struct PutRecord {
    points: usize,
    bytes: usize,
}

struct Ledger {
    puts: Vec<PutRecord>,
    retrieves: Vec<usize>,
}

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

fn documents(recipe: EmbeddingRecipe, first: u64, count: usize) -> Vec<DocumentVector> {
    (0..count)
        .map(|offset| {
            let id = CandidateId::new(first + u64::try_from(offset).expect("offset"))
                .expect("candidate");
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

fn serve(ledger: Arc<Mutex<Ledger>>) -> String {
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
                let response = serde_json::json!({"result": points}).to_string();
                ledger
                    .lock()
                    .expect("ledger")
                    .retrieves
                    .push(response.len());
                response
            } else if first.starts_with("PUT ") {
                let request: serde_json::Value =
                    serde_json::from_slice(&body).expect("upsert JSON");
                let points = request["points"].as_array().cloned().unwrap_or_default();
                ledger.lock().expect("ledger").puts.push(PutRecord {
                    points: points.len(),
                    bytes: body.len(),
                });
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

fn config(endpoint: String) -> QdrantHttpConfig {
    QdrantHttpConfig {
        endpoint,
        collection: "vectors".to_owned(),
        api_key: None,
        connect_deadline: Duration::from_secs(2),
        read_deadline: Duration::from_secs(2),
        attempts: NonZeroU8::new(1).expect("attempts"),
        max_response_bytes: 2 * 1024 * 1024,
        max_request_bytes: 2 * 1024 * 1024,
        max_batch_points: 256,
    }
}

fn run(size: usize) {
    let recipe = recipe();
    let binding = binding(recipe);
    let ledger = Arc::new(Mutex::new(Ledger {
        puts: Vec::new(),
        retrieves: Vec::new(),
    }));
    let endpoint = serve(Arc::clone(&ledger));
    let client = QdrantHttpClient::new(config(endpoint), recipe).expect("client");
    let mut cold = [0_u128; SAMPLES];
    let mut warm = [0_u128; SAMPLES];
    let mut delta = [0_u128; SAMPLES];
    let mut cold_bytes = [0_usize; SAMPLES];
    let mut delta_bytes = [0_usize; SAMPLES];
    let mut cold_read_bytes = [0_usize; SAMPLES];
    let mut warm_read_bytes = [0_usize; SAMPLES];
    for sample in 0..(WARMUPS + SAMPLES) {
        let origin = u64::try_from(sample)
            .expect("sample")
            .saturating_mul(u64::try_from(size + 1).expect("stride"))
            .saturating_add(1);
        let batch = documents(recipe, origin, size);
        let extra = documents(recipe, origin + u64::try_from(size).expect("extra"), 1);
        let (puts_before, reads_before) = {
            let ledger = ledger.lock().expect("ledger");
            (ledger.puts.len(), ledger.retrieves.len())
        };
        let started = Instant::now();
        let cold_receipt = client.upsert(binding, &batch).expect("cold upsert");
        let cold_elapsed = started.elapsed().as_nanos();
        let started = Instant::now();
        let warm_receipt = client.upsert(binding, &batch).expect("warm upsert");
        let warm_elapsed = started.elapsed().as_nanos();
        let mut grown = batch.clone();
        grown.extend(extra);
        let started = Instant::now();
        let delta_receipt = client.upsert(binding, &grown).expect("delta upsert");
        let delta_elapsed = started.elapsed().as_nanos();
        assert_eq!(
            cold_receipt,
            QdrantMutationReceipt {
                points: size,
                batches: 1,
            }
        );
        assert_eq!(
            warm_receipt,
            QdrantMutationReceipt {
                points: 0,
                batches: 0,
            }
        );
        assert_eq!(
            delta_receipt,
            QdrantMutationReceipt {
                points: 1,
                batches: 1,
            }
        );
        let ledger = ledger.lock().expect("ledger");
        assert_eq!(ledger.puts.len(), puts_before + 2);
        assert_eq!(ledger.retrieves.len(), reads_before + 3);
        let cold_put = &ledger.puts[puts_before];
        let delta_put = &ledger.puts[puts_before + 1];
        let cold_read = ledger.retrieves[reads_before];
        let warm_read = ledger.retrieves[reads_before + 1];
        assert_eq!(cold_put.points, size);
        assert_eq!(delta_put.points, 1);
        assert!(delta_put.bytes < cold_put.bytes);
        assert!(warm_read > cold_read);
        assert!(
            warm_read < cold_put.bytes,
            "a payload-only readback must be smaller than the vector put: warm {warm_read} cold put {}",
            cold_put.bytes
        );
        if sample >= WARMUPS {
            let recorded = sample - WARMUPS;
            cold[recorded] = cold_elapsed;
            warm[recorded] = warm_elapsed;
            delta[recorded] = delta_elapsed;
            cold_bytes[recorded] = cold_put.bytes;
            delta_bytes[recorded] = delta_put.bytes;
            cold_read_bytes[recorded] = cold_read;
            warm_read_bytes[recorded] = warm_read;
        }
    }
    println!(
        "http_delta size={size} dimensions={DIMENSIONS} samples={SAMPLES} warmups={WARMUPS} cold_upsert median={}ns p95={}ns retrieve_bytes={} put_bytes={} warm_skip median={}ns p95={}ns retrieve_bytes={} put_bytes=0 delta_one median={}ns p95={}ns put_bytes={}",
        percentile(&mut cold, SAMPLES / 2),
        percentile(&mut cold, SAMPLES * 95 / 100),
        cold_read_bytes[0],
        cold_bytes[0],
        percentile(&mut warm, SAMPLES / 2),
        percentile(&mut warm, SAMPLES * 95 / 100),
        warm_read_bytes[0],
        percentile(&mut delta, SAMPLES / 2),
        percentile(&mut delta, SAMPLES * 95 / 100),
        delta_bytes[0]
    );
}

fn main() {
    for size in [32, 128] {
        run(size);
    }
}
