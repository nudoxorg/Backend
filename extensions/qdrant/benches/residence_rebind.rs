//! Loopback benchmark for a view-fence rebind of resident Qdrant coordinates.
//!
//! A cold batch uploads every vector. A later batch with the same coordinates,
//! a new frontier, and reminted candidate ids restamps payload and uploads no
//! vector. A one-document coordinate change uploads that vector and restamps
//! the siblings' payloads.
//!
//! Timers wrap only the resident upsert. Document construction and the
//! loopback ledger stay outside the measurement.
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
    Authority, Binding, CandidateId, CandidateRelation, CoordinateWrite, DocumentVector,
    EmbeddingEncoding, EmbeddingNormalization, EmbeddingPooling, EmbeddingRecipe, Frontier, Metric,
    ModelVersion, PointResidence, QdrantHttpClient, QdrantHttpConfig, ReadManifest,
    ResidentDocument, ResidentMutationReceipt, TokenizerVersion, TreatmentVersion,
};
use backend_version::{
    AuthorityScopeClaim, CoverageWitness, ProducerObservationClaims, ProducerObservationVerifier,
    RelationState, ScopeRoot, UntrustedProducerObservation, WorkspaceManifest,
    admit_complete_scope, admit_producer_observation,
};

const SAMPLES: usize = 12;
const WARMUPS: usize = 2;

struct BodyRecord {
    points: usize,
    bytes: usize,
}

