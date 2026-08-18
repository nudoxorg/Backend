#![cfg(feature = "server")]
//! Integration + unit tests for `POST /v1/compiled/lookup` (SMOLVM-PLAN §5, SV-6).
//!
//! Test coverage:
//! - DTO serde roundtrips: hit + miss exact JSON shape.
//! - Validation failures: bad hex, >1024 keys, empty list.
//! - Handler: all-miss on keys nothing was ever recorded for.
//! - Handler: a generation seeded via `store.record()` (the VCS-native write
//!   path) comes back as a hit with channel, tip, and generation_stamp.
//! - Handler: correct HTTP status codes.
//! - Handler: body limit enforced (>256 KiB body → 413).
//! - Ordering: results follow input order.
//! - Dedup: duplicate keys each produce an independent entry.
//! - Client: bad tip hex and bad stamp hex fail verification.

#[allow(unused_imports)]
use index::server::registry;
mod server_common;

use axum::http::StatusCode;
use index::server::http::dto::{COMPILED_LOOKUP_MAX_KEYS, CompiledLookupRequest, JobKeyHex};
use index::server::http::router::router;

// ─────────────────────────────────────────────────────────────────────────────
// § DTO unit tests — no server needed
// ─────────────────────────────────────────────────────────────────────────────

/// A valid 64-lowercase-hex-char job key.
fn valid_hex_key() -> String {
    "a".repeat(64)
}

/// Parse a [`JobKeyHex`] from a bare string value.
fn parse_key(s: &str) -> serde_json::Result<JobKeyHex> {
    serde_json::from_value(serde_json::Value::String(s.to_owned()))
}

// ── JobKeyHex deserialization ─────────────────────────────────────────────────

#[test]
fn job_key_hex_valid_roundtrip() {
    let key = parse_key(&valid_hex_key()).expect("valid 64-char lowercase hex deserializes");
    assert_eq!(
        key.as_str(),
        valid_hex_key(),
        "roundtrip preserves the hex string"
    );
    let raw = key.clone().into_bytes();
    assert_eq!(raw, [0xaa_u8; 32], "0xaa repeated 32 times matches 'a'*64");
    let serialized = serde_json::to_value(&key).expect("JobKeyHex serializes");
    assert_eq!(serialized, serde_json::Value::String(valid_hex_key()));
}

#[test]
fn job_key_hex_rejects_odd_length() {
    assert!(parse_key("abc").is_err(), "odd-length hex must be rejected");
}

#[test]
fn job_key_hex_rejects_63_chars() {
    assert!(
        parse_key(&"a".repeat(63)).is_err(),
        "63 chars (not 64) must be rejected"
    );
}

#[test]
fn job_key_hex_rejects_65_chars() {
    assert!(
        parse_key(&"a".repeat(65)).is_err(),
        "65 chars (not 64) must be rejected"
    );
}

#[test]
fn job_key_hex_rejects_uppercase() {
    let upper = "A".repeat(64);
    assert!(parse_key(&upper).is_err(), "uppercase hex must be rejected");
}

#[test]
fn job_key_hex_rejects_mixed_case() {
    let mixed = format!("{}{}", "a".repeat(32), "A".repeat(32));
    assert!(
        parse_key(&mixed).is_err(),
        "mixed-case hex must be rejected"
    );
}

#[test]
fn job_key_hex_rejects_non_hex() {
    let bad = format!("{}{}", "z".repeat(32), "a".repeat(32));
    assert!(
        parse_key(&bad).is_err(),
        "non-hex character must be rejected"
    );
}

// ── CompiledLookupRequest list-level validation ───────────────────────────────

#[test]
fn request_empty_list_is_invalid() {
    let req: CompiledLookupRequest = serde_json::from_value(serde_json::json!({ "job_keys": [] }))
        .expect("empty list deserializes without error");
    let err = req
        .validate()
        .expect_err("empty job_keys must be rejected by validate()");
    let msg = err.to_string();
    assert!(
        msg.contains("job_keys"),
        "error must mention the missing field; got: {msg}"
    );
}

#[test]
fn request_over_1024_keys_is_invalid() {
    let keys: Vec<String> = (0..COMPILED_LOOKUP_MAX_KEYS + 1)
        .map(|i| format!("{:064x}", i as u64))
        .collect();
    let req: CompiledLookupRequest =
        serde_json::from_value(serde_json::json!({ "job_keys": keys }))
            .expect(">1024 keys deserialize; validation happens in validate()");
    let err = req
        .validate()
        .expect_err("1025 keys must be rejected by validate()");
    let msg = err.to_string();
    assert!(
        msg.contains("1025"),
        "error must state the actual count; got: {msg}"
    );
}

