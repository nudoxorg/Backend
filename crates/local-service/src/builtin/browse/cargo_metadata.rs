//! Exact Cargo metadata resolution, input witnessing, and lockfile admission.
//!
//! This private boundary owns Cargo's exact metadata response and the lockfile
//! capability probe. Tree construction and cache orchestration remain in
//! browse.rs.

use super::{
    CargoToolSelection, CargoToolWitnessReuse, CoherentMetadata, InputObservation,
    MAX_CARGO_CONFIG_BYTES, MAX_CARGO_CONFIG_DEPTH, MAX_CARGO_CONFIG_INPUTS,
    MAX_CARGO_METADATA_PACKAGES, MAX_CARGO_METADATA_TARGETS_PER_PACKAGE,
    MAX_CARGO_OBSERVATION_FILE_BYTES, MAX_CARGO_OBSERVATION_PATHS, MAX_METADATA_BYTES,
    RequestedCargoManifest, basic_input_paths, cargo_config_paths, cargo_environment_witness,
    cargo_tool_selection, metadata_tool_witness, observation_budget, observation_path_key,
    read_observation_file, run_with_default_rustc, run_with_default_rustc_and_overrides,
    rustup_selection_paths, sccache_configuration_paths, selected_cargo_program,
    strict_observation_witness,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Describes where the Cargo.lock bytes used for one metadata answer came
/// from. This is an internal data distinction, not a cryptographic proof or
/// a capability that untrusted callers can use to authenticate a lockfile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CargoMetadataLockOrigin {
    /// Cargo used an existing lockfile selected by its ordinary configuration.
    Observed,
    /// Cargo generated the lockfile only at a private CLI-selected path.
    EphemeralGeneratedCargoLockV1,
}

/// Lockfile facts retained while the exact Cargo metadata query is coherent.
#[derive(Clone, Debug, Eq, PartialEq)]
struct CargoMetadataRun {
    metadata: Vec<u8>,
    host: String,
    tool_witness: [u8; 32],
    no_deps_witness: Option<[u8; 32]>,
    lockfile: Option<String>,
    lockfile_digest: Option<[u8; 32]>,
    lock_origin: Option<CargoMetadataLockOrigin>,
    /// Existing selected lock path, or the Cargo-reported missing source path
    /// whose absence authorized the private lockfile branch.
    lock_path_witness: Option<PathBuf>,
}

impl From<(Vec<u8>, String, [u8; 32])> for CargoMetadataRun {
    fn from((metadata, host, tool_witness): (Vec<u8>, String, [u8; 32])) -> Self {
        Self {
            metadata,
            host,
            tool_witness,
            no_deps_witness: None,
            lockfile: None,
            lockfile_digest: None,
            lock_origin: None,
            lock_path_witness: None,
        }
    }
}

pub(super) fn coherent_metadata(
    requested: &RequestedCargoManifest,
    cached_tool: Option<&CargoToolWitnessReuse>,
) -> Result<CoherentMetadata, String> {
    let mut session = CargoMetadataResolutionSession::default();
    let mut tool_reuse = cached_tool.cloned();
    coherent_metadata_with(
        requested,
        |manifest| {
            if manifest != requested.manifest {
                return Err("Cargo metadata changed the exact requested manifest".to_owned());
            }
            session.run(requested, cached_tool)
        },
        |workspace, files| {
            let observation =
                strict_observation_witness(workspace, &requested.root, files, tool_reuse.as_ref())?;
            tool_reuse = observation.tool_witness_reuse.clone();
            Ok(observation)
        },
    )
}

fn coherent_metadata_with<R: Into<CargoMetadataRun>>(
    requested: &RequestedCargoManifest,
    mut run_metadata: impl FnMut(&Path) -> Result<R, String>,
    mut observe: impl FnMut(&Path, &[PathBuf]) -> Result<InputObservation, String>,
) -> Result<CoherentMetadata, String> {
    observation_budget()?;
    let discovery = run_metadata(&requested.manifest)?.into();
    observation_budget()?;
    let discovery_document = CargoMetadataDocument::parse(&discovery.metadata)?;
    let discovery_workspace = discovery_document.proves_requested_manifest(requested)?;
    let mut discovery_paths = discovery_document
        .inputs
        .observation_paths(&requested.root, &discovery_workspace)?;
    if let Some(lock_path) = &discovery.lock_path_witness {
        add_observed_path(&mut discovery_paths, lock_path)?;
    }
    let required_manifests = discovery_document
        .inputs
        .required_manifests(&requested.manifest, &discovery_workspace);
    let required_registry_checksums = discovery_document.inputs.registry_checksums.clone();
    let before = observe(&discovery_workspace, &discovery_paths)?;
    observation_budget()?;
    require_required_manifests_present(&before, &required_manifests)?;
    require_required_manifests_present(&before, &required_registry_checksums)?;
    require_lock_path_state(&before, &discovery)?;

    let observed = run_metadata(&requested.manifest)?.into();
    observation_budget()?;
    if observed.metadata != discovery.metadata
        || observed.host != discovery.host
        || observed.tool_witness != discovery.tool_witness
        || observed.no_deps_witness != discovery.no_deps_witness
        || observed.lockfile != discovery.lockfile
        || observed.lockfile_digest != discovery.lockfile_digest
        || observed.lock_origin != discovery.lock_origin
        || observed.lock_path_witness != discovery.lock_path_witness
    {
        return Err("Cargo metadata inputs or tool changed between observation passes".to_owned());
    }
    let observed_document = CargoMetadataDocument::parse(&observed.metadata)?;
    let effective_workspace = observed_document.proves_requested_manifest(requested)?;
    if effective_workspace != discovery_workspace {
        return Err("Cargo effective workspace changed between observation passes".to_owned());
    }
    let mut watched = observed_document
        .inputs
        .observation_paths(&requested.root, &effective_workspace)?;
    if let Some(lock_path) = &observed.lock_path_witness {
        add_observed_path(&mut watched, lock_path)?;
    }
    watched.sort();
    watched.dedup();
    if watched != discovery_paths {
        return Err("Cargo metadata input set changed during observation".to_owned());
    }
    if observed_document != discovery_document {
        return Err("Cargo metadata file set changed during observation".to_owned());
    }
    let after = observe(&effective_workspace, &watched)?;
    observation_budget()?;
    require_required_manifests_present(&after, &required_manifests)?;
    require_required_manifests_present(&after, &required_registry_checksums)?;
    require_lock_path_state(&after, &observed)?;
    if before.digest != after.digest {
        return Err(
            "Cargo manifests, lockfile, configuration, or tools changed during metadata".to_owned(),
        );
    }
    let lock_origin_witness = lock_origin_witness_for_run(&observed)?;
    let metadata_witness = cargo_metadata_output_witness(&observed.metadata, &observed.host);
    let input_witness =
        compose_cargo_input_witness(after.digest, metadata_witness, lock_origin_witness);
    Ok(CoherentMetadata {
        metadata: observed.metadata,
        host: observed.host,
        workspace_root: effective_workspace,
        input_witness,
        file_witness: after.digest,
        metadata_witness,
        lock_origin_witness,
        tool_witness_reuse: after.tool_witness_reuse,
        lockfile: observed.lockfile.or(after.lockfile),
        watched,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CargoMetadataDocument {
    workspace_root: PathBuf,
    workspace_members: BTreeSet<String>,
    package_ids: Vec<(String, PathBuf)>,
    inputs: CargoMetadataInputs,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CargoMetadataInputs {
    package_manifests: Vec<PathBuf>,
    registry_checksums: Vec<PathBuf>,
    target_sources: Vec<PathBuf>,
    package_roots: Vec<PathBuf>,
}

impl CargoMetadataDocument {
    fn parse(metadata: &[u8]) -> Result<Self, String> {
        if metadata.len() > MAX_METADATA_BYTES {
            return Err("Cargo metadata exceeded its bounded response size".to_owned());
        }
        let value: serde_json::Value = serde_json::from_slice(metadata)
            .map_err(|error| format!("Cargo metadata JSON is malformed: {error}"))?;
        let packages = value
            .get("packages")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "Cargo metadata has no package array".to_owned())?;
        if packages.len() > MAX_CARGO_METADATA_PACKAGES {
            return Err("Cargo metadata package set exceeds the observation limit".to_owned());
        }
        let workspace_root_text = value
            .get("workspace_root")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "Cargo metadata omitted workspace_root".to_owned())?;
        let workspace_root = canonical_metadata_workspace(workspace_root_text)?;
        let members = value
            .get("workspace_members")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "Cargo metadata has no workspace member array".to_owned())?;
        if members.len() > MAX_CARGO_METADATA_PACKAGES {
            return Err("Cargo metadata workspace member set exceeds its limit".to_owned());
        }
        let mut workspace_members = BTreeSet::new();
        let mut package_ids = Vec::with_capacity(packages.len());
        let mut total_id_bytes = 0_usize;
        for member in members {
            observation_budget()?;
            let id = member
                .as_str()
                .ok_or_else(|| "Cargo metadata workspace member id is malformed".to_owned())?;
            account_metadata_identifier_bytes(&mut total_id_bytes, id)?;
            if !workspace_members.insert(id.to_owned()) {
                return Err("Cargo metadata repeated a workspace member id".to_owned());
            }
        }

        let mut manifests = BTreeSet::new();
        let mut checksums = BTreeSet::new();
        let mut target_sources = BTreeSet::new();
        let mut package_roots = BTreeSet::new();
        let mut total_path_bytes = 0_usize;
        for package in packages {
            observation_budget()?;
            let manifest = metadata_path(package, "manifest_path", "Cargo.toml")?;
            let id = package
                .get("id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "Cargo metadata package omitted id".to_owned())?;
            account_metadata_identifier_bytes(&mut total_id_bytes, id)?;
            package_ids.push((id.to_owned(), manifest.clone()));
            account_metadata_path_bytes(&mut total_path_bytes, &manifest)?;
            let package_root = manifest
                .parent()
                .ok_or_else(|| "Cargo metadata package manifest has no parent".to_owned())?;
            manifests.insert(manifest.clone());
            package_roots.insert(package_root.to_path_buf());

            if package
                .get("source")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|source| {
                    source.starts_with("registry+") || source.starts_with("sparse+")
                })
            {
                checksums.insert(package_root.join(".cargo-checksum.json"));
            }

            let targets = package
                .get("targets")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| "Cargo metadata package omitted targets".to_owned())?;
            if targets.len() > MAX_CARGO_METADATA_TARGETS_PER_PACKAGE {
                return Err("Cargo metadata target set exceeds the observation limit".to_owned());
            }
            for target in targets {
                observation_budget()?;
                let source = metadata_target_source_path(target)?;
                account_metadata_path_bytes(&mut total_path_bytes, &source)?;
                target_sources.insert(source);
                if manifests.len() + checksums.len() + target_sources.len()
                    > MAX_CARGO_OBSERVATION_PATHS
                {
                    return Err("Cargo metadata input set exceeds the observation limit".to_owned());
                }
            }
        }
        Ok(Self {
            workspace_root,
            workspace_members,
            package_ids,
            inputs: CargoMetadataInputs {
                package_manifests: manifests.into_iter().collect(),
                registry_checksums: checksums.into_iter().collect(),
                target_sources: target_sources.into_iter().collect(),
                package_roots: package_roots.into_iter().collect(),
            },
        })
    }

    fn proves_requested_manifest(
        &self,
        requested: &RequestedCargoManifest,
    ) -> Result<PathBuf, String> {
        let requested_is_member = self.package_ids.iter().any(|(id, manifest)| {
            manifest == &requested.manifest && self.workspace_members.contains(id)
        });
        if requested.has_package && !requested_is_member {
            return Err(
                "Cargo metadata did not admit the exact requested package manifest as a workspace member".to_owned(),
            );
        }
        if requested.has_workspace && self.workspace_root != requested.root {
            return Err(
                "Cargo resolved the requested workspace manifest to a different workspace root"
                    .to_owned(),
            );
        }
        if !requested.has_package && !requested.has_workspace {
            return Err("requested Cargo manifest declares no project scope".to_owned());
        }
        if !requested.has_package && self.workspace_root != requested.root {
            return Err(
                "Cargo metadata did not resolve the exact requested virtual workspace root"
                    .to_owned(),
            );
        }
        Ok(self.workspace_root.clone())
    }
}

