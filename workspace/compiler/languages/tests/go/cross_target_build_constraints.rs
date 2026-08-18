//! Cross-target proof that the Go oracle lets Go select the active files.
//!
//! This deliberately runs the same module twice with explicit GOOS/GOARCH
//! values. A Darwin host therefore cannot accidentally turn the Linux case
//! into a host-only test, and a missing Go toolchain is a hard test failure.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

fn oracle_bin() -> PathBuf {
    let oracle_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("oracle/go");
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("nudox-go-oracle-cross-target");
    let status = Command::new("go")
        .current_dir(&oracle_dir)
        .args(["build", "-o"])
        .arg(&out)
        .arg("./...")
        .status()
        .expect("Go 1.26 must be provisioned by Nix; cannot build the Go oracle");
    assert!(
        status.success(),
        "pinned Go toolchain failed to build the oracle"
    );
    out
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/go/fixtures/cross-target")
}

fn run_target(oracle: &Path, root: &Path, goos: &str) -> Value {
    let output = Command::new(oracle)
        .arg(root)
        // These values are intentionally set on the oracle subprocess, rather
        // than inferred from the Rust test host.
        .env("GOOS", goos)
        .env("GOARCH", "amd64")
        .env("GOPROXY", "off")
        .env("GOSUMDB", "off")
        .output()
        .unwrap_or_else(|error| panic!("oracle failed to execute for GOOS={goos}: {error}"));
    assert!(
        output.status.success(),
        "oracle exited unsuccessfully for GOOS={goos}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("invalid oracle JSON for GOOS={goos}: {error}"));
    assert!(
        value["errors"].as_array().is_none_or(Vec::is_empty),
        "oracle reported diagnostics for GOOS={goos}: {}",
        value["errors"]
    );
    value
}

fn package<'a>(output: &'a Value, goos: &str) -> &'a Value {
    output["packages"]
        .as_array()
        .and_then(|packages| packages.iter().find(|pkg| pkg["name"] == "tagged"))
        .unwrap_or_else(|| panic!("oracle returned no tagged package for GOOS={goos}: {output}"))
}

fn names(package: &Value, key: &str) -> Vec<String> {
    package[key]
        .as_array()
        .unwrap_or_else(|| panic!("oracle field {key} was not an array: {package}"))
        .iter()
        .filter_map(|decl| decl["name"].as_str().map(str::to_owned))
        .collect()
}

fn constrained_names(package: &Value, file: &str) -> Vec<String> {
    package["buildConstraints"]
        .as_array()
        .unwrap_or_else(|| panic!("buildConstraints was not an array: {package}"))
        .iter()
        .find(|entry| entry["file"].as_str().is_some_and(|path| path.ends_with(file)))
        .unwrap_or_else(|| panic!("oracle did not report excluded {file}: {package}"))["exportedDecls"]
        .as_array()
        .unwrap_or_else(|| panic!("exportedDecls missing for {file}: {package}"))
        .iter()
        .filter_map(|decl| decl["name"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn oracle_selects_mutually_exclusive_linux_and_darwin_files() {
    let fixture = fixture();
    let oracle = oracle_bin();

    let linux_output = run_target(&oracle, &fixture, "linux");
    let linux = package(&linux_output, "linux");
    let linux_selected = names(linux, "decls");
    assert!(linux_selected.iter().any(|name| name == "LinuxOnly"));
    assert!(linux_selected.iter().any(|name| name == "LinuxFunc"));
    assert!(!linux_selected.iter().any(|name| name == "DarwinOnly"));
    assert_eq!(
        constrained_names(linux, "darwin.go"),
        ["DarwinOnly".to_owned(), "DarwinFunc".to_owned()]
    );

    let darwin_output = run_target(&oracle, &fixture, "darwin");
    let darwin = package(&darwin_output, "darwin");
    let darwin_selected = names(darwin, "decls");
    assert!(darwin_selected.iter().any(|name| name == "DarwinOnly"));
    assert!(darwin_selected.iter().any(|name| name == "DarwinFunc"));
    assert!(!darwin_selected.iter().any(|name| name == "LinuxOnly"));
    assert_eq!(
        constrained_names(darwin, "linux.go"),
        ["LinuxOnly".to_owned(), "LinuxFunc".to_owned()]
    );
}
