//! Verifies the production dependency closure of `interface-core` stays portable.
//!
//! This test intentionally asks Cargo for `normal` edges only.  The production client may use
//! only the two no-std identity and borrowed-core foundations (`server-index-core` and
//! `server-index-vocabulary`) for local borrowed queries; all server execution, writer, and
//! storage crates remain forbidden.
//! Running through Cargo's own tree resolver makes this check cover transitive path and registry
//! dependencies without duplicating Cargo's feature resolution in test code.

use std::process::Command;

const FORBIDDEN_MARKERS: &[&str] = &[
    "server-index-acquire",
    "server-index-build",
    "server-index-catalog",
    "server-index-graph-vector",
    "server-index-ingest",
    "server-index-publish",
    "server-index-qdrant",
    "server-index-retrieval",
    "server-index-routing",
    "tantivy",
    "server-index-tantivy",
    "server-index-trustfall",
    "server-journal",
    "backend-runtime",
    "server-workflow",
    "qdrant",
    "object-store",
    "object_store",
    "aws-sdk",
    "azure",
    "google-cloud",
    "gcp-",
    "opentelemetry",
    "tracing-opentelemetry",
    "embedding",
    "fastembed",
    "candle",
    "ort",
    "reqwest",
];

#[test]
fn normal_interface_core_closure_is_portable() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest_dir
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or_else(|| {
            std::io::Error::other("interface/core is nested below the workspace root")
        })?;
    let output = Command::new("cargo")
        .args([
            "tree",
            "--locked",
            "--offline",
            "-e",
            "normal",
            "-p",
            "interface-core",
            "--prefix",
            "depth",
        ])
        .current_dir(workspace)
        .output()?;
    assert!(
        output.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let tree = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    let forbidden: Vec<&str> = FORBIDDEN_MARKERS
        .iter()
        .copied()
        .filter(|marker| tree.contains(marker))
        .collect();
    assert!(
        forbidden.is_empty(),
        "portable interface-core normal dependency closure contains forbidden crates: {forbidden:?}\n{tree}"
    );
    assert!(tree.contains("interface-core"));
    assert!(tree.contains("backend-version"));
    assert!(tree.contains("compiler-vocabulary"));
    Ok(())
}