impl CargoMetadataInputs {
    fn observation_paths(
        &self,
        request_context: &Path,
        workspace: &Path,
    ) -> Result<Vec<PathBuf>, String> {
        // Cargo resolves configuration and rustup selectors from its exact
        // invocation CWD. An explicitly associated workspace can be outside
        // that ancestry; its manifest and package rows are witnessed below,
        // but its unrelated config files are not part of this query.
        let config_paths = cargo_config_paths(request_context)?;
        let sccache_paths = sccache_configuration_paths(request_context)?;
        let rustup_paths = rustup_selection_paths(request_context)?;
        self.assemble_observation_paths(workspace, &config_paths, &sccache_paths, &rustup_paths)
    }

    fn assemble_observation_paths(
        &self,
        workspace: &Path,
        config_paths: &[PathBuf],
        sccache_paths: &[PathBuf],
        rustup_paths: &[PathBuf],
    ) -> Result<Vec<PathBuf>, String> {
        let mut paths = basic_input_paths(workspace)?;
        paths.extend(self.package_manifests.iter().cloned());
        paths.extend(self.registry_checksums.iter().cloned());
        // Exact target membership and `src_path` values remain in the complete
        // Cargo metadata witness. Source contents do not affect this dependency
        // graph, so avoid reading every target file just to validate the cache.
        paths.extend(config_paths.iter().cloned());
        paths.extend(sccache_paths.iter().cloned());
        paths.extend(rustup_paths.iter().cloned());
        paths.sort();
        paths.dedup();
        if paths.len() > MAX_CARGO_OBSERVATION_PATHS {
            return Err("Cargo metadata input set exceeds the observation limit".to_owned());
        }
        Ok(paths)
    }

    fn required_manifests(&self, requested: &Path, workspace: &Path) -> Vec<PathBuf> {
        self.package_manifests
            .iter()
            .cloned()
            .chain([requested.to_path_buf(), workspace.join("Cargo.toml")])
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

fn canonical_metadata_workspace(text: &str) -> Result<PathBuf, String> {
    if text.len() > 4 * 1024 {
        return Err("Cargo metadata workspace path exceeds the observation limit".to_owned());
    }
    let workspace = PathBuf::from(text);
    let canonical_workspace = workspace
        .canonicalize()
        .map_err(|_| "Cargo metadata returned a missing workspace root".to_owned())?;
    if !workspace.is_absolute()
        || text.contains("//")
        || workspace.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
        || canonical_workspace != workspace
    {
        return Err("Cargo metadata returned a noncanonical workspace root".to_owned());
    }
    Ok(workspace)
}

fn account_metadata_identifier_bytes(total: &mut usize, id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 4 * 1024 {
        return Err("Cargo metadata package id exceeds the observation limit".to_owned());
    }
    *total = total.saturating_add(id.len());
    if *total > 16 * 1024 * 1024 {
        return Err("Cargo metadata package ids exceed their byte budget".to_owned());
    }
    Ok(())
}

fn metadata_path(
    object: &serde_json::Value,
    field: &str,
    expected_file_name: &str,
) -> Result<PathBuf, String> {
    let text = object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("Cargo metadata omitted {field}"))?;
    if text.len() > 4 * 1024 {
        return Err("Cargo metadata path exceeds the observation limit".to_owned());
    }
    let path = PathBuf::from(text);
    if !path.is_absolute()
        || text.contains("//")
        || (!expected_file_name.is_empty()
            && path.file_name().and_then(std::ffi::OsStr::to_str) != Some(expected_file_name))
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
    {
        return Err(format!("Cargo metadata returned a noncanonical {field}"));
    }
    Ok(path)
}

/// Cargo may report an explicit target whose manifest-relative path crosses
/// package parents. Keep that exact absolute spelling in the metadata
/// document; target paths are not opened by this dependency-graph observer.
fn metadata_target_source_path(target: &serde_json::Value) -> Result<PathBuf, String> {
    let text = target
        .get("src_path")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "Cargo metadata target omitted src_path".to_owned())?;
    if text.is_empty()
        || text.len() > 4 * 1024
        || text.contains("//")
        || text.chars().any(char::is_control)
    {
        return Err("Cargo metadata target path exceeds the observation limit".to_owned());
    }
    let path = PathBuf::from(text);
    if !path.is_absolute()
        || path.file_name().is_none()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::CurDir))
    {
        return Err("Cargo metadata returned a noncanonical target src_path".to_owned());
    }
    Ok(path)
}

fn account_metadata_path_bytes(total: &mut usize, path: &Path) -> Result<(), String> {
    *total = total.saturating_add(path.as_os_str().as_encoded_bytes().len());
    if *total > 16 * 1024 * 1024 {
        return Err("Cargo metadata paths exceed their byte budget".to_owned());
    }
    Ok(())
}

#[cfg(test)]
fn metadata_observation_paths(
    request_context: &Path,
    workspace: &Path,
    metadata: &[u8],
) -> Result<Vec<PathBuf>, String> {
    CargoMetadataDocument::parse(metadata)?
        .inputs
        .observation_paths(request_context, workspace)
}

fn metadata_package_roots(metadata: &[u8]) -> Result<Vec<PathBuf>, String> {
    Ok(CargoMetadataDocument::parse(metadata)?.inputs.package_roots)
}

#[cfg(test)]
fn metadata_required_manifests(
    metadata: &[u8],
    requested_manifest: &Path,
    workspace: &Path,
) -> Result<Vec<PathBuf>, String> {
    Ok(CargoMetadataDocument::parse(metadata)?
        .inputs
        .required_manifests(requested_manifest, workspace))
}

fn require_required_manifests_present(
    observation: &InputObservation,
    required_manifests: &[PathBuf],
) -> Result<(), String> {
    if required_manifests.iter().any(|path| {
        observation
            .missing_path_keys
            .contains(&observation_path_key(path))
    }) {
        return Err(
            "a Cargo metadata-listed package manifest was absent during observation".to_owned(),
        );
    }
    Ok(())
}

fn add_observed_path(paths: &mut Vec<PathBuf>, path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path.file_name().and_then(std::ffi::OsStr::to_str) != Some("Cargo.lock")
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
    {
        return Err("Cargo selected a noncanonical lockfile path".to_owned());
    }
    paths.push(path.to_path_buf());
    paths.sort();
    paths.dedup();
    if paths.len() > MAX_CARGO_OBSERVATION_PATHS {
        return Err("Cargo metadata input set exceeds the observation limit".to_owned());
    }
    Ok(())
}

fn require_lock_path_state(
    observation: &InputObservation,
    run: &CargoMetadataRun,
) -> Result<(), String> {
    let Some(origin) = run.lock_origin else {
        return Ok(());
    };
    let path = run
        .lock_path_witness
        .as_ref()
        .ok_or_else(|| "Cargo lockfile provenance omitted its selected path".to_owned())?;
    let missing = observation
        .missing_path_keys
        .contains(&observation_path_key(path));
    let digest = run
        .lockfile
        .as_ref()
        .map(|lockfile| *blake3::hash(lockfile.as_bytes()).as_bytes())
        .ok_or_else(|| "Cargo lockfile provenance omitted its bytes".to_owned())?;
    if Some(digest) != run.lockfile_digest {
        return Err("Cargo lockfile provenance digest does not match its bytes".to_owned());
    }
    match origin {
        CargoMetadataLockOrigin::Observed if missing => {
            Err("Cargo's selected observed lockfile disappeared during metadata".to_owned())
        }
        CargoMetadataLockOrigin::EphemeralGeneratedCargoLockV1 if !missing => {
            Err("the Cargo-selected source lockfile appeared during private resolution".to_owned())
        }
        CargoMetadataLockOrigin::Observed
        | CargoMetadataLockOrigin::EphemeralGeneratedCargoLockV1 => Ok(()),
    }
}

fn lock_origin_witness_for_run(run: &CargoMetadataRun) -> Result<[u8; 32], String> {
    let Some(origin) = run.lock_origin else {
        return Ok([0; 32]);
    };
    let path = run
        .lock_path_witness
        .as_ref()
        .ok_or_else(|| "Cargo lockfile provenance omitted its selected path".to_owned())?;
    let digest = run
        .lockfile_digest
        .ok_or_else(|| "Cargo lockfile provenance omitted its digest".to_owned())?;
    Ok(cargo_lock_origin_witness(origin, path, digest))
}

pub(super) fn cargo_lock_origin_witness(
    origin: CargoMetadataLockOrigin,
    path: &Path,
    content_digest: [u8; 32],
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.cargo-lock-origin.v1\0");
    hasher.update(&[match origin {
        CargoMetadataLockOrigin::Observed => 1,
        CargoMetadataLockOrigin::EphemeralGeneratedCargoLockV1 => 2,
    }]);
    let path = path.as_os_str().as_encoded_bytes();
    hasher.update(&(path.len() as u64).to_le_bytes());
    hasher.update(path);
    hasher.update(&content_digest);
    *hasher.finalize().as_bytes()
}

pub(super) fn compose_cargo_input_witness(
    file_witness: [u8; 32],
    metadata_witness: [u8; 32],
    lock_origin_witness: [u8; 32],
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.cargo-metadata-input-composition.v2\0");
    hasher.update(&file_witness);
    hasher.update(&metadata_witness);
    hasher.update(&lock_origin_witness);
    *hasher.finalize().as_bytes()
}

