//! The canonical wire shapes of the mutation/lookup DTOs, pinned byte-for-byte.
//!
//! # Why this file exists
//!
//! `LOCAL-REMOTE-CONTRACT.md` §0.6 records a live defect: `heart::client::dto`
//! and `index::server::http::dto` independently declared same-named,
//! structurally identical wire types (`AddPackageDto`, `HealthDto`, `JobKeyHex`,
//! `CompiledLookupRequest`/`Entry`/`Response`, `RerankRequestDto`/`ResponseDto`)
//! with no shared definition — two copies free to drift. The fix collapses them
//! onto this crate's copy as the single source of truth.
//!
//! A collapse is only safe if it does not move the wire. These tests pin the
//! exact JSON every one of those shapes serializes to and deserializes from, so
//! the collapse (and every later edit) is guarded: change a field name, a
//! `#[serde(default)]`, a `skip_serializing_if`, or an enum's `rename_all` and
//! one of these fails loudly instead of silently changing what a shipped client
//! sends or a shipped server accepts. This is the teeth behind "one source of
//! truth" — the pin is what makes the second copy's deletion provably lossless.

use heart::client::dto::{
    AddPackageDto, COMPILED_LOOKUP_MAX_KEYS, CompiledLookupEntry, CompiledLookupRequest,
    CompiledLookupResponse, HealthDto, JobKeyHex, RERANK_MAX_DOCUMENTS, RerankDocument,
    RerankRequestDto, RerankResponseDto, RerankScore,
};
use heart::{BackendKind, Language};
use serde_json::json;

/// Serialize `value`, assert it equals `expected`, then round-trip the JSON
/// back and assert the deserialized value re-serializes identically. Round-trip
/// (rather than `PartialEq` on the value) keeps this usable for the response
/// DTOs, which are serialize-only and carry `f32`s.
fn assert_wire<T: serde::Serialize + serde::de::DeserializeOwned>(
    value: &T,
    expected: serde_json::Value,
) {
    let got = serde_json::to_value(value).expect("serializes");
    assert_eq!(got, expected, "wire shape drifted");
    let back: T = serde_json::from_value(expected.clone()).expect("deserializes");
    let reser = serde_json::to_value(&back).expect("re-serializes");
    assert_eq!(reser, expected, "round-trip changed the wire shape");
}

// ── AddPackageDto ────────────────────────────────────────────────────────────

#[test]
fn add_package_dto_wire() {
    // Ecosystem renders lowercase (`Language`'s `rename_all = "lowercase"`);
    // an explicit custom-registry origin is carried verbatim.
    assert_wire(
        &AddPackageDto {
            ecosystem: Language::Rust,
            name: "serde".to_owned(),
            version: "1.0.203".to_owned(),
            origin: Some("my-mirror".to_owned()),
        },
        json!({
            "ecosystem": "rust",
            "name": "serde",
            "version": "1.0.203",
            "origin": "my-mirror",
        }),
    );
}

#[test]
fn add_package_dto_origin_defaults_to_none() {
    // `origin` is `#[serde(default)]`: a body that omits it must deserialize
    // (the common case — the default registry is resolved server-side). The
    // serialized form still carries an explicit `null`, which must also
    // round-trip to `None`.
    let omitted: AddPackageDto = serde_json::from_value(json!({
        "ecosystem": "python",
        "name": "requests",
        "version": "2.32.3",
    }))
    .expect("origin may be omitted");
    assert_eq!(omitted.origin, None);
    assert_eq!(omitted.ecosystem, Language::Python);

    let explicit_null: AddPackageDto = serde_json::from_value(json!({
        "ecosystem": "python",
        "name": "requests",
        "version": "2.32.3",
        "origin": null,
    }))
    .expect("explicit null origin");
    assert_eq!(explicit_null.origin, None);
}

// ── HealthDto ────────────────────────────────────────────────────────────────

#[test]
fn health_dto_wire() {
    assert_wire(
        &HealthDto {
            ready: true,
            degraded: Vec::new(),
        },
        json!({ "ready": true, "degraded": [] }),
    );

    // `BackendKind` has no `rename_all`, so variants render as their exact
    // names — the readyz probe's degraded list is these tokens verbatim.
    assert_wire(
        &HealthDto {
            ready: false,
            degraded: vec![BackendKind::Qdrant, BackendKind::Catalog],
        },
        json!({ "ready": false, "degraded": ["Qdrant", "Catalog"] }),
    );
}