#[test]
fn request_exactly_1024_keys_is_valid() {
    let keys: Vec<String> = (0..COMPILED_LOOKUP_MAX_KEYS)
        .map(|i| format!("{:064x}", i as u64))
        .collect();
    let req: CompiledLookupRequest =
        serde_json::from_value(serde_json::json!({ "job_keys": keys }))
            .expect("1024 keys deserialize");
    req.validate()
        .expect("exactly 1024 keys is within the limit");
}

#[test]
fn request_single_key_is_valid() {
    let req: CompiledLookupRequest =
        serde_json::from_value(serde_json::json!({ "job_keys": [valid_hex_key()] }))
            .expect("single key deserializes");
    req.validate().expect("single key is valid");
}

// ── Miss JSON shape ────────────────────────────────────────────────────────────

#[test]
fn miss_entry_exact_json_shape() {
    let key = parse_key(&valid_hex_key()).expect("valid key");
    let entry = index::server::http::dto::CompiledLookupEntry::miss(&key);
    let json = serde_json::to_value(&entry).expect("miss entry serializes");
    assert_eq!(json["job_key"], serde_json::Value::String(valid_hex_key()));
    assert_eq!(json["hit"], serde_json::Value::Bool(false));
    assert!(
        json.get("generation_stamp").is_none(),
        "miss must not carry generation_stamp"
    );
    assert!(json.get("channel").is_none(), "miss must not carry channel");
    assert!(json.get("tip").is_none(), "miss must not carry tip");
}

// ─────────────────────────────────────────────────────────────────────────────
// § Handler integration tests — require assembled server (opt-in)
// ─────────────────────────────────────────────────────────────────────────────

/// Build a `POST /v1/compiled/lookup` request from a list of hex keys.
fn lookup_request(job_keys: &[&str]) -> axum::http::Request<axum::body::Body> {
    server_common::post_json(
        "/v1/compiled/lookup",
        &serde_json::json!({ "job_keys": job_keys }),
    )
}

/// All genuinely-unknown keys return miss entries.
#[tokio::test]
async fn all_unknown_keys_return_miss() {
    let (server, _data) =
        server_common::required_assembled_server("compiled_lookup_all_miss").await;
    let keys = ["a".repeat(64), "b".repeat(64)];
    let keys_ref: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
    let (status, response) = server_common::call(router(server), lookup_request(&keys_ref)).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "handler must return 200 for valid keys: {response}"
    );
    let results = response["results"].as_array().expect("results is an array");
    assert_eq!(results.len(), 2, "one result per input key");
    for (result, key) in results.iter().zip(keys.iter()) {
        assert_eq!(result["hit"], false, "unrecorded keys miss");
        assert_eq!(result["job_key"].as_str().unwrap(), key.as_str());
        assert!(
            result.get("generation_stamp").is_none(),
            "miss has no generation_stamp"
        );
        assert!(result.get("channel").is_none(), "miss has no channel");
        assert!(result.get("tip").is_none(), "miss has no tip");
    }
}

/// Results preserve input order.
#[tokio::test]
async fn results_preserve_input_order() {
    let (server, _data) =
        server_common::required_assembled_server("compiled_lookup_ordering").await;
    let keys: Vec<String> = (0..8_u64).map(|i| format!("{:064x}", i)).collect();
    let keys_ref: Vec<&str> = keys.iter().map(|s| s.as_str()).collect();
    let (status, response) = server_common::call(router(server), lookup_request(&keys_ref)).await;

    assert_eq!(status, StatusCode::OK);
    let results = response["results"].as_array().expect("results is an array");
    assert_eq!(
        results.len(),
        keys.len(),
        "result count must match key count"
    );
    for (result, key) in results.iter().zip(keys.iter()) {
        assert_eq!(
            result["job_key"].as_str().unwrap(),
            key.as_str(),
            "result order must match input order"
        );
    }
}

