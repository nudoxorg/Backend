#![allow(
    clippy::expect_used,
    reason = "loopback fixtures fail locally and loudly when their deterministic setup breaks"
)]
use std::{
    io::{Read, Write},
    net::TcpListener,
    num::{NonZeroU8, NonZeroU32},
    thread,
};

use backend_version::ObjectVersion;

use super::*;
use crate::{
    EmbeddingNormalization, EmbeddingPooling, Metric, ModelSchema, TokenizerSchema, TreatmentSchema,
};

fn test_recipe() -> EmbeddingRecipe {
    EmbeddingRecipe {
        model: ObjectVersion::<ModelSchema>::from_value(&[1; 32]),
        tokenizer: ObjectVersion::<TokenizerSchema>::from_value(&[2; 32]),
        dimensions: NonZeroU32::new(2).expect("dimension"),
        metric: Metric::CosineDistance,
        pooling: EmbeddingPooling::Mean,
        normalization: EmbeddingNormalization::UnitL2,
        encoding: EmbeddingEncoding::Float32,
        query_treatment: ObjectVersion::<TreatmentSchema>::from_value(b"query".as_slice()),
        document_treatment: ObjectVersion::<TreatmentSchema>::from_value(b"document".as_slice()),
    }
}

fn upsert_test_recipe() -> EmbeddingRecipe {
    EmbeddingRecipe {
        normalization: EmbeddingNormalization::None,
        ..test_recipe()
    }
}

fn upsert_binding() -> Binding {
    use crate::Frontier;

    let recipe = upsert_test_recipe();
    let (mut binding, _) = crate::tests::binding(&[]);
    binding.recipe = recipe.version();
    binding.with_frontier(Frontier::from_value(&[3; 32]))
}

fn test_config(endpoint: String) -> QdrantHttpConfig {
    QdrantHttpConfig {
        endpoint,
        collection: "vectors".to_owned(),
        api_key: None,
        connect_deadline: Duration::from_secs(1),
        read_deadline: Duration::from_secs(1),
        attempts: NonZeroU8::new(1).expect("attempts"),
        max_response_bytes: 4096,
        max_request_bytes: 4096,
        max_batch_points: 32,
    }
}

#[test]
fn configuration_rejects_header_injection_and_ambiguous_authorities() {
    assert!(ApiKey::new("secret\r\nx-injected: yes").is_err());
    for endpoint in [
        "http://user@127.0.0.1:6333",
        "http://127.0.0.1:6333?target=elsewhere",
        "ftp://127.0.0.1:6333",
        "http:///missing-authority",
        "http://example.com:6333",
    ] {
        assert!(test_config(endpoint.to_owned()).validate().is_err());
    }
}

#[test]
fn encoded_request_is_rejected_before_network_io() {
    let mut config = test_config("http://127.0.0.1:1".to_owned());
    config.max_request_bytes = 1;
    let client = QdrantHttpClient::new(config, test_recipe()).expect("client");
    let error = client
        .transport
        .request(Method::Post, "http://127.0.0.1:1", Some(&vec![0_u8; 64]))
        .expect_err("oversized request");
    assert!(matches!(error, HttpProviderError::RequestTooLarge));
}

#[test]
fn exact_quality_request_disables_approximate_candidate_selection() {
    let encoded = serde_json::to_value(SearchParams { exact: true }).expect("encode exact query");
    assert_eq!(encoded["exact"], true);
}

