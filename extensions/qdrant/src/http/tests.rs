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
    let payload = PointPayload::for_candidate(binding, candidate);
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