/// Duplicate keys each produce an independent entry.
#[tokio::test]
async fn duplicate_keys_produce_independent_entries() {
    let (server, _data) = server_common::required_assembled_server("compiled_lookup_dedup").await;
    let key = "a".repeat(64);
    let (status, response) = server_common::call(
        router(server),
        lookup_request(&[key.as_str(), key.as_str()]),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let results = response["results"].as_array().expect("results is an array");
    assert_eq!(results.len(), 2, "two entries for two (duplicate) keys");
}

/// Empty `job_keys` list → 400.
#[tokio::test]
async fn empty_keys_returns_400() {
    let (server, _data) =
        server_common::required_assembled_server("compiled_lookup_empty_keys").await;
    let (status, _response) = server_common::call(router(server), lookup_request(&[])).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "empty job_keys must return 400"
    );
}

/// A body larger than 256 KiB is rejected with 413.
#[tokio::test]
async fn oversized_body_is_rejected() {
    let (server, _data) =
        server_common::required_assembled_server("compiled_lookup_oversize").await;
    let keys: Vec<String> = (0..4097_u64).map(|i| format!("{:064x}", i)).collect();
    let body = serde_json::json!({ "job_keys": keys }).to_string();
    let request = axum::http::Request::builder()
        .method("POST")
        .uri("/v1/compiled/lookup")
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body))
        .expect("oversized request is well-formed");

    let (status, _) = server_common::call(router(server), request).await;
    assert_eq!(
        status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "a body > 256 KiB must be rejected with 413"
    );
}

/// The end-to-end handshake: a record seeded via `store.record()` (the VCS-
/// native write path — the iroh SyncService apply-hook) comes back as a hit
/// with the correct channel, tip, and generation_stamp.
#[tokio::test]
async fn seeded_record_returns_vcs_hit() {
    use heart::content::ContentHash;
    use registry::compiled::{ChangeId, ChannelName, CompiledRecord, ObjectCompiledStore};

    let (server, _data) =
        server_common::required_assembled_server("compiled_lookup_seeded_hit").await;

    let package = server_common::package_id("compiled-lookup-seeded-fixture");
    let job_key = heart::JobKey::derive(
        b"producer-1.0.0",
        b"rustc-1.85.0",
        b"seeded-hit",
        b"lockfile",
    );

    let channel = ChannelName::new("main").expect("valid channel");
    let tip_hex = "ab".repeat(32); // 64 chars
    let tip = ChangeId::new(tip_hex.clone()).expect("valid tip");
    let generation_stamp = ContentHash::of_bytes(b"generation-stamp-fixture");

    let record = CompiledRecord {
        package,
        channel: channel.clone(),
        tip: tip.clone(),
        generation_stamp,
        recorded_at: 0,
    };

    // Seed via store.record() directly — the VCS-native write path.
    ObjectCompiledStore::new(server.blobs().backend())
        .record(job_key, record)
        .await
        .expect("record write succeeds");

    let key_hex = job_key.hex();
    let (status, response) =
        server_common::call(router(server), lookup_request(&[key_hex.as_str()])).await;
    assert_eq!(status, StatusCode::OK, "lookup succeeds: {response}");
    let results = response["results"].as_array().expect("results is an array");
    assert_eq!(results.len(), 1, "one result for one key");
    let entry = &results[0];
    assert_eq!(entry["job_key"].as_str().unwrap(), key_hex);
    assert_eq!(entry["hit"], true, "a seeded key hits");
    assert_eq!(
        entry["channel"].as_str().unwrap(),
        channel.as_str(),
        "channel echoes back"
    );
    assert_eq!(
        entry["tip"].as_str().unwrap(),
        tip.as_str(),
        "tip echoes back as 64-char lowercase hex"
    );
    assert_eq!(
        entry["generation_stamp"].as_str().unwrap(),
        generation_stamp.hex(),
        "generation_stamp echoes back"
    );

    // The client's verify_entry accepts the entry.
    let wire: registry::compiled::client::WireEntry =
        serde_json::from_value(entry.clone()).expect("the wire entry deserializes");
    let verified = registry::compiled::client::verify_entry(job_key, &wire)
        .expect("an honestly-served hit passes client verification");
    assert!(
        matches!(verified, registry::compiled::LookupResult::Hit(_)),
        "the client surfaces the verified hit"
    );
}