pub(super) fn cargo_metadata_output_witness(metadata: &[u8], host: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.cargo-metadata-output.v1\0");
    hasher.update(&(host.len() as u64).to_le_bytes());
    hasher.update(host.as_bytes());
    hasher.update(&(metadata.len() as u64).to_le_bytes());
    hasher.update(metadata);
    *hasher.finalize().as_bytes()
}

pub(super) fn metadata_proves_requested_manifest(
    metadata: &[u8],
    requested: &RequestedCargoManifest,
) -> Result<PathBuf, String> {
    CargoMetadataDocument::parse(metadata)?.proves_requested_manifest(requested)
}

#[derive(Default)]
struct CargoMetadataResolutionSession {
    no_deps_witness: Option<[u8; 32]>,
    lock_state: Option<CargoMetadataLockState>,
}

impl CargoMetadataResolutionSession {
    /// Runs Cargo for the exact requested manifest. A missing source lock is
    /// recognized only from Cargo's own `--locked` diagnostic; the only
    /// unlocked full metadata pass is redirected by CLI config to a private
    /// RAII directory, whose lock is then proved with `--locked`. Before that
    /// user-source write attempt, the selected executable must pass a real
    /// private-fixture precedence and replay probe. Cargo remains a trusted
    /// executable in this design; path checks are not a process sandbox.
    fn run(
        &mut self,
        requested: &RequestedCargoManifest,
        cached: Option<&CargoToolWitnessReuse>,
    ) -> Result<CargoMetadataRun, String> {
        let requested_manifest = &requested.manifest;
        let request_context = requested_manifest
            .parent()
            .ok_or_else(|| "requested Cargo manifest has no parent directory".to_owned())?;
        let environment_before = cargo_environment_witness()?;
        let cargo = selected_cargo_program(request_context)?;
        let version = run(&cargo, request_context, &["-vV"], 64 * 1024)?;
        if cargo_environment_witness()? != environment_before {
            return Err(
                "Cargo tool-selection environment changed during metadata admission".to_owned(),
            );
        }
        let host = String::from_utf8_lossy(&version)
            .lines()
            .find_map(|line| {
                line.strip_prefix("host: ")
                    .map(str::trim)
                    .map(ToOwned::to_owned)
            })
            .ok_or_else(|| "cargo -vV named no host".to_owned())?;
        let selection_before = cargo_tool_selection(&cargo, request_context, &version)?;
        let tool_before =
            metadata_tool_witness(request_context, &version, &selection_before, cached)?;

        let no_deps = run_cargo_metadata_query(
            &cargo,
            request_context,
            &selection_before,
            &host,
            requested_manifest,
            true,
            false,
            None,
        )?;
        let no_deps_workspace = metadata_proves_requested_manifest(&no_deps, requested)?;
        let no_deps_witness = *blake3::hash(&no_deps).as_bytes();
        if self
            .no_deps_witness
            .is_some_and(|previous| previous != no_deps_witness)
        {
            return Err("Cargo no-deps workspace membership changed between passes".to_owned());
        }
        self.no_deps_witness = Some(no_deps_witness);

        // Resolve the existing lock path before the full query when Cargo's
        // ordinary inputs are unambiguous. This lets the successful --locked
        // path prove the same lock bytes were present on both sides of Cargo.
        // If the effective configuration is ambiguous, an exact missing-lock
        // diagnostic can still authorize the private-lock path below; a
        // successful query will be refused because its lock cannot be safely
        // bound to an observed path.
        let selected_lock_before =
            match cargo_selected_lockfile_path(request_context, &no_deps_workspace) {
                Ok(path) => Some((
                    path.clone(),
                    read_observation_file(&path, MAX_CARGO_OBSERVATION_FILE_BYTES)?,
                )),
                Err(_) => None,
            };

        let metadata = if self.lock_state.is_none() {
            match run_cargo_metadata_query(
                &cargo,
                request_context,
                &selection_before,
                &host,
                requested_manifest,
                false,
                true,
                None,
            ) {
                Ok(metadata) => {
                    let workspace = metadata_proves_requested_manifest(&metadata, requested)?;
                    if workspace != no_deps_workspace {
                        return Err(
                            "Cargo effective workspace changed between no-deps and full metadata"
                                .to_owned(),
                        );
                    }
                    let (path_before, bytes_before) = selected_lock_before
                        .as_ref()
                        .ok_or_else(|| "Cargo used an existing lockfile whose configured path could not be proven".to_owned())?;
                    let path = cargo_selected_lockfile_path(request_context, &workspace)?;
                    if &path != path_before {
                        return Err(
                            "Cargo selected lockfile path changed during metadata".to_owned()
                        );
                    }
                    let bytes_before = bytes_before.as_ref().ok_or_else(|| {
                        "Cargo succeeded with --locked although its selected lockfile was absent before metadata".to_owned()
                    })?;
                    let (lockfile, digest) = read_required_cargo_lockfile(&path)?;
                    if lockfile.as_bytes() != bytes_before.as_slice() {
                        return Err("Cargo's selected lockfile changed during metadata".to_owned());
                    }
                    self.lock_state = Some(CargoMetadataLockState::Observed {
                        path,
                        lockfile,
                        digest,
                    });
                    metadata
                }
                Err(error) => {
                    let Some(missing_source_path) = missing_cargo_lockfile_path(&error) else {
                        return Err(error);
                    };
                    if read_observation_file(
                        &missing_source_path,
                        MAX_CARGO_OBSERVATION_FILE_BYTES,
                    )?
                    .is_some()
                    {
                        return Err(format!(
                            "Cargo reported a missing lockfile that is present: {}",
                            missing_source_path.display()
                        ));
                    }
                    if let Some((selected_path, selected_bytes)) = &selected_lock_before {
                        if selected_path != &missing_source_path {
                            return Err("Cargo's missing-lock diagnostic disagreed with the observed lock path".to_owned());
                        }
                        if selected_bytes.is_some() {
                            return Err("Cargo reported a missing lockfile that was present before metadata".to_owned());
                        }
                    }
                    let mut source_roots =
                        BTreeSet::from([requested.root.clone(), no_deps_workspace.clone()]);
                    source_roots.extend(metadata_package_roots(&no_deps)?);
                    let source_roots = source_roots.into_iter().collect::<Vec<_>>();
                    verify_private_cargo_lockfile_redirect(
                        &cargo,
                        &selection_before,
                        &host,
                        &source_roots,
                    )
                    .map_err(|probe| {
                        format!(
                            "{error}; selected Cargo did not prove safe private lockfile redirection: {probe}"
                        )
                    })?;
                    let (directory, private_directory) =
                        create_private_cargo_lock_directory(&source_roots)?;
                    let private_path = private_directory.join("Cargo.lock");
                    validate_cargo_lockfile_path(&private_path)?;
                    let generated = run_cargo_metadata_query(
                        &cargo,
                        request_context,
                        &selection_before,
                        &host,
                        requested_manifest,
                        false,
                        false,
                        Some(&private_path),
                    )?;
                    if metadata_proves_requested_manifest(&generated, requested)?
                        != metadata_proves_requested_manifest(&no_deps, requested)?
                    {
                        return Err(
                            "Cargo workspace changed during private lockfile generation".to_owned()
                        );
                    }
                    let (lockfile, digest) = read_required_cargo_lockfile(&private_path)?;
                    if read_observation_file(
                        &missing_source_path,
                        MAX_CARGO_OBSERVATION_FILE_BYTES,
                    )?
                    .is_some()
                    {
                        return Err(
                            "the Cargo-selected source lock appeared during private resolution"
                                .to_owned(),
                        );
                    }
                    let locked = run_cargo_metadata_query(
                        &cargo,
                        request_context,
                        &selection_before,
                        &host,
                        requested_manifest,
                        false,
                        true,
                        Some(&private_path),
                    )?;
                    if metadata_proves_requested_manifest(&locked, requested)? != no_deps_workspace
                    {
                        return Err(
                            "Cargo workspace changed during private locked metadata".to_owned()
                        );
                    }
                    let (locked_file, locked_digest) = read_required_cargo_lockfile(&private_path)?;
                    if generated != locked || lockfile != locked_file || digest != locked_digest {
                        return Err(
                            "private Cargo lockfile did not reproduce locked metadata".to_owned()
                        );
                    }
                    if read_observation_file(
                        &missing_source_path,
                        MAX_CARGO_OBSERVATION_FILE_BYTES,
                    )?
                    .is_some()
                    {
                        return Err(
                            "the Cargo-selected source lock appeared during private resolution"
                                .to_owned(),
                        );
                    }
                    self.lock_state = Some(CargoMetadataLockState::Ephemeral {
                        _directory: directory,
                        private_path,
                        missing_source_path,
                        lockfile,
                        digest,
                    });
                    locked
                }
            }
        } else {
            match self.lock_state.as_ref().expect("lock state was checked") {
                CargoMetadataLockState::Observed {
                    path,
                    lockfile,
                    digest,
                } => {
                    let selected_path =
                        cargo_selected_lockfile_path(request_context, &no_deps_workspace)?;
                    if &selected_path != path {
                        return Err(
                            "Cargo selected lockfile path changed between metadata passes"
                                .to_owned(),
                        );
                    }
                    let (current, current_digest) = read_required_cargo_lockfile(path)?;
                    if &current != lockfile || &current_digest != digest {
                        return Err(
                            "Cargo's selected lockfile changed between metadata passes".to_owned()
                        );
                    }
                    let metadata = run_cargo_metadata_query(
                        &cargo,
                        request_context,
                        &selection_before,
                        &host,
                        requested_manifest,
                        false,
                        true,
                        None,
                    )?;
                    if metadata_proves_requested_manifest(&metadata, requested)?
                        != no_deps_workspace
                    {
                        return Err(
                            "Cargo effective workspace changed during locked metadata".to_owned()
                        );
                    }
                    let (after, after_digest) = read_required_cargo_lockfile(path)?;
                    if &after != lockfile || &after_digest != digest {
                        return Err("Cargo's selected lockfile changed during metadata".to_owned());
                    }
                    metadata
                }
                CargoMetadataLockState::Ephemeral {
                    private_path,
                    missing_source_path,
                    lockfile,
                    digest,
                    ..
                } => {
                    if read_observation_file(missing_source_path, MAX_CARGO_OBSERVATION_FILE_BYTES)?
                        .is_some()
                    {
                        return Err(
                            "the Cargo-selected source lock appeared during private resolution"
                                .to_owned(),
                        );
                    }
                    let (current, current_digest) = read_required_cargo_lockfile(private_path)?;
                    if &current != lockfile || &current_digest != digest {
                        return Err(
                            "private Cargo lockfile changed between metadata passes".to_owned()
                        );
                    }
                    let metadata = run_cargo_metadata_query(
                        &cargo,
                        request_context,
                        &selection_before,
                        &host,
                        requested_manifest,
                        false,
                        true,
                        Some(private_path),
                    )?;
                    if metadata_proves_requested_manifest(&metadata, requested)?
                        != no_deps_workspace
                    {
                        return Err(
                            "Cargo workspace changed during private locked metadata".to_owned()
                        );
                    }
                    let (after, after_digest) = read_required_cargo_lockfile(private_path)?;
                    if &after != lockfile || &after_digest != digest {
                        return Err("private Cargo lockfile changed during metadata".to_owned());
                    }
                    if read_observation_file(missing_source_path, MAX_CARGO_OBSERVATION_FILE_BYTES)?
                        .is_some()
                    {
                        return Err(
                            "the Cargo-selected source lock appeared during private resolution"
                                .to_owned(),
                        );
                    }
                    metadata
                }
            }
        };

        let (lockfile, lockfile_digest, lock_origin, lock_path_witness) = match self
            .lock_state
            .as_ref()
            .expect("metadata resolution established a lock state")
        {
            CargoMetadataLockState::Observed {
                path,
                lockfile,
                digest,
            } => (
                Some(lockfile.clone()),
                Some(*digest),
                Some(CargoMetadataLockOrigin::Observed),
                Some(path.clone()),
            ),
            CargoMetadataLockState::Ephemeral {
                missing_source_path,
                lockfile,
                digest,
                ..
            } => (
                Some(lockfile.clone()),
                Some(*digest),
                Some(CargoMetadataLockOrigin::EphemeralGeneratedCargoLockV1),
                Some(missing_source_path.clone()),
            ),
        };
        let selection_after = cargo_tool_selection(&cargo, request_context, &version)?;
        let tool_after =
            metadata_tool_witness(request_context, &version, &selection_after, cached)?;
        let environment_after = cargo_environment_witness()?;
        if tool_before.digest != tool_after.digest || environment_before != environment_after {
            return Err("Cargo tools or selection environment changed during metadata".to_owned());
        }
        Ok(CargoMetadataRun {
            metadata,
            host,
            tool_witness: tool_after.digest,
            no_deps_witness: Some(no_deps_witness),
            lockfile,
            lockfile_digest,
            lock_origin,
            lock_path_witness,
        })
    }
}

