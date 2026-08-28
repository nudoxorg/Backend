//! A measured baseline for the cost of the wire contract's serialization.
//!
//! # Why this exists
//!
//! The client/remote contract moves two kinds of bytes: the streaming search
//! payload (`Scored<SymbolHit>`, the `Symbols` item) over NDJSON-JSON, and the
//! remote *object* plane (`IrSnapshot` in `nudox-engine::store::remote`), which
//! today serializes with `serde_json`. "Optimize memory and compute when
//! connected to a remote" has a concrete first move — encode those payloads with
//! `postcard` (already a `heart` dependency, already how the IR body channel is
//! framed) instead of JSON — but a change is only worth making if it is
//! measured. This is the *before* number.
//!
//! Per the repo convention (`heart::cost::measured`, doctrine §4: every
//! integration test is also a benchmark), each case prints a stable
//! `cost case=… wall_ms=… disk_delta_bytes=…` line for `perf-report.nu`. The
//! encoded artifact is written under the case directory, so `disk_delta_bytes`
//! *is* the on-wire size of that encoding — the number Stage B will move.
//!
//! It is also a real test, not just a print: it asserts postcard is strictly
//! smaller than JSON for this payload (the thesis Stage B rests on) and that
//! both codecs round-trip the batch losslessly (so a Stage-B swap cannot quietly
//! drop a field — the failure mode [[count-based-tests-cannot-see-field-loss]]
//! warns about).
//!
//! # A constraint this benchmark surfaced
//!
//! `SymbolHit::{signature, reference}` are `#[serde(default,
//! skip_serializing_if = "Option::is_none")]`. That omission is deliberate and
//! load-bearing for the JSON hot path (a `"signature":null` on every one of
//! thousands of `/search` lines is pure overhead). But postcard is *not*
//! self-describing — it reads fields positionally — so a struct that sometimes
//! omits a field cannot be postcard-decoded: the decoder reads the next field's
//! bytes where the omitted one should be. `postcard_cannot_roundtrip_omitted_
//! fields` pins exactly that. The consequence for Stage B is concrete: moving
//! the object/wire plane to postcard is **not** a drop-in re-encode of the
//! JSON-shaped types — it needs either an explicit codec-specific shape (no
//! `skip_serializing_if`) or a length/​presence prefix. The clean codec numbers
//! below are therefore measured on the all-fields-present shape, which both
//! codecs handle, so the comparison is apples-to-apples.

use std::fs;
use std::path::{Path, PathBuf};

use heart::cost::measured;
use heart::identity::PackageId;
use heart::query::StableReference;
use heart::surface::{Signature, SigToken, SymbolHit};
use heart::{Language, Score, Scored, SymbolKind};

/// How many hits a realistic large answer carries. A precise symbol search over
/// a broad corpus routinely returns thousands of candidates before paging; this
/// is a deliberately generous batch so the codec difference is legible.
const BATCH: usize = 4000;

fn bare_hit(i: usize) -> SymbolHit {
    SymbolHit {
        package: PackageId::from_uuid(uuid::Uuid::from_u128(i as u128 % 128)),
        path: format!("serde_json::value::from_str_{i}").into(),
        display_name: format!("from_str_{i}").into(),
        ecosystem: Language::Rust,
        kind: SymbolKind::Function,
        signature: None,
        reference: None,
    }
}

/// A fully-populated hit: both `Option` fields `Some`, so no field is ever
/// omitted and the struct has a fixed shape both codecs can read positionally.
/// (See the omitted-field test for why the bare shape cannot be postcard-
/// decoded.)
fn full_hit(i: usize) -> SymbolHit {
    let target = StableReference::parse("F:rust/serde#0a1b2c3d").expect("frozen grammar parses");
    SymbolHit {
        signature: Some(Signature::new(vec![
            SigToken::keyword("fn"),
            SigToken::space(),
            SigToken::ident(format!("from_str_{i}")),
            SigToken::punct("("),
            SigToken::ident("s"),
            SigToken::punct(":"),
            SigToken::space(),
            // Both type tokens carry a linked target: `SigToken::Ty.target` is
            // *also* `skip_serializing_if`, so a `None` here would omit a field
            // deep in the tree and desync the positional postcard decoder — the
            // same incompatibility as the top-level fields, one level down.
            SigToken::ty("&str", Some(target.clone())),
            SigToken::punct(")"),
            SigToken::space(),
            SigToken::punct("->"),
            SigToken::space(),
            SigToken::ty("Value", Some(target.clone())),
        ])),
        reference: Some(target),
        ..bare_hit(i)
    }
}

fn batch() -> Vec<Scored<SymbolHit>> {
    (0..BATCH)
        .map(|i| {
            // Descending scores, as a real answer arrives.
            let score = Score::try_new(1.0 - (i as f32) / (BATCH as f32)).expect("valid score");
            Scored::new(full_hit(i), score)
        })
        .collect()
}

