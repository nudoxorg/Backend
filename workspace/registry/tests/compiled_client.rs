//! Pipeline part: **verifying lookup client** (`registry::compiled::client`).
//!
//! Specs for shape validation: a hit is only surfaced after hex fields parse
//! into their newtypes and all mandatory fields are present. Tampering scenarios
//! drive [`verify_entry`] directly.

mod common;

use heart::JobKey;
use registry::compiled::LookupResult;
use registry::compiled::client::{VerificationFailure, WireEntry, verify_entry};

/// A deterministic fixture JobKey.
fn job_key() -> JobKey {
    JobKey::derive(b"producer-1.0.0", b"rustc-1.85.0", b"client-spec", b"lockfile")
}

/// A wire entry as an honest server would produce it.
fn honest_entry(key: JobKey) -> WireEntry {
    WireEntry {
        job_key: key.hex(),
        hit: true,
        package: Some(uuid::Uuid::new_v4().to_string()),
        channel: Some("main".to_owned()),
        tip: Some("ab".repeat(32)),
        generation_stamp: Some("cd".repeat(32)),
    }
}

/// An honest hit passes every check.
#[test]
fn verified_hit_roundtrips() {
    let key = job_key();
    let entry = honest_entry(key);
    let LookupResult::Hit(hit) = verify_entry(key, &entry).expect("an honest hit verifies")
    else {
        panic!("a hit entry must verify to a hit");
    };
    assert_eq!(hit.channel.as_str(), "main");
    assert_eq!(hit.tip.as_str(), &"ab".repeat(32));
}

/// A miss passes through untouched.
#[test]
fn miss_passes_through() {
    let key = job_key();
    let entry = WireEntry {
        job_key: key.hex(),
        hit: false,
        package: None,
        channel: None,
        tip: None,
        generation_stamp: None,
    };
    assert!(
        matches!(verify_entry(key, &entry).expect("a miss verifies trivially"), LookupResult::Miss),
        "a miss entry surfaces as Miss"
    );
}

/// A tip with non-hex characters is rejected.
#[test]
fn bad_tip_hex_chars_rejected() {
    let key = job_key();
    let mut entry = honest_entry(key);
    entry.tip = Some("ZZ".repeat(32)); // uppercase not hex
    let failure = verify_entry(key, &entry).expect_err("bad tip hex must be rejected");
    assert!(
        matches!(failure, VerificationFailure::BadTipHex),
        "the failure is BadTipHex; got: {failure}"
    );
}

/// A tip with wrong length (< 64 chars) is rejected.
#[test]
fn short_tip_hex_rejected() {
    let key = job_key();
    let mut entry = honest_entry(key);
    entry.tip = Some("ab".repeat(16)); // 32 chars
    let failure = verify_entry(key, &entry).expect_err("short tip must be rejected");
    assert!(
        matches!(failure, VerificationFailure::BadTipHex),
        "the failure is BadTipHex; got: {failure}"
    );
}

/// A generation_stamp with non-hex characters is rejected.
#[test]
fn bad_stamp_hex_chars_rejected() {
    let key = job_key();
    let mut entry = honest_entry(key);
    entry.generation_stamp = Some("ZZ".repeat(32));
    let failure = verify_entry(key, &entry).expect_err("bad stamp hex must be rejected");
    assert!(
        matches!(failure, VerificationFailure::BadStampHex),
        "the failure is BadStampHex; got: {failure}"
    );
}

/// An empty channel is rejected.
#[test]
fn empty_channel_rejected() {
    let key = job_key();
    let mut entry = honest_entry(key);
    entry.channel = Some(String::new());
    let failure = verify_entry(key, &entry).expect_err("empty channel must be rejected");
    assert!(
        matches!(failure, VerificationFailure::EmptyChannel),
        "the failure is EmptyChannel; got: {failure}"
    );
}

/// An entry answering a different key than requested is rejected.
#[test]
fn wrong_key_echo_fails_verification() {
    let key = job_key();
    let mut entry = honest_entry(key);
    entry.job_key = "0".repeat(64);
    let failure = verify_entry(key, &entry).expect_err("a mis-echoed key must be rejected");
    assert!(matches!(failure, VerificationFailure::KeyEcho { .. }), "got: {failure}");
}

/// A hit stripped of its tip is rejected.
#[test]
fn hit_without_tip_fails_verification() {
    let key = job_key();
    let mut entry = honest_entry(key);
    entry.tip = None;
    let failure = verify_entry(key, &entry).expect_err("an unverifiable hit must be rejected");
    assert!(
        matches!(failure, VerificationFailure::MissingField { field: "tip" }),
        "got: {failure}"
    );
}

/// A hit stripped of its channel is rejected.
#[test]
fn hit_without_channel_fails_verification() {
    let key = job_key();
    let mut entry = honest_entry(key);
    entry.channel = None;
    let failure = verify_entry(key, &entry).expect_err("missing channel must be rejected");
    assert!(
        matches!(failure, VerificationFailure::MissingField { field: "channel" }),
        "got: {failure}"
    );
}

/// A hit stripped of its generation_stamp is rejected.
#[test]
fn hit_without_stamp_fails_verification() {
    let key = job_key();
    let mut entry = honest_entry(key);
    entry.generation_stamp = None;
    let failure = verify_entry(key, &entry).expect_err("missing stamp must be rejected");
    assert!(
        matches!(failure, VerificationFailure::MissingField { field: "generation_stamp" }),
        "got: {failure}"
    );
}