fn run_cargo_metadata_query(
    cargo: &Path,
    request_context: &Path,
    selection: &CargoToolSelection,
    host: &str,
    requested_manifest: &Path,
    no_deps: bool,
    locked: bool,
    private_lockfile: Option<&Path>,
) -> Result<Vec<u8>, String> {
    let manifest = requested_manifest
        .to_str()
        .ok_or_else(|| "requested Cargo manifest path is not UTF-8".to_owned())?;
    let mut arguments = Vec::<String>::new();
    if let Some(lockfile) = private_lockfile {
        validate_cargo_lockfile_path(lockfile)?;
        arguments.push("--config".to_owned());
        arguments.push(cargo_lockfile_path_config(lockfile)?);
    }
    arguments.push("metadata".to_owned());
    arguments.push("--offline".to_owned());
    if no_deps {
        arguments.push("--no-deps".to_owned());
    } else if locked {
        arguments.push("--locked".to_owned());
    }
    arguments.extend(["--format-version".to_owned(), "1".to_owned()]);
    if !no_deps {
        arguments.extend(["--filter-platform".to_owned(), host.to_owned()]);
    }
    arguments.extend(["--manifest-path".to_owned(), manifest.to_owned()]);
    let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
    run_with_default_rustc(
        cargo,
        request_context,
        &arguments,
        MAX_METADATA_BYTES,
        selection
            .inject_default_rustc
            .then_some(selection.rustc.as_path()),
    )
}

fn cargo_lockfile_path_config(lockfile: &Path) -> Result<String, String> {
    validate_cargo_lockfile_path(lockfile)?;
    let value = lockfile
        .to_str()
        .ok_or_else(|| "private Cargo lockfile path is not UTF-8".to_owned())?;
    let quoted = serde_json::to_string(value)
        .map_err(|error| format!("cannot encode private Cargo lockfile path: {error}"))?;
    Ok(format!("resolver.lockfile-path={quoted}"))
}

fn create_private_cargo_lock_directory(
    source_roots: &[PathBuf],
) -> Result<(tempfile::TempDir, PathBuf), String> {
    create_private_cargo_lock_directory_in(source_roots, &std::env::temp_dir())
}

fn create_private_cargo_lock_directory_in(
    source_roots: &[PathBuf],
    temporary_root: &Path,
) -> Result<(tempfile::TempDir, PathBuf), String> {
    if source_roots.len() > MAX_CARGO_METADATA_PACKAGES {
        return Err("Cargo source-root set exceeds the private-lock limit".to_owned());
    }
    let mut canonical_roots = Vec::with_capacity(source_roots.len());
    for root in source_roots {
        if !root.is_absolute()
            || root.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::CurDir | std::path::Component::ParentDir
                )
            })
        {
            return Err("Cargo source root is not a canonical absolute path".to_owned());
        }
        let canonical = root
            .canonicalize()
            .map_err(|error| format!("cannot resolve Cargo source root: {error}"))?;
        if &canonical != root {
            return Err("Cargo source root changed through a path alias".to_owned());
        }
        canonical_roots.push(canonical);
    }

    let temporary_root = temporary_root
        .canonicalize()
        .map_err(|error| format!("cannot resolve system temporary directory: {error}"))?;
    if canonical_roots
        .iter()
        .any(|source_root| temporary_root.starts_with(source_root))
    {
        return Err("system temporary directory is inside the Cargo source tree".to_owned());
    }
    let directory = tempfile::Builder::new()
        .prefix("backend-cargo-metadata-")
        .tempdir_in(&temporary_root)
        .map_err(|source| format!("cannot create private Cargo lock directory: {source}"))?;
    let private_directory = directory
        .path()
        .canonicalize()
        .map_err(|source| format!("cannot resolve private Cargo lock directory: {source}"))?;
    if canonical_roots
        .iter()
        .any(|source_root| private_directory.starts_with(source_root))
    {
        return Err("private Cargo lock directory was created inside a source root".to_owned());
    }
    Ok((directory, private_directory))
}

fn validate_cargo_lockfile_path(path: &Path) -> Result<(), String> {
    let text = path
        .to_str()
        .ok_or_else(|| "Cargo lockfile path is not UTF-8".to_owned())?;
    if text.is_empty()
        || text.len() > 4 * 1024
        || text.chars().any(char::is_control)
        || !path.is_absolute()
        || path.file_name().and_then(std::ffi::OsStr::to_str) != Some("Cargo.lock")
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
    {
        return Err(
            "Cargo lockfile path is not a bounded canonical absolute Cargo.lock".to_owned(),
        );
    }
    Ok(())
}

fn read_required_cargo_lockfile(path: &Path) -> Result<(String, [u8; 32]), String> {
    validate_cargo_lockfile_path(path)?;
    let bytes = read_observation_file(path, MAX_CARGO_OBSERVATION_FILE_BYTES)?
        .ok_or_else(|| format!("Cargo selected a missing lockfile: {}", path.display()))?;
    let lockfile = String::from_utf8(bytes)
        .map_err(|_| "Cargo selected lockfile is not valid UTF-8".to_owned())?;
    let digest = *blake3::hash(lockfile.as_bytes()).as_bytes();
    Ok((lockfile, digest))
}

fn missing_cargo_lockfile_path(error: &str) -> Option<PathBuf> {
    let path = error
        .strip_prefix("cargo metadata failed: error: cannot create the lock file ")?
        .strip_suffix(" because --locked was passed to prevent this")?;
    let path = PathBuf::from(path);
    validate_cargo_lockfile_path(&path).ok()?;
    Some(path)
}

/// Proves the selected Cargo's CLI lockfile redirect in an isolated fixture
/// before allowing any unlocked metadata command against the requested source.
/// The fixture deliberately conflicts at project-config and environment
/// priority, then requires both unlocked generation and a locked replay to
/// touch only the private CLI-selected lockfile.
fn verify_private_cargo_lockfile_redirect(
    cargo: &Path,
    selection: &CargoToolSelection,
    host: &str,
    source_roots: &[PathBuf],
) -> Result<(), String> {
    let (directory, private_root) = create_private_cargo_lock_directory(source_roots)?;
    let temporary_root = directory.path().to_path_buf();
    let workspace = temporary_root.join("probe-workspace");
    let config_dir = workspace.join(".cargo");
    let cli_lock_directory = private_root.join("cli-lock");
    let environment_lock = private_root.join("environment-sentinel/Cargo.lock");
    let config_lock = private_root.join("config-sentinel/Cargo.lock");
    let cli_lock = cli_lock_directory.join("Cargo.lock");
    std::fs::create_dir_all(workspace.join("src"))
        .map_err(|error| format!("cannot create Cargo lockfile capability fixture: {error}"))?;
    std::fs::create_dir_all(&config_dir)
        .map_err(|error| format!("cannot create Cargo capability config: {error}"))?;
    std::fs::create_dir_all(cli_lock_directory)
        .map_err(|error| format!("cannot create private CLI lock directory: {error}"))?;
    std::fs::create_dir_all(environment_lock.parent().expect("sentinel parent"))
        .map_err(|error| format!("cannot create environment sentinel directory: {error}"))?;
    std::fs::create_dir_all(config_lock.parent().expect("sentinel parent"))
        .map_err(|error| format!("cannot create config sentinel directory: {error}"))?;
    let workspace = workspace
        .canonicalize()
        .map_err(|error| format!("cannot resolve Cargo capability fixture: {error}"))?;
    let manifest = workspace.join("Cargo.toml");
    let config_lock_value = serde_json::to_string(
        config_lock
            .to_str()
            .ok_or_else(|| "Cargo config sentinel path is not UTF-8".to_owned())?,
    )
    .map_err(|error| format!("cannot encode Cargo config sentinel: {error}"))?;
    std::fs::write(
        &manifest,
        "[package]\nname = \"cargo-lockfile-capability-probe\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
    )
    .map_err(|error| format!("cannot write Cargo capability manifest: {error}"))?;
    std::fs::write(workspace.join("src/lib.rs"), "pub fn probe() {}\n")
        .map_err(|error| format!("cannot write Cargo capability source: {error}"))?;
    std::fs::write(
        config_dir.join("config.toml"),
        format!("[resolver]\nlockfile-path = {config_lock_value}\n"),
    )
    .map_err(|error| format!("cannot write Cargo capability config: {error}"))?;

    let cli_config = cargo_lockfile_path_config(&cli_lock)?;
    let manifest_text = manifest
        .to_str()
        .ok_or_else(|| "Cargo capability manifest path is not UTF-8".to_owned())?;
    let mut unlocked_arguments = vec![
        "--config".to_owned(),
        cli_config.clone(),
        "metadata".to_owned(),
        "--offline".to_owned(),
        "--format-version".to_owned(),
        "1".to_owned(),
        "--filter-platform".to_owned(),
        host.to_owned(),
        "--manifest-path".to_owned(),
        manifest_text.to_owned(),
    ];
    let environment_overrides = [(
        std::ffi::OsString::from("CARGO_RESOLVER_LOCKFILE_PATH"),
        environment_lock.as_os_str().to_os_string(),
    )];
    let unlocked_refs = unlocked_arguments
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let unlocked = run_with_default_rustc_and_overrides(
        cargo,
        &workspace,
        &unlocked_refs,
        MAX_METADATA_BYTES,
        selection
            .inject_default_rustc
            .then_some(selection.rustc.as_path()),
        &environment_overrides,
    )
    .map_err(|error| format!("private redirect fixture resolution failed: {error}"))?;
    let (private_lock, private_digest) = read_required_cargo_lockfile(&cli_lock)
        .map_err(|error| format!("CLI-selected lockfile was not generated: {error}"))?;
    if workspace.join("Cargo.lock").exists() || environment_lock.exists() || config_lock.exists() {
        return Err(
            "Cargo wrote a source, environment, or project-config sentinel lock".to_owned(),
        );
    }
    let requested = RequestedCargoManifest {
        root: workspace.clone(),
        manifest: manifest.clone(),
        has_package: true,
        has_workspace: false,
    };
    metadata_proves_requested_manifest(&unlocked, &requested)?;

    unlocked_arguments.insert(4, "--locked".to_owned());
    let locked_refs = unlocked_arguments
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let locked = run_with_default_rustc_and_overrides(
        cargo,
        &workspace,
        &locked_refs,
        MAX_METADATA_BYTES,
        selection
            .inject_default_rustc
            .then_some(selection.rustc.as_path()),
        &environment_overrides,
    )
    .map_err(|error| format!("private redirect locked replay failed: {error}"))?;
    let (replayed_lock, replayed_digest) = read_required_cargo_lockfile(&cli_lock)
        .map_err(|error| format!("CLI-selected lockfile disappeared on replay: {error}"))?;
    if locked != unlocked || replayed_lock != private_lock || replayed_digest != private_digest {
        return Err("CLI-selected lockfile did not reproduce the same Cargo metadata".to_owned());
    }
    if workspace.join("Cargo.lock").exists() || environment_lock.exists() || config_lock.exists() {
        return Err(
            "Cargo touched a source, environment, or project-config sentinel lock".to_owned(),
        );
    }

    directory
        .close()
        .map_err(|error| format!("cannot remove Cargo lockfile capability fixture: {error}"))?;
    if temporary_root.exists() {
        return Err("Cargo lockfile capability fixture was not removed".to_owned());
    }
    Ok(())
}