/// A fresh, empty directory to hang `disk_delta_bytes` measurements on.
fn case_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("nudox-wire-cost-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create case dir");
    dir
}

/// Encode `batch` with both codecs, measure each, and prove the postcard
/// encoding is both smaller and lossless — the baseline Stage B improves on.
#[test]
fn symbols_wire_json_vs_postcard() {
    let dir = case_dir();
    let batch = batch();

    // JSON encode → the artifact's byte size lands in `disk_delta_bytes`.
    let (json_bytes, json_cost) = measured("wire_symbols_json_encode", &dir, || {
        let bytes = serde_json::to_vec(&batch).expect("json encodes");
        fs::write(dir.join("symbols.json"), &bytes).expect("write json artifact");
        bytes
    });

    // postcard encode into a separate directory so its `disk_delta_bytes` is
    // its own size, not the sum with the JSON artifact already present.
    let post_dir = dir.join("post");
    fs::create_dir_all(&post_dir).expect("create postcard dir");
    let (postcard_bytes, postcard_cost) = measured("wire_symbols_postcard_encode", &post_dir, || {
        let bytes = postcard::to_allocvec(&batch).expect("postcard encodes");
        fs::write(post_dir.join("symbols.postcard"), &bytes).expect("write postcard artifact");
        bytes
    });

    // Decode timing (pure CPU; disk delta ~0).
    let (json_back, _) = measured("wire_symbols_json_decode", &dir, || {
        serde_json::from_slice::<Vec<Scored<SymbolHit>>>(&json_bytes).expect("json decodes")
    });
    let (postcard_back, _) = measured("wire_symbols_postcard_decode", &dir, || {
        postcard::from_bytes::<Vec<Scored<SymbolHit>>>(&postcard_bytes).expect("postcard decodes")
    });

    // Lossless, both directions — a Stage-B codec swap must preserve every
    // field, not merely the row count.
    assert_eq!(json_back, batch, "json round-trip dropped or altered a field");
    assert_eq!(
        postcard_back, batch,
        "postcard round-trip dropped or altered a field"
    );

    // The thesis Stage B rests on: postcard is materially smaller on the wire.
    let json_size = json_bytes.len();
    let postcard_size = postcard_bytes.len();
    assert!(
        postcard_size < json_size,
        "postcard ({postcard_size} B) must be smaller than json ({json_size} B) for the \
         same payload; if this ever fails the Stage-B premise is wrong"
    );

    // Surface the ratio on stdout alongside the `cost case=` lines so a reader
    // sees the magnitude, not just the direction, of the win.
    let saved = 100 - (postcard_size * 100 / json_size);
    println!(
        "wire_symbols codec: json={json_size}B ({json_ms:.2}ms enc) \
         postcard={postcard_size}B ({post_ms:.2}ms enc) saved={saved}%",
        json_ms = json_cost.wall.as_secs_f64() * 1000.0,
        post_ms = postcard_cost.wall.as_secs_f64() * 1000.0,
    );

    let _ = Path::new(&dir); // keep the artifacts for a curious operator; temp dir is reaped by the OS
}

/// The constraint Stage B must design around: a `SymbolHit` with an omitted
/// (`None`, `skip_serializing_if`) field cannot survive a postcard round-trip,
/// because postcard is not self-describing — the decoder has no field names and
/// reads the *next* field's bytes where the omitted one should be. JSON, which
/// carries field names and honours `#[serde(default)]`, round-trips the same
/// value fine. So a postcard wire is not a free re-encode of these JSON-shaped
/// types; it needs a codec-specific shape without field omission.
#[test]
fn postcard_cannot_roundtrip_omitted_fields() {
    let bare = bare_hit(1);
    assert!(bare.signature.is_none() && bare.reference.is_none());

    // JSON: lossless, because it is self-describing and the fields default.
    let json = serde_json::to_vec(&bare).expect("json encodes");
    let json_back: SymbolHit = serde_json::from_slice(&json).expect("json decodes");
    assert_eq!(json_back, bare, "json round-trips the omitted fields via default");

    // postcard: encodes (it simply omits the skipped fields), but the result is
    // not decodable back into the same struct — the omission desynchronizes the
    // positional reader. Either the decode errors, or (worse) it silently
    // yields a different value; both are failures, and both are proof the swap
    // is not a drop-in.
    let encoded = postcard::to_allocvec(&bare).expect("postcard encodes (omitting the None fields)");
    match postcard::from_bytes::<SymbolHit>(&encoded) {
        Err(_) => {} // the expected outcome: a decode error
        Ok(decoded) => assert_ne!(
            decoded, bare,
            "if postcard decodes at all, the omitted-field shape must not silently \
             reproduce the original — that would hide the incompatibility"
        ),
    }
}