#[test]
fn physical_point_identity_is_scoped_by_the_complete_binding() {
    use crate::Frontier;

    let (binding, _) = crate::tests::binding(&[]);
    let binding = binding.with_frontier(Frontier::from_value(&[3; 32]));
    let candidate = CandidateId::new(7).expect("candidate");
    let first = PhysicalPointId::for_candidate(binding, candidate);
    let mut next = binding;
    next.frontier = Frontier::from_value(&[4; 32]);
    let second = PhysicalPointId::for_candidate(next, candidate);

    assert_ne!(first, second);
    assert_eq!(first.0.len(), 36);
    assert_eq!(first.0.bytes().filter(|byte| *byte == b'-').count(), 4);
    let payload = PointPayload::for_candidate(binding, candidate, &[0.25, -0.5]);
    assert_eq!(payload.candidate(), Some(candidate));
    assert!(payload.matches_binding(binding));
    assert!(!payload.matches_binding(next));
    let mut noncanonical = payload;
    noncanonical.candidate = "7".to_owned();
    assert_eq!(noncanonical.candidate(), None);
}

#[test]
fn qdrant_authority_cannot_redirect_the_client() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let address = listener.local_addr().expect("address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("connection");
        let mut request = Vec::new();
        let mut byte = [0_u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).expect("request byte");
            request.push(byte[0]);
        }
        write!(
            stream,
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://127.0.0.1:1/stolen\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .expect("response");
    });
    let client = QdrantHttpClient::new(test_config(format!("http://{address}")), test_recipe())
        .expect("client");
    assert!(matches!(
        client.ensure_collection(),
        Err(HttpProviderError::HttpStatus(307))
    ));
    server.join().expect("server");
}

#[test]
fn collection_probe_retries_and_redacts_authenticated_configuration() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let address = listener.local_addr().expect("address");
    let server = thread::spawn(move || {
        for index in 0..3 {
            let (mut stream, _) = listener.accept().expect("connection");
            let mut request = Vec::new();
            let mut byte = [0_u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).expect("request byte");
                request.push(byte[0]);
            }
            let text = String::from_utf8(request).expect("HTTP request");
            assert!(text.contains("api-key: top-secret") || text.contains("api-key:top-secret"));
            let (status, body) = if index == 0 {
                ("503 Service Unavailable", "{}")
            } else {
                (
                    "200 OK",
                    r#"{"result":{"config":{"params":{"vectors":{"size":2,"distance":"Cosine"}}}}}"#,
                )
            };
            write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).expect("response");
        }
    });
    let key = ApiKey::new("top-secret").expect("key");
    assert_eq!(format!("{key:?}"), "ApiKey([REDACTED])");
    let recipe = test_recipe();
    let client = QdrantHttpClient::new(
        QdrantHttpConfig {
            endpoint: format!("http://{address}"),
            collection: "vectors".to_owned(),
            api_key: Some(key),
            connect_deadline: Duration::from_secs(1),
            read_deadline: Duration::from_secs(1),
            attempts: NonZeroU8::new(2).expect("attempts"),
            max_response_bytes: 4096,
            max_request_bytes: 4096,
            max_batch_points: 32,
        },
        recipe,
    )
    .expect("client");
    client.ensure_collection().expect("verified collection");
    server.join().expect("server");
}

#[test]
fn coordinate_key_is_stable_for_identical_bits_and_changes_with_one_bit_flip() {
    let values = vec![0.25_f32, -0.5];
    let first = coordinate_key(&values);
    let second = coordinate_key(&values);
    assert_eq!(first, second);

    let mut flipped = values.clone();
    flipped[0] = f32::from_bits(values[0].to_bits() ^ 1);
    assert_ne!(first, coordinate_key(&flipped));
}

#[test]
fn coordinate_disposition_classifies_legacy_missing_and_matching_keys() {
    assert_eq!(
        coordinate_disposition("expected", None),
        CoordinateDisposition::Due
    );
    assert_eq!(
        coordinate_disposition("expected", Some("")),
        CoordinateDisposition::Due
    );
    assert_eq!(
        coordinate_disposition("expected", Some("expected")),
        CoordinateDisposition::Unchanged
    );
    assert_eq!(
        coordinate_disposition("expected", Some("other")),
        CoordinateDisposition::Conflict
    );
}

