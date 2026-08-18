#![cfg(feature = "embed")]
//! Pinned-artifact policy: sha verification and graceful missing-weights.

use registry::vector::embed::weights::{Error, WeightsSpec};
use sha2::{Digest, Sha256};

const FAKE_ONNX: &[u8] = b"definitely not a real onnx graph, but stable bytes";

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn spec_with(dir: &std::path::Path, expected: Option<[u8; 32]>) -> WeightsSpec {
    WeightsSpec {
        dir: dir.to_owned(),
        onnx_file: "model_quantized.onnx".to_owned(),
        expected_sha256: expected,
    }
}

#[test]
fn verify_matching_pin_succeeds() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("model_quantized.onnx"), FAKE_ONNX).expect("write");

    let verified = spec_with(dir.path(), Some(sha256(FAKE_ONNX)))
        .verify()
        .expect("verify");
    assert_eq!(verified.sha256, sha256(FAKE_ONNX));
    assert_eq!(verified.bytes, FAKE_ONNX.len() as u64);
}

#[test]
fn verify_unpinned_reports_actual_sha() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("model_quantized.onnx"), FAKE_ONNX).expect("write");

    let verified = spec_with(dir.path(), None).verify().expect("verify");
    assert_eq!(
        verified.sha256,
        sha256(FAKE_ONNX),
        "actual digest reported even unpinned"
    );
}

#[test]
fn sha_mismatch_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("model_quantized.onnx"), FAKE_ONNX).expect("write");

    let result = spec_with(dir.path(), Some(sha256(b"the pinned artifact"))).verify();
    assert!(
        matches!(result, Err(Error::Sha256Mismatch { .. })),
        "corrupt/swapped weights must never load"
    );
}

#[test]
fn missing_weights_is_graceful_signal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let result = spec_with(dir.path(), None).verify();
    assert!(
        matches!(result, Err(Error::MissingWeights { .. })),
        "missing artifact → semantic search disabled, not a crash"
    );
}
