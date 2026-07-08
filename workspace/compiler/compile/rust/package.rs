//! Resolving a Rust package + its documented local/workspace members and running
//! `cargo rustdoc` via the `rustdoc-driver` binary.
//!
//! The driver is invoked via `RUSTDOC=<driver>`, which runs the doc pass
//! in-process, lowers `rustdoc_types::Crate → IR` inside the driver, and writes
//! only IR JSON to `NUDOX_IR_OUT` — no rustdoc JSON file is ever written or read.
//!
//! The driver binary is located via `NUDOX_RUSTDOC_DRIVER` env var, a sibling
//! binary next to the current executable (Buck build layout), or `PATH`.

use std::{
    collections::{BTreeSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use ir::{
    kind::Entry,
    pipeline::{Collected, Ir},
};
use rustc_hash::FxHashMap as HashMap;
use semver::Version;
use serde::Deserialize;
use tracing::{info, instrument};

use rust_lowering::error::{MetadataError, Package, ProcessFailure, ProcessFailureKind};

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

// ─── Driver lookup ────────────────────────────────────────────────────────────

fn effective_driver_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("NUDOX_RUSTDOC_DRIVER") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join("rustdoc-driver")))
        .filter(|p| p.exists())
        .or_else(|| {
            std::env::var_os("PATH").and_then(|path| {
                std::env::split_paths(&path)
                    .map(|dir| dir.join("rustdoc-driver"))
                    .find(|p| p.exists())
            })
        })
}

impl RustPackage {
    fn cargo_package_spec_for(package_name: &str, version: &Version) -> String {
        format!("{package_name}@{version}")
    }

    fn run_cargo_with_driver(
        &self,
        code: &Path,
        driver: &Path,
        ir_out: &Path,
        source_map_out: &Path,
        package_name: &str,
        version: &Version,
        lib_only: bool,
    ) -> std::result::Result<std::process::Output, Package> {
        let mut command = Command::new("cargo");
        command
            .arg("rustdoc")
            .arg("--package")
            .arg(Self::cargo_package_spec_for(package_name, version));
        if lib_only {
            command.arg("--lib");
        }
        command
            .arg("--")
            .args(self.direct_repo.then_some("--document-private-items"))
            .arg("-Z")
            .arg("unstable-options")
            .arg("--output-format")
            .arg("json")
            .env("RUSTDOC", driver)
            .env("NUDOX_IR_OUT", ir_out)
            .env("NUDOX_SOURCE_MAP_OUT", source_map_out)
            .env("NUDOX_WORKSPACE_ROOT", code)
            .current_dir(code);

        let output = command.output()?;
        if output.status.success() {
            return Ok(output);
        }

        let stderr = summarize_command_output(&output.stderr);
        let stdout = summarize_command_output(&output.stdout);
        Err(ProcessFailure::Failed {
            command: if lib_only {
                "cargo rustdoc (driver) --lib".into()
            } else {
                "cargo rustdoc (driver)".into()
            },
            kind:   ProcessFailureKind::NonZeroExit { status: output.status },
            stdout: if stdout.is_empty() { None } else { Some(stdout) },
            stderr: if stderr.is_empty() { None } else { Some(stderr) },
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
        let driver = effective_driver_path().ok_or_else(|| {
            Package::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "rustdoc-driver not found; set NUDOX_RUSTDOC_DRIVER or place it on PATH",
            ))
        })?;
        self.generate_ir_via_driver(code, package_name, version, &driver)
    }

    fn generate_ir_via_driver(
        &self,
        code: &Path,
        package_name: &str,
        version: &Version,
        driver: &Path,
    ) -> std::result::Result<(Ir<Collected>, HashMap<String, String>), Package> {
        let tmp = std::env::temp_dir();
        let safe_name = package_name.replace(['/', ':'], "_");
        let ir_out = tmp.join(format!("nudox-{safe_name}-ir.json"));
        let sm_out = tmp.join(format!("nudox-{safe_name}-sm.json"));

        match self.run_cargo_with_driver(code, driver, &ir_out, &sm_out, package_name, version, false) {
            Ok(_) => {}
            Err(Package::Process(ProcessFailure::Failed { stderr: Some(ref d), .. }))
                if d.contains("extra arguments to `rustdoc` can only be passed to one target") =>
            {
                self.run_cargo_with_driver(
                    code, driver, &ir_out, &sm_out, package_name, version, true,
                )?;
            }
            Err(error) => return Err(error),
        }

        let entries: Vec<Entry> = serde_json::from_slice(&fs::read(&ir_out)?)?;
        let source_map: HashMap<String, String> =
            serde_json::from_slice(&fs::read(&sm_out)?)?;

        let ir = Ir::from_entries(entries);
        info!(entries = ir.len(), "IR generation complete (in-process driver)");
        Ok((ir, source_map))
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

// ─── cargo metadata ──────────────────────────────────────────────────────────

fn cargo_metadata(code: &Path) -> std::result::Result<Metadata, Package> {
    let output = Command::new("cargo")
        .arg("metadata")
        .arg("--format-version")
        .arg("1")
        .current_dir(code)
        .output()?;

    if !output.status.success() {
        return Err(
            MetadataError::CargoFailed { stderr: summarize_command_output(&output.stderr) }
                .into(),
        );
    }

    serde_json::from_slice(&output.stdout).map_err(MetadataError::from).map_err(Into::into)
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