#[test]
fn point_payload_coordinate_key_round_trips_and_defaults_for_legacy_json() {
    use crate::Frontier;

    let (binding, _) = crate::tests::binding(&[]);
    let binding = binding.with_frontier(Frontier::from_value(&[3; 32]));
    let candidate = CandidateId::new(7).expect("candidate");
    let payload = PointPayload::for_candidate(binding, candidate, &[0.25, -0.5]);
    assert!(!payload.coordinate_key.is_empty());

    let encoded = serde_json::to_string(&payload).expect("encode payload");
    assert!(encoded.contains("coordinate_key"));

    let decoded: PointPayload = serde_json::from_str(&encoded).expect("decode payload");
    assert_eq!(decoded, payload);

    let legacy = r#"{
        "workspace":"00",
        "root":"00",
        "recipe":"00",
        "authority":"00",
        "read_manifest":"00",
        "frontier":"00",
        "candidate":"0000000000000007"
    }"#;
    let legacy_payload: PointPayload = serde_json::from_str(legacy).expect("legacy payload");
    assert!(legacy_payload.coordinate_key.is_empty());
}

#[test]
fn upsert_skips_put_when_retrieved_coordinate_key_matches() {
    let binding = upsert_binding();
    let candidate = CandidateId::new(7).expect("candidate");
    let values = vec![0.25_f32, -0.5];
    let document = DocumentVector::new(upsert_test_recipe(), candidate, values.clone())
        .expect("document vector");
    let physical_id = PhysicalPointId::for_candidate(binding, candidate);
    let payload = PointPayload::for_candidate(binding, candidate, &values);
    let retrieve_body = serde_json::json!({
        "result": [{
            "id": physical_id.0,
            "payload": payload,
        }]
    })
    .to_string();

    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let address = listener.local_addr().expect("address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("connection");
        let mut request = Vec::new();
        let mut byte = [0_u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).expect("request byte");
            request.push(byte[0]);
        }
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            retrieve_body.len(),
            retrieve_body
        )
        .expect("response");
    });

    let client = QdrantHttpClient::new(
        test_config(format!("http://{address}")),
        upsert_test_recipe(),
    )
    .expect("client");
    let receipt = client
        .upsert(binding, std::slice::from_ref(&document))
        .expect("upsert");
    assert_eq!(
        receipt,
        QdrantMutationReceipt {
            points: 0,
            batches: 0,
        }
    );
    server.join().expect("server");
}

#[test]
fn upsert_puts_missing_points_after_empty_retrieve() {
    let binding = upsert_binding();
    let candidate = CandidateId::new(7).expect("candidate");
    let document = DocumentVector::new(upsert_test_recipe(), candidate, vec![0.25_f32, -0.5])
        .expect("document vector");
    let retrieve_body = r#"{"result":[]}"#;
    let put_body = r#"{"result":{"status":"completed"}}"#;

    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let address = listener.local_addr().expect("address");
    let server = thread::spawn(move || {
        for (index, body) in [retrieve_body, put_body].into_iter().enumerate() {
            let (mut stream, _) = listener.accept().expect("connection");
            let mut request = Vec::new();
            let mut byte = [0_u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).expect("request byte");
                request.push(byte[0]);
            }
            let text = String::from_utf8(request).expect("HTTP request");
            if index == 1 {
                assert!(text.starts_with("PUT "));
            }
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("response");
        }
    });

    let client = QdrantHttpClient::new(
        test_config(format!("http://{address}")),
        upsert_test_recipe(),
    )
    .expect("client");
    let receipt = client
        .upsert(binding, std::slice::from_ref(&document))
        .expect("upsert");
    assert_eq!(
        receipt,
        QdrantMutationReceipt {
            points: 1,
            batches: 1,
        }
    );
    server.join().expect("server");
}

struct DeltaScript {
    requests: usize,
    corrupt_after_put: bool,
}