// ── JobKeyHex ────────────────────────────────────────────────────────────────

#[test]
fn job_key_hex_wire_and_validation() {
    let sixty_four = "a".repeat(64);
    let key: JobKeyHex = serde_json::from_value(json!(sixty_four)).expect("64 lower-hex chars");
    // `#[serde(transparent)]`: the wire form is the bare string, not an object.
    assert_eq!(serde_json::to_value(&key).unwrap(), json!(sixty_four));
    assert_eq!(key.as_str(), sixty_four);

    // Exactly 64 chars; lowercase only; hex only.
    assert!(serde_json::from_value::<JobKeyHex>(json!("abcd")).is_err());
    assert!(serde_json::from_value::<JobKeyHex>(json!("A".repeat(64))).is_err());
    assert!(serde_json::from_value::<JobKeyHex>(json!("g".repeat(64))).is_err());
}

// ── CompiledLookup* ──────────────────────────────────────────────────────────

#[test]
fn compiled_lookup_request_wire() {
    let key = "b".repeat(64);
    let req: CompiledLookupRequest =
        serde_json::from_value(json!({ "job_keys": [key] })).expect("valid request");
    assert!(req.validate().is_ok());
    assert_eq!(req.job_keys.len(), 1);

    // Bounds are pinned constants the server relies on.
    assert_eq!(COMPILED_LOOKUP_MAX_KEYS, 1024);
    let empty = CompiledLookupRequest { job_keys: vec![] };
    assert!(empty.validate().is_err());
}

#[test]
fn compiled_lookup_entry_wire() {
    let key: JobKeyHex = serde_json::from_value(json!("c".repeat(64))).unwrap();

    // Miss: only `job_key` + `hit:false`; every optional elided.
    let miss = serde_json::to_value(CompiledLookupEntry::miss(&key)).unwrap();
    assert_eq!(
        miss,
        json!({ "job_key": "c".repeat(64), "hit": false }),
        "a miss must not carry package/channel/tip/generation_stamp keys",
    );

    // Hit: all four optionals present.
    let hit = CompiledLookupEntry::hit(
        &key,
        "pkg-uuid".to_owned(),
        "main".to_owned(),
        "d".repeat(64),
        "e".repeat(64),
    );
    assert_eq!(
        serde_json::to_value(&hit).unwrap(),
        json!({
            "job_key": "c".repeat(64),
            "hit": true,
            "package": "pkg-uuid",
            "channel": "main",
            "tip": "d".repeat(64),
            "generation_stamp": "e".repeat(64),
        }),
    );

    let response = CompiledLookupResponse {
        results: vec![CompiledLookupEntry::miss(&key)],
    };
    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        json!({ "results": [{ "job_key": "c".repeat(64), "hit": false }] }),
    );
}

// ── Rerank* ──────────────────────────────────────────────────────────────────

#[test]
fn rerank_request_wire() {
    let req: RerankRequestDto = serde_json::from_value(json!({
        "query": "parse a toml file",
        "documents": [{ "id": "a", "text": "toml::from_str" }],
        "top_k": 5,
    }))
    .expect("valid rerank request");
    assert_eq!(req.query, "parse a toml file");
    assert_eq!(req.documents.len(), 1);
    assert_eq!(req.top_k.get(), 5);
    assert!(req.validate().is_ok());

    assert_eq!(RERANK_MAX_DOCUMENTS, 256);

    // A document round-trips as `{id, text}` with no extra keys.
    assert_wire(
        &RerankDocument {
            id: "doc-1".to_owned(),
            text: "fn main() {}".to_owned(),
        },
        json!({ "id": "doc-1", "text": "fn main() {}" }),
    );
}

#[test]
fn rerank_response_wire() {
    let response = RerankResponseDto {
        scores: vec![
            RerankScore {
                id: "a".to_owned(),
                score: 0.5,
            },
            RerankScore {
                id: "b".to_owned(),
                score: 0.25,
            },
        ],
    };
    assert_eq!(
        serde_json::to_value(&response).unwrap(),
        json!({ "scores": [{ "id": "a", "score": 0.5 }, { "id": "b", "score": 0.25 }] }),
    );
}