/// Resolves only the lockfile override that Cargo successfully used. An
/// environment override is unambiguous; file configuration is admitted only
/// when every observed `resolver.lockfile-path` agrees on one absolute path.
/// Conflicting or relative config semantics are refused instead of guessed.
pub(super) fn cargo_selected_lockfile_path(
    request_context: &Path,
    workspace: &Path,
) -> Result<PathBuf, String> {
    let environment = std::env::var_os("CARGO_RESOLVER_LOCKFILE_PATH");
    cargo_selected_lockfile_path_with_override(request_context, workspace, environment.as_deref())
}

fn cargo_selected_lockfile_path_with_override(
    request_context: &Path,
    workspace: &Path,
    environment: Option<&std::ffi::OsStr>,
) -> Result<PathBuf, String> {
    if let Some(value) = environment {
        let value = value
            .to_str()
            .ok_or_else(|| "CARGO_RESOLVER_LOCKFILE_PATH is not UTF-8".to_owned())?;
        let path = PathBuf::from(value);
        validate_cargo_lockfile_path(&path)?;
        return Ok(path);
    }
    let config_paths = cargo_config_paths(request_context)?;
    cargo_selected_lockfile_path_from_config_files(workspace, &config_paths)
}

fn cargo_selected_lockfile_path_from_config_files(
    workspace: &Path,
    config_paths: &[PathBuf],
) -> Result<PathBuf, String> {
    if config_paths.len() > MAX_CARGO_CONFIG_INPUTS {
        return Err("Cargo configuration input set exceeds its limit".to_owned());
    }
    let mut configured = BTreeSet::new();
    for config_path in config_paths {
        let Some(bytes) = read_observation_file(&config_path, MAX_CARGO_CONFIG_BYTES)? else {
            continue;
        };
        let document: toml::Value = std::str::from_utf8(&bytes)
            .map_err(|_| "Cargo config is not UTF-8".to_owned())?
            .parse()
            .map_err(|error: toml::de::Error| format!("Cargo config is malformed: {error}"))?;
        let Some(value) = document
            .get("resolver")
            .and_then(toml::Value::as_table)
            .and_then(|resolver| resolver.get("lockfile-path"))
        else {
            continue;
        };
        let value = value
            .as_str()
            .ok_or_else(|| "Cargo resolver.lockfile-path config is not a string".to_owned())?;
        let path = PathBuf::from(value);
        validate_cargo_lockfile_path(&path)?;
        configured.insert(path);
        if configured.len() > 1 {
            return Err(
                "Cargo resolver.lockfile-path differs across config inputs; refusing to guess Cargo precedence".to_owned(),
            );
        }
    }
    match configured.into_iter().next() {
        Some(path) => Ok(path),
        None => {
            let path = workspace.join("Cargo.lock");
            validate_cargo_lockfile_path(&path)?;
            Ok(path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        ObservationControl, observation_witness_with_context, requested_cargo_manifest, run,
        with_observation_control,
    };
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(prefix: &str) -> Scratch {
        Scratch(std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        )))
    }

    #[cfg(unix)]
    #[test]
    fn cancelled_private_resolution_releases_its_ephemeral_lock_directory() {
        let (directory, private_root) =
            create_private_cargo_lock_directory(&[]).expect("private lock directory");
        let private_root = private_root.to_path_buf();
        let private_lock = private_root.join("Cargo.lock");
        std::fs::write(&private_lock, "version = 4\n").expect("private generated lock");

        let control = Arc::new(ObservationControl::new());
        let cancelling = Arc::clone(&control);
        let trigger = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            cancelling.cancel();
        });
        let result = with_observation_control(control, || {
            run(
                Path::new("/bin/sh"),
                &private_root,
                &["-c", "sleep 10"],
                1024,
            )
        });
        trigger.join().expect("cancellation trigger");
        assert!(
            result.is_err_and(|error| error.contains("cancelled")),
            "the bounded resolver must stop on cancellation"
        );

        directory
            .close()
            .expect("remove cancelled resolver's private directory");
        assert!(
            !private_root.exists(),
            "the ephemeral generated lock and its directory must be cleaned"
        );
    }

    #[test]
    fn requested_manifest_is_exact_and_does_not_guess_an_ancestor_workspace() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let repository = repository.canonicalize().expect("repository");
        let package = repository.join("crates/present");
        let requested = requested_cargo_manifest(&package)
            .expect("exact manifest read")
            .expect("package manifest");
        assert_eq!(requested.root, package);
        assert_eq!(requested.manifest, package.join("Cargo.toml"));
        assert!(requested.has_package);
        assert!(!requested.has_workspace);
    }

    #[test]
    fn cargo_metadata_membership_binds_an_exact_manifest_and_request_root() {
        let scratch = scratch("backend-browse-member-scope");
        let workspace = scratch.0.join("backend");
        let member = workspace.join("tests/journeys");
        std::fs::create_dir_all(&member).expect("workspace member");
        std::fs::write(
            workspace.join("Cargo.toml"),
            "[workspace]\nmembers = [\"tests/*\"]\ndefault-members = []\nresolver = \"3\"\n",
        )
        .expect("workspace manifest");
        std::fs::write(
            member.join("Cargo.toml"),
            "[package]\nname = \"backend-journeys\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("member manifest");
        let workspace = workspace.canonicalize().expect("canonical workspace");
        let member = member.canonicalize().expect("canonical member");
        let requested = requested_cargo_manifest(&member)
            .expect("member manifest read")
            .expect("member request");
        let id = "backend-journeys 0.1.0 (path+file:///backend-journeys)";
        let metadata = serde_json::to_vec(&serde_json::json!({
            "workspace_root": workspace.clone(),
            "workspace_members": [id],
            "packages": [{ "id": id, "manifest_path": requested.manifest.clone(), "targets": [] }]
        }))
        .expect("Cargo-shaped metadata");
        // Cargo's metadata is the authority for glob and default-member
        // expansion. This function only checks its exact manifest/member rows.
        let effective = metadata_proves_requested_manifest(&metadata, &requested)
            .expect("Cargo metadata member proof");
        let observed_paths = metadata_observation_paths(&requested.root, &effective, &metadata)
            .expect("bounded Cargo input set");
        let required_paths =
            metadata_required_manifests(&metadata, &requested.manifest, &effective)
                .expect("required metadata manifest set");
        assert!(observed_paths.contains(&requested.manifest));
        assert!(required_paths.contains(&requested.manifest));
        assert!(observed_paths.contains(&workspace.join("Cargo.toml")));
        assert!(required_paths.contains(&workspace.join("Cargo.toml")));
        assert!(observed_paths.contains(&workspace.join("Cargo.lock")));

        assert_eq!(effective, workspace);
        let effective_text = effective.to_str().expect("UTF-8 workspace path");
        let member_binding = backend_library::browse::ProjectTreeRequestBindingV1::for_paths(
            &member,
            effective_text,
        )
        .expect("member request binding");
        let workspace_binding = backend_library::browse::ProjectTreeRequestBindingV1::for_paths(
            &workspace,
            effective_text,
        )
        .expect("workspace request binding");
        assert!(member_binding.matches_requested_root(&member));
        assert!(member_binding.matches_effective_workspace_root(effective_text));
        assert_ne!(
            member_binding.requested_root_digest, workspace_binding.requested_root_digest,
            "the requested member remains explicit in its workspace binding"
        );
        assert_eq!(
            member_binding.effective_workspace_root_digest,
            workspace_binding.effective_workspace_root_digest
        );
    }

    #[test]
    fn metadata_honors_an_explicit_nonancestor_workspace_and_rejects_nonmembers() {
        let scratch = scratch("backend-browse-explicit-workspace");
        let workspace = scratch.0.join("shared-workspace");
        let package = scratch.0.join("separate-tree/packages/tool");
        std::fs::create_dir_all(&workspace).expect("non-ancestor workspace");
        std::fs::create_dir_all(&package).expect("package outside workspace ancestry");
        std::fs::write(
            workspace.join("Cargo.toml"),
            "[workspace]\nmembers = [\"../separate-tree/packages/tool\"]\nresolver = \"3\"\n",
        )
        .expect("workspace manifest");
        std::fs::write(
            package.join("Cargo.toml"),
            "[package]\nname = \"tool\"\nversion = \"0.1.0\"\nedition = \"2024\"\nworkspace = \"../../../shared-workspace\"\n",
        )
        .expect("explicit package workspace manifest");
        let request_config = package.join(".cargo/config.toml");
        let workspace_config = workspace.join(".cargo/config.toml");
        let workspace_toolchain = workspace.join("rust-toolchain.toml");
        let malformed_config = scratch.0.join("malformed-cargo-config.toml");
        let malformed_toolchain = scratch.0.join("malformed-rust-toolchain.toml");
        std::fs::create_dir_all(request_config.parent().expect("request config directory"))
            .expect("request config directory");
        std::fs::create_dir_all(
            workspace_config
                .parent()
                .expect("workspace config directory"),
        )
        .expect("workspace config directory");
        std::fs::write(&request_config, "[net]\noffline = true\n")
            .expect("request-context Cargo config");
        std::fs::write(&malformed_config, "[build\ntarget = [\n")
            .expect("unrelated malformed Cargo config");
        std::fs::write(&malformed_toolchain, "[toolchain\nchannel = [\n")
            .expect("unrelated malformed rustup selector");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&malformed_config, &workspace_config)
                .expect("unrelated workspace Cargo config symlink");
            std::os::unix::fs::symlink(&malformed_toolchain, &workspace_toolchain)
                .expect("unrelated workspace rustup selector symlink");
        }
        #[cfg(not(unix))]
        {
            std::fs::write(&workspace_config, "[build\ntarget = [\n")
                .expect("unrelated malformed workspace Cargo config");
            std::fs::write(&workspace_toolchain, "[toolchain\nchannel = [\n")
                .expect("unrelated malformed workspace rustup selector");
        }
        let workspace = workspace.canonicalize().expect("canonical workspace");
        let package = package.canonicalize().expect("canonical package");
        let request_config = package.join(".cargo/config.toml");
        let workspace_config = workspace.join(".cargo/config.toml");
        let workspace_toolchain = workspace.join("rust-toolchain.toml");
        let requested = requested_cargo_manifest(&package)
            .expect("exact package manifest read")
            .expect("package request");
        let id = "tool 0.1.0 (fixture)";
        let metadata = serde_json::to_vec(&serde_json::json!({
            "workspace_root": workspace.clone(),
            "workspace_members": [id],
            "packages": [{ "id": id, "manifest_path": requested.manifest.clone(), "targets": [] }]
        }))
        .expect("Cargo-shaped metadata");
        assert_eq!(
            metadata_proves_requested_manifest(&metadata, &requested)
                .expect("explicit non-ancestor workspace proof"),
            workspace,
            "the Cargo result, not ancestor scanning, selects the effective workspace"
        );
        let cargo_home = scratch.0.join("isolated-cargo-home");
        std::fs::create_dir_all(&cargo_home).expect("isolated Cargo home");
        let config_paths = super::super::cargo_config_paths_with(&package, &cargo_home, None)
            .expect("request-CWD Cargo config paths");
        let sccache_paths = super::super::sccache_configuration_paths(&package)
            .expect("request-CWD sccache config paths");
        let rustup_paths =
            super::super::rustup_selection_paths(&package).expect("request-CWD rustup selectors");
        let document = CargoMetadataDocument::parse(&metadata).expect("bounded metadata inputs");
        let watched = document
            .inputs
            .assemble_observation_paths(&workspace, &config_paths, &sccache_paths, &rustup_paths)
            .expect("request-scoped observation paths with a non-ancestor workspace");
        assert!(watched.contains(&request_config));
        assert!(watched.contains(&workspace.join("Cargo.toml")));
        assert!(!watched.contains(&workspace_config));
        assert!(!watched.contains(&workspace_toolchain));
        super::super::observation_witness_with_context(&workspace, &watched, [1; 32], [2; 32])
            .expect("the strict source witness reads only selected CWD config and toolchain paths");

        let excluded_metadata = serde_json::to_vec(&serde_json::json!({
            "workspace_root": workspace.clone(),
            "workspace_members": [],
            "packages": [{ "id": id, "manifest_path": requested.manifest.clone(), "targets": [] }]
        }))
        .expect("nonmember Cargo-shaped metadata");
        assert!(
            metadata_proves_requested_manifest(&excluded_metadata, &requested)
                .is_err_and(|error| error.contains("workspace member")),
            "a package row without workspace_members membership is not admitted"
        );
    }

    #[test]
    fn cargo_lockfile_path_encoding_and_missing_diagnostic_are_narrow() {
        let scratch = scratch("backend-cargo-lock-path-policy");
        let lockfile = scratch.0.join("private lock directory/Cargo.lock");
        let override_value = cargo_lockfile_path_config(&lockfile).expect("TOML override");
        let parsed: toml::Value = override_value.parse().expect("valid Cargo --config value");
        assert_eq!(
            parsed
                .get("resolver")
                .and_then(toml::Value::as_table)
                .and_then(|resolver| resolver.get("lockfile-path"))
                .and_then(toml::Value::as_str),
            lockfile.to_str(),
            "the private absolute path must survive TOML quoting"
        );
        assert!(validate_cargo_lockfile_path(Path::new("Cargo.lock")).is_err());
        assert!(validate_cargo_lockfile_path(Path::new("/tmp/a/../Cargo.lock")).is_err());

        let missing = format!(
            "cargo metadata failed: error: cannot create the lock file {} because --locked was passed to prevent this",
            lockfile.display()
        );
        assert_eq!(missing_cargo_lockfile_path(&missing), Some(lockfile));
        assert!(
            missing_cargo_lockfile_path("cargo metadata failed: error: registry unavailable")
                .is_none()
        );
        assert!(missing_cargo_lockfile_path(
            "cargo metadata failed: error: cannot create the lock file relative/Cargo.lock because --locked was passed to prevent this"
        )
        .is_none());
    }

    #[test]
    fn configured_lock_path_selection_accepts_only_absolute_unambiguous_inputs() {
        let scratch = scratch("backend-cargo-lock-config-policy");
        std::fs::create_dir_all(&scratch.0).expect("config policy root");
        let workspace = scratch.0.canonicalize().expect("canonical workspace");
        let selected = workspace.join("observed/Cargo.lock");
        let conflicting = workspace.join("other/Cargo.lock");
        let config_a = workspace.join("config-a.toml");
        let config_b = workspace.join("config-b.toml");
        let write_config = |path: &Path, lockfile: &Path| {
            let encoded = serde_json::to_string(lockfile.to_str().expect("UTF-8 path"))
                .expect("TOML string encoding");
            std::fs::write(path, format!("[resolver]\nlockfile-path = {encoded}\n"))
                .expect("Cargo config");
        };
        write_config(&config_a, &selected);
        write_config(&config_b, &selected);

        assert_eq!(
            cargo_selected_lockfile_path_from_config_files(
                &workspace,
                &[config_a.clone(), config_b.clone()]
            )
            .expect("identical absolute config values are unambiguous"),
            selected
        );
        assert_eq!(
            cargo_selected_lockfile_path_from_config_files(&workspace, &[])
                .expect("default workspace lock path"),
            workspace.join("Cargo.lock")
        );

        write_config(&config_b, &conflicting);
        assert!(
            cargo_selected_lockfile_path_from_config_files(
                &workspace,
                &[config_a.clone(), config_b.clone()]
            )
            .is_err()
        );
        std::fs::write(
            &config_b,
            "[resolver]\nlockfile-path = \"relative/Cargo.lock\"\n",
        )
        .expect("relative path config");
        assert!(
            cargo_selected_lockfile_path_from_config_files(&workspace, &[config_b.clone()])
                .is_err()
        );

        let environment_path = workspace.join("environment/Cargo.lock");
        assert_eq!(
            cargo_selected_lockfile_path_with_override(
                &workspace,
                &workspace,
                Some(environment_path.as_os_str())
            )
            .expect("environment override is unambiguous"),
            environment_path,
            "the Cargo environment override takes precedence over file values"
        );
    }

    #[cfg(unix)]
    #[test]
    fn lockfile_environment_override_rejects_non_utf8_without_fallback() {
        use std::os::unix::ffi::OsStrExt;

        let non_utf8 = std::ffi::OsStr::from_bytes(b"\xff");
        let result = cargo_selected_lockfile_path_with_override(
            Path::new("/request"),
            Path::new("/workspace"),
            Some(non_utf8),
        );
        assert_eq!(
            result.expect_err("non-UTF-8 selected lock path must refuse"),
            "CARGO_RESOLVER_LOCKFILE_PATH is not UTF-8"
        );
    }

    #[test]
    fn registry_checksums_are_observed_and_target_paths_are_in_metadata_witness() {
        let scratch = scratch("backend-cargo-metadata-input-membership");
        let workspace = scratch.0.join("workspace");
        std::fs::create_dir_all(&workspace).expect("fixture workspace");
        let workspace = workspace
            .canonicalize()
            .expect("canonical fixture workspace");
        let registry_manifest = scratch.0.join("registry/serde/Cargo.toml");
        let local_manifest = workspace.join("local/Cargo.toml");
        let outside_target = workspace.join("local/../../external/custom-main.rs");
        let metadata = serde_json::to_vec(&serde_json::json!({
            "workspace_root": workspace.clone(),
            "workspace_members": ["local 0.1.0 (fixture)"],
            "packages": [
                {
                    "id": "serde 1.0.0 (registry)",
                    "source": "registry+https://github.com/rust-lang/crates.io-index",
                    "manifest_path": registry_manifest,
                    "targets": []
                },
                {
                    "id": "local 0.1.0 (fixture)",
                    "source": null,
                    "manifest_path": local_manifest,
                    "targets": [{ "src_path": outside_target, "kind": ["bin"] }]
                }
            ]
        }))
        .expect("metadata package rows");
        let document = CargoMetadataDocument::parse(&metadata).expect("bounded metadata inputs");
        let inputs = &document.inputs;
        assert_eq!(
            inputs.registry_checksums,
            [scratch.0.join("registry/serde/.cargo-checksum.json")]
        );
        assert_eq!(inputs.target_sources, [outside_target.clone()]);
        let observed = inputs
            .observation_paths(&scratch.0, &workspace)
            .expect("bounded metadata observation paths");
        assert!(observed.iter().any(|path| path.ends_with("Cargo.toml")));
        assert!(!observed.contains(&outside_target));

        let lock_bytes = b"version = 4\n";
        let digest = *blake3::hash(lock_bytes).as_bytes();
        let path = scratch.0.join("workspace/Cargo.lock");
        let observed = cargo_lock_origin_witness(CargoMetadataLockOrigin::Observed, &path, digest);
        let ephemeral = cargo_lock_origin_witness(
            CargoMetadataLockOrigin::EphemeralGeneratedCargoLockV1,
            &path,
            digest,
        );
        assert_ne!(
            observed, ephemeral,
            "generated and observed lock bytes have distinct origins"
        );
        assert_ne!(
            compose_cargo_input_witness([1; 32], [2; 32], observed),
            compose_cargo_input_witness([1; 32], [2; 32], ephemeral)
        );
        let first_output = cargo_metadata_output_witness(&metadata, "fixture-host");
        let mut updated_metadata: serde_json::Value =
            serde_json::from_slice(&metadata).expect("metadata json");
        updated_metadata["packages"][1]["targets"][0]["src_path"] = serde_json::Value::String(
            scratch
                .0
                .join("external/updated-main.rs")
                .display()
                .to_string(),
        );
        let second_output = cargo_metadata_output_witness(
            &serde_json::to_vec(&updated_metadata).expect("updated metadata"),
            "fixture-host",
        );
        assert_ne!(
            first_output, second_output,
            "Cargo target rows are part of the graph witness"
        );
        assert_ne!(
            cargo_metadata_output_witness(&metadata, "fixture-host"),
            cargo_metadata_output_witness(&metadata, "updated-host"),
            "the Cargo host selection is part of the metadata witness"
        );
    }

    #[test]
    fn private_lock_directory_refuses_a_temporary_root_inside_source() {
        fn source_tree_hash(root: &Path) -> [u8; 32] {
            fn visit(root: &Path, directory: &Path, hasher: &mut blake3::Hasher) {
                let mut entries = std::fs::read_dir(directory)
                    .expect("read source fixture directory")
                    .map(|entry| entry.expect("source fixture entry"))
                    .collect::<Vec<_>>();
                entries.sort_by_key(std::fs::DirEntry::file_name);
                for entry in entries {
                    let path = entry.path();
                    let relative = path.strip_prefix(root).expect("relative source path");
                    hasher.update(relative.as_os_str().as_encoded_bytes());
                    let kind = entry.file_type().expect("source fixture file type");
                    if kind.is_dir() {
                        hasher.update(&[0]);
                        visit(root, &path, hasher);
                    } else {
                        assert!(kind.is_file(), "test fixture contains only regular files");
                        let bytes = std::fs::read(&path).expect("source fixture file");
                        hasher.update(&[1]);
                        hasher.update(&(bytes.len() as u64).to_le_bytes());
                        hasher.update(&bytes);
                    }
                }
            }

            let mut hasher = blake3::Hasher::new();
            hasher.update(b"backend.private-lock-source-fixture.v1\0");
            visit(root, root, &mut hasher);
            *hasher.finalize().as_bytes()
        }

        let scratch = scratch("backend-cargo-private-lock-source-overlap");
        let source_root = scratch.0.join("project");
        let temporary_root = source_root.join(".tmp");
        std::fs::create_dir_all(temporary_root.join("existing")).expect("nested temp root");
        std::fs::write(
            source_root.join("Cargo.toml"),
            "[package]\nname = \"source\"\nversion = \"0.1.0\"\n",
        )
        .expect("source manifest");
        std::fs::write(temporary_root.join("existing/marker"), b"stable")
            .expect("existing temp-root marker");
        let source_root = source_root.canonicalize().expect("canonical source root");
        let temporary_root = temporary_root
            .canonicalize()
            .expect("canonical nested temporary root");
        let before = source_tree_hash(&source_root);
        assert!(
            create_private_cargo_lock_directory_in(
                std::slice::from_ref(&source_root),
                &temporary_root,
            )
            .is_err(),
            "the lock resolver must reject an injected temp root before writing inside source"
        );
        assert_eq!(source_tree_hash(&source_root), before);
        assert!(
            std::fs::read_dir(&temporary_root)
                .expect("temporary root remains readable")
                .all(|entry| entry.expect("temp entry").file_name() == "existing"),
            "refusal must not create a child beside the pre-existing marker"
        );
    }

    #[cfg(unix)]
    #[test]
    fn configured_lockfile_observation_does_not_follow_a_link() {
        let scratch = scratch("backend-cargo-lock-link-refusal");
        let source = scratch.0.join("source/Cargo.lock");
        let selected = scratch.0.join("selected/Cargo.lock");
        std::fs::create_dir_all(source.parent().expect("source parent"))
            .expect("source lock directory");
        std::fs::create_dir_all(selected.parent().expect("selected parent"))
            .expect("selected lock directory");
        std::fs::write(&source, "version = 4\n").expect("source lockfile");
        std::os::unix::fs::symlink(&source, &selected).expect("selected lock symlink");
        let selected = selected
            .parent()
            .expect("selected parent")
            .canonicalize()
            .expect("canonical selected parent")
            .join("Cargo.lock");
        assert!(
            read_required_cargo_lockfile(&selected).is_err(),
            "configured lockfiles are read through a no-follow directory capability"
        );
    }

    #[test]
    fn lockless_metadata_uses_a_private_lock_and_preserves_the_requested_tree() {
        let scratch = scratch("backend-cargo-lockless-metadata");
        std::fs::create_dir_all(&scratch.0).expect("lockless fixture root");
        let scratch_root = scratch.0.canonicalize().expect("canonical fixture root");
        let workspace = scratch_root.join("workspace");
        let app = workspace.join("app");
        let dependency = scratch_root.join("path-dependency");
        let config_dir = workspace.join(".cargo");
        std::fs::create_dir_all(app.join("src")).expect("app source");
        std::fs::create_dir_all(dependency.join("src")).expect("path dependency source");
        std::fs::create_dir_all(&config_dir).expect("Cargo config directory");
        let redirect = scratch_root.join("unwritten redirected lock/Cargo.lock");
        let config_path = config_dir.join("config.toml");
        let config = cargo_lockfile_path_config(&redirect).expect("absolute private path setting");
        std::fs::write(
            workspace.join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\"]\nresolver = \"3\"\n",
        )
        .expect("workspace manifest");
        std::fs::write(
            app.join("Cargo.toml"),
            "[package]\nname = \"lockless-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nlockless-helper = { path = \"../../path-dependency\" }\n",
        )
        .expect("app manifest");
        std::fs::write(app.join("src/lib.rs"), "pub fn fixture() {}\n").expect("app source file");
        std::fs::write(
            dependency.join("Cargo.toml"),
            "[package]\nname = \"lockless-helper\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("path dependency manifest");
        std::fs::write(dependency.join("src/lib.rs"), "pub fn helper() {}\n")
            .expect("path dependency source file");
        std::fs::write(&config_path, format!("{config}\n")).expect("resolver config");
        assert!(
            std::env::var_os("CARGO_RESOLVER_LOCKFILE_PATH").is_none(),
            "this integration fixture exercises the project-config lock path"
        );

        let workspace = workspace.canonicalize().expect("canonical workspace");
        let app = app.canonicalize().expect("canonical app");
        let requested = requested_cargo_manifest(&app)
            .expect("exact lockless manifest read")
            .expect("lockless package");
        let source_before = [
            workspace.join("Cargo.toml"),
            app.join("Cargo.toml"),
            app.join("src/lib.rs"),
            dependency.join("Cargo.toml"),
            dependency.join("src/lib.rs"),
            config_path.clone(),
        ]
        .into_iter()
        .map(|path| std::fs::read(path).expect("source input bytes"))
        .collect::<Vec<_>>();

        let mut session = CargoMetadataResolutionSession::default();
        let first = session
            .run(&requested, None)
            .expect("isolated Cargo probe and lockless exact metadata resolution");
        assert_eq!(
            first.lock_origin,
            Some(CargoMetadataLockOrigin::EphemeralGeneratedCargoLockV1)
        );
        assert_eq!(first.lock_path_witness.as_deref(), Some(redirect.as_path()));
        assert!(first.lockfile.is_some());
        assert_eq!(
            metadata_proves_requested_manifest(&first.metadata, &requested)
                .expect("resolved exact workspace"),
            workspace
        );
        let json: serde_json::Value =
            serde_json::from_slice(&first.metadata).expect("full metadata");
        assert!(
            json.get("resolve")
                .is_some_and(|resolve| !resolve.is_null())
        );
        let graph = metadata_input(&first.metadata, &first.host, first.lockfile.as_deref())
            .expect("resolved lockless dependency graph");
        let helper = graph
            .packages
            .iter()
            .find(|package| package.name == "lockless-helper")
            .expect("exact path dependency package row");
        assert!(
            graph.edges.iter().any(|edge| {
                graph
                    .packages
                    .iter()
                    .any(|package| package.id == edge.from && package.name == "lockless-fixture")
                    && edge.to == helper.id
            }),
            "the graph must contain Cargo's resolved app-to-helper edge: {:?}",
            graph.edges
        );

        let private_path = match session.lock_state.as_ref().expect("retained lock state") {
            CargoMetadataLockState::Ephemeral { private_path, .. } => private_path.clone(),
            CargoMetadataLockState::Observed { .. } => {
                panic!("the lockless fixture must not use a project lockfile")
            }
        };
        let private_directory = private_path
            .parent()
            .expect("private lock directory")
            .to_path_buf();
        assert!(!private_directory.starts_with(&workspace));
        assert!(!private_directory.starts_with(&app));
        assert!(
            private_path.is_file(),
            "the generated lock exists only in the private directory"
        );

        let second = session
            .run(&requested, None)
            .expect("locked metadata repeats from the private generated lock");
        assert_eq!(second.metadata, first.metadata);
        assert_eq!(second.lockfile, first.lockfile);
        assert_eq!(second.lockfile_digest, first.lockfile_digest);
        assert_eq!(second.lock_origin, first.lock_origin);
        assert!(!workspace.join("Cargo.lock").exists());
        assert!(!app.join("Cargo.lock").exists());
        assert!(!redirect.exists());
        for (path, expected) in [
            workspace.join("Cargo.toml"),
            app.join("Cargo.toml"),
            app.join("src/lib.rs"),
            dependency.join("Cargo.toml"),
            dependency.join("src/lib.rs"),
            config_path,
        ]
        .into_iter()
        .zip(source_before)
        {
            assert_eq!(
                std::fs::read(path).expect("source still readable"),
                expected
            );
        }
        drop(session);
        assert!(
            !private_directory.exists(),
            "the private generated lock directory is removed with its RAII owner"
        );
    }

    #[test]
    fn a_nested_workspace_request_requires_cargos_exact_root_witness() {
        let scratch = scratch("backend-browse-nested-workspace-scope");
        let outer = scratch.0.join("outer");
        let nested = outer.join("nested");
        std::fs::create_dir_all(&nested).expect("nested workspace");
        std::fs::write(
            outer.join("Cargo.toml"),
            "[workspace]\nmembers = []\nexclude = [\"nested\"]\nresolver = \"3\"\n",
        )
        .expect("outer workspace manifest");
        std::fs::write(
            nested.join("Cargo.toml"),
            "[workspace]\nmembers = []\nresolver = \"3\"\n",
        )
        .expect("nested workspace manifest");
        let nested = nested.canonicalize().expect("canonical nested workspace");
        let requested = requested_cargo_manifest(&nested)
            .expect("nested manifest read")
            .expect("nested workspace request");
        let metadata = serde_json::to_vec(&serde_json::json!({
            "workspace_root": nested.clone(),
            "workspace_members": [],
            "packages": []
        }))
        .expect("Cargo-shaped virtual-workspace metadata");
        assert_eq!(
            metadata_proves_requested_manifest(&metadata, &requested)
                .expect("exact nested workspace proof"),
            nested
        );
    }

    #[cfg(unix)]
    #[test]
    fn cargo_project_scope_does_not_follow_requested_directory_or_manifest_symlinks() {
        let scratch = scratch("backend-browse-symlink-scope");
        let target = scratch.0.join("target");
        std::fs::create_dir_all(&target).expect("target project");
        std::fs::write(
            target.join("Cargo.toml"),
            "[package]\nname = \"symlink-target\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("target manifest");

        let directory_link = scratch.0.join("directory-link");
        std::os::unix::fs::symlink(&target, &directory_link).expect("project directory symlink");
        assert!(
            requested_cargo_manifest(&directory_link).is_err(),
            "requested project directories are opened without following links"
        );

        let manifest_link_root = scratch.0.join("manifest-link-root");
        std::fs::create_dir_all(&manifest_link_root).expect("manifest-link project");
        std::os::unix::fs::symlink(
            target.join("Cargo.toml"),
            manifest_link_root.join("Cargo.toml"),
        )
        .expect("manifest symlink");
        assert!(
            requested_cargo_manifest(&manifest_link_root).is_err(),
            "the exact requested manifest is opened without following links"
        );
    }

    #[test]
    fn a_path_dependency_manifest_change_during_metadata_refuses_the_source_observation() {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "backend-cargo-observation-race-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        )));
        let workspace = scratch.0.join("workspace");
        let app = workspace.join("app");
        let dependency = scratch.0.join("path-dependency");
        std::fs::create_dir_all(app.join("src")).expect("app directory");
        std::fs::create_dir_all(dependency.join("src")).expect("dependency directory");
        std::fs::write(
            workspace.join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\"]\nresolver = \"2\"\n",
        )
        .expect("workspace manifest");
        std::fs::write(workspace.join("Cargo.lock"), "version = 4\n").expect("lockfile");
        std::fs::write(
            app.join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("app manifest");
        std::fs::write(
            dependency.join("Cargo.toml"),
            "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("path dependency manifest");
        let workspace = workspace.canonicalize().expect("canonical workspace");
        let app_manifest = app.join("Cargo.toml").canonicalize().expect("app path");
        let requested = requested_cargo_manifest(&app)
            .expect("request manifest read")
            .expect("app package request");
        let dependency_manifest = dependency
            .join("Cargo.toml")
            .canonicalize()
            .expect("dependency path");
        let metadata = serde_json::to_vec(&serde_json::json!({
            "workspace_root": workspace,
            "workspace_members": ["app 0.1.0 (path+file:///workspace/app)"],
            "packages": [
                {
                    "id": "app 0.1.0 (path+file:///workspace/app)",
                    "name": "app",
                    "version": "0.1.0",
                    "manifest_path": app_manifest,
                    "targets": []
                },
                {
                    "id": "dep 0.1.0 (path+file:///path-dependency)",
                    "name": "dep",
                    "version": "0.1.0",
                    "manifest_path": dependency_manifest,
                    "targets": []
                }
            ]
        }))
        .expect("metadata fixture");
        let mut runs = 0;
        let result = coherent_metadata_with(
            &requested,
            |manifest: &Path| {
                assert_eq!(manifest, requested.manifest);
                runs += 1;
                if runs == 2 {
                    std::fs::write(
                        &dependency_manifest,
                        "[package]\nname = \"dep\"\nversion = \"0.2.0\"\nedition = \"2021\"\n",
                    )
                    .expect("mutate path dependency during second metadata pass");
                }
                Ok((
                    metadata.clone(),
                    "x86_64-unknown-linux-gnu".to_owned(),
                    [9; 32],
                ))
            },
            |workspace, paths| observation_witness_with_context(workspace, paths, [1; 32], [2; 32]),
        );
        assert!(
            result
                .err()
                .expect("changed path dependency must invalidate source authority")
                .contains("changed during metadata"),
            "the observation must fail closed when Cargo ran across an input replacement"
        );
        assert_eq!(
            runs, 2,
            "no retry may turn this race into a success receipt"
        );
    }

    #[test]
    fn metadata_authority_rejects_a_required_manifest_absent_across_an_absent_present_absent_aba() {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "backend-cargo-manifest-aba-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        )));
        let workspace = scratch.0.join("workspace");
        let app = workspace.join("app");
        let dependency = scratch.0.join("path-dependency");
        std::fs::create_dir_all(&app).expect("app directory");
        std::fs::create_dir_all(&dependency).expect("dependency directory");
        std::fs::write(workspace.join("Cargo.toml"), "[workspace]\n").expect("workspace manifest");
        std::fs::write(workspace.join("Cargo.lock"), "version = 4\n").expect("lockfile");
        let app_manifest = app.join("Cargo.toml");
        let dependency_manifest = dependency.join("Cargo.toml");
        std::fs::write(&app_manifest, "[package]\nname=\"app\"\n").expect("app manifest");
        std::fs::write(&dependency_manifest, "[package]\nname=\"dep\"\n")
            .expect("dependency manifest");
        let workspace = workspace.canonicalize().expect("canonical workspace");
        let app_manifest = app_manifest.canonicalize().expect("canonical app manifest");
        let requested = requested_cargo_manifest(&app)
            .expect("request manifest read")
            .expect("app package request");
        let dependency_manifest = dependency_manifest
            .canonicalize()
            .expect("canonical dependency manifest");
        let metadata = serde_json::to_vec(&serde_json::json!({
            "workspace_root": workspace,
            "workspace_members": ["app 0.1.0 (path+file:///workspace/app)"],
            "packages": [
                {"id": "app 0.1.0 (path+file:///workspace/app)", "manifest_path": app_manifest, "targets": []},
                {"id": "dep 0.1.0 (path+file:///path-dependency)", "manifest_path": dependency_manifest, "targets": []}
            ]
        }))
        .expect("metadata fixture");
        let required = metadata_required_manifests(&metadata, &requested.manifest, &workspace)
            .expect("required manifest set");
        let watched = vec![
            workspace.join("Cargo.lock"),
            workspace.join("Cargo.toml"),
            app_manifest.clone(),
            dependency_manifest.clone(),
        ];

        // This is the exact digest-only ABA that used to compare equal:
        // sampling sees the required path absent, Cargo could see it present,
        // and the final sample sees it absent again.
        std::fs::remove_file(&dependency_manifest).expect("begin absent interval");
        let before = observation_witness_with_context(&workspace, &watched, [1; 32], [2; 32])
            .expect("pre-pass observation");
        std::fs::write(&dependency_manifest, "[package]\nname=\"dep\"\n")
            .expect("transiently restore manifest");
        std::fs::remove_file(&dependency_manifest).expect("end absent interval");
        let after = observation_witness_with_context(&workspace, &watched, [1; 32], [2; 32])
            .expect("post-pass observation");
        assert_eq!(
            before.digest, after.digest,
            "the absent/present/absent ABA preserves the old byte witness"
        );
        assert!(require_required_manifests_present(&before, &required).is_err());
        assert!(require_required_manifests_present(&after, &required).is_err());

        // The production two-pass gate refuses before starting another Cargo
        // pass when its required manifest is already missing.
        std::fs::write(&dependency_manifest, "[package]\nname=\"dep\"\n")
            .expect("restore manifest before gate");
        let mut cargo_runs = 0;
        let mut observations = 0;
        let result = coherent_metadata_with(
            &requested,
            |_| {
                cargo_runs += 1;
                Ok((metadata.clone(), "host".to_owned(), [3; 32]))
            },
            |workspace, paths| {
                observations += 1;
                if observations == 1 {
                    std::fs::remove_file(&dependency_manifest)
                        .expect("manifest disappears before second Cargo pass");
                }
                observation_witness_with_context(workspace, paths, [1; 32], [2; 32])
            },
        );
        assert!(
            result
                .err()
                .expect("required absent manifest must refuse authority")
                .contains("metadata-listed package manifest was absent"),
            "the refusal must identify the required-input condition without exposing a path"
        );
        assert_eq!(
            cargo_runs, 1,
            "the second Cargo pass must not run on an incomplete input set"
        );
        assert_eq!(observations, 1);
    }

    #[test]
    fn stable_metadata_witness_covers_nonmember_path_dependency_manifests() {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "backend-cargo-observation-stable-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        )));
        let workspace = scratch.0.join("workspace");
        let app = workspace.join("app");
        let dependency = scratch.0.join("path-dependency");
        std::fs::create_dir_all(&app).expect("app directory");
        std::fs::create_dir_all(&dependency).expect("dependency directory");
        std::fs::write(workspace.join("Cargo.toml"), "[workspace]\n").expect("workspace manifest");
        std::fs::write(workspace.join("Cargo.lock"), "version = 4\n").expect("lockfile");
        std::fs::write(app.join("Cargo.toml"), "[package]\nname=\"app\"\n").expect("app manifest");
        std::fs::write(dependency.join("Cargo.toml"), "[package]\nname=\"dep\"\n")
            .expect("dependency manifest");
        let workspace = workspace.canonicalize().expect("canonical workspace");
        let app_manifest = app.join("Cargo.toml").canonicalize().expect("app path");
        let dependency_manifest = dependency
            .join("Cargo.toml")
            .canonicalize()
            .expect("dependency path");
        let metadata = serde_json::to_vec(&serde_json::json!({
            "workspace_root": workspace,
            "workspace_members": ["app 0.1.0 (path+file:///workspace/app)"],
            "packages": [
                {"id": "app 0.1.0 (path+file:///workspace/app)", "manifest_path": app_manifest, "targets": []},
                {"id": "dep 0.1.0 (path+file:///path-dependency)", "manifest_path": dependency_manifest, "targets": []}
            ]
        }))
        .expect("metadata fixture");
        let requested = requested_cargo_manifest(&app)
            .expect("request manifest read")
            .expect("app package request");
        let result = coherent_metadata_with(
            &requested,
            |_| Ok((metadata.clone(), "host".to_owned(), [3; 32])),
            |workspace, paths| observation_witness_with_context(workspace, paths, [1; 32], [2; 32]),
        )
        .expect("stable input observation");
        assert_ne!(result.input_witness, [0; 32]);
        assert!(result.watched.contains(&dependency_manifest));
    }
}