fn serve_delta(script: DeltaScript) -> (String, thread::JoinHandle<Vec<(&'static str, usize)>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
    let address = listener.local_addr().expect("address");
    let server = thread::spawn(move || {
        let mut stored = std::collections::HashMap::<String, serde_json::Value>::new();
        let mut transcript = Vec::with_capacity(script.requests);
        for _ in 0..script.requests {
            let (mut stream, _) = listener.accept().expect("connection");
            let (header, body) = read_http(&mut stream);
            let first = header.lines().next().expect("request line");
            let (kind, put_points, response) = if first.starts_with("POST ") {
                let request: serde_json::Value =
                    serde_json::from_slice(&body).expect("retrieve JSON");
                let points = request["ids"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|id| id.as_str())
                    .filter_map(|id| stored.get(id).cloned())
                    .collect::<Vec<_>>();
                (
                    "retrieve",
                    0,
                    serde_json::json!({"result": points}).to_string(),
                )
            } else if first.starts_with("PUT ") {
                let request: serde_json::Value =
                    serde_json::from_slice(&body).expect("upsert JSON");
                let points = request["points"].as_array().cloned().unwrap_or_default();
                let count = points.len();
                for mut point in points {
                    let Some(id) = point["id"].as_str().map(str::to_owned) else {
                        continue;
                    };
                    if script.corrupt_after_put {
                        point["payload"]["coordinate_key"] =
                            serde_json::json!("rewritten-coordinate");
                    }
                    stored.insert(id, point);
                }
                (
                    "put",
                    count,
                    r#"{"result":{"status":"completed"}}"#.to_owned(),
                )
            } else {
                panic!("unexpected Qdrant request: {first}");
            };
            transcript.push((kind, put_points));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                response.len()
            )
            .expect("response");
        }
        transcript
    });
    (format!("http://{address}"), server)
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

#[test]
fn upsert_rejects_a_rewritten_coordinate_without_another_put() {
    let binding = upsert_binding();
    let recipe = upsert_test_recipe();
    let document = DocumentVector::new(
        recipe,
        CandidateId::new(7).expect("candidate"),
        vec![0.25, -0.5],
    )
    .expect("document vector");
    let (endpoint, server) = serve_delta(DeltaScript {
        requests: 3,
        corrupt_after_put: true,
    });
    let client = QdrantHttpClient::new(test_config(endpoint), recipe).expect("client");
    let cold = client
        .upsert(binding, std::slice::from_ref(&document))
        .expect("cold upsert");
    assert_eq!(
        cold,
        QdrantMutationReceipt {
            points: 1,
            batches: 1,
        }
    );
    let error = client
        .upsert(binding, std::slice::from_ref(&document))
        .expect_err("rewritten coordinate");
    assert!(matches!(error, HttpProviderError::ImmutableVector));
    let transcript = server.join().expect("server");
    assert_eq!(transcript, [("retrieve", 0), ("put", 1), ("retrieve", 0)]);
}

#[test]
fn upsert_writes_only_the_point_missing_from_retrieve() {
    let binding = upsert_binding();
    let recipe = upsert_test_recipe();
    let present = DocumentVector::new(
        recipe,
        CandidateId::new(7).expect("candidate"),
        vec![0.25, -0.5],
    )
    .expect("present vector");
    let missing = DocumentVector::new(
        recipe,
        CandidateId::new(8).expect("candidate"),
        vec![0.5, 0.25],
    )
    .expect("missing vector");
    let (endpoint, server) = serve_delta(DeltaScript {
        requests: 4,
        corrupt_after_put: false,
    });
    let client = QdrantHttpClient::new(test_config(endpoint), recipe).expect("client");
    let cold = client
        .upsert(binding, std::slice::from_ref(&present))
        .expect("cold upsert");
    assert_eq!(cold.points, 1);
    let delta = client
        .upsert(binding, &[present, missing])
        .expect("delta upsert");
    assert_eq!(
        delta,
        QdrantMutationReceipt {
            points: 1,
            batches: 1,
        }
    );
    let transcript = server.join().expect("server");
    assert_eq!(
        transcript,
        [("retrieve", 0), ("put", 1), ("retrieve", 0), ("put", 1)]
    );
}
