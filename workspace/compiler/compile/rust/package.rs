//! Resolving a Rust package + its documented local/workspace members, running
//! `cargo rustdoc --output-format json`, and capturing the resulting crate JSON.
//!
//! A short-lived `RUSTDOC` wrapper rewrites cargo's `-o`/`--out-dir` to a private
//! staging directory, then copies the emitted `{crate}.json` into
//! `NUDOX_RUSTDOC_OUT`. (Modern rustdoc writes JSON next to the HTML out-dir,
//! not to stdout — stripping out-dir and redirecting stdout is a no-op.)
//! The bytes are parsed in-process and lowered via [`super::context::RustdocParser`].

use std::{
    collections::{BTreeSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use ir::pipeline::{Collected, Ir};
use rustc_hash::FxHashMap as HashMap;
use semver::Version;
use serde::Deserialize;
use tracing::{debug, info, instrument};

use crate::compile::isolate::{self, IsolatedCommand, IsolatedFailure, IsolatedFailureKind};

use super::{
    context::RustdocParser,
    error::{MetadataError, Package, ProcessFailure, ProcessFailureKind},
};
use sandbox::{KillReason, ProducerProfile};

/// A Rust package to document from a local source tree.
#[derive(Clone, Debug)]
pub struct RustPackage {
    /// The (root) package name as `cargo metadata` reports it.
    pub name: String,

    /// Direct-repo mode: run the `--document-private-items` pass and pull the
    /// workspace's local library dependencies into the documented set.
    pub direct_repo: bool,
}

// ─── Minimal `cargo metadata` mirror ─────────────────────────────────────────

type PackageId = String;

#[derive(Deserialize)]
struct Metadata {
    packages:       Vec<CargoPackage>,
    resolve:        Option<Resolve>,
    workspace_root: PathBuf,
}

#[derive(Deserialize)]
struct CargoPackage {
    id:            PackageId,
    name:          String,
    manifest_path: PathBuf,
    targets:       Vec<Target>,
}

#[derive(Deserialize)]
struct Target {
    name: String,
    kind: Vec<String>,
}

#[derive(Deserialize)]
struct Resolve {
    nodes: Vec<Node>,
}

#[derive(Deserialize)]
struct Node {
    id:   PackageId,
    deps: Vec<Dep>,
}

#[derive(Deserialize)]
struct Dep {
    pkg: PackageId,
}

impl RustPackage {
    fn cargo_package_spec_for(package_name: &str, version: &Version) -> String {
        format!("{package_name}@{version}")
    }

    fn run_cargo_rustdoc(
        &self,
        code: &Path,
        package_name: &str,
        version: &Version,
        json_out: &Path,
        lib_only: bool,
    ) -> std::result::Result<std::process::Output, Package> {
        let wrapper = write_rustdoc_stdout_wrapper()?;
        let system_rustdoc = system_rustdoc_path();

        let mut cmd = IsolatedCommand::new("cargo", ProducerProfile::Rust)
            .arg("rustdoc")
            .arg("--package")
            .arg(Self::cargo_package_spec_for(package_name, version));
        if lib_only {
            cmd = cmd.arg("--lib");
        }
        cmd = cmd
            // P-parse is network-off; deps must already be materialised (design §8).
            .arg("--offline")
            .arg("--")
            .args(self.direct_repo.then_some("--document-private-items").into_iter())
            .arg("-Z")
            .arg("unstable-options")
            .arg("--output-format")
            .arg("json")
            // Unstable rustdoc JSON needs nightly (or bootstrap).
            .env("RUSTC_BOOTSTRAP", "1")
            .env("CARGO_NET_OFFLINE", "true")
            .env("RUSTDOC", &wrapper)
            .env("NUDOX_RUSTDOC_SYSTEM", &system_rustdoc)
            .env("NUDOX_RUSTDOC_OUT", json_out)
            .cwd(code)
            .ro(code)
            .rw(std::env::temp_dir())
            .rw(json_out.parent().unwrap_or(Path::new("/tmp")));

        // Package target/ must be writable for cargo.
        cmd = cmd.rw(code);

        let output = isolate::run_isolated(cmd).map_err(isolated_to_package)?;
        if output.status.success() {
            return Ok(output);
        }

        let stderr = summarize_command_output(&output.stderr);
        let stdout = summarize_command_output(&output.stdout);
        Err(ProcessFailure::Failed {
            command: if lib_only { "cargo rustdoc --lib".into() } else { "cargo rustdoc".into() },
            kind:    ProcessFailureKind::NonZeroExit { status: output.status },
            stdout:  if stdout.is_empty() { None } else { Some(stdout) },
            stderr:  if stderr.is_empty() { None } else { Some(stderr) },
        }
        .into())
    }

    #[instrument(skip_all, fields(package = %self.name))]
    fn generate_ir_for_package(
        &self,
        code: &Path,
        package_name: &str,
        version: &Version,
    ) -> std::result::Result<(Ir<Collected>, HashMap<String, String>), Package> {
        // Unique per invocation so concurrent generate/test runs cannot race.
        let safe_name = package_name.replace(['/', ':'], "_");
        let json_file = tempfile::Builder::new()
            .prefix(&format!("nudox-{safe_name}-rustdoc-"))
            .suffix(".json")
            .tempfile()
            .map_err(Package::from)?;
        // Persist so cargo/rustdoc can rewrite the path without a held fd.
        let (_, json_out) = json_file.keep().map_err(|e| Package::from(e.error))?;

        match self.run_cargo_rustdoc(code, package_name, version, &json_out, false) {
            Ok(_) => {}
            Err(Package::Process(ProcessFailure::Failed { stderr: Some(ref d), .. }))
                if d.contains("extra arguments to `rustdoc` can only be passed to one target") =>
            {
                self.run_cargo_rustdoc(code, package_name, version, &json_out, true)?;
            }
            Err(error) => {
                let _ = fs::remove_file(&json_out);
                return Err(error);
            }
        }

        let json_bytes = fs::read(&json_out)?;
        let _ = fs::remove_file(&json_out);
        let rustdoc_crate = parse_rustdoc_crate(&json_bytes)?;
        drop(json_bytes);
        debug!("rustdoc output parsed");

        let source_map = source_map_from_crate(&rustdoc_crate, code);

        let mut parser = RustdocParser::from_doc(rustdoc_crate)?;
        let parse_result = parser.parse()?;
        info!(entries = parse_result.len(), "IR generation complete");

        Ok((Ir::from_entries(parse_result), source_map))
    }

    pub fn generate_ir_with_sources(
        &self,
        code: &Path,
        version: &Version,
    ) -> std::result::Result<(Ir<Collected>, HashMap<String, String>), Package> {
        let metadata = cargo_metadata(code)?;
        let packages = documented_local_packages(&metadata, &self.name, self.direct_repo);

        let mut entries = Vec::new();
        let mut source_map = HashMap::default();
        for package_id in &packages {
            let package = metadata
                .packages
                .iter()
                .find(|candidate| candidate.id == *package_id)
                .expect("documented package id should exist in metadata");
            let (package_ir, package_sources) =
                self.generate_ir_for_package(code, &package.name, version)?;
            entries.extend(package_ir.into_entries());
            source_map.extend(package_sources);
        }

        Ok((Ir::from_entries(entries), source_map))
    }

    pub fn generate_ir(
        &self,
        code: &Path,
        version: &Version,
    ) -> std::result::Result<Ir<Collected>, Package> {
        self.generate_ir_with_sources(code, version).map(|(ir, _)| ir)
    }
}

// ─── rustdoc JSON capture ────────────────────────────────────────────────────

/// Write a short-lived shell wrapper that cargo invokes via `RUSTDOC=…`.
///
/// Cargo always passes `-o target/doc` (or `--out-dir`). With
/// `--output-format json`, rustdoc writes `{crate}.json` into that directory.
/// The wrapper rewrites the out-dir to a private staging path and copies the
/// single top-level `.json` into `NUDOX_RUSTDOC_OUT`.
fn write_rustdoc_stdout_wrapper() -> std::result::Result<PathBuf, Package> {
    // Unique path (not bare pid) so concurrent Rust jobs in one process cannot
    // clobber each other's RUSTDOC wrapper.
    let file = tempfile::Builder::new()
        .prefix("nudox-rustdoc-wrapper-")
        .suffix(".sh")
        .tempfile()
        .map_err(Package::from)?;
    let (_, path) = file.keep().map_err(|e| Package::from(e.error))?;
    let script = r#"#!/bin/sh
set -eu
out="${NUDOX_RUSTDOC_OUT:?NUDOX_RUSTDOC_OUT must be set}"
system="${NUDOX_RUSTDOC_SYSTEM:-rustdoc}"
stage="$(dirname "$out")/nudox-rustdoc-stage-$$"
mkdir -p "$stage"
cleanup() { rm -rf "$stage"; }
trap cleanup EXIT

skip=0
args=""
for arg in "$@"; do
  if [ "$skip" -eq 1 ]; then
    # Replace cargo's out-dir path with our staging directory.
    args="$args $(printf '%q' "$stage")"
    skip=0
    continue
  fi
  if [ "$arg" = "--out-dir" ] || [ "$arg" = "-o" ] || [ "$arg" = "--output" ]; then
    skip=1
    args="$args $(printf '%q' "$arg")"
    continue
  fi
  case "$arg" in
    --out-dir=*|--output=*)
      args="$args $(printf '%q' "--out-dir=$stage")"
      continue
      ;;
  esac
  args="$args $(printf '%q' "$arg")"
done

# shellcheck disable=SC2086
eval "\"$system\" $args"

# Prefer the single crate JSON at the stage root (not nested fingerprints).
json=""
for candidate in "$stage"/*.json; do
  if [ -f "$candidate" ]; then
    json="$candidate"
    break
  fi
done
if [ -z "$json" ]; then
  echo "nudox rustdoc wrapper: no *.json under $stage" >&2
  ls -la "$stage" >&2 || true
  exit 1
fi
cp "$json" "$out"
"#;
    fs::write(&path, script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms)?;
    }
    Ok(path)
}

fn system_rustdoc_path() -> PathBuf {
    if let Ok(p) = std::env::var("NUDOX_RUSTDOC_SYSTEM") {
        return PathBuf::from(p);
    }
    if let Ok(out) = isolate::run_isolated(
        IsolatedCommand::new("rustc", ProducerProfile::Tiny)
            .arg("--print")
            .arg("sysroot")
            .rw(std::env::temp_dir()),
    ) {
        if out.status.success() {
            let sysroot = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
            let candidate = sysroot.join("bin/rustdoc");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from("rustdoc")
}

// ─── rustdoc output parsing ──────────────────────────────────────────────────

fn parse_rustdoc_crate(bytes: &[u8]) -> std::result::Result<rustdoc_types::Crate, Package> {
    serde_json::from_slice(bytes).map_err(Into::into)
}

// ─── cargo metadata ──────────────────────────────────────────────────────────

fn cargo_metadata(code: &Path) -> std::result::Result<Metadata, Package> {
    // Metadata is pure resolution against already-fetched sources; still
    // network-off so a malicious crate cannot phone home during the parse phase.
    // Network off at the sandbox layer (bwrap/seatbelt). Do not pass
    // --offline here: local cargo caches still work; missing deps fail
    // without ambient network when isolation is production-grade.
    let output = isolate::run_isolated(
        IsolatedCommand::new("cargo", ProducerProfile::Rust)
            .arg("metadata")
            .arg("--format-version")
            .arg("1")
            .cwd(code)
            .ro(code)
            .rw(code)
            .rw(std::env::temp_dir()),
    )
    .map_err(isolated_to_package)?;

    if !output.status.success() {
        return Err(
            MetadataError::CargoFailed { stderr: summarize_command_output(&output.stderr) }
                .into(),
        );
    }

    serde_json::from_slice(&output.stdout).map_err(MetadataError::from).map_err(Into::into)
}

fn isolated_to_package(err: IsolatedFailure) -> Package {
    let kind = match err.kind {
        IsolatedFailureKind::Resource(KillReason::Wall | KillReason::CpuTime) => {
            ProcessFailureKind::TimedOut
        }
        IsolatedFailureKind::Resource(_) => ProcessFailureKind::Signaled,
        IsolatedFailureKind::NonZero { status } => {
            // We don't have a real ExitStatus here — surface as Signaled-like.
            let _ = status;
            ProcessFailureKind::Signaled
        }
        IsolatedFailureKind::Sandbox(_) | IsolatedFailureKind::ToolchainMissing(_) => {
            ProcessFailureKind::Signaled
        }
    };
    ProcessFailure::Failed {
        command: err.command,
        kind,
        stdout: err.stdout,
        stderr: err.stderr,
    }
    .into()
}

fn source_map_from_crate(
    krate: &rustdoc_types::Crate,
    workspace: &Path,
) -> HashMap<String, String> {
    let mut map = HashMap::default();
    let mut file_cache: HashMap<PathBuf, Option<(Arc<str>, Vec<usize>)>> = HashMap::default();

    for (id, item) in &krate.index {
        if !matches!(&item.inner, rustdoc_types::ItemEnum::Function(_)) {
            continue;
        }
        let Some(span) = &item.span else { continue };
        let Some(summary) = krate.paths.get(id) else { continue };
        let fq_name = summary.path.join("::");

        let source_file = workspace.join(&span.filename);
        let cached = file_cache.entry(source_file.clone()).or_insert_with(|| {
            let source = fs::read_to_string(&source_file).ok()?;
            let offsets = line_start_offsets(&source);
            Some((Arc::<str>::from(source), offsets))
        });
        let Some((source, line_starts)) = cached.as_ref() else { continue };

        let Some(&start) = line_starts.get(span.begin.0.saturating_sub(1)) else { continue };
        let end = line_starts.get(span.end.0).copied().unwrap_or(source.len());
        let raw: String = source[start..end].lines().collect::<Vec<_>>().join("\n");

        if !raw.is_empty() {
            map.insert(fq_name, raw);
        }
    }
    map
}

fn line_start_offsets(source: &str) -> Vec<usize> {
    let mut offsets = vec![0usize];
    for (idx, byte) in source.bytes().enumerate() {
        if byte == b'\n' {
            offsets.push(idx + 1);
        }
    }
    offsets
}

// ─── Package graph helpers ────────────────────────────────────────────────────

fn documented_local_packages(
    metadata: &Metadata,
    root_package_name: &str,
    direct_repo: bool,
) -> Vec<PackageId> {
    let Some(root_package) =
        metadata.packages.iter().find(|package| package.name == root_package_name)
    else {
        return Vec::new();
    };
    let mut documented = vec![root_package.id.clone()];
    if !direct_repo || package_has_library(root_package) {
        return documented;
    }

    let Some(resolve) = &metadata.resolve else {
        return documented;
    };
    let package_map: HashMap<_, _> =
        metadata.packages.iter().map(|package| (&package.id, package)).collect();
    let node_map: HashMap<_, _> =
        resolve.nodes.iter().map(|node| (&node.id, node)).collect();
    let workspace_root = metadata.workspace_root.as_path();
    let mut seen = BTreeSet::from([root_package.id.clone()]);
    let mut queue = VecDeque::from([root_package.id.clone()]);

    while let Some(package_id) = queue.pop_front() {
        let Some(node) = node_map.get(&package_id) else { continue };

        for dependency in &node.deps {
            let dependency_id = &dependency.pkg;
            if !seen.insert(dependency_id.clone()) {
                continue;
            }
            queue.push_back(dependency_id.clone());

            let Some(package) = package_map.get(dependency_id) else { continue };
            if !package.manifest_path.starts_with(workspace_root) {
                continue;
            }
            if package_has_library(package) {
                documented.push(package.id.clone());
            }
        }
    }

    documented
}

fn package_has_library(package: &CargoPackage) -> bool {
    package.targets.iter().any(|target| target.kind.iter().any(|k| is_library_target_kind(k)))
}

fn is_library_target_kind(kind: &str) -> bool {
    matches!(kind, "lib" | "rlib" | "staticlib" | "cdylib" | "dylib")
}

fn summarize_command_output(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    const LIMIT: usize = 2_000;
    if trimmed.len() <= LIMIT {
        return trimmed.to_owned();
    }

    let mut end = LIMIT;
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }

    format!("{}...", &trimmed[..end])
}

#[cfg(test)]
mod tests {
    use semver::Version;

    use super::RustPackage;

    #[test]
    fn cargo_package_spec_is_version_qualified() {
        assert_eq!(
            RustPackage::cargo_package_spec_for("serde_json", &Version::parse("1.0.82").unwrap()),
            "serde_json@1.0.82"
        );
    }
}