struct Ledger {
    puts: Vec<BodyRecord>,
    payloads: Vec<BodyRecord>,
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

fn recipe(dimensions: usize) -> EmbeddingRecipe {
    EmbeddingRecipe {
        model: ModelVersion::from_value(&[1; 32]),
        tokenizer: TokenizerVersion::from_value(&[2; 32]),
        dimensions: NonZeroU32::new(u32::try_from(dimensions).expect("dimensions"))
            .expect("dimension"),
        metric: Metric::CosineDistance,
        pooling: EmbeddingPooling::Mean,
        normalization: EmbeddingNormalization::None,
        encoding: EmbeddingEncoding::Float32,
        query_treatment: TreatmentVersion::from_value(b"query"),
        document_treatment: TreatmentVersion::from_value(b"document"),
    }
}

fn binding(recipe: EmbeddingRecipe, frontier: [u8; 32]) -> Binding {
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
    .with_frontier(Frontier::from_value(&frontier))
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
            let response = if first.contains("/points/batch") {
                let request: serde_json::Value =
                    serde_json::from_slice(&body).expect("payload JSON");
                let mut points = 0_usize;
                for operation in request["operations"].as_array().into_iter().flatten() {
                    let payload = operation["set_payload"]["payload"].clone();
                    for id in operation["set_payload"]["points"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|id| id.as_str())
                    {
                        let Some(point) = stored.get_mut(id) else {
                            continue;
                        };
                        point["payload"] = payload.clone();
                        points += 1;
                    }
                }
                ledger.lock().expect("ledger").payloads.push(BodyRecord {
                    points,
                    bytes: body.len(),
                });
                r#"{"result":{"status":"completed"}}"#.to_owned()
            } else if first.starts_with("POST ") {
                let request: serde_json::Value =
                    serde_json::from_slice(&body).expect("retrieve JSON");
                let points = request["ids"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|id| id.as_str())
                    .filter_map(|id| {
                        let mut point = stored.get(id)?.clone();
                        if let Some(object) = point.as_object_mut() {
                            object.remove("vector");
                        }
                        Some(point)
                    })
                    .collect::<Vec<_>>();
                serde_json::json!({"result": points}).to_string()
            } else if first.starts_with("PUT ") {
                let request: serde_json::Value =
                    serde_json::from_slice(&body).expect("upsert JSON");
                let points = request["points"].as_array().cloned().unwrap_or_default();
                ledger.lock().expect("ledger").puts.push(BodyRecord {
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
        max_response_bytes: 8 * 1024 * 1024,
        max_request_bytes: 8 * 1024 * 1024,
        max_batch_points: 256,
    }
}

fn prepare(
    embedding: EmbeddingRecipe,
    frontier: [u8; 32],
    sample: usize,
    size: usize,
    candidate_base: u64,
    revised: Option<usize>,
) -> (Binding, Vec<PointResidence>, Vec<DocumentVector>) {
    let binding = binding(embedding, frontier);
    let residences = (0..size)
        .map(|index| {
            PointResidence::for_row(
                binding.workspace,
                binding.recipe,
                &format!("row-{sample}-{index}"),
            )
            .expect("residence")
        })
        .collect::<Vec<_>>();
    let documents = (0..size)
        .map(|index| {
            let id = CandidateId::new(candidate_base + u64::try_from(index).expect("index"))
                .expect("candidate");
            let values = (0..usize::try_from(embedding.dimensions.get()).expect("dimensions"))
                .map(|dimension| {
                    let mut lane = (index + dimension) % 251 + 1;
                    if revised == Some(index) {
                        lane = lane.saturating_add(17);
                    }
                    f32::from(u16::try_from(lane).expect("coordinate")) / 251.0
                })
                .collect();
            DocumentVector::new(embedding, id, values).expect("document vector")
        })
        .collect();
    (binding, residences, documents)
}

fn residents<'a>(
    residences: &'a [PointResidence],
    documents: &'a [DocumentVector],
    revised: Option<usize>,
) -> Vec<ResidentDocument<'a>> {
    residences
        .iter()
        .zip(documents.iter())
        .enumerate()
        .map(|(index, (residence, document))| ResidentDocument {
            residence: *residence,
            write: if revised == Some(index) {
                CoordinateWrite::Replace
            } else {
                CoordinateWrite::Hold
            },
            document,
        })
        .collect()
}

fn run(size: usize, dimensions: usize) {
    let embedding = recipe(dimensions);
    let ledger = Arc::new(Mutex::new(Ledger {
        puts: Vec::new(),
        payloads: Vec::new(),
    }));
    let endpoint = serve(Arc::clone(&ledger));
    let client = QdrantHttpClient::new(config(endpoint), embedding).expect("client");
    let mut cold = [0_u128; SAMPLES];
    let mut rebind = [0_u128; SAMPLES];
    let mut revise = [0_u128; SAMPLES];
    let mut cold_bytes = [0_usize; SAMPLES];
    let mut rebind_bytes = [0_usize; SAMPLES];
    let mut revise_bytes = [0_usize; SAMPLES];
    for sample in 0..(WARMUPS + SAMPLES) {
        let origin = u64::try_from(sample)
            .expect("sample")
            .saturating_mul(10_000)
            .saturating_add(1);
        let (cold_binding, residences, cold_documents) =
            prepare(embedding, [3; 32], sample, size, origin, None);
        let cold_residents = residents(&residences, &cold_documents, None);
        let (fence_binding, _, fence_documents) =
            prepare(embedding, [4; 32], sample, size, origin + 100_000, None);
        let fence_residents = residents(&residences, &fence_documents, None);
        let (revise_binding, _, revise_documents) =
            prepare(embedding, [5; 32], sample, size, origin + 200_000, Some(0));
        let revise_residents = residents(&residences, &revise_documents, Some(0));
        let (puts_before, payloads_before) = {
            let ledger = ledger.lock().expect("ledger");
            (ledger.puts.len(), ledger.payloads.len())
        };
        let started = Instant::now();
        let cold_receipt = client
            .upsert_resident(cold_binding, &cold_residents)
            .expect("cold upsert");
        let cold_elapsed = started.elapsed().as_nanos();
        let started = Instant::now();
        let rebind_receipt = client
            .upsert_resident(fence_binding, &fence_residents)
            .expect("fence rebind");
        let rebind_elapsed = started.elapsed().as_nanos();
        let started = Instant::now();
        let revise_receipt = client
            .upsert_resident(revise_binding, &revise_residents)
            .expect("one revision");
        let revise_elapsed = started.elapsed().as_nanos();
        assert_eq!(
            cold_receipt,
            ResidentMutationReceipt {
                vectors: size,
                payloads: 0,
                unchanged: 0,
                batches: 1,
            }
        );
        assert_eq!(
            rebind_receipt,
            ResidentMutationReceipt {
                vectors: 0,
                payloads: size,
                unchanged: 0,
                batches: 1,
            }
        );
        assert_eq!(
            revise_receipt,
            ResidentMutationReceipt {
                vectors: 1,
                payloads: size.saturating_sub(1),
                unchanged: 0,
                batches: 2,
            }
        );
        let ledger = ledger.lock().expect("ledger");
        let cold_put = &ledger.puts[puts_before];
        let revise_put = &ledger.puts[puts_before + 1];
        let rebind_payload = &ledger.payloads[payloads_before];
        assert_eq!(cold_put.points, size);
        assert_eq!(rebind_payload.points, size);
        assert_eq!(revise_put.points, 1);
        assert!(rebind_payload.bytes < cold_put.bytes);
        assert!(revise_put.bytes < cold_put.bytes);
        if sample >= WARMUPS {
            let recorded = sample - WARMUPS;
            cold[recorded] = cold_elapsed;
            rebind[recorded] = rebind_elapsed;
            revise[recorded] = revise_elapsed;
            cold_bytes[recorded] = cold_put.bytes;
            rebind_bytes[recorded] = rebind_payload.bytes;
            revise_bytes[recorded] = revise_put.bytes;
        }
    }
    println!(
        "residence_rebind size={size} dimensions={dimensions} samples={SAMPLES} warmups={WARMUPS} cold_vectors median={}ns p95={}ns put_bytes={} fence_rebind median={}ns p95={}ns payload_bytes={} vector_put_bytes=0 one_replace median={}ns p95={}ns vector_put_bytes={}",
        percentile(&mut cold, SAMPLES / 2),
        percentile(&mut cold, SAMPLES * 95 / 100),
        cold_bytes[0],
        percentile(&mut rebind, SAMPLES / 2),
        percentile(&mut rebind, SAMPLES * 95 / 100),
        rebind_bytes[0],
        percentile(&mut revise, SAMPLES / 2),
        percentile(&mut revise, SAMPLES * 95 / 100),
        revise_bytes[0]
    );
}

fn main() {
    for dimensions in [32, 384] {
        for size in [32, 128] {
            run(size, dimensions);
        }
    }
}