/// Invalid hex in a key → 422.
#[tokio::test]
async fn bad_hex_key_returns_422() {
    let (server, _data) = server_common::required_assembled_server("compiled_lookup_bad_hex").await;
    let bad_key = "g".repeat(64);
    let (status, _) =
        server_common::call(router(server), lookup_request(&[bad_key.as_str()])).await;
    assert!(
        status == StatusCode::UNPROCESSABLE_ENTITY || status == StatusCode::BAD_REQUEST,
        "invalid hex must return 400 or 422; got {status}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// § Client verify_entry unit tests — bad hex fields
// ─────────────────────────────────────────────────────────────────────────────

fn valid_job_key() -> heart::JobKey {
    heart::JobKey::derive(b"p", b"t", b"s", b"l")
}

fn honest_wire_entry(key: heart::JobKey) -> registry::compiled::client::WireEntry {
    registry::compiled::client::WireEntry {
        job_key: key.hex(),
        hit: true,
        package: Some(uuid::Uuid::new_v4().to_string()),
        channel: Some("main".to_owned()),
        tip: Some("ab".repeat(32)),
        generation_stamp: Some("cd".repeat(32)),
    }
}

#[test]
fn verify_entry_honest_hit_passes() {
    let key = valid_job_key();
    let entry = honest_wire_entry(key);
    let result = registry::compiled::client::verify_entry(key, &entry)
        .expect("honest entry passes verification");
    assert!(matches!(result, registry::compiled::LookupResult::Hit(_)));
}

#[test]
fn verify_entry_miss_passes() {
    let key = valid_job_key();
    let entry = registry::compiled::client::WireEntry {
        job_key: key.hex(),
        hit: false,
        package: None,
        channel: None,
        tip: None,
        generation_stamp: None,
    };
    assert!(matches!(
        registry::compiled::client::verify_entry(key, &entry).expect("miss verifies"),
        registry::compiled::LookupResult::Miss
    ));
}

#[test]
fn verify_entry_bad_tip_hex_rejected() {
    let key = valid_job_key();
    let mut entry = honest_wire_entry(key);
    entry.tip = Some("ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ".to_owned());
    let err =
        registry::compiled::client::verify_entry(key, &entry).expect_err("bad tip hex must fail");
    assert!(
        matches!(
            err,
            registry::compiled::client::VerificationFailure::BadTipHex
        ),
        "got: {err}"
    );
}

#[test]
fn verify_entry_short_tip_hex_rejected() {
    let key = valid_job_key();
    let mut entry = honest_wire_entry(key);
    entry.tip = Some("ab".repeat(16)); // 32 chars, not 64
    let err =
        registry::compiled::client::verify_entry(key, &entry).expect_err("short tip hex must fail");
    assert!(
        matches!(
            err,
            registry::compiled::client::VerificationFailure::BadTipHex
        ),
        "got: {err}"
    );
}

#[test]
fn verify_entry_bad_stamp_hex_rejected() {
    let key = valid_job_key();
    let mut entry = honest_wire_entry(key);
    entry.generation_stamp =
        Some("ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ".to_owned());
    let err =
        registry::compiled::client::verify_entry(key, &entry).expect_err("bad stamp hex must fail");
    assert!(
        matches!(
            err,
            registry::compiled::client::VerificationFailure::BadStampHex
        ),
        "got: {err}"
    );
}

#[test]
fn verify_entry_wrong_key_echo_rejected() {
    let key = valid_job_key();
    let mut entry = honest_wire_entry(key);
    entry.job_key = "0".repeat(64);
    let err = registry::compiled::client::verify_entry(key, &entry)
        .expect_err("wrong key echo must fail");
    assert!(
        matches!(
            err,
            registry::compiled::client::VerificationFailure::KeyEcho { .. }
        ),
        "got: {err}"
    );
}

#[test]
fn verify_entry_empty_channel_rejected() {
    let key = valid_job_key();
    let mut entry = honest_wire_entry(key);
    entry.channel = Some(String::new());
    let err =
        registry::compiled::client::verify_entry(key, &entry).expect_err("empty channel must fail");
    assert!(
        matches!(
            err,
            registry::compiled::client::VerificationFailure::EmptyChannel
        ),
        "got: {err}"
    );
}

#[test]
fn verify_entry_missing_tip_rejected() {
    let key = valid_job_key();
    let mut entry = honest_wire_entry(key);
    entry.tip = None;
    let err =
        registry::compiled::client::verify_entry(key, &entry).expect_err("missing tip must fail");
    assert!(
        matches!(
            err,
            registry::compiled::client::VerificationFailure::MissingField { field: "tip" }
        ),
        "got: {err}"
    );
}
