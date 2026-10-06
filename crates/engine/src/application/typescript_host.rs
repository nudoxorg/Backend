//! Request-scoped admission for a project's installed TypeScript compiler.
//!
//! Project discovery is deliberately rooted at the package resolver's selected path. It never
//! searches `PATH`, downloads packages, or executes a package-manager wrapper. A local `tsc`
//! entry is resolved to the `typescript` package's JavaScript file and probed through the exact
//! admitted Node executable in a closed environment.

use std::{
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

use backend_frontend_typescript::legacy::{Checker, ExplicitTypeScriptChecker};
use backend_semantic::vocabulary::NativeTool;
use backend_version::{ContentId, SourceFactDomain};
use blake3::Hasher;
use thiserror::Error;

use crate::application::{ToolchainProbeError, ToolchainProbeLimits};
use crate::driver::{ResolvedToolchain, ToolchainResolutionError};

const MAX_PROJECT_ANCESTORS: usize = 32;
const MAX_PACKAGE_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_PNPM_WORKSPACE_BYTES: usize = 64 * 1024;
const MAX_PROJECT_CONFIG_FILES: usize = 256;
const MAX_PROJECT_CONFIG_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ANGULAR_WORKSPACE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_RESOLVED_SOURCE_FILES: usize = 4096;
const MAX_RESOLVED_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SOURCE_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TYPESCRIPT_PACKAGE_FILES: usize = 256;
const MAX_TYPESCRIPT_PACKAGE_BYTES: u64 = 96 * 1024 * 1024;
const MAX_NODE_EXECUTABLE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_COMPILER_SHIM_BYTES: usize = 16 * 1024;
const MAX_SHEBANG_BYTES: usize = 256;
const MAX_RESOLVER_DIRECTORIES: usize = 4096;
const MAX_RESOLVER_OBSERVATIONS: usize = 16_384;
const MAX_RESOLVER_DIRECTORY_ENTRIES: usize = 16_384;
const MAX_RESOLVER_TOTAL_DIRECTORY_ENTRIES: usize = 65_536;
const MAX_RESOLVER_METADATA_BYTES: usize = 64 * 1024 * 1024;
const MAX_RESOLVER_DEPTH: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FileIdentity {
    pub(crate) first: u64,
    pub(crate) second: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileSnapshot {
    path: Box<Path>,
    identity: FileIdentity,
    length: u64,
    digest: [u8; 32],
}

/// Private package-scoped proof that binds a selected compiler launch to the host's full
/// TypeScript project witness. Content is revalidated at package boundaries; each child launch
/// uses this token only for cheap same-object checks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TypeScriptProjectInvocationLease {
    compiler_path: Box<Path>,
    compiler_identity: FileIdentity,
    compiler_digest: [u8; 32],
    node_path: Box<Path>,
    node_identity: FileIdentity,
    node_digest: [u8; 32],
    module_root: Box<Path>,
    package_root: Box<Path>,
    package_identity: FileIdentity,
    module_closure_digest: [u8; 32],
}

impl TypeScriptProjectInvocationLease {
    pub(crate) fn matches_invocation(
        &self,
        compiler: &Path,
        node: &Path,
        module_root: &Path,
    ) -> bool {
        self.compiler_path.as_ref() == compiler
            && self.node_path.as_ref() == node
            && self.module_root.as_ref() == module_root
            && self.package_root.parent() == Some(module_root)
    }

    pub(crate) const fn compiler_digest(&self) -> [u8; 32] {
        self.compiler_digest
    }

    pub(crate) const fn node_digest(&self) -> [u8; 32] {
        self.node_digest
    }

    pub(crate) const fn module_closure_digest(&self) -> [u8; 32] {
        self.module_closure_digest
    }

    pub(crate) fn validate_launch_objects(
        &self,
    ) -> Result<(), crate::driver::NativeInvocationError> {
        for (role, path, expected) in [
            (
                crate::driver::NativeInvocationFileRole::Script,
                self.compiler_path.as_ref(),
                self.compiler_identity,
            ),
            (
                crate::driver::NativeInvocationFileRole::Interpreter,
                self.node_path.as_ref(),
                self.node_identity,
            ),
        ] {
            let observed =
                crate::application::executable_object_identity(path).map_err(|source| {
                    crate::driver::NativeInvocationError::Inspect {
                        role,
                        path: path.to_path_buf().into_boxed_path(),
                        source,
                    }
                })?;
            if observed != (expected.first, expected.second) {
                return Err(crate::driver::NativeInvocationError::Changed {
                    role,
                    path: path.to_path_buf().into_boxed_path(),
                });
            }
        }
        if self.package_root.parent() != Some(self.module_root.as_ref()) {
            return Err(crate::driver::NativeInvocationError::Changed {
                role: crate::driver::NativeInvocationFileRole::CompilerModule,
                path: self.package_root.clone(),
            });
        }
        let observed = crate::application::compiler_directory_object_identity(&self.package_root)
            .map_err(|source| crate::driver::NativeInvocationError::Inspect {
            role: crate::driver::NativeInvocationFileRole::CompilerModule,
            path: self.package_root.clone(),
            source,
        })?;
        if observed != (self.package_identity.first, self.package_identity.second) {
            return Err(crate::driver::NativeInvocationError::Changed {
                role: crate::driver::NativeInvocationFileRole::CompilerModule,
                path: self.package_root.clone(),
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TypeScriptFileInput {
    pub(crate) path: Box<Path>,
    pub(crate) bytes: Box<[u8]>,
    pub(crate) content_id: ContentId<SourceFactDomain>,
    pub(crate) identity: FileIdentity,
}

#[derive(Clone, Debug)]
pub(crate) struct TypeScriptConfigInput {
    pub(crate) path: Box<Path>,
    pub(crate) bytes: Box<[u8]>,
    pub(crate) content_id: ContentId<SourceFactDomain>,
    pub(crate) identity: FileIdentity,
    pub(crate) project_candidate: bool,
    pub(crate) selected_build_config: bool,
    pub(crate) extends: Box<[Box<Path>]>,
    pub(crate) references: Box<[Box<Path>]>,
}

#[derive(Clone, Debug)]
pub(crate) struct TypeScriptProjectInputs<'a> {
    pub(crate) package_root: &'a Path,
    pub(crate) workspace_root: &'a Path,
    pub(crate) typescript_module_root: &'a Path,
    pub(crate) compiler_path: &'a Path,
    pub(crate) compiler_version: &'a [u8],
    pub(crate) compiler_origin: TypeScriptSelectionOrigin,
    pub(crate) node_path: &'a Path,
    pub(crate) node_version: &'a [u8],
    pub(crate) node_origin: TypeScriptSelectionOrigin,
    pub(crate) fingerprint: [u8; 32],
    pub(crate) config_candidates: &'a [TypeScriptConfigInput],
    pub(crate) selected_build_config_paths: &'a [Box<Path>],
    pub(crate) typescript_files: &'a [TypeScriptFileInput],
    witness: &'a TypeScriptProjectWitness,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum TypeScriptSelectionOrigin {
    ExplicitConfiguration = 1,
    ProjectLocalInstallation = 2,
    OrdinarySearchPath = 3,
    PlatformLocation = 4,
    ValidatedApplicationBundle = 5,
    /// A Node runtime in the same executable directory as a selected global `tsc`.
    PairedHostInstall = 6,
}

impl TypeScriptProjectInputs<'_> {
    /// Starts an isolated observation ledger for one compiler program.
    pub(crate) fn resolver(&self) -> TypeScriptResolverCapability<'_> {
        TypeScriptResolverCapability {
            witness: self.witness,
            observations: ResolverObservationLedger::default(),
        }
    }

    pub(crate) fn load_source(
        &self,
        path: &Path,
    ) -> Result<TypeScriptFileInput, TypeScriptProjectHostError> {
        self.witness.load_source(path)
    }

    pub(crate) fn validate_current(&self) -> Result<(), TypeScriptProjectHostError> {
        self.witness.validate_current()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TypeScriptDirectoryEntryKind {
    RegularFile,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TypeScriptDirectoryEntry {
    pub(crate) path: Box<Path>,
    pub(crate) canonical_path: Option<Box<Path>>,
    pub(crate) kind: TypeScriptDirectoryEntryKind,
    pub(crate) identity: FileIdentity,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum TypeScriptResolverObservationRef<'a> {
    Source(&'a TypeScriptFileInput),
    MissingPath {
        path: &'a Path,
        nearest_existing_parent: Option<&'a Path>,
    },
    Directory {
        path: &'a Path,
        identity: FileIdentity,
        entries: &'a [TypeScriptDirectoryEntry],
    },
    Realpath {
        path: &'a Path,
        canonical_path: Option<&'a Path>,
        identity: Option<FileIdentity>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RealpathSnapshot {
    path: Box<Path>,
    canonical_path: Option<Box<Path>>,
    identity: Option<FileIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DirectorySnapshot {
    path: Box<Path>,
    identity: FileIdentity,
    entries: Box<[TypeScriptDirectoryEntry]>,
}

#[derive(Debug, Default)]
struct ResolverObservationLedger {
    loaded_sources: std::collections::BTreeMap<PathBuf, TypeScriptFileInput>,
    missing_paths: std::collections::BTreeMap<PathBuf, Option<PathBuf>>,
    directories: std::collections::BTreeMap<PathBuf, DirectorySnapshot>,
    realpaths: std::collections::BTreeMap<PathBuf, RealpathSnapshot>,
    loaded_source_bytes: u64,
    directory_entries: usize,
    retained_metadata_bytes: usize,
}

impl ResolverObservationLedger {
    fn observation_count(&self) -> usize {
        self.loaded_sources
            .len()
            .saturating_add(self.missing_paths.len())
            .saturating_add(self.directories.len())
            .saturating_add(self.realpaths.len())
    }
}

/// Per-program filesystem capability for TypeScript configuration and module resolution.
///
/// Every positive read and negative lookup is retained and can be included in the program recipe.
/// The capability is scoped to the selected package workspace and TypeScript module root.
#[derive(Debug)]
pub(crate) struct TypeScriptResolverCapability<'a> {
    witness: &'a TypeScriptProjectWitness,
    observations: ResolverObservationLedger,
}

impl TypeScriptResolverCapability<'_> {
    pub(crate) fn try_load_source<'a>(
        &'a mut self,
        path: &Path,
    ) -> Result<Option<&'a TypeScriptFileInput>, TypeScriptProjectHostError> {
        let lexical = self.witness.admit_lexical_path(path)?;
        let Some((canonical, identity)) = self.capture_realpath(&lexical)? else {
            self.record_missing(lexical, None)?;
            return Ok(None);
        };
        if !self.witness.path_is_admitted(&canonical) {
            return Err(TypeScriptProjectHostError::SourceOutsideCapability {
                path: canonical.into_boxed_path(),
            });
        }
        if self.observations.missing_paths.contains_key(&lexical) {
            return Err(TypeScriptProjectHostError::WitnessChanged {
                path: lexical.into_boxed_path(),
            });
        }
        if self.observations.loaded_sources.contains_key(&canonical) {
            record_realpath_ledger(
                &mut self.observations,
                lexical,
                Some(canonical.clone()),
                identity,
            )?;
            return Ok(self.observations.loaded_sources.get(&canonical));
        }
        let (snapshot, bytes) = read_regular_file(&canonical, MAX_SOURCE_FILE_BYTES)?;
        if self
            .capture_realpath(&lexical)?
            .as_ref()
            .map(|(path, id)| (path.as_path(), id))
            != Some((canonical.as_path(), &identity))
        {
            return Err(TypeScriptProjectHostError::WitnessChanged {
                path: lexical.clone().into_boxed_path(),
            });
        }
        let input = TypeScriptFileInput {
            path: canonical.clone().into_boxed_path(),
            content_id: ContentId::<SourceFactDomain>::from_canonical_bytes(&bytes),
            identity: snapshot.identity,
            bytes: bytes.into_boxed_slice(),
        };
        if self.observations.observation_count() >= MAX_RESOLVER_OBSERVATIONS {
            return Err(TypeScriptProjectHostError::ResolverObservationLimit {
                observed: self.observations.observation_count().saturating_add(1),
                maximum: MAX_RESOLVER_OBSERVATIONS,
            });
        }
        if self.observations.loaded_sources.len() >= MAX_RESOLVED_SOURCE_FILES {
            return Err(TypeScriptProjectHostError::ResolvedSourceLimit {
                observed: self.observations.loaded_sources.len().saturating_add(1),
                maximum: MAX_RESOLVED_SOURCE_FILES,
            });
        }
        let observed_bytes = self
            .observations
            .loaded_source_bytes
            .saturating_add(u64::try_from(input.bytes.len()).unwrap_or(u64::MAX));
        if observed_bytes > MAX_RESOLVED_SOURCE_BYTES {
            return Err(TypeScriptProjectHostError::ResolvedSourceBytes {
                observed: observed_bytes,
                maximum: MAX_RESOLVED_SOURCE_BYTES,
            });
        }
        let path_cost = canonical.as_os_str().as_encoded_bytes().len();
        ensure_metadata_capacity(&self.observations, path_cost, &canonical)?;
        record_realpath_ledger(
            &mut self.observations,
            lexical,
            Some(canonical.clone()),
            identity,
        )?;
        self.observations.loaded_source_bytes = observed_bytes;
        self.observations.retained_metadata_bytes += path_cost;
        self.observations
            .loaded_sources
            .insert(canonical.clone(), input);
        Ok(self.observations.loaded_sources.get(&canonical))
    }

    pub(crate) fn directory_exists(
        &mut self,
        path: &Path,
    ) -> Result<bool, TypeScriptProjectHostError> {
        let lexical = self.witness.admit_lexical_path(path)?;
        let Some((canonical, identity)) = self.capture_realpath(&lexical)? else {
            self.record_missing(lexical, None)?;
            return Ok(false);
        };
        if !self.witness.path_is_admitted(&canonical) {
            return Err(TypeScriptProjectHostError::SourceOutsideCapability {
                path: canonical.into_boxed_path(),
            });
        }
        self.record_realpath(lexical.clone(), Some(canonical.clone()), identity)?;
        let metadata =
            fs::metadata(&canonical).map_err(|source| TypeScriptProjectHostError::PackagePath {
                path: canonical.clone().into_boxed_path(),
                source,
            })?;
        if !metadata.is_dir() {
            return Ok(false);
        }
        self.observe_directory(&canonical)?;
        Ok(true)
    }

    pub(crate) fn realpath(
        &mut self,
        path: &Path,
    ) -> Result<Option<Box<Path>>, TypeScriptProjectHostError> {
        let lexical = self.witness.admit_lexical_path(path)?;
        let Some((canonical, identity)) = self.capture_realpath(&lexical)? else {
            self.record_missing(lexical, None)?;
            return Ok(None);
        };
        if !self.witness.path_is_admitted(&canonical) {
            return Err(TypeScriptProjectHostError::SourceOutsideCapability {
                path: canonical.into_boxed_path(),
            });
        }
        self.record_realpath(lexical, Some(canonical.clone()), identity)?;
        Ok(Some(canonical.into_boxed_path()))
    }

    /// Returns matching regular files, sorted by canonical path. Symlinked directories are
    /// traversed only when their resolved target stays inside an admitted root.
    pub(crate) fn read_directory(
        &mut self,
        path: &Path,
        extensions: &[&str],
        recursive: bool,
        maximum_depth: usize,
        maximum_entries: usize,
    ) -> Result<Vec<TypeScriptDirectoryEntry>, TypeScriptProjectHostError> {
        if maximum_depth > MAX_RESOLVER_DEPTH
            || maximum_entries == 0
            || maximum_entries > MAX_RESOLVER_DIRECTORY_ENTRIES
        {
            return Err(TypeScriptProjectHostError::ResolverDirectoryLimit {
                requested_depth: maximum_depth,
                requested_entries: maximum_entries,
                maximum_depth: MAX_RESOLVER_DEPTH,
                maximum_entries: MAX_RESOLVER_DIRECTORY_ENTRIES,
            });
        }
        let mut pending = vec![(path.to_path_buf(), 0_usize)];
        let mut visited = std::collections::BTreeSet::new();
        let mut output = Vec::new();
        while let Some((directory, depth)) = pending.pop() {
            if !visited.insert(directory.clone()) {
                continue;
            }
            if depth > maximum_depth {
                return Err(TypeScriptProjectHostError::ResolverDirectoryLimit {
                    requested_depth: depth,
                    requested_entries: maximum_entries,
                    maximum_depth,
                    maximum_entries,
                });
            }
            let lexical = self.witness.admit_lexical_path(&directory)?;
            let Some((canonical, identity)) = self.capture_realpath(&lexical)? else {
                self.record_missing(lexical, None)?;
                continue;
            };
            if !self.witness.path_is_admitted(&canonical) {
                return Err(TypeScriptProjectHostError::SourceOutsideCapability {
                    path: canonical.into_boxed_path(),
                });
            }
            let metadata = fs::metadata(&canonical).map_err(|source| {
                TypeScriptProjectHostError::PackagePath {
                    path: canonical.clone().into_boxed_path(),
                    source,
                }
            })?;
            if !metadata.is_dir() {
                continue;
            }
            self.record_realpath(lexical, Some(canonical.clone()), identity)?;
            self.observe_directory(&canonical)?;
            let snapshot = self
                .observations
                .directories
                .get(&canonical)
                .expect("directory snapshot was recorded above");
            for entry in snapshot.entries.iter() {
                let actual = entry
                    .canonical_path
                    .as_deref()
                    .unwrap_or(entry.path.as_ref());
                if !self.witness.path_is_admitted(actual) {
                    return Err(TypeScriptProjectHostError::SourceOutsideCapability {
                        path: actual.to_path_buf().into_boxed_path(),
                    });
                }
                let actual_metadata = match fs::metadata(actual) {
                    Ok(metadata) => metadata,
                    Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
                    Err(source) => {
                        return Err(TypeScriptProjectHostError::PackagePath {
                            path: actual.to_path_buf().into_boxed_path(),
                            source,
                        });
                    }
                };
                if actual_metadata.is_dir() {
                    if recursive && depth < maximum_depth {
                        pending.push((entry.path.to_path_buf(), depth + 1));
                    }
                    continue;
                }
                if !actual_metadata.is_file() || !extension_matches(&entry.path, extensions) {
                    continue;
                }
                if !self.witness.path_is_admitted(actual) {
                    return Err(TypeScriptProjectHostError::SourceOutsideCapability {
                        path: actual.to_path_buf().into_boxed_path(),
                    });
                }
                output.push(entry.clone());
                if output.len() > maximum_entries {
                    return Err(TypeScriptProjectHostError::ResolverDirectoryLimit {
                        requested_depth: depth,
                        requested_entries: output.len(),
                        maximum_depth,
                        maximum_entries,
                    });
                }
            }
        }
        output.sort_unstable_by(|left, right| left.path.cmp(&right.path));
        Ok(output)
    }

    pub(crate) fn loaded_sources(
        &self,
    ) -> &std::collections::BTreeMap<PathBuf, TypeScriptFileInput> {
        &self.observations.loaded_sources
    }

    pub(crate) fn observations(&self) -> Vec<TypeScriptResolverObservationRef<'_>> {
        let mut output = Vec::new();
        for input in self.observations.loaded_sources.values() {
            output.push(TypeScriptResolverObservationRef::Source(input));
        }
        for (path, parent) in &self.observations.missing_paths {
            output.push(TypeScriptResolverObservationRef::MissingPath {
                path,
                nearest_existing_parent: parent.as_deref(),
            });
        }
        for snapshot in self.observations.directories.values() {
            output.push(TypeScriptResolverObservationRef::Directory {
                path: &snapshot.path,
                identity: snapshot.identity,
                entries: &snapshot.entries,
            });
        }
        for snapshot in self.observations.realpaths.values() {
            output.push(TypeScriptResolverObservationRef::Realpath {
                path: &snapshot.path,
                canonical_path: snapshot.canonical_path.as_deref(),
                identity: snapshot.identity,
            });
        }
        output.sort_unstable_by(|left, right| {
            resolver_observation_ref_path(left)
                .cmp(resolver_observation_ref_path(right))
                .then_with(|| {
                    resolver_observation_ref_tag(left).cmp(&resolver_observation_ref_tag(right))
                })
        });
        output
    }

    pub(crate) fn resolver_witness(&self) -> Result<[u8; 32], TypeScriptProjectHostError> {
        let mut digest = Hasher::new();
        digest.update(b"compiler.typescript.resolver-observations.v2\0");
        update_len(&mut digest, self.observations.observation_count());
        for input in self.observations.loaded_sources.values() {
            digest.update(&[1]);
            update_logical_path(&mut digest, self.witness, &input.path)?;
            digest.update(input.content_id.as_ref());
        }
        for (path, parent) in &self.observations.missing_paths {
            digest.update(&[2]);
            update_logical_path(&mut digest, self.witness, path)?;
            match parent {
                Some(parent) => {
                    digest.update(&[1]);
                    update_logical_path(&mut digest, self.witness, parent)?;
                }
                None => {
                    digest.update(&[0]);
                }
            }
        }
        for snapshot in self.observations.directories.values() {
            digest.update(&[3]);
            update_logical_path(&mut digest, self.witness, &snapshot.path)?;
            update_len(&mut digest, snapshot.entries.len());
            for entry in snapshot.entries.iter() {
                update_logical_path(&mut digest, self.witness, &entry.path)?;
                digest.update(&[match entry.kind {
                    TypeScriptDirectoryEntryKind::RegularFile => 1,
                    TypeScriptDirectoryEntryKind::Directory => 2,
                    TypeScriptDirectoryEntryKind::Symlink => 3,
                    TypeScriptDirectoryEntryKind::Other => 4,
                }]);
                match entry.canonical_path.as_deref() {
                    Some(path) => {
                        digest.update(&[1]);
                        update_logical_path(&mut digest, self.witness, path)?;
                    }
                    None => {
                        digest.update(&[0]);
                    }
                }
            }
        }
        for snapshot in self.observations.realpaths.values() {
            digest.update(&[4]);
            update_logical_path(&mut digest, self.witness, &snapshot.path)?;
            match snapshot.canonical_path.as_deref() {
                Some(path) => {
                    digest.update(&[1]);
                    update_logical_path(&mut digest, self.witness, path)?;
                }
                None => {
                    digest.update(&[0]);
                }
            }
        }
        Ok(*digest.finalize().as_bytes())
    }

    pub(crate) fn validate_current(&self) -> Result<(), TypeScriptProjectHostError> {
        self.witness.validate_current()?;
        for input in self.observations.loaded_sources.values() {
            let observed = capture_file_snapshot(&input.path, MAX_SOURCE_FILE_BYTES)?;
            if observed.identity != input.identity
                || observed.length != u64::try_from(input.bytes.len()).unwrap_or(u64::MAX)
                || observed.digest != *blake3::hash(&input.bytes).as_bytes()
            {
                return Err(TypeScriptProjectHostError::WitnessChanged {
                    path: input.path.clone(),
                });
            }
        }
        for (path, nearest_parent) in &self.observations.missing_paths {
            if !matches!(fs::symlink_metadata(path), Err(ref source) if source.kind() == io::ErrorKind::NotFound)
            {
                return Err(TypeScriptProjectHostError::WitnessChanged {
                    path: path.clone().into_boxed_path(),
                });
            }
            if let Some(parent) = nearest_parent {
                let observed = capture_directory_snapshot(parent, self.witness)?;
                if !self
                    .observations
                    .directories
                    .get(parent)
                    .is_some_and(|expected| expected == &observed)
                {
                    return Err(TypeScriptProjectHostError::WitnessChanged {
                        path: parent.clone().into_boxed_path(),
                    });
                }
            }
        }
        for (path, expected) in &self.observations.directories {
            let observed = capture_directory_snapshot(path, self.witness)?;
            if &observed != expected {
                return Err(TypeScriptProjectHostError::WitnessChanged {
                    path: path.clone().into_boxed_path(),
                });
            }
        }
        for (path, expected) in &self.observations.realpaths {
            let observed = self.capture_realpath(path)?;
            let canonical = observed.as_ref().map(|(path, _)| path.as_path());
            if canonical != expected.canonical_path.as_deref()
                || observed.as_ref().and_then(|(_, identity)| *identity) != expected.identity
            {
                return Err(TypeScriptProjectHostError::WitnessChanged {
                    path: path.clone().into_boxed_path(),
                });
            }
        }
        Ok(())
    }

    fn capture_realpath(
        &self,
        path: &Path,
    ) -> Result<Option<(PathBuf, Option<FileIdentity>)>, TypeScriptProjectHostError> {
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                let identity = file_identity(&metadata).map_err(|source| {
                    TypeScriptProjectHostError::PackagePath {
                        path: path.to_path_buf().into_boxed_path(),
                        source,
                    }
                })?;
                let canonical = fs::canonicalize(path).map_err(|source| {
                    TypeScriptProjectHostError::PackagePath {
                        path: path.to_path_buf().into_boxed_path(),
                        source,
                    }
                })?;
                Ok(Some((canonical, Some(identity))))
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(TypeScriptProjectHostError::PackagePath {
                path: path.to_path_buf().into_boxed_path(),
                source,
            }),
        }
    }

    fn record_missing(
        &mut self,
        path: PathBuf,
        nearest_existing_parent: Option<PathBuf>,
    ) -> Result<(), TypeScriptProjectHostError> {
        let nearest_existing_parent = match nearest_existing_parent {
            Some(parent) => Some(parent),
            None => nearest_existing_admitted_parent(&path, self.witness)?,
        };
        if self
            .observations
            .realpaths
            .get(&path)
            .is_some_and(|snapshot| snapshot.canonical_path.is_some())
        {
            return Err(TypeScriptProjectHostError::WitnessChanged {
                path: path.into_boxed_path(),
            });
        }
        if let Some(parent) = nearest_existing_parent.as_deref() {
            self.observe_directory(parent)?;
        }
        let existed = self.observations.missing_paths.contains_key(&path);
        if !existed {
            ensure_observation_capacity(&self.observations, &path)?;
            let metadata_cost = path.as_os_str().as_encoded_bytes().len()
                + nearest_existing_parent
                    .as_deref()
                    .map_or(0, |parent| parent.as_os_str().as_encoded_bytes().len());
            ensure_metadata_capacity(&self.observations, metadata_cost, &path)?;
            self.observations.retained_metadata_bytes += metadata_cost;
        }
        match self.observations.missing_paths.get(&path) {
            Some(previous) if previous != &nearest_existing_parent => {
                Err(TypeScriptProjectHostError::WitnessChanged {
                    path: path.into_boxed_path(),
                })
            }
            Some(_) => Ok(()),
            None => {
                self.observations
                    .missing_paths
                    .insert(path, nearest_existing_parent);
                Ok(())
            }
        }
    }

    fn record_realpath(
        &mut self,
        path: PathBuf,
        canonical_path: Option<PathBuf>,
        identity: Option<FileIdentity>,
    ) -> Result<(), TypeScriptProjectHostError> {
        record_realpath_ledger(&mut self.observations, path, canonical_path, identity)
    }

    fn observe_directory(&mut self, path: &Path) -> Result<(), TypeScriptProjectHostError> {
        let snapshot = capture_directory_snapshot(path, self.witness)?;
        if let Some(previous) = self.observations.directories.get(path) {
            if previous != &snapshot {
                return Err(TypeScriptProjectHostError::WitnessChanged {
                    path: path.to_path_buf().into_boxed_path(),
                });
            }
            return Ok(());
        }
        if self.observations.directories.len() >= MAX_RESOLVER_DIRECTORIES {
            return Err(TypeScriptProjectHostError::ResolverObservationLimit {
                observed: self.observations.directories.len().saturating_add(1),
                maximum: MAX_RESOLVER_DIRECTORIES,
            });
        }
        ensure_observation_capacity(&self.observations, path)?;
        let entries = self
            .observations
            .directory_entries
            .saturating_add(snapshot.entries.len());
        if entries > MAX_RESOLVER_TOTAL_DIRECTORY_ENTRIES {
            return Err(TypeScriptProjectHostError::ResolverDirectoryLimit {
                requested_depth: 0,
                requested_entries: entries,
                maximum_depth: MAX_RESOLVER_DEPTH,
                maximum_entries: MAX_RESOLVER_TOTAL_DIRECTORY_ENTRIES,
            });
        }
        let metadata_cost = directory_snapshot_metadata_cost(&snapshot);
        ensure_metadata_capacity(&self.observations, metadata_cost, path)?;
        self.observations.directory_entries = entries;
        self.observations.retained_metadata_bytes += metadata_cost;
        self.observations
            .directories
            .insert(path.to_path_buf(), snapshot);
        Ok(())
    }
}

fn ensure_observation_capacity(
    ledger: &ResolverObservationLedger,
    path: &Path,
) -> Result<(), TypeScriptProjectHostError> {
    let observed = ledger.observation_count().saturating_add(1);
    if observed > MAX_RESOLVER_OBSERVATIONS {
        return Err(TypeScriptProjectHostError::ResolverObservationLimit {
            observed,
            maximum: MAX_RESOLVER_OBSERVATIONS,
        });
    }
    let _ = path;
    Ok(())
}

fn ensure_metadata_capacity(
    ledger: &ResolverObservationLedger,
    additional: usize,
    path: &Path,
) -> Result<(), TypeScriptProjectHostError> {
    let observed = ledger.retained_metadata_bytes.saturating_add(additional);
    if observed > MAX_RESOLVER_METADATA_BYTES {
        return Err(TypeScriptProjectHostError::ResolverMetadataLimit {
            path: path.to_path_buf().into_boxed_path(),
            observed,
            maximum: MAX_RESOLVER_METADATA_BYTES,
        });
    }
    Ok(())
}

fn record_realpath_ledger(
    ledger: &mut ResolverObservationLedger,
    path: PathBuf,
    canonical_path: Option<PathBuf>,
    identity: Option<FileIdentity>,
) -> Result<(), TypeScriptProjectHostError> {
    let snapshot = RealpathSnapshot {
        path: path.clone().into_boxed_path(),
        canonical_path: canonical_path.map(PathBuf::into_boxed_path),
        identity,
    };
    if snapshot.canonical_path.is_some() && ledger.missing_paths.contains_key(&path) {
        return Err(TypeScriptProjectHostError::WitnessChanged {
            path: path.into_boxed_path(),
        });
    }
    if let Some(previous) = ledger.realpaths.get(&path) {
        if previous != &snapshot {
            return Err(TypeScriptProjectHostError::WitnessChanged {
                path: path.into_boxed_path(),
            });
        }
        return Ok(());
    }
    ensure_observation_capacity(ledger, &path)?;
    let metadata_cost = path.as_os_str().as_encoded_bytes().len()
        + snapshot.canonical_path.as_deref().map_or(0, |canonical| {
            canonical.as_os_str().as_encoded_bytes().len()
        });
    ensure_metadata_capacity(ledger, metadata_cost, &path)?;
    ledger.retained_metadata_bytes += metadata_cost;
    ledger.realpaths.insert(path, snapshot);
    Ok(())
}

fn directory_snapshot_metadata_cost(snapshot: &DirectorySnapshot) -> usize {
    snapshot.path.as_os_str().as_encoded_bytes().len()
        + snapshot
            .entries
            .iter()
            .map(|entry| {
                entry.path.as_os_str().as_encoded_bytes().len()
                    + entry
                        .canonical_path
                        .as_deref()
                        .map_or(0, |path| path.as_os_str().as_encoded_bytes().len())
            })
            .sum::<usize>()
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WorkspaceBoundary {
    root: Box<Path>,
    files: Box<[FileSnapshot]>,
}

#[derive(Debug)]
pub(crate) struct TypeScriptProjectWitness {
    project_root: Box<Path>,
    home_root: Option<Box<Path>>,
    discovered_compiler: Box<Path>,
    discovered_module_root: Box<Path>,
    discovered_package_root: Box<Path>,
    discovered_version: Box<str>,
    compiler: Box<Path>,
    node: Box<Path>,
    module_root: Box<Path>,
    package_root: Box<Path>,
    workspace: Option<WorkspaceBoundary>,
    workspace_root: Box<Path>,
    files: Box<[FileSnapshot]>,
    typescript_files: Box<[TypeScriptFileInput]>,
    config_candidates: Box<[TypeScriptConfigInput]>,
    selected_build_config_paths: Box<[Box<Path>]>,
    config_paths: Box<[Box<Path>]>,
    loaded_source_files: std::sync::Mutex<std::collections::BTreeMap<PathBuf, FileSnapshot>>,
    fingerprint: [u8; 32],
    invocation_lease: TypeScriptProjectInvocationLease,
}

impl TypeScriptProjectWitness {
    fn invocation_lease(&self) -> &TypeScriptProjectInvocationLease {
        &self.invocation_lease
    }

    fn capture(
        project_root: &Path,
        home_root: Option<&Path>,
        discovered: &ProjectTypeScript,
        compiler: &Path,
        node: &Path,
        module_root: &Path,
        package_root: &Path,
        workspace: Option<&WorkspaceBoundary>,
    ) -> Result<Self, TypeScriptProjectHostError> {
        Self::capture_with_node_snapshot(
            project_root,
            home_root,
            discovered,
            compiler,
            node,
            module_root,
            package_root,
            workspace,
            None,
        )
    }

    fn capture_with_node_snapshot(
        project_root: &Path,
        home_root: Option<&Path>,
        discovered: &ProjectTypeScript,
        compiler: &Path,
        node: &Path,
        module_root: &Path,
        package_root: &Path,
        workspace: Option<&WorkspaceBoundary>,
        admitted_node_snapshot: Option<&FileSnapshot>,
    ) -> Result<Self, TypeScriptProjectHostError> {
        // Temporary directories on macOS commonly have both `/var/...` and
        // `/private/var/...` spellings. Keep every boundary in the same
        // canonical namespace as file snapshots so capability checks do not
        // mistake the alias for an escape.
        let project_root = canonical_directory(project_root)?;
        let module_root = canonical_directory(module_root)?;
        let package_root = fs::canonicalize(package_root).map_err(|source| {
            TypeScriptProjectHostError::PackagePath {
                path: package_root.to_path_buf().into_boxed_path(),
                source,
            }
        })?;
        let discovered_package_root = fs::canonicalize(discovered.module_root.join("typescript"))
            .map_err(|source| {
            TypeScriptProjectHostError::PackagePath {
                path: discovered.module_root.join("typescript").into_boxed_path(),
                source,
            }
        })?;
        let mut canonical_workspace = workspace.cloned();
        if let Some(workspace) = canonical_workspace.as_mut() {
            workspace.root = canonical_directory(&workspace.root)?.into_boxed_path();
        }
        let mut files = Vec::new();
        let node_snapshot = match admitted_node_snapshot {
            Some(snapshot) if snapshot.path.as_ref() == node => snapshot.clone(),
            Some(_) => {
                return Err(TypeScriptProjectHostError::WitnessChanged {
                    path: node.to_path_buf().into_boxed_path(),
                });
            }
            None => capture_file_snapshot(node, MAX_NODE_EXECUTABLE_BYTES)?,
        };
        files.push(node_snapshot);
        let typescript_files = collect_module_inputs(&package_root)?;
        files.extend(
            typescript_files
                .iter()
                .map(|input| snapshot_from_input(input)),
        );
        if !compiler.starts_with(&package_root) {
            files.push(capture_file_snapshot(compiler, MAX_NODE_EXECUTABLE_BYTES)?);
        }
        // Explicit selection may use another TypeScript installation. Keep
        // the discovered project closure witnessed without exposing it as the
        // selected compiler's inputs or expanding its source capability.
        if discovered_package_root != package_root {
            files.extend(
                collect_module_inputs(&discovered_package_root)?
                    .iter()
                    .map(snapshot_from_input),
            );
            if !discovered.compiler.starts_with(&discovered_package_root) {
                files.push(capture_file_snapshot(
                    &discovered.compiler,
                    MAX_NODE_EXECUTABLE_BYTES,
                )?);
            }
        }
        if let Some(workspace) = canonical_workspace.as_ref() {
            files.extend(workspace.files.iter().cloned());
        }
        let workspace_root = canonical_workspace
            .as_ref()
            .map(|workspace| workspace.root.to_path_buf())
            .unwrap_or_else(|| project_root.clone());
        let (selected_build_config_paths, angular_snapshot) =
            angular_build_config_paths(&workspace_root, &module_root)?;
        if let Some(snapshot) = angular_snapshot {
            files.push(snapshot);
        }
        let config_candidates = collect_project_configs(
            &project_root,
            &workspace_root,
            &module_root,
            &selected_build_config_paths,
        )?;
        files.extend(
            config_candidates
                .iter()
                .map(|input| snapshot_from_config(input)),
        );
        files.sort_unstable_by(|left, right| left.path.cmp(&right.path));
        files.dedup_by(|left, right| left.path == right.path);
        let fingerprint = witness_fingerprint(&files);
        let selected_snapshot = |path: &Path| {
            files
                .iter()
                .find(|snapshot| snapshot.path.as_ref() == path)
                .cloned()
                .ok_or_else(|| TypeScriptProjectHostError::WitnessChanged {
                    path: path.to_path_buf().into_boxed_path(),
                })
        };
        let compiler_snapshot = selected_snapshot(compiler)?;
        let node_snapshot = selected_snapshot(node)?;
        let package_metadata = fs::metadata(&package_root).map_err(|source| {
            TypeScriptProjectHostError::PackagePath {
                path: package_root.clone().into_boxed_path(),
                source,
            }
        })?;
        if !package_metadata.is_dir() {
            return Err(TypeScriptProjectHostError::RegularDirectoryRequired {
                path: package_root.clone().into_boxed_path(),
            });
        }
        let package_identity = file_identity(&package_metadata).map_err(|source| {
            TypeScriptProjectHostError::PackagePath {
                path: package_root.clone().into_boxed_path(),
                source,
            }
        })?;
        let module_closure_digest = module_files_digest(&typescript_files, &package_root)?;
        let invocation_lease = TypeScriptProjectInvocationLease {
            compiler_path: compiler.to_path_buf().into_boxed_path(),
            compiler_identity: compiler_snapshot.identity,
            compiler_digest: compiler_snapshot.digest,
            node_path: node.to_path_buf().into_boxed_path(),
            node_identity: node_snapshot.identity,
            node_digest: node_snapshot.digest,
            module_root: module_root.to_path_buf().into_boxed_path(),
            package_root: package_root.clone().into_boxed_path(),
            package_identity,
            module_closure_digest,
        };
        Ok(Self {
            project_root: project_root.into_boxed_path(),
            home_root: home_root.map(|root| root.to_path_buf().into_boxed_path()),
            discovered_compiler: discovered.compiler.to_path_buf().into_boxed_path(),
            discovered_module_root: discovered.module_root.to_path_buf().into_boxed_path(),
            discovered_package_root: discovered_package_root.into_boxed_path(),
            discovered_version: discovered.version.clone().into_boxed_str(),
            compiler: compiler.to_path_buf().into_boxed_path(),
            node: node.to_path_buf().into_boxed_path(),
            module_root: module_root.into_boxed_path(),
            package_root: package_root.into_boxed_path(),
            workspace: canonical_workspace,
            workspace_root: workspace_root.into_boxed_path(),
            files: files.into_boxed_slice(),
            typescript_files: typescript_files.into_boxed_slice(),
            config_paths: config_candidates
                .iter()
                .map(|config| config.path.clone())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            selected_build_config_paths: selected_build_config_paths
                .iter()
                .map(|path| path.clone().into_boxed_path())
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            config_candidates: config_candidates.into_boxed_slice(),
            loaded_source_files: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            fingerprint,
            invocation_lease,
        })
    }

    fn load_source(&self, path: &Path) -> Result<TypeScriptFileInput, TypeScriptProjectHostError> {
        if !path.is_absolute() {
            return Err(TypeScriptProjectHostError::SourceOutsideCapability {
                path: path.to_path_buf().into_boxed_path(),
            });
        }
        let canonical =
            fs::canonicalize(path).map_err(|source| TypeScriptProjectHostError::PackagePath {
                path: path.to_path_buf().into_boxed_path(),
                source,
            })?;
        if !canonical.starts_with(&self.workspace_root) && !canonical.starts_with(&self.module_root)
        {
            return Err(TypeScriptProjectHostError::SourceOutsideCapability {
                path: canonical.into_boxed_path(),
            });
        }
        let (snapshot, bytes) = read_regular_file(&canonical, MAX_SOURCE_FILE_BYTES)?;
        let mut loaded = self.loaded_source_files.lock().map_err(|_| {
            TypeScriptProjectHostError::WitnessLockPoisoned {
                path: canonical.clone().into_boxed_path(),
            }
        })?;
        if let Some(previous) = loaded.get(&canonical) {
            if previous != &snapshot {
                return Err(TypeScriptProjectHostError::WitnessChanged {
                    path: canonical.into_boxed_path(),
                });
            }
        } else {
            if loaded.len() >= MAX_RESOLVED_SOURCE_FILES {
                return Err(TypeScriptProjectHostError::ResolvedSourceLimit {
                    observed: loaded.len().saturating_add(1),
                    maximum: MAX_RESOLVED_SOURCE_FILES,
                });
            }
            let observed = loaded
                .values()
                .map(|file| file.length)
                .fold(snapshot.length, u64::saturating_add);
            if observed > MAX_RESOLVED_SOURCE_BYTES {
                return Err(TypeScriptProjectHostError::ResolvedSourceBytes {
                    observed,
                    maximum: MAX_RESOLVED_SOURCE_BYTES,
                });
            }
            loaded.insert(canonical.clone(), snapshot.clone());
        }
        Ok(TypeScriptFileInput {
            path: canonical.into_boxed_path(),
            content_id: ContentId::<SourceFactDomain>::from_canonical_bytes(&bytes),
            identity: snapshot.identity,
            bytes: bytes.into_boxed_slice(),
        })
    }

    fn path_is_admitted(&self, path: &Path) -> bool {
        path.starts_with(&self.workspace_root) || path.starts_with(&self.module_root)
    }

    fn admit_lexical_path(&self, path: &Path) -> Result<PathBuf, TypeScriptProjectHostError> {
        let Some(path) = normalize_absolute_path(path) else {
            return Err(TypeScriptProjectHostError::SourceOutsideCapability {
                path: path.to_path_buf().into_boxed_path(),
            });
        };
        if !self.path_is_admitted(&path) {
            return Err(TypeScriptProjectHostError::SourceOutsideCapability {
                path: path.into_boxed_path(),
            });
        }
        Ok(path)
    }

    pub(crate) fn validate_current(&self) -> Result<(), TypeScriptProjectHostError> {
        let current_project =
            match find_project_typescript_with_home(&self.project_root, self.home_root.as_deref())?
            {
                ProjectTypeScriptSearch::Found(project) => project,
                ProjectTypeScriptSearch::NotFound | ProjectTypeScriptSearch::Pnp(_) => {
                    return Err(TypeScriptProjectHostError::WitnessChanged {
                        path: self.project_root.to_path_buf().into_boxed_path(),
                    });
                }
            };
        let package_root = fs::canonicalize(current_project.module_root.join("typescript"))
            .map_err(|source| TypeScriptProjectHostError::PackagePath {
                path: current_project
                    .module_root
                    .join("typescript")
                    .into_boxed_path(),
                source,
            })?;
        if current_project.compiler.as_path() != self.discovered_compiler.as_ref()
            || current_project.module_root.as_path() != self.discovered_module_root.as_ref()
            || current_project.version.as_str() != self.discovered_version.as_ref()
            || package_root.as_path() != self.discovered_package_root.as_ref()
            || current_project.workspace != self.workspace
        {
            return Err(TypeScriptProjectHostError::WitnessChanged {
                path: self.project_root.to_path_buf().into_boxed_path(),
            });
        }

        let selected_package_root =
            fs::canonicalize(self.module_root.join("typescript")).map_err(|source| {
                TypeScriptProjectHostError::PackagePath {
                    path: self.module_root.join("typescript").into_boxed_path(),
                    source,
                }
            })?;
        if selected_package_root.as_path() != self.package_root.as_ref() {
            return Err(TypeScriptProjectHostError::WitnessChanged {
                path: self.module_root.join("typescript").into_boxed_path(),
            });
        }

        let current = Self::capture(
            &self.project_root,
            self.home_root.as_deref(),
            &current_project,
            &self.compiler,
            &self.node,
            &self.module_root,
            &self.package_root,
            self.workspace.as_ref(),
        )?;
        if current.files != self.files || current.fingerprint != self.fingerprint {
            let changed = first_changed_snapshot(&self.files, &current.files)
                .unwrap_or_else(|| self.package_root.to_path_buf());
            return Err(TypeScriptProjectHostError::WitnessChanged {
                path: changed.into_boxed_path(),
            });
        }
        if current.config_paths != self.config_paths
            || current.selected_build_config_paths != self.selected_build_config_paths
        {
            return Err(TypeScriptProjectHostError::WitnessChanged {
                path: self.workspace_root.to_path_buf().into_boxed_path(),
            });
        }
        let loaded = self.loaded_source_files.lock().map_err(|_| {
            TypeScriptProjectHostError::WitnessLockPoisoned {
                path: self.project_root.clone(),
            }
        })?;
        for (path, expected) in loaded.iter() {
            let observed = capture_file_snapshot(path, MAX_SOURCE_FILE_BYTES)?;
            if &observed != expected {
                return Err(TypeScriptProjectHostError::WitnessChanged {
                    path: path.clone().into_boxed_path(),
                });
            }
        }
        Ok(())
    }
}

fn normalize_absolute_path(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => normalized.push(std::path::MAIN_SEPARATOR_STR),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            std::path::Component::Normal(name) => normalized.push(name),
        }
    }
    normalized.is_absolute().then_some(normalized)
}

fn nearest_existing_admitted_parent(
    path: &Path,
    witness: &TypeScriptProjectWitness,
) -> Result<Option<PathBuf>, TypeScriptProjectHostError> {
    let mut candidate = path.parent();
    while let Some(parent) = candidate {
        match fs::symlink_metadata(parent) {
            Ok(_) => {
                if !fs::metadata(parent)
                    .map(|metadata| metadata.is_dir())
                    .unwrap_or(false)
                {
                    return Err(TypeScriptProjectHostError::RegularDirectoryRequired {
                        path: parent.to_path_buf().into_boxed_path(),
                    });
                }
                let canonical = fs::canonicalize(parent).map_err(|source| {
                    TypeScriptProjectHostError::PackagePath {
                        path: parent.to_path_buf().into_boxed_path(),
                        source,
                    }
                })?;
                if !witness.path_is_admitted(&canonical) {
                    return Err(TypeScriptProjectHostError::SourceOutsideCapability {
                        path: canonical.into_boxed_path(),
                    });
                }
                return Ok(Some(canonical));
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                candidate = parent.parent();
            }
            Err(source) => {
                return Err(TypeScriptProjectHostError::PackagePath {
                    path: parent.to_path_buf().into_boxed_path(),
                    source,
                });
            }
        }
    }
    Ok(None)
}

fn capture_directory_snapshot(
    path: &Path,
    witness: &TypeScriptProjectWitness,
) -> Result<DirectorySnapshot, TypeScriptProjectHostError> {
    let canonical =
        fs::canonicalize(path).map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: path.to_path_buf().into_boxed_path(),
            source,
        })?;
    if !witness.path_is_admitted(&canonical) {
        return Err(TypeScriptProjectHostError::SourceOutsideCapability {
            path: canonical.into_boxed_path(),
        });
    }
    let metadata =
        fs::metadata(&canonical).map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: canonical.clone().into_boxed_path(),
            source,
        })?;
    if !metadata.is_dir() {
        return Err(TypeScriptProjectHostError::RegularDirectoryRequired {
            path: canonical.into_boxed_path(),
        });
    }
    let identity =
        file_identity(&metadata).map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: canonical.clone().into_boxed_path(),
            source,
        })?;
    let mut entries = Vec::new();
    for entry in
        fs::read_dir(&canonical).map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: canonical.clone().into_boxed_path(),
            source,
        })?
    {
        let entry = entry.map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: canonical.clone().into_boxed_path(),
            source,
        })?;
        if entries.len() >= MAX_RESOLVER_DIRECTORY_ENTRIES {
            return Err(TypeScriptProjectHostError::ResolverDirectoryLimit {
                requested_depth: 0,
                requested_entries: entries.len().saturating_add(1),
                maximum_depth: MAX_RESOLVER_DEPTH,
                maximum_entries: MAX_RESOLVER_DIRECTORY_ENTRIES,
            });
        }
        let entry_path = entry.path();
        let entry_metadata = fs::symlink_metadata(&entry_path).map_err(|source| {
            TypeScriptProjectHostError::PackagePath {
                path: entry_path.clone().into_boxed_path(),
                source,
            }
        })?;
        let entry_identity = file_identity(&entry_metadata).map_err(|source| {
            TypeScriptProjectHostError::PackagePath {
                path: entry_path.clone().into_boxed_path(),
                source,
            }
        })?;
        let kind = if entry_metadata.file_type().is_symlink() {
            TypeScriptDirectoryEntryKind::Symlink
        } else if entry_metadata.is_dir() {
            TypeScriptDirectoryEntryKind::Directory
        } else if entry_metadata.is_file() {
            TypeScriptDirectoryEntryKind::RegularFile
        } else {
            TypeScriptDirectoryEntryKind::Other
        };
        let canonical_path = match fs::canonicalize(&entry_path) {
            Ok(path) => Some(path.into_boxed_path()),
            Err(source) if source.kind() == io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(TypeScriptProjectHostError::PackagePath {
                    path: entry_path.into_boxed_path(),
                    source,
                });
            }
        };
        entries.push(TypeScriptDirectoryEntry {
            path: entry_path.into_boxed_path(),
            canonical_path,
            kind,
            identity: entry_identity,
        });
    }
    entries.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    Ok(DirectorySnapshot {
        path: canonical.into_boxed_path(),
        identity,
        entries: entries.into_boxed_slice(),
    })
}

fn extension_matches(path: &Path, extensions: &[&str]) -> bool {
    if extensions.is_empty() {
        return true;
    }
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| {
            extensions
                .iter()
                .any(|candidate| candidate.trim_start_matches('.') == extension)
        })
}

fn resolver_observation_ref_path<'a>(
    observation: &TypeScriptResolverObservationRef<'a>,
) -> &'a Path {
    match observation {
        TypeScriptResolverObservationRef::Source(input) => &input.path,
        TypeScriptResolverObservationRef::MissingPath { path, .. }
        | TypeScriptResolverObservationRef::Directory { path, .. }
        | TypeScriptResolverObservationRef::Realpath { path, .. } => path,
    }
}

fn resolver_observation_ref_tag(observation: &TypeScriptResolverObservationRef<'_>) -> u8 {
    match observation {
        TypeScriptResolverObservationRef::Source(_) => 1,
        TypeScriptResolverObservationRef::MissingPath { .. } => 2,
        TypeScriptResolverObservationRef::Directory { .. } => 3,
        TypeScriptResolverObservationRef::Realpath { .. } => 4,
    }
}

fn update_len(digest: &mut Hasher, length: usize) {
    digest.update(&u64::try_from(length).unwrap_or(u64::MAX).to_le_bytes());
}

fn update_logical_path(
    digest: &mut Hasher,
    witness: &TypeScriptProjectWitness,
    path: &Path,
) -> Result<(), TypeScriptProjectHostError> {
    let (root_tag, root) = if path.starts_with(witness.workspace_root.as_ref()) {
        (1_u8, witness.workspace_root.as_ref())
    } else if path.starts_with(witness.module_root.as_ref()) {
        (2_u8, witness.module_root.as_ref())
    } else {
        return Err(TypeScriptProjectHostError::SourceOutsideCapability {
            path: path.to_path_buf().into_boxed_path(),
        });
    };
    let relative = path.strip_prefix(root).map_err(|_| {
        TypeScriptProjectHostError::SourceOutsideCapability {
            path: path.to_path_buf().into_boxed_path(),
        }
    })?;
    let mut logical = String::new();
    for component in relative.components() {
        let component = component.as_os_str().to_str().ok_or_else(|| {
            TypeScriptProjectHostError::NonPortablePath {
                path: path.to_path_buf().into_boxed_path(),
            }
        })?;
        if !logical.is_empty() {
            logical.push('/');
        }
        logical.push_str(component);
    }
    digest.update(&[root_tag]);
    update_len(digest, logical.len());
    digest.update(logical.as_bytes());
    Ok(())
}

fn snapshot_from_input(input: &TypeScriptFileInput) -> FileSnapshot {
    FileSnapshot {
        path: input.path.clone(),
        identity: input.identity,
        length: u64::try_from(input.bytes.len()).unwrap_or(u64::MAX),
        digest: *blake3::hash(&input.bytes).as_bytes(),
    }
}

fn snapshot_from_config(input: &TypeScriptConfigInput) -> FileSnapshot {
    FileSnapshot {
        path: input.path.clone(),
        identity: input.identity,
        length: u64::try_from(input.bytes.len()).unwrap_or(u64::MAX),
        digest: *blake3::hash(&input.bytes).as_bytes(),
    }
}

fn first_changed_snapshot(expected: &[FileSnapshot], observed: &[FileSnapshot]) -> Option<PathBuf> {
    for (expected, observed) in expected.iter().zip(observed) {
        if expected != observed {
            return Some(expected.path.to_path_buf());
        }
    }
    expected
        .get(observed.len())
        .or_else(|| observed.get(expected.len()))
        .map(|snapshot| snapshot.path.to_path_buf())
}

fn module_files_digest(
    inputs: &[TypeScriptFileInput],
    package_root: &Path,
) -> Result<[u8; 32], TypeScriptProjectHostError> {
    let mut files = Vec::with_capacity(inputs.len());
    for input in inputs {
        let relative = input.path.strip_prefix(package_root).map_err(|_| {
            TypeScriptProjectHostError::WitnessChanged {
                path: input.path.clone(),
            }
        })?;
        files.push((
            relative.to_path_buf(),
            u64::try_from(input.bytes.len()).unwrap_or(u64::MAX),
            *blake3::hash(&input.bytes).as_bytes(),
        ));
    }
    crate::application::typescript_module_files_digest(
        files
            .iter()
            .map(|(path, length, digest)| (path.as_path(), *length, *digest)),
    )
    .map_err(|_| TypeScriptProjectHostError::WitnessChanged {
        path: package_root.to_path_buf().into_boxed_path(),
    })
}

fn witness_fingerprint(files: &[FileSnapshot]) -> [u8; 32] {
    let mut digest = Hasher::new();
    digest.update(b"compiler.typescript.immutable-files.v1\0");
    for file in files {
        let path = file.path.as_os_str().as_encoded_bytes();
        digest.update(&(path.len() as u64).to_be_bytes());
        digest.update(path);
        digest.update(&file.identity.first.to_be_bytes());
        digest.update(&file.identity.second.to_be_bytes());
        digest.update(&file.length.to_be_bytes());
        digest.update(&file.digest);
    }
    *digest.finalize().as_bytes()
}

fn canonical_directory(path: &Path) -> Result<PathBuf, TypeScriptProjectHostError> {
    let canonical =
        fs::canonicalize(path).map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: path.to_path_buf().into_boxed_path(),
            source,
        })?;
    let metadata =
        fs::metadata(&canonical).map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: canonical.clone().into_boxed_path(),
            source,
        })?;
    if !metadata.is_dir() {
        return Err(TypeScriptProjectHostError::RegularDirectoryRequired {
            path: canonical.into_boxed_path(),
        });
    }
    Ok(canonical)
}

fn capture_file_snapshot(
    path: &Path,
    maximum_bytes: u64,
) -> Result<FileSnapshot, TypeScriptProjectHostError> {
    let (snapshot, _) = read_regular_file(path, maximum_bytes)?;
    Ok(snapshot)
}

fn read_regular_file(
    path: &Path,
    maximum_bytes: u64,
) -> Result<(FileSnapshot, Vec<u8>), TypeScriptProjectHostError> {
    #[cfg(unix)]
    let mut file = {
        use rustix::fs::{Mode, OFlags, open};
        use std::os::fd::OwnedFd;

        let descriptor: OwnedFd = open(
            path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: path.to_path_buf().into_boxed_path(),
            source: io::Error::from(source),
        })?;
        File::from(descriptor)
    };
    #[cfg(not(unix))]
    let mut file = {
        let before = fs::symlink_metadata(path).map_err(|source| {
            TypeScriptProjectHostError::PackagePath {
                path: path.to_path_buf().into_boxed_path(),
                source,
            }
        })?;
        if !before.file_type().is_file() {
            return Err(TypeScriptProjectHostError::RegularFileRequired {
                path: path.to_path_buf().into_boxed_path(),
            });
        }
        File::open(path).map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: path.to_path_buf().into_boxed_path(),
            source,
        })?
    };

    let before = file
        .metadata()
        .map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: path.to_path_buf().into_boxed_path(),
            source,
        })?;
    if !before.file_type().is_file() {
        return Err(TypeScriptProjectHostError::RegularFileRequired {
            path: path.to_path_buf().into_boxed_path(),
        });
    }
    if before.len() > maximum_bytes {
        return Err(TypeScriptProjectHostError::FileTooLarge {
            path: path.to_path_buf().into_boxed_path(),
            observed: before.len(),
            maximum: maximum_bytes,
        });
    }
    let identity =
        file_identity(&before).map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: path.to_path_buf().into_boxed_path(),
            source,
        })?;
    let reserve = usize::try_from(before.len()).unwrap_or(usize::MAX);
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(reserve).map_err(|source| {
        TypeScriptProjectHostError::FileAllocation {
            path: path.to_path_buf().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        }
    })?;
    let mut limited = (&mut file).take(maximum_bytes.saturating_add(1));
    limited
        .read_to_end(&mut bytes)
        .map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: path.to_path_buf().into_boxed_path(),
            source,
        })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum_bytes {
        return Err(TypeScriptProjectHostError::FileTooLarge {
            path: path.to_path_buf().into_boxed_path(),
            observed: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            maximum: maximum_bytes,
        });
    }
    let after = file
        .metadata()
        .map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: path.to_path_buf().into_boxed_path(),
            source,
        })?;
    if identity
        != file_identity(&after).map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: path.to_path_buf().into_boxed_path(),
            source,
        })?
        || after.len() != u64::try_from(bytes.len()).unwrap_or(u64::MAX)
    {
        return Err(TypeScriptProjectHostError::WitnessChanged {
            path: path.to_path_buf().into_boxed_path(),
        });
    }
    let snapshot = FileSnapshot {
        path: path.to_path_buf().into_boxed_path(),
        identity,
        length: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        digest: *blake3::hash(&bytes).as_bytes(),
    };
    Ok((snapshot, bytes))
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt;

    Ok(FileIdentity {
        first: metadata.dev(),
        second: metadata.ino(),
    })
}

#[cfg(windows)]
fn file_identity(metadata: &fs::Metadata) -> io::Result<FileIdentity> {
    use std::os::windows::fs::MetadataExt;

    Ok(FileIdentity {
        first: u64::from(metadata.volume_serial_number().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "file volume identity is unavailable",
            )
        })?),
        second: metadata.file_index().ok_or_else(|| {
            io::Error::new(io::ErrorKind::Unsupported, "file index is unavailable")
        })?,
    })
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_metadata: &fs::Metadata) -> io::Result<FileIdentity> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this platform cannot provide stable file identity",
    ))
}

fn collect_module_inputs(
    package_root: &Path,
) -> Result<Vec<TypeScriptFileInput>, TypeScriptProjectHostError> {
    let package_root = fs::canonicalize(package_root).map_err(|source| {
        TypeScriptProjectHostError::PackagePath {
            path: package_root.to_path_buf().into_boxed_path(),
            source,
        }
    })?;
    let mut paths = Vec::new();
    collect_regular_module_paths(&package_root, &mut paths)?;
    paths.sort_unstable();
    if paths.len() > MAX_TYPESCRIPT_PACKAGE_FILES {
        return Err(TypeScriptProjectHostError::ModuleFileLimit {
            observed: paths.len(),
            maximum: MAX_TYPESCRIPT_PACKAGE_FILES,
        });
    }
    let mut total_bytes = 0_u64;
    let mut output = Vec::new();
    for path in paths {
        let remaining = MAX_TYPESCRIPT_PACKAGE_BYTES.saturating_sub(total_bytes);
        let (snapshot, bytes) = read_regular_file(&path, remaining)?;
        total_bytes = total_bytes.saturating_add(snapshot.length);
        output.push(TypeScriptFileInput {
            path: snapshot.path,
            content_id: ContentId::<SourceFactDomain>::from_canonical_bytes(&bytes),
            identity: snapshot.identity,
            bytes: bytes.into_boxed_slice(),
        });
    }
    Ok(output)
}

fn collect_project_configs(
    project_root: &Path,
    workspace_root: &Path,
    module_root: &Path,
    selected_build_configs: &[PathBuf],
) -> Result<Vec<TypeScriptConfigInput>, TypeScriptProjectHostError> {
    let project_root = canonical_directory(project_root)?;
    let workspace_root = canonical_directory(workspace_root)?;
    let module_root = canonical_directory(module_root)?;
    let mut paths = Vec::new();
    collect_tsconfig_paths(&workspace_root, &mut paths, 0)?;
    paths.extend(selected_build_configs.iter().cloned());
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    paths.sort_unstable();
    paths.dedup();
    if paths.len() > MAX_PROJECT_CONFIG_FILES {
        return Err(TypeScriptProjectHostError::ConfigFileLimit {
            observed: paths.len(),
            maximum: MAX_PROJECT_CONFIG_FILES,
        });
    }

    let mut candidate_paths = std::collections::BTreeSet::new();
    for path in &paths {
        candidate_paths.insert(canonical_regular_config(
            path,
            &workspace_root,
            &module_root,
        )?);
    }
    let mut selected_paths = std::collections::BTreeSet::new();
    for path in selected_build_configs {
        selected_paths.insert(canonical_regular_config(
            path,
            &workspace_root,
            &module_root,
        )?);
    }
    let mut pending = paths;
    let mut captured = std::collections::BTreeMap::<PathBuf, TypeScriptConfigInput>::new();
    let mut total_bytes = 0_u64;
    while let Some(path) = pending.pop() {
        if captured.contains_key(&path) {
            continue;
        }
        if captured.len() >= MAX_PROJECT_CONFIG_FILES {
            return Err(TypeScriptProjectHostError::ConfigFileLimit {
                observed: captured.len().saturating_add(1),
                maximum: MAX_PROJECT_CONFIG_FILES,
            });
        }
        let canonical = canonical_regular_config(&path, &workspace_root, &module_root)?;
        if captured.contains_key(&canonical) {
            continue;
        }
        let remaining = MAX_PROJECT_CONFIG_BYTES.saturating_sub(total_bytes);
        let (snapshot, bytes) = read_regular_file(&canonical, remaining)?;
        total_bytes = total_bytes.saturating_add(snapshot.length);
        let value = parse_typescript_config(&canonical, &bytes)?;
        let extends = config_paths_from_field(
            &value,
            "extends",
            &canonical,
            &project_root,
            &workspace_root,
            &module_root,
            false,
        )?;
        let references = config_paths_from_field(
            &value,
            "references",
            &canonical,
            &project_root,
            &workspace_root,
            &module_root,
            true,
        )?;
        pending.extend(extends.iter().cloned());
        pending.extend(references.iter().cloned());
        captured.insert(
            canonical.clone(),
            TypeScriptConfigInput {
                path: canonical.clone().into_boxed_path(),
                bytes: bytes.clone().into_boxed_slice(),
                content_id: ContentId::<SourceFactDomain>::from_canonical_bytes(&bytes),
                identity: snapshot.identity,
                project_candidate: candidate_paths.contains(&canonical),
                selected_build_config: selected_paths.contains(&canonical),
                extends: extends
                    .into_iter()
                    .map(PathBuf::into_boxed_path)
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
                references: references
                    .into_iter()
                    .map(PathBuf::into_boxed_path)
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
            },
        );
    }
    validate_config_graph_acyclic(&captured)?;
    Ok(captured.into_values().collect())
}

fn validate_config_graph_acyclic(
    configs: &std::collections::BTreeMap<PathBuf, TypeScriptConfigInput>,
) -> Result<(), TypeScriptProjectHostError> {
    fn visit(
        path: &Path,
        configs: &std::collections::BTreeMap<PathBuf, TypeScriptConfigInput>,
        visiting: &mut std::collections::BTreeSet<PathBuf>,
        visited: &mut std::collections::BTreeSet<PathBuf>,
    ) -> Result<(), TypeScriptProjectHostError> {
        if visited.contains(path) {
            return Ok(());
        }
        if !visiting.insert(path.to_path_buf()) {
            return Err(TypeScriptProjectHostError::ConfigCycle {
                config: path.to_path_buf().into_boxed_path(),
            });
        }
        if let Some(config) = configs.get(path) {
            for dependency in config.extends.iter().chain(config.references.iter()) {
                visit(dependency, configs, visiting, visited)?;
            }
        }
        visiting.remove(path);
        visited.insert(path.to_path_buf());
        Ok(())
    }

    let mut visiting = std::collections::BTreeSet::new();
    let mut visited = std::collections::BTreeSet::new();
    for path in configs.keys() {
        visit(path, configs, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn angular_build_config_paths(
    workspace_root: &Path,
    module_root: &Path,
) -> Result<(Vec<PathBuf>, Option<FileSnapshot>), TypeScriptProjectHostError> {
    let workspace_root = canonical_directory(workspace_root)?;
    let module_root = canonical_directory(module_root)?;
    let path = workspace_root.join("angular.json");
    match fs::symlink_metadata(&path) {
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok((Vec::new(), None)),
        Err(source) => {
            return Err(TypeScriptProjectHostError::PackagePath {
                path: path.into_boxed_path(),
                source,
            });
        }
        Ok(_) => {}
    }
    let (snapshot, bytes) = read_regular_file(&path, MAX_ANGULAR_WORKSPACE_BYTES)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|source| {
        TypeScriptProjectHostError::ConfigInvalid {
            config: path.clone().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        }
    })?;
    let Some(projects) = value.get("projects") else {
        return Err(TypeScriptProjectHostError::ConfigInvalid {
            config: path.into_boxed_path(),
            message: "Angular workspace must contain a projects object".into(),
        });
    };
    let Some(projects) = projects.as_object() else {
        return Err(TypeScriptProjectHostError::ConfigInvalid {
            config: path.into_boxed_path(),
            message: "Angular workspace projects must be an object".into(),
        });
    };
    let mut selected = Vec::new();
    for project in projects.values() {
        for targets_key in ["architect", "targets"] {
            if let Some(targets) = project
                .get(targets_key)
                .and_then(serde_json::Value::as_object)
                && let Some(build) = targets.get("build")
            {
                collect_angular_tsconfig(build, &mut selected, &path)?;
            }
        }
    }
    let mut resolved = Vec::new();
    for raw in selected {
        let candidate = workspace_root.join(raw);
        resolved.push(resolve_relative_config(
            &candidate,
            &workspace_root,
            &module_root,
        )?);
    }
    resolved.sort_unstable();
    resolved.dedup();
    Ok((resolved, Some(snapshot)))
}

fn collect_angular_tsconfig(
    target: &serde_json::Value,
    output: &mut Vec<String>,
    workspace: &Path,
) -> Result<(), TypeScriptProjectHostError> {
    if let Some(options) = target.get("options") {
        if !options.is_object() {
            return Err(TypeScriptProjectHostError::ConfigInvalid {
                config: workspace.to_path_buf().into_boxed_path(),
                message: "Angular build options must be an object".into(),
            });
        }
        if let Some(value) = options.get("tsConfig") {
            let value =
                value
                    .as_str()
                    .ok_or_else(|| TypeScriptProjectHostError::ConfigInvalid {
                        config: workspace.to_path_buf().into_boxed_path(),
                        message: "Angular build tsConfig must be a string".into(),
                    })?;
            output.push(value.to_owned());
        }
    }
    if let Some(configurations) = target
        .get("configurations")
        .and_then(serde_json::Value::as_object)
    {
        for configuration in configurations.values() {
            if let Some(value) = configuration.get("tsConfig") {
                let value =
                    value
                        .as_str()
                        .ok_or_else(|| TypeScriptProjectHostError::ConfigInvalid {
                            config: workspace.to_path_buf().into_boxed_path(),
                            message: "Angular build configuration tsConfig must be a string".into(),
                        })?;
                output.push(value.to_owned());
            }
        }
    }
    Ok(())
}

fn collect_tsconfig_paths(
    directory: &Path,
    output: &mut Vec<PathBuf>,
    depth: usize,
) -> Result<(), TypeScriptProjectHostError> {
    if depth > MAX_PROJECT_ANCESTORS * 2 {
        return Err(TypeScriptProjectHostError::ConfigDirectoryDepth {
            directory: directory.to_path_buf().into_boxed_path(),
        });
    }
    for entry in
        fs::read_dir(directory).map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: directory.to_path_buf().into_boxed_path(),
            source,
        })?
    {
        let entry = entry.map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: directory.to_path_buf().into_boxed_path(),
            source,
        })?;
        let path = entry.path();
        let name = entry.file_name();
        if name == "node_modules"
            || name == ".git"
            || name == ".yarn"
            || name == "dist"
            || name == "build"
            || name == "target"
        {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).map_err(|source| {
            TypeScriptProjectHostError::PackagePath {
                path: path.clone().into_boxed_path(),
                source,
            }
        })?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            collect_tsconfig_paths(&path, output, depth + 1)?;
        } else if metadata.is_file() {
            let file_name = name.to_string_lossy();
            if file_name == "tsconfig.json"
                || file_name.starts_with("tsconfig.") && file_name.ends_with(".json")
            {
                output.push(path);
                if output.len() > MAX_PROJECT_CONFIG_FILES {
                    return Err(TypeScriptProjectHostError::ConfigFileLimit {
                        observed: output.len(),
                        maximum: MAX_PROJECT_CONFIG_FILES,
                    });
                }
            }
        }
    }
    Ok(())
}

fn canonical_regular_config(
    path: &Path,
    workspace_root: &Path,
    module_root: &Path,
) -> Result<PathBuf, TypeScriptProjectHostError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: path.to_path_buf().into_boxed_path(),
            source,
        })?;
    if !metadata.file_type().is_file() {
        return Err(TypeScriptProjectHostError::RegularFileRequired {
            path: path.to_path_buf().into_boxed_path(),
        });
    }
    let canonical =
        fs::canonicalize(path).map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: path.to_path_buf().into_boxed_path(),
            source,
        })?;
    if !canonical.starts_with(workspace_root) && !canonical.starts_with(module_root) {
        return Err(TypeScriptProjectHostError::ConfigEscapesBoundary {
            config: canonical.into_boxed_path(),
            boundary: workspace_root.to_path_buf().into_boxed_path(),
        });
    }
    Ok(canonical)
}

fn parse_typescript_config(
    path: &Path,
    bytes: &[u8],
) -> Result<serde_json::Value, TypeScriptProjectHostError> {
    let text =
        std::str::from_utf8(bytes).map_err(|source| TypeScriptProjectHostError::ConfigInvalid {
            config: path.to_path_buf().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        })?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let stripped = strip_jsonc(text).ok_or_else(|| TypeScriptProjectHostError::ConfigInvalid {
        config: path.to_path_buf().into_boxed_path(),
        message: "unterminated string or comment".into(),
    })?;
    let value: serde_json::Value = serde_json::from_str(&stripped).map_err(|source| {
        TypeScriptProjectHostError::ConfigInvalid {
            config: path.to_path_buf().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        }
    })?;
    if !value.is_object() {
        return Err(TypeScriptProjectHostError::ConfigInvalid {
            config: path.to_path_buf().into_boxed_path(),
            message: "configuration root must be an object".into(),
        });
    }
    Ok(value)
}

fn strip_jsonc(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            output.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
        } else if byte == b'"' {
            in_string = true;
            output.push(byte);
            index += 1;
        } else if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            output.extend_from_slice(b"  ");
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                output.push(b' ');
                index += 1;
            }
        } else if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            output.extend_from_slice(b"  ");
            index += 2;
            let mut closed = false;
            while index < bytes.len() {
                if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                    output.extend_from_slice(b"  ");
                    index += 2;
                    closed = true;
                    break;
                }
                output.push(if bytes[index] == b'\n' { b'\n' } else { b' ' });
                index += 1;
            }
            if !closed {
                return None;
            }
        } else {
            output.push(byte);
            index += 1;
        }
    }
    if in_string {
        return None;
    }
    let mut compact = Vec::with_capacity(output.len());
    let mut next_significant = vec![0_u8; output.len()];
    let mut next = 0_u8;
    for index in (0..output.len()).rev() {
        if !output[index].is_ascii_whitespace() {
            next = output[index];
        }
        next_significant[index] = next;
    }
    let mut in_string = false;
    let mut escaped = false;
    let mut index = 0;
    while index < output.len() {
        let byte = output[index];
        if in_string {
            compact.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
        } else if byte == b'"' {
            in_string = true;
            compact.push(byte);
            index += 1;
        } else if byte == b','
            && next_significant
                .get(index + 1)
                .is_some_and(|next| *next == b'}' || *next == b']')
        {
            index += 1;
        } else {
            compact.push(byte);
            index += 1;
        }
    }
    String::from_utf8(compact).ok()
}

fn config_paths_from_field(
    value: &serde_json::Value,
    field: &str,
    source: &Path,
    project_root: &Path,
    workspace_root: &Path,
    module_root: &Path,
    references: bool,
) -> Result<Vec<PathBuf>, TypeScriptProjectHostError> {
    let Some(field_value) = value.get(field) else {
        return Ok(Vec::new());
    };
    let strings = if references {
        let entries =
            field_value
                .as_array()
                .ok_or_else(|| TypeScriptProjectHostError::ConfigInvalid {
                    config: source.to_path_buf().into_boxed_path(),
                    message: "references must be an array".into(),
                })?;
        entries
            .iter()
            .map(|entry| {
                entry
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| TypeScriptProjectHostError::ConfigInvalid {
                        config: source.to_path_buf().into_boxed_path(),
                        message: "each reference must contain a string path".into(),
                    })
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        match field_value {
            serde_json::Value::String(value) => vec![value.as_str()],
            serde_json::Value::Array(values) => values
                .iter()
                .map(|entry| {
                    entry
                        .as_str()
                        .ok_or_else(|| TypeScriptProjectHostError::ConfigInvalid {
                            config: source.to_path_buf().into_boxed_path(),
                            message: "extends entries must be strings".into(),
                        })
                })
                .collect::<Result<Vec<_>, _>>()?,
            _ => {
                return Err(TypeScriptProjectHostError::ConfigInvalid {
                    config: source.to_path_buf().into_boxed_path(),
                    message: "extends must be a string or an array of strings".into(),
                });
            }
        }
    };
    let mut resolved = Vec::new();
    for raw in strings {
        if references || raw.starts_with('.') || Path::new(raw).is_absolute() {
            let candidate = if Path::new(raw).is_absolute() {
                PathBuf::from(raw)
            } else {
                source.parent().unwrap_or(project_root).join(raw)
            };
            let config = resolve_relative_config(&candidate, workspace_root, module_root)?;
            resolved.push(config);
        } else {
            let config = match resolve_package_config(raw, source, workspace_root, module_root)? {
                Some(config) => Some(config),
                None => resolve_config_from_module_root(raw, workspace_root, module_root)?,
            }
            .ok_or_else(|| TypeScriptProjectHostError::ConfigMissing {
                config: PathBuf::from(raw).into_boxed_path(),
            })?;
            resolved.push(config);
        }
    }
    Ok(resolved)
}

fn resolve_config_from_module_root(
    raw: &str,
    workspace_root: &Path,
    module_root: &Path,
) -> Result<Option<PathBuf>, TypeScriptProjectHostError> {
    let candidate = module_root.join(raw);
    for choice in [
        candidate.clone(),
        candidate.with_extension("json"),
        candidate.join("tsconfig.json"),
    ] {
        match fs::symlink_metadata(&choice) {
            Ok(metadata) if metadata.file_type().is_file() => {
                return canonical_regular_config(&choice, workspace_root, module_root).map(Some);
            }
            Ok(_) => continue,
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(TypeScriptProjectHostError::PackagePath {
                    path: choice.into_boxed_path(),
                    source,
                });
            }
        }
    }
    Ok(None)
}

fn resolve_relative_config(
    candidate: &Path,
    workspace_root: &Path,
    module_root: &Path,
) -> Result<PathBuf, TypeScriptProjectHostError> {
    let choices = if candidate.is_dir() {
        vec![candidate.join("tsconfig.json")]
    } else if candidate.extension().is_none() {
        vec![
            candidate.with_extension("json"),
            candidate.join("tsconfig.json"),
            candidate.to_path_buf(),
        ]
    } else {
        vec![candidate.to_path_buf()]
    };
    for choice in choices {
        if fs::symlink_metadata(&choice).is_ok_and(|metadata| metadata.file_type().is_file()) {
            return canonical_regular_config(&choice, workspace_root, module_root);
        }
    }
    Err(TypeScriptProjectHostError::ConfigMissing {
        config: candidate.to_path_buf().into_boxed_path(),
    })
}

fn resolve_package_config(
    raw: &str,
    source: &Path,
    workspace_root: &Path,
    module_root: &Path,
) -> Result<Option<PathBuf>, TypeScriptProjectHostError> {
    let mut ancestor = source.parent();
    while let Some(directory) = ancestor {
        let node_modules = directory.join("node_modules");
        if node_modules.starts_with(workspace_root) || node_modules == module_root {
            let candidate = node_modules.join(raw);
            for choice in [
                candidate.clone(),
                candidate.with_extension("json"),
                candidate.join("tsconfig.json"),
            ] {
                if fs::symlink_metadata(&choice)
                    .is_ok_and(|metadata| metadata.file_type().is_file())
                {
                    return canonical_regular_config(&choice, workspace_root, module_root)
                        .map(Some);
                }
            }
        }
        if directory == workspace_root {
            break;
        }
        ancestor = directory.parent();
    }
    Ok(None)
}

fn collect_regular_module_paths(
    directory: &Path,
    output: &mut Vec<PathBuf>,
) -> Result<(), TypeScriptProjectHostError> {
    let metadata = fs::symlink_metadata(directory).map_err(|source| {
        TypeScriptProjectHostError::PackagePath {
            path: directory.to_path_buf().into_boxed_path(),
            source,
        }
    })?;
    if !metadata.is_dir() {
        return Err(TypeScriptProjectHostError::RegularDirectoryRequired {
            path: directory.to_path_buf().into_boxed_path(),
        });
    }
    for entry in
        fs::read_dir(directory).map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: directory.to_path_buf().into_boxed_path(),
            source,
        })?
    {
        let entry = entry.map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: directory.to_path_buf().into_boxed_path(),
            source,
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path).map_err(|source| {
            TypeScriptProjectHostError::PackagePath {
                path: path.clone().into_boxed_path(),
                source,
            }
        })?;
        if metadata.file_type().is_symlink() {
            return Err(TypeScriptProjectHostError::ModuleEntrySymlink {
                path: path.into_boxed_path(),
            });
        }
        if metadata.is_dir() {
            collect_regular_module_paths(&path, output)?;
        } else if metadata.is_file() {
            output.push(path);
        } else {
            return Err(TypeScriptProjectHostError::RegularFileRequired {
                path: path.into_boxed_path(),
            });
        }
        if output.len() > MAX_TYPESCRIPT_PACKAGE_FILES {
            return Err(TypeScriptProjectHostError::ModuleFileLimit {
                observed: output.len(),
                maximum: MAX_TYPESCRIPT_PACKAGE_FILES,
            });
        }
    }
    Ok(())
}

/// Closed host inputs used to admit TypeScript projects after package-root selection.
#[derive(Clone, Debug)]
pub struct TypeScriptProjectHost {
    explicit_compiler: Option<Box<Path>>,
    node: Option<Box<Path>>,
    node_origin: Option<TypeScriptSelectionOrigin>,
    home_root: Option<Box<Path>>,
    explicit_module_root: Option<Box<Path>>,
    report_program: Option<Box<Path>>,
    probe_limits: ToolchainProbeLimits,
    node_identity: std::sync::Arc<std::sync::Mutex<Option<NodeRuntimeIdentity>>>,
}

#[derive(Clone, Debug)]
struct NodeRuntimeIdentity {
    path: Box<Path>,
    version: Box<[u8]>,
    snapshot: FileSnapshot,
}

impl TypeScriptProjectHost {
    pub(crate) fn new(
        explicit_compiler: Option<PathBuf>,
        node: Option<PathBuf>,
        explicit_module_root: Option<PathBuf>,
        report_program: Option<PathBuf>,
        probe_limits: ToolchainProbeLimits,
    ) -> Self {
        let node_origin = node
            .as_ref()
            .map(|_| TypeScriptSelectionOrigin::ExplicitConfiguration);
        Self::new_with_node_origin(
            explicit_compiler,
            node,
            node_origin,
            None,
            explicit_module_root,
            report_program,
            probe_limits,
        )
    }

    pub(crate) fn new_with_node_origin(
        explicit_compiler: Option<PathBuf>,
        node: Option<PathBuf>,
        node_origin: Option<TypeScriptSelectionOrigin>,
        home_root: Option<PathBuf>,
        explicit_module_root: Option<PathBuf>,
        report_program: Option<PathBuf>,
        probe_limits: ToolchainProbeLimits,
    ) -> Self {
        Self {
            explicit_compiler: explicit_compiler.map(PathBuf::into_boxed_path),
            node: node.map(PathBuf::into_boxed_path),
            node_origin,
            home_root: home_root.map(PathBuf::into_boxed_path),
            explicit_module_root: explicit_module_root.map(PathBuf::into_boxed_path),
            report_program: report_program.map(PathBuf::into_boxed_path),
            probe_limits,
            node_identity: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    fn admit_node_runtime(
        &self,
        node: &Path,
    ) -> Result<NodeRuntimeIdentity, TypeScriptProjectHostError> {
        let mut cached = self.node_identity.lock().map_err(|_| {
            TypeScriptProjectHostError::WitnessLockPoisoned {
                path: node.to_path_buf().into_boxed_path(),
            }
        })?;
        if let Some(identity) = cached.as_ref() {
            if identity.path.as_ref() != node {
                return Err(TypeScriptProjectHostError::WitnessChanged {
                    path: node.to_path_buf().into_boxed_path(),
                });
            }
            let current = capture_file_snapshot(node, MAX_NODE_EXECUTABLE_BYTES)?;
            if current != identity.snapshot {
                return Err(TypeScriptProjectHostError::WitnessChanged {
                    path: node.to_path_buf().into_boxed_path(),
                });
            }
            return Ok(identity.clone());
        }

        let before = capture_file_snapshot(node, MAX_NODE_EXECUTABLE_BYTES)?;
        let version = crate::application::toolchain_probe::probe_command(
            NativeTool::TypeScriptCompiler,
            node,
            &["--version"],
            self.probe_limits,
        )
        .map_err(|source| TypeScriptProjectHostError::NodeProbe {
            node: node.to_path_buf().into_boxed_path(),
            source,
        })?;
        let after = capture_file_snapshot(node, MAX_NODE_EXECUTABLE_BYTES)?;
        if before != after {
            return Err(TypeScriptProjectHostError::WitnessChanged {
                path: node.to_path_buf().into_boxed_path(),
            });
        }
        let identity = NodeRuntimeIdentity {
            path: node.to_path_buf().into_boxed_path(),
            version,
            snapshot: after,
        };
        *cached = Some(identity.clone());
        Ok(identity)
    }

    /// Resolves and admits the TypeScript installation selected by one exact package root.
    ///
    /// `Ok(None)` means no project-local installation exists, allowing the caller to preserve an
    /// already configured host-wide checker. Every present-but-invalid installation is a typed
    /// refusal and must not silently fall back to another project's or the host's compiler.
    pub(crate) fn admit(
        &self,
        package_root: &Path,
    ) -> Result<Option<AdmittedTypeScriptProject>, TypeScriptProjectHostError> {
        if !package_root.is_absolute() {
            return Err(TypeScriptProjectHostError::RelativePackageRoot {
                package_root: package_root.to_path_buf().into_boxed_path(),
            });
        }
        let project_root = fs::canonicalize(package_root).map_err(|source| {
            TypeScriptProjectHostError::PackageRoot {
                package_root: package_root.to_path_buf().into_boxed_path(),
                source,
            }
        })?;

        let home_root = self
            .home_root
            .as_deref()
            .map(|home| {
                fs::canonicalize(home).map_err(|source| TypeScriptProjectHostError::HomePath {
                    home: home.to_path_buf().into_boxed_path(),
                    source,
                })
            })
            .transpose()?;
        let project = match find_project_typescript_with_home(&project_root, home_root.as_deref())?
        {
            ProjectTypeScriptSearch::Found(project) => Some(project),
            ProjectTypeScriptSearch::NotFound => None,
            ProjectTypeScriptSearch::Pnp(marker) => {
                return Err(TypeScriptProjectHostError::YarnPnpUnsupported {
                    marker: marker.into_boxed_path(),
                });
            }
        };

        let Some(project) = project else {
            return Ok(None);
        };

        let selected_compiler = self
            .explicit_compiler
            .as_deref()
            .unwrap_or(project.compiler.as_path());
        let compiler = fs::canonicalize(selected_compiler).map_err(|source| {
            TypeScriptProjectHostError::PackagePath {
                path: selected_compiler.to_path_buf().into_boxed_path(),
                source,
            }
        })?;
        let selected_node =
            self.node
                .as_deref()
                .ok_or_else(|| TypeScriptProjectHostError::NodeUnavailable {
                    package_root: project_root.clone().into_boxed_path(),
                })?;
        let node = fs::canonicalize(selected_node).map_err(|source| {
            TypeScriptProjectHostError::NodePath {
                node: selected_node.to_path_buf().into_boxed_path(),
                source,
            }
        })?;

        let (module_root, expected_version) = match self.explicit_module_root.as_deref() {
            Some(root) => read_typescript_module(root)?,
            None if self.explicit_compiler.is_some() => {
                let root = find_module_root_for_compiler(&compiler)?;
                match root {
                    Some(root) => read_typescript_module(&root)?,
                    None if compiler == project.compiler => {
                        (project.module_root.clone(), project.version.clone())
                    }
                    None => {
                        return Err(TypeScriptProjectHostError::ExplicitModuleRootRequired {
                            compiler: compiler.into_boxed_path(),
                        });
                    }
                }
            }
            None => (project.module_root.clone(), project.version.clone()),
        };
        let package_root = fs::canonicalize(module_root.join("typescript")).map_err(|source| {
            TypeScriptProjectHostError::PackagePath {
                path: module_root.join("typescript").into_boxed_path(),
                source,
            }
        })?;
        if !package_root.starts_with(&module_root) {
            return Err(TypeScriptProjectHostError::PackageEscapesNodeModules {
                package: package_root.into_boxed_path(),
                node_modules: module_root.into_boxed_path(),
            });
        }
        let node_identity = self.admit_node_runtime(&node)?;
        let witness = TypeScriptProjectWitness::capture_with_node_snapshot(
            &project_root,
            home_root.as_deref(),
            &project,
            &compiler,
            &node,
            &module_root,
            &package_root,
            project.workspace.as_ref(),
            Some(&node_identity.snapshot),
        )?;
        witness.validate_current()?;
        let node_version = node_identity.version;

        let version = if is_module_tsc_script(&compiler, &module_root) {
            crate::application::toolchain_probe::probe_typescript_script_with_node(
                &compiler,
                &node,
                self.probe_limits,
            )
            .map_err(|source| TypeScriptProjectHostError::CompilerProbe {
                compiler: compiler.to_path_buf().into_boxed_path(),
                node: node.to_path_buf().into_boxed_path(),
                source,
            })?
        } else {
            crate::application::toolchain_probe::probe_version(
                NativeTool::TypeScriptCompiler,
                &compiler,
                self.probe_limits,
            )
            .map_err(|source| TypeScriptProjectHostError::CompilerProbe {
                compiler: compiler.to_path_buf().into_boxed_path(),
                node: node.to_path_buf().into_boxed_path(),
                source,
            })?
        };
        witness.validate_current()?;
        let observed_version = parse_tsc_version(&version).ok_or_else(|| {
            TypeScriptProjectHostError::InvalidCompilerVersion {
                compiler: compiler.to_path_buf().into_boxed_path(),
                output: bounded_text(&version),
            }
        })?;
        if observed_version != expected_version {
            return Err(TypeScriptProjectHostError::VersionMismatch {
                compiler: compiler.to_path_buf().into_boxed_path(),
                expected: expected_version.into_boxed_str(),
                observed: observed_version.into_boxed_str(),
            });
        }

        let checker = match self.report_program.as_deref() {
            Some(program) => Checker::default().with_program(program.to_path_buf()),
            None => Checker::default().with_node(node.to_path_buf(), module_root.clone()),
        }
        .map_err(|source| TypeScriptProjectHostError::CheckerConfiguration {
            node: node.to_path_buf().into_boxed_path(),
            module_root: module_root.clone().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        })?;

        let fingerprint = project_fingerprint(
            &project_root,
            &compiler,
            &node,
            &module_root,
            &expected_version,
            &node_version,
            checker.local_configuration_fingerprint(),
            witness.fingerprint,
            if self.explicit_compiler.is_some() {
                TypeScriptSelectionOrigin::ExplicitConfiguration
            } else {
                TypeScriptSelectionOrigin::ProjectLocalInstallation
            },
            self.node_origin
                .unwrap_or(TypeScriptSelectionOrigin::PlatformLocation),
        );
        Ok(Some(AdmittedTypeScriptProject {
            checker,
            compiler: compiler.to_path_buf().into_boxed_path(),
            compiler_version: version,
            compiler_origin: if self.explicit_compiler.is_some() {
                TypeScriptSelectionOrigin::ExplicitConfiguration
            } else {
                TypeScriptSelectionOrigin::ProjectLocalInstallation
            },
            node: node.to_path_buf().into_boxed_path(),
            node_version,
            node_origin: self
                .node_origin
                .unwrap_or(TypeScriptSelectionOrigin::PlatformLocation),
            module_root: module_root.into_boxed_path(),
            fingerprint,
            witness: std::sync::Arc::new(witness),
        }))
    }
}

/// One project-local checker and the exact toolchain facts admitted for it.
#[derive(Debug)]
pub(crate) struct AdmittedTypeScriptProject {
    pub(crate) checker: ExplicitTypeScriptChecker,
    pub(crate) compiler: Box<Path>,
    pub(crate) compiler_version: Box<[u8]>,
    compiler_origin: TypeScriptSelectionOrigin,
    node: Box<Path>,
    node_version: Box<[u8]>,
    node_origin: TypeScriptSelectionOrigin,
    module_root: Box<Path>,
    pub(crate) fingerprint: [u8; 32],
    pub(crate) witness: std::sync::Arc<TypeScriptProjectWitness>,
}

impl AdmittedTypeScriptProject {
    pub(crate) fn resolved_toolchain(
        &self,
    ) -> Result<ResolvedToolchain<'_>, TypeScriptProjectHostError> {
        if is_module_tsc_script(&self.compiler, &self.module_root) {
            ResolvedToolchain::from_project_invocation(
                NativeTool::TypeScriptCompiler,
                &self.node,
                &self.compiler,
                &self.module_root,
                &self.compiler_version,
                &self.node_version,
                self.witness.invocation_lease(),
            )
            .map_err(|source| TypeScriptProjectHostError::ToolchainResolution { source })
        } else {
            ResolvedToolchain::from_version(
                NativeTool::TypeScriptCompiler,
                &self.compiler,
                &self.compiler_version,
            )
            .map_err(|source| TypeScriptProjectHostError::ToolchainResolution { source })
        }
    }

    pub(crate) fn inputs(&self) -> TypeScriptProjectInputs<'_> {
        TypeScriptProjectInputs {
            package_root: &self.witness.project_root,
            workspace_root: &self.witness.workspace_root,
            typescript_module_root: &self.module_root,
            compiler_path: &self.compiler,
            compiler_version: &self.compiler_version,
            compiler_origin: self.compiler_origin,
            node_path: &self.node,
            node_version: &self.node_version,
            node_origin: self.node_origin,
            fingerprint: self.fingerprint,
            config_candidates: &self.witness.config_candidates,
            selected_build_config_paths: &self.witness.selected_build_config_paths,
            typescript_files: &self.witness.typescript_files,
            witness: &self.witness,
        }
    }

    pub(crate) fn validate_current(&self) -> Result<(), TypeScriptProjectHostError> {
        self.witness.validate_current()
    }
}

#[derive(Debug)]
struct ProjectTypeScript {
    module_root: PathBuf,
    compiler: PathBuf,
    version: String,
    workspace: Option<WorkspaceBoundary>,
}

enum ProjectTypeScriptSearch {
    Found(ProjectTypeScript),
    NotFound,
    Pnp(PathBuf),
}

fn find_project_typescript(
    root: &Path,
) -> Result<ProjectTypeScriptSearch, TypeScriptProjectHostError> {
    find_project_typescript_with_home(root, None)
}

fn find_project_typescript_with_home(
    root: &Path,
    home_root: Option<&Path>,
) -> Result<ProjectTypeScriptSearch, TypeScriptProjectHostError> {
    let canonical_root =
        fs::canonicalize(root).map_err(|source| TypeScriptProjectHostError::PackageRoot {
            package_root: root.to_path_buf().into_boxed_path(),
            source,
        })?;
    let canonical_home = home_root
        .map(|home| {
            fs::canonicalize(home).map_err(|source| TypeScriptProjectHostError::HomePath {
                home: home.to_path_buf().into_boxed_path(),
                source,
            })
        })
        .transpose()?;
    let root = canonical_root.as_path();
    let workspace = discover_workspace_boundary(root, canonical_home.as_deref())?;
    let scan_root = workspace
        .as_ref()
        .map(|workspace| workspace.root.as_ref())
        .unwrap_or(root);
    let mut ancestors = Vec::new();
    let mut current = root;
    loop {
        ancestors.push(current);
        if current == scan_root || ancestors.len() >= MAX_PROJECT_ANCESTORS {
            break;
        }
        let Some(parent) = current.parent() else {
            break;
        };
        if !parent.starts_with(scan_root) {
            break;
        }
        current = parent;
    }
    let mut pnp = None;
    for ancestor in ancestors {
        let node_modules = ancestor.join("node_modules");
        let package = node_modules.join("typescript");
        match fs::symlink_metadata(&package) {
            Ok(_) => return inspect_project_package(&node_modules, &package, workspace),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(TypeScriptProjectHostError::PackagePath {
                    path: package.into_boxed_path(),
                    source,
                });
            }
        }
        for marker_name in [".pnp.cjs", ".pnp.loader.mjs"] {
            let marker = ancestor.join(marker_name);
            if path_exists(&marker)? && pnp.is_none() {
                pnp = Some(marker);
            }
        }
    }
    Ok(match pnp {
        Some(marker) => ProjectTypeScriptSearch::Pnp(marker),
        None => ProjectTypeScriptSearch::NotFound,
    })
}

fn inspect_project_package(
    node_modules: &Path,
    package: &Path,
    workspace: Option<WorkspaceBoundary>,
) -> Result<ProjectTypeScriptSearch, TypeScriptProjectHostError> {
    let node_modules = fs::canonicalize(node_modules).map_err(|source| {
        TypeScriptProjectHostError::PackagePath {
            path: node_modules.to_path_buf().into_boxed_path(),
            source,
        }
    })?;
    let package =
        fs::canonicalize(package).map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: package.to_path_buf().into_boxed_path(),
            source,
        })?;
    if !package.starts_with(&node_modules) {
        return Err(TypeScriptProjectHostError::PackageEscapesNodeModules {
            package: package.into_boxed_path(),
            node_modules: node_modules.into_boxed_path(),
        });
    }
    let manifest = package.join("package.json");
    let (name, version) = read_package_manifest(&manifest)?;
    if name != "typescript" {
        return Err(TypeScriptProjectHostError::InvalidPackageName {
            manifest: manifest.into_boxed_path(),
            name: name.into_boxed_str(),
        });
    }
    validate_semver(&version).ok_or_else(|| TypeScriptProjectHostError::InvalidPackageVersion {
        manifest: manifest.clone().into_boxed_path(),
        version: version.clone().into_boxed_str(),
    })?;
    let compiler = package.join("bin/tsc");
    let compiler =
        fs::canonicalize(&compiler).map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: compiler.clone().into_boxed_path(),
            source,
        })?;
    if !compiler.starts_with(&package) || !compiler.is_file() {
        return Err(TypeScriptProjectHostError::CompilerEscapesPackage {
            compiler: compiler.into_boxed_path(),
            package: package.into_boxed_path(),
        });
    }
    let bin_link = node_modules.join(".bin/tsc");
    match fs::symlink_metadata(&bin_link) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                let target = fs::canonicalize(&bin_link).map_err(|source| {
                    TypeScriptProjectHostError::PackagePath {
                        path: bin_link.clone().into_boxed_path(),
                        source,
                    }
                })?;
                if target != compiler {
                    return Err(TypeScriptProjectHostError::CompilerLinkMismatch {
                        shim: bin_link.into_boxed_path(),
                        target: target.into_boxed_path(),
                        expected: compiler.into_boxed_path(),
                    });
                }
            } else if metadata.is_file() {
                let (_, bytes) = read_regular_file(
                    &bin_link,
                    u64::try_from(MAX_COMPILER_SHIM_BYTES).unwrap_or(u64::MAX),
                )?;
                if !has_bounded_shebang(&bytes) {
                    return Err(TypeScriptProjectHostError::CompilerShimRejected {
                        shim: bin_link.into_boxed_path(),
                    });
                }
            } else {
                return Err(TypeScriptProjectHostError::CompilerShimRejected {
                    shim: bin_link.into_boxed_path(),
                });
            }
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(TypeScriptProjectHostError::PackagePath {
                path: bin_link.into_boxed_path(),
                source,
            });
        }
    }
    Ok(ProjectTypeScriptSearch::Found(ProjectTypeScript {
        module_root: node_modules,
        compiler,
        version,
        workspace,
    }))
}

fn has_bounded_shebang(bytes: &[u8]) -> bool {
    if bytes.len() > MAX_COMPILER_SHIM_BYTES || !bytes.starts_with(b"#!") {
        return false;
    }
    let Some(end) = bytes
        .iter()
        .take(MAX_SHEBANG_BYTES)
        .position(|byte| *byte == b'\n')
    else {
        return false;
    };
    end > 2
        && bytes[2..end]
            .iter()
            .all(|byte| byte.is_ascii_graphic() || *byte == b' ')
}

fn discover_workspace_boundary(
    root: &Path,
    home_root: Option<&Path>,
) -> Result<Option<WorkspaceBoundary>, TypeScriptProjectHostError> {
    let canonical_root =
        fs::canonicalize(root).map_err(|source| TypeScriptProjectHostError::PackageRoot {
            package_root: root.to_path_buf().into_boxed_path(),
            source,
        })?;
    let canonical_home = home_root
        .map(|home| {
            fs::canonicalize(home).map_err(|source| TypeScriptProjectHostError::HomePath {
                home: home.to_path_buf().into_boxed_path(),
                source,
            })
        })
        .transpose()?;
    for (index, ancestor) in canonical_root
        .ancestors()
        .take(MAX_PROJECT_ANCESTORS)
        .enumerate()
    {
        if index > 0
            && canonical_home
                .as_deref()
                .is_some_and(|home| ancestor == home)
        {
            break;
        }
        if index > 0 && home_root.is_none() {
            break;
        }
        let mut files = Vec::new();
        let manifest = ancestor.join("package.json");
        if path_exists(&manifest)? {
            let (snapshot, bytes) = read_regular_file(
                &manifest,
                u64::try_from(MAX_PACKAGE_MANIFEST_BYTES).unwrap_or(u64::MAX),
            )?;
            let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|source| {
                TypeScriptProjectHostError::WorkspaceConfigInvalid {
                    path: manifest.clone().into_boxed_path(),
                    message: source.to_string().into_boxed_str(),
                }
            })?;
            if has_valid_workspaces_field(&value, &manifest)? {
                files.push(snapshot);
            }
        }
        let pnpm = ancestor.join("pnpm-workspace.yaml");
        if path_exists(&pnpm)? {
            let (snapshot, bytes) = read_regular_file(
                &pnpm,
                u64::try_from(MAX_PNPM_WORKSPACE_BYTES).unwrap_or(u64::MAX),
            )?;
            if !valid_pnpm_workspace(&bytes) {
                return Err(TypeScriptProjectHostError::WorkspaceConfigInvalid {
                    path: pnpm.into_boxed_path(),
                    message: "pnpm workspace file must contain a bounded non-empty packages list"
                        .into(),
                });
            }
            files.push(snapshot);
        }
        if !files.is_empty() {
            return Ok(Some(WorkspaceBoundary {
                root: ancestor.to_path_buf().into_boxed_path(),
                files: files.into_boxed_slice(),
            }));
        }
    }
    Ok(None)
}

fn path_exists(path: &Path) -> Result<bool, TypeScriptProjectHostError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(TypeScriptProjectHostError::PackagePath {
            path: path.to_path_buf().into_boxed_path(),
            source,
        }),
    }
}

fn has_valid_workspaces_field(
    value: &serde_json::Value,
    path: &Path,
) -> Result<bool, TypeScriptProjectHostError> {
    let Some(workspaces) = value.get("workspaces") else {
        return Ok(false);
    };
    let patterns = workspaces
        .as_array()
        .or_else(|| {
            workspaces
                .get("packages")
                .and_then(serde_json::Value::as_array)
        })
        .ok_or_else(|| TypeScriptProjectHostError::WorkspaceConfigInvalid {
            path: path.to_path_buf().into_boxed_path(),
            message: "workspaces must be an array or an object with a packages array".into(),
        })?;
    if patterns.is_empty()
        || patterns.iter().any(|pattern| {
            pattern
                .as_str()
                .is_none_or(|pattern| pattern.trim().is_empty() || Path::new(pattern).is_absolute())
                || pattern.as_str().is_some_and(|pattern| {
                    Path::new(pattern)
                        .components()
                        .any(|component| component == std::path::Component::ParentDir)
                })
        })
    {
        return Err(TypeScriptProjectHostError::WorkspaceConfigInvalid {
            path: path.to_path_buf().into_boxed_path(),
            message: "workspace package patterns must be non-empty relative strings".into(),
        });
    }
    Ok(true)
}

fn valid_pnpm_workspace(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let mut in_packages = false;
    let mut count = 0;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed == "packages:" {
            in_packages = true;
            continue;
        }
        if in_packages {
            if line.len() != trimmed.len() && trimmed.starts_with('-') {
                let pattern = trimmed[1..].trim().trim_matches(['\'', '"']);
                if pattern.is_empty()
                    || Path::new(pattern).is_absolute()
                    || Path::new(pattern)
                        .components()
                        .any(|component| component == std::path::Component::ParentDir)
                {
                    return false;
                }
                count += 1;
                continue;
            }
            if line.len() != trimmed.len() {
                continue;
            }
            break;
        }
        if trimmed.starts_with('-') {
            return false;
        }
    }
    in_packages && count > 0
}

fn find_module_root_for_compiler(
    compiler: &Path,
) -> Result<Option<PathBuf>, TypeScriptProjectHostError> {
    for ancestor in compiler.ancestors().take(MAX_PROJECT_ANCESTORS) {
        if ancestor
            .file_name()
            .is_some_and(|name| name == "node_modules")
        {
            if ancestor.join("typescript/package.json").is_file() {
                return Ok(Some(ancestor.to_path_buf()));
            }
        }
    }
    Ok(None)
}

fn is_module_tsc_script(compiler: &Path, module_root: &Path) -> bool {
    let expected = module_root.join("typescript/bin/tsc");
    matches!(
        (fs::canonicalize(compiler), fs::canonicalize(expected)),
        (Ok(compiler), Ok(expected)) if compiler == expected
    )
}

fn read_typescript_module(root: &Path) -> Result<(PathBuf, String), TypeScriptProjectHostError> {
    let root =
        fs::canonicalize(root).map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: root.to_path_buf().into_boxed_path(),
            source,
        })?;
    let package = fs::canonicalize(root.join("typescript")).map_err(|source| {
        TypeScriptProjectHostError::PackagePath {
            path: root.join("typescript").into_boxed_path(),
            source,
        }
    })?;
    if !package.starts_with(&root) {
        return Err(TypeScriptProjectHostError::PackageEscapesNodeModules {
            package: package.into_boxed_path(),
            node_modules: root.into_boxed_path(),
        });
    }
    let manifest = package.join("package.json");
    let (name, version) = read_package_manifest(&manifest)?;
    if name != "typescript" {
        return Err(TypeScriptProjectHostError::InvalidPackageName {
            manifest: manifest.into_boxed_path(),
            name: name.into_boxed_str(),
        });
    }
    validate_semver(&version).ok_or_else(|| TypeScriptProjectHostError::InvalidPackageVersion {
        manifest: manifest.clone().into_boxed_path(),
        version: version.clone().into_boxed_str(),
    })?;
    Ok((root, version))
}

fn read_package_manifest(path: &Path) -> Result<(String, String), TypeScriptProjectHostError> {
    let (_, bytes) = read_regular_file(
        path,
        u64::try_from(MAX_PACKAGE_MANIFEST_BYTES).unwrap_or(u64::MAX),
    )?;
    if bytes.len() > MAX_PACKAGE_MANIFEST_BYTES {
        return Err(TypeScriptProjectHostError::ManifestTooLarge {
            manifest: path.to_path_buf().into_boxed_path(),
            maximum: MAX_PACKAGE_MANIFEST_BYTES,
        });
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|source| {
        TypeScriptProjectHostError::ManifestInvalid {
            manifest: path.to_path_buf().into_boxed_path(),
            message: source.to_string().into_boxed_str(),
        }
    })?;
    let name = value.get("name").and_then(serde_json::Value::as_str);
    let version = value.get("version").and_then(serde_json::Value::as_str);
    match (name, version) {
        (Some(name), Some(version)) => Ok((name.to_owned(), version.to_owned())),
        _ => Err(TypeScriptProjectHostError::ManifestInvalid {
            manifest: path.to_path_buf().into_boxed_path(),
            message: "package.json must contain string name and version fields".into(),
        }),
    }
}

fn parse_tsc_version(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?.trim();
    let version = text.strip_prefix("Version ").unwrap_or(text).trim();
    validate_semver(version).map(|()| version.to_owned())
}

fn validate_semver(version: &str) -> Option<()> {
    if version.is_empty()
        || version.len() > 128
        || version.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return None;
    }
    let (without_build, build) = version
        .split_once('+')
        .map_or((version, None), |(a, b)| (a, Some(b)));
    if build.is_some_and(|value| !valid_identifiers(value)) {
        return None;
    }
    let (core, prerelease) = without_build
        .split_once('-')
        .map_or((without_build, None), |(a, b)| (a, Some(b)));
    if prerelease.is_some_and(|value| !valid_identifiers(value)) {
        return None;
    }
    let mut parts = core.split('.');
    for _ in 0..3 {
        let part = parts.next()?;
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        if part.len() > 1 && part.starts_with('0') {
            return None;
        }
        if part.parse::<u64>().is_err() {
            return None;
        }
    }
    parts.next().is_none().then_some(())
}

fn valid_identifiers(value: &str) -> bool {
    !value.is_empty()
        && value.split('.').all(|identifier| {
            !identifier.is_empty()
                && identifier
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn bounded_text(bytes: &[u8]) -> Box<str> {
    let shown = &bytes[..bytes.len().min(512)];
    String::from_utf8_lossy(shown).into_owned().into_boxed_str()
}

fn project_fingerprint(
    package_root: &Path,
    compiler: &Path,
    node: &Path,
    module_root: &Path,
    package_version: &str,
    node_version: &[u8],
    checker_fingerprint: [u8; 32],
    witness_fingerprint: [u8; 32],
    compiler_origin: TypeScriptSelectionOrigin,
    node_origin: TypeScriptSelectionOrigin,
) -> [u8; 32] {
    let mut digest = Hasher::new();
    digest.update(b"compiler.typescript.project-admission.v1\0");
    for path in [package_root, compiler, node, module_root] {
        let bytes = path.as_os_str().as_encoded_bytes();
        digest.update(&(bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    digest.update(&(package_version.len() as u64).to_be_bytes());
    digest.update(package_version.as_bytes());
    digest.update(&(node_version.len() as u64).to_be_bytes());
    digest.update(node_version);
    digest.update(&checker_fingerprint);
    digest.update(&witness_fingerprint);
    digest.update(&[compiler_origin as u8, node_origin as u8]);
    *digest.finalize().as_bytes()
}

/// Typed terminal from resolving or probing one project-owned TypeScript installation.
///
/// Each variant's display text identifies the selected path or the bounded process failure.
/// Its field names are intentionally descriptive in the structured error chain.
#[derive(Debug, Error)]
#[allow(missing_docs)]
pub enum TypeScriptProjectHostError {
    #[error("admitted TypeScript executable could not be bound to its version identity")]
    ToolchainResolution {
        #[source]
        source: ToolchainResolutionError,
    },
    #[error("TypeScript package root is not absolute: {package_root:?}")]
    RelativePackageRoot { package_root: Box<Path> },
    #[error("could not canonicalize the host home boundary {home:?}")]
    HomePath {
        home: Box<Path>,
        #[source]
        source: std::io::Error,
    },
    #[error("could not resolve selected TypeScript package root {package_root:?}")]
    PackageRoot {
        package_root: Box<Path>,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "Yarn Plug'n'Play manifest {marker:?} is present, but this checker currently requires a local node_modules layout"
    )]
    YarnPnpUnsupported { marker: Box<Path> },
    #[error(
        "project has a TypeScript installation but no explicitly admitted Node executable is available: {package_root:?}"
    )]
    NodeUnavailable { package_root: Box<Path> },
    #[error("could not probe the selected Node executable at {node:?}")]
    NodeProbe {
        node: Box<Path>,
        #[source]
        source: ToolchainProbeError,
    },
    #[error("could not resolve selected Node executable {node:?}")]
    NodePath {
        node: Box<Path>,
        #[source]
        source: std::io::Error,
    },
    #[error("explicit TypeScript executable {compiler:?} requires its matching module root")]
    ExplicitModuleRootRequired { compiler: Box<Path> },
    #[error("could not read TypeScript package path {path:?}")]
    PackagePath {
        path: Box<Path>,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "TypeScript package {package:?} escapes its admitted node_modules directory {node_modules:?}"
    )]
    PackageEscapesNodeModules {
        package: Box<Path>,
        node_modules: Box<Path>,
    },
    #[error("TypeScript package manifest {manifest:?} names {name:?}, not typescript")]
    InvalidPackageName { manifest: Box<Path>, name: Box<str> },
    #[error("TypeScript package manifest {manifest:?} has invalid semver version {version:?}")]
    InvalidPackageVersion {
        manifest: Box<Path>,
        version: Box<str>,
    },
    #[error("TypeScript package manifest {manifest:?} exceeds the {maximum}-byte bound")]
    ManifestTooLarge { manifest: Box<Path>, maximum: usize },
    #[error("could not decode TypeScript package manifest {manifest:?}: {message}")]
    ManifestInvalid {
        manifest: Box<Path>,
        message: Box<str>,
    },
    #[error("workspace configuration {path:?} is malformed: {message}")]
    WorkspaceConfigInvalid { path: Box<Path>, message: Box<str> },
    #[error("TypeScript configuration {config:?} is malformed: {message}")]
    ConfigInvalid {
        config: Box<Path>,
        message: Box<str>,
    },
    #[error("TypeScript configuration {config:?} does not exist")]
    ConfigMissing { config: Box<Path> },
    #[error("TypeScript configuration graph contains a cycle at {config:?}")]
    ConfigCycle { config: Box<Path> },
    #[error("TypeScript configuration {config:?} escapes workspace boundary {boundary:?}")]
    ConfigEscapesBoundary {
        config: Box<Path>,
        boundary: Box<Path>,
    },
    #[error("project contains {observed} TypeScript configuration files, maximum is {maximum}")]
    ConfigFileLimit { observed: usize, maximum: usize },
    #[error("TypeScript configuration scan exceeded its directory depth at {directory:?}")]
    ConfigDirectoryDepth { directory: Box<Path> },
    #[error(
        "captured TypeScript file {path:?} exceeds its {maximum}-byte bound (observed {observed})"
    )]
    FileTooLarge {
        path: Box<Path>,
        observed: u64,
        maximum: u64,
    },
    #[error("captured TypeScript file allocation failed at {path:?}: {message}")]
    FileAllocation { path: Box<Path>, message: Box<str> },
    #[error("expected a regular file at {path:?}")]
    RegularFileRequired { path: Box<Path> },
    #[error("expected a regular directory at {path:?}")]
    RegularDirectoryRequired { path: Box<Path> },
    #[error("TypeScript module contains a symlink at {path:?}")]
    ModuleEntrySymlink { path: Box<Path> },
    #[error("TypeScript package contains {observed} regular files, maximum is {maximum}")]
    ModuleFileLimit { observed: usize, maximum: usize },
    #[error("TypeScript project witness changed at {path:?} after admission")]
    WitnessChanged { path: Box<Path> },
    #[error(
        "resolved TypeScript source path {path:?} is outside the admitted workspace and compiler roots"
    )]
    SourceOutsideCapability { path: Box<Path> },
    #[error("resolved TypeScript source count {observed} exceeds the {maximum}-file bound")]
    ResolvedSourceLimit { observed: usize, maximum: usize },
    #[error("resolved TypeScript source bytes {observed} exceed the {maximum}-byte bound")]
    ResolvedSourceBytes { observed: u64, maximum: u64 },
    #[error("TypeScript resolver observations {observed} exceed the {maximum}-observation bound")]
    ResolverObservationLimit { observed: usize, maximum: usize },
    #[error(
        "TypeScript directory query requested depth {requested_depth} and {requested_entries} entries; limits are {maximum_depth} and {maximum_entries}"
    )]
    ResolverDirectoryLimit {
        requested_depth: usize,
        requested_entries: usize,
        maximum_depth: usize,
        maximum_entries: usize,
    },
    #[error(
        "TypeScript resolver metadata at {path:?} exceeds the {maximum}-byte bound (observed {observed})"
    )]
    ResolverMetadataLimit {
        path: Box<Path>,
        observed: usize,
        maximum: usize,
    },
    #[error("TypeScript resolver path {path:?} cannot be encoded as a portable UTF-8 logical path")]
    NonPortablePath { path: Box<Path> },
    #[error("TypeScript project witness lock was poisoned at {path:?}")]
    WitnessLockPoisoned { path: Box<Path> },
    #[error("TypeScript compiler entry {compiler:?} is outside package {package:?}")]
    CompilerEscapesPackage {
        compiler: Box<Path>,
        package: Box<Path>,
    },
    #[error("automatically discovered TypeScript shim {shim:?} is not a package-manager symlink")]
    CompilerShimRejected { shim: Box<Path> },
    #[error(
        "TypeScript shim {shim:?} resolves to {target:?}, expected the admitted package compiler {expected:?}"
    )]
    CompilerLinkMismatch {
        shim: Box<Path>,
        target: Box<Path>,
        expected: Box<Path>,
    },
    #[error("could not probe TypeScript compiler {compiler:?} with admitted Node {node:?}")]
    CompilerProbe {
        compiler: Box<Path>,
        node: Box<Path>,
        #[source]
        source: ToolchainProbeError,
    },
    #[error("TypeScript compiler {compiler:?} returned an invalid --version result: {output:?}")]
    InvalidCompilerVersion {
        compiler: Box<Path>,
        output: Box<str>,
    },
    #[error(
        "TypeScript compiler {compiler:?} reports {observed}, but its selected module root provides {expected}"
    )]
    VersionMismatch {
        compiler: Box<Path>,
        expected: Box<str>,
        observed: Box<str>,
    },
    #[error(
        "could not configure TypeScript checker for Node {node:?} and module root {module_root:?}: {message}"
    )]
    CheckerConfiguration {
        node: Box<Path>,
        module_root: Box<Path>,
        message: Box<str>,
    },
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::symlink,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "typescript-project-host-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir_all(&root).expect("create project fixture");
            Self(fs::canonicalize(root).expect("canonical project fixture"))
        }

        fn install(&self, version: &str) -> PathBuf {
            let modules = self.0.join("node_modules");
            let package = modules.join("typescript");
            fs::create_dir_all(package.join("bin")).expect("create package");
            fs::write(
                package.join("package.json"),
                format!("{{\"name\":\"typescript\",\"version\":\"{version}\"}}"),
            )
            .expect("write package manifest");
            fs::write(package.join("bin/tsc"), "#!/usr/bin/env node\n").expect("write compiler");
            fs::create_dir_all(modules.join(".bin")).expect("create bin links");
            symlink(package.join("bin/tsc"), modules.join(".bin/tsc")).expect("link compiler");
            modules
        }

        fn limits() -> ToolchainProbeLimits {
            ToolchainProbeLimits::new(
                std::time::Duration::from_secs(2),
                std::num::NonZeroUsize::new(4096).expect("nonzero fixture bound"),
            )
            .expect("valid fixture probe limits")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn project_discovery_accepts_exact_npm_package_link_and_version() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let result = find_project_typescript(&fixture.0).expect("inspect project installation");
        let ProjectTypeScriptSearch::Found(project) = result else {
            panic!("local TypeScript package should be selected");
        };
        assert_eq!(
            project.module_root,
            fs::canonicalize(modules).expect("canonical root")
        );
        assert_eq!(project.version, "5.9.3");
        assert!(project.compiler.ends_with("typescript/bin/tsc"));
    }

    #[test]
    fn project_discovery_rejects_wrong_shim_and_escape_symlinks() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let wrong = fixture.0.join("other-tsc");
        fs::write(&wrong, "not TypeScript").expect("write wrong shim target");
        fs::remove_file(modules.join(".bin/tsc")).expect("remove selected link");
        symlink(&wrong, modules.join(".bin/tsc")).expect("install wrong link");
        assert!(matches!(
            find_project_typescript(&fixture.0),
            Err(TypeScriptProjectHostError::CompilerLinkMismatch { .. })
        ));

        fs::remove_file(modules.join(".bin/tsc")).expect("remove wrong link");
        fs::remove_dir_all(modules.join("typescript")).expect("remove package");
        let outside = fixture.0.join("outside-typescript");
        fs::create_dir_all(outside.join("bin")).expect("create outside package");
        fs::write(
            outside.join("package.json"),
            "{\"name\":\"typescript\",\"version\":\"5.9.3\"}",
        )
        .expect("write outside manifest");
        fs::write(outside.join("bin/tsc"), "#!/usr/bin/env node\n")
            .expect("write outside compiler");
        symlink(&outside, modules.join("typescript")).expect("link package outside node_modules");
        assert!(matches!(
            find_project_typescript(&fixture.0),
            Err(TypeScriptProjectHostError::PackageEscapesNodeModules { .. })
        ));
    }

    #[test]
    fn project_discovery_accepts_pnpm_package_symlinks_and_bounded_shebang_shims() {
        let fixture = Fixture::new();
        let modules = fixture.0.join("node_modules");
        let pnpm_package = modules.join(".pnpm/typescript@5.9.3/node_modules/typescript");
        fs::create_dir_all(pnpm_package.join("bin")).expect("create pnpm package");
        fs::write(
            pnpm_package.join("package.json"),
            "{\"name\":\"typescript\",\"version\":\"5.9.3\"}",
        )
        .expect("write pnpm package manifest");
        fs::write(pnpm_package.join("bin/tsc"), "#!/usr/bin/env node\n")
            .expect("write pnpm compiler");
        fs::create_dir_all(modules.join(".bin")).expect("create package manager bin");
        symlink(&pnpm_package, modules.join("typescript")).expect("link pnpm package");
        symlink(pnpm_package.join("bin/tsc"), modules.join(".bin/tsc")).expect("link pnpm shim");
        let ProjectTypeScriptSearch::Found(project) =
            find_project_typescript(&fixture.0).expect("inspect pnpm installation")
        else {
            panic!("pnpm TypeScript package should be selected");
        };
        assert!(
            project.compiler.starts_with(
                fs::canonicalize(modules.join(".pnpm")).expect("canonical pnpm store")
            )
        );

        fs::remove_file(modules.join(".bin/tsc")).expect("remove package manager shim");
        fs::write(modules.join(".bin/tsc"), "#!/bin/sh\nexit 0\n").expect("write unknown wrapper");
        let ProjectTypeScriptSearch::Found(project) =
            find_project_typescript(&fixture.0).expect("inspect regular package-manager shim")
        else {
            panic!("regular bounded shim metadata should not replace direct package compiler");
        };
        assert!(project.compiler.ends_with("typescript/bin/tsc"));
        assert!(!project.compiler.ends_with("node_modules/.bin/tsc"));
    }

    #[test]
    fn project_discovery_reports_yarn_pnp_without_executing_it() {
        let fixture = Fixture::new();
        fs::write(
            fixture.0.join(".pnp.cjs"),
            "throw new Error('must not execute')",
        )
        .expect("write marker");
        assert!(matches!(
            find_project_typescript(&fixture.0),
            Ok(ProjectTypeScriptSearch::Pnp(_))
        ));
    }

    #[test]
    fn project_discovery_reports_a_broken_typescript_symlink() {
        let fixture = Fixture::new();
        let modules = fixture.0.join("node_modules");
        fs::create_dir_all(&modules).expect("create node_modules");
        symlink(
            fixture.0.join("missing-typescript"),
            modules.join("typescript"),
        )
        .expect("create broken package symlink");
        assert!(matches!(
            find_project_typescript(&fixture.0),
            Err(TypeScriptProjectHostError::PackagePath { .. })
        ));
    }

    #[test]
    fn semver_parser_rejects_malformed_and_preserves_pre_release_identity() {
        assert!(validate_semver("5.9.3").is_some());
        assert!(validate_semver("5.9.3-rc.1+build.2").is_some());
        assert!(validate_semver("05.9.3").is_none());
        assert!(validate_semver("5.9").is_none());
        assert!(validate_semver("5.9.3; malicious").is_none());
        assert_eq!(parse_tsc_version(b"Version 5.9.3\n"), Some("5.9.3".into()));
    }

    #[test]
    fn local_tsc_probe_uses_only_the_admitted_node_directory() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let node_directory = fixture.0.join("node dir");
        fs::create_dir_all(&node_directory).expect("create Node directory");
        let node = node_directory.join("node");
        fs::write(
            &node,
            r##"#!/bin/sh
test -z "${HOME+x}" || exit 31
test -z "${NODE_OPTIONS+x}" || exit 32
node_directory=${0%/*}
test "$PATH" = "$node_directory" || exit 33
test -f "$1" || exit 34
test "$2" = "--version" || exit 35
printf 'Version 5.9.3\n'
"##,
        )
        .expect("write Node environment fixture");
        fs::set_permissions(&node, fs::Permissions::from_mode(0o755))
            .expect("make Node executable");
        let compiler = modules.join("typescript/bin/tsc");
        let result = crate::application::toolchain_probe::probe_typescript_script_with_node(
            &compiler,
            &node,
            ToolchainProbeLimits::new(
                std::time::Duration::from_secs(2),
                std::num::NonZeroUsize::new(1024).expect("nonzero test bound"),
            )
            .expect("valid test probe limits"),
        );
        let output = result.expect("isolated test process should observe exact Node path");
        assert_eq!(output.as_ref(), b"Version 5.9.3\n");
        // This probes process isolation only; fixture output never creates a ready toolchain row.
    }

    #[test]
    fn project_admission_reports_missing_node_and_compiler_version_mismatch() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new();
        fixture.install("5.9.3");
        let unavailable = TypeScriptProjectHost::new(None, None, None, None, Fixture::limits());
        assert!(matches!(
            unavailable.admit(&fixture.0),
            Err(TypeScriptProjectHostError::NodeUnavailable { .. })
        ));

        let node_directory = fixture.0.join("node path");
        fs::create_dir_all(&node_directory).expect("create Node directory");
        let node = node_directory.join("node");
        fs::write(
            &node,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf 'v22.0.0\\n'; else printf 'Version 5.9.2\\n'; fi\n",
        )
        .expect("write bounded Node probe fixture");
        fs::set_permissions(&node, fs::Permissions::from_mode(0o755))
            .expect("make fixture executable");
        let mismatch = TypeScriptProjectHost::new(None, Some(node), None, None, Fixture::limits());
        assert!(matches!(
            mismatch.admit(&fixture.0),
            Err(TypeScriptProjectHostError::VersionMismatch {
                expected,
                observed,
                ..
            }) if expected.as_ref() == "5.9.3" && observed.as_ref() == "5.9.2"
        ));
        // Deliberately mismatched fixture output is only an admission refusal test; it never
        // constructs a Ready authority or validates a synthetic TypeScript installation.
    }

    #[test]
    fn explicit_compiler_never_falls_back_to_an_unrelated_project_module_root() {
        let fixture = Fixture::new();
        fixture.install("5.9.3");
        let explicit_compiler = fixture.0.join("global-bin/tsc");
        fs::create_dir_all(explicit_compiler.parent().unwrap()).expect("create compiler dir");
        fs::write(&explicit_compiler, "#!/bin/sh\nexit 0\n").expect("write compiler file");
        let node = fixture.0.join("node");
        fs::write(&node, "Node fixture").expect("write Node file");
        let host = TypeScriptProjectHost::new(
            Some(explicit_compiler),
            Some(node),
            None,
            None,
            Fixture::limits(),
        );
        assert!(matches!(
            host.admit(&fixture.0),
            Err(TypeScriptProjectHostError::ExplicitModuleRootRequired { .. })
        ));
    }

    #[test]
    fn each_project_root_selects_its_own_typescript_installation() {
        let fixture = Fixture::new();
        let first = fixture.0.join("apps/first");
        let second = fixture.0.join("apps/second");
        fs::create_dir_all(&first).expect("create first package root");
        fs::create_dir_all(&second).expect("create second package root");
        let first_modules = first.join("node_modules");
        let second_modules = second.join("node_modules");
        install_at(&first_modules, "5.9.3");
        install_at(&second_modules, "5.8.4");

        let ProjectTypeScriptSearch::Found(first_project) =
            find_project_typescript(&first).expect("inspect first project")
        else {
            panic!("first project TypeScript should be found");
        };
        let ProjectTypeScriptSearch::Found(second_project) =
            find_project_typescript(&second).expect("inspect second project")
        else {
            panic!("second project TypeScript should be found");
        };
        assert_eq!(first_project.version, "5.9.3");
        assert_eq!(second_project.version, "5.8.4");
        assert_ne!(first_project.module_root, second_project.module_root);
    }

    #[test]
    fn workspace_boundary_finds_nearest_root_install_and_excludes_home_parent() {
        let fixture = Fixture::new();
        let home = fixture.0.join("home");
        let workspace = home.join("repo");
        let app = workspace.join("apps/web");
        fs::create_dir_all(&app).expect("create selected app root");
        fs::create_dir_all(&home).expect("create synthetic home");
        fs::write(
            workspace.join("package.json"),
            r#"{"name":"workspace","private":true,"workspaces":["apps/*"]}"#,
        )
        .expect("write npm workspace manifest");
        install_at(&workspace.join("node_modules"), "5.9.3");
        install_at(&home.join("node_modules"), "5.8.4");

        let ProjectTypeScriptSearch::Found(project) =
            find_project_typescript_with_home(&app, Some(&home))
                .expect("admit nearest workspace installation")
        else {
            panic!("workspace TypeScript should be found");
        };
        assert_eq!(project.version, "5.9.3");
        assert_eq!(project.workspace.as_ref().unwrap().root.as_ref(), workspace);

        let unrelated = home.join("loose-project");
        fs::create_dir_all(&unrelated).expect("create loose project");
        assert!(matches!(
            find_project_typescript_with_home(&unrelated, Some(&home))
                .expect("home install is outside the project search boundary"),
            ProjectTypeScriptSearch::NotFound
        ));
    }

    #[test]
    fn malformed_workspace_and_typescript_configurations_are_typed_refusals() {
        let fixture = Fixture::new();
        fs::write(
            fixture.0.join("package.json"),
            r#"{"name":"broken","workspaces":{"packages":"apps/*"}}"#,
        )
        .expect("write malformed workspace declaration");
        assert!(matches!(
            discover_workspace_boundary(&fixture.0, None),
            Err(TypeScriptProjectHostError::WorkspaceConfigInvalid { .. })
        ));

        fs::remove_file(fixture.0.join("package.json"))
            .expect("remove malformed workspace declaration");
        let modules = fixture.install("5.9.3");
        fs::write(fixture.0.join("tsconfig.json"), r#"{"compilerOptions": }"#)
            .expect("write malformed TypeScript config");
        let ProjectTypeScriptSearch::Found(project) =
            find_project_typescript(&fixture.0).expect("discover selected TypeScript")
        else {
            panic!("local TypeScript package should be found");
        };
        let module_root = fs::canonicalize(modules).expect("canonical module root");
        assert!(matches!(
            collect_project_configs(&fixture.0, &fixture.0, &module_root, &[]),
            Err(TypeScriptProjectHostError::ConfigInvalid { .. })
        ));
        assert_eq!(project.version, "5.9.3");
    }

    #[test]
    fn config_candidates_preserve_jsonc_edges_and_include_config_closure() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let member = fixture.0.join("packages/member");
        fs::create_dir_all(member.join("src")).expect("create referenced project");
        fs::create_dir_all(fixture.0.join("configs")).expect("create base config directory");
        fs::write(
            fixture.0.join("tsconfig.json"),
            "{\n // root candidate\n \"extends\": \"./configs/base\",\n \"references\": [{\"path\": \"./packages/member\"}],\n}\n",
        )
        .expect("write JSONC config candidate");
        fs::write(
            fixture.0.join("configs/base.json"),
            "{\"compilerOptions\":{}}",
        )
        .expect("write base config closure file");
        fs::write(member.join("tsconfig.json"), "{\"compilerOptions\":{}}")
            .expect("write referenced project config");
        let module_root = fs::canonicalize(modules).expect("canonical module root");

        let configs = collect_project_configs(&fixture.0, &fixture.0, &module_root, &[])
            .expect("capture config candidates and closure");
        assert_eq!(
            configs
                .iter()
                .filter(|config| config.project_candidate)
                .count(),
            2
        );
        assert_eq!(configs.len(), 3);
        let root =
            fs::canonicalize(fixture.0.join("tsconfig.json")).expect("canonical root config");
        let base =
            fs::canonicalize(fixture.0.join("configs/base.json")).expect("canonical base config");
        let member_config =
            fs::canonicalize(member.join("tsconfig.json")).expect("canonical referenced config");
        let root_input = configs
            .iter()
            .find(|config| config.path.as_ref() == root)
            .expect("root candidate is retained");
        assert_eq!(root_input.extends.len(), 1);
        assert_eq!(root_input.extends[0].as_ref(), base.as_path());
        assert_eq!(root_input.references.len(), 1);
        assert_eq!(root_input.references[0].as_ref(), member_config.as_path());
    }

    #[test]
    fn config_graph_cycles_and_workspace_escapes_are_typed_refusals() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let module_root = fs::canonicalize(modules).expect("canonical module root");

        fs::write(
            fixture.0.join("tsconfig.json"),
            r#"{"extends":"./configs/base","references":[{"path":"./child"}]}"#,
        )
        .expect("write root config");
        fs::create_dir_all(fixture.0.join("configs")).expect("create config closure");
        fs::write(
            fixture.0.join("configs/base.json"),
            r#"{"references":[{"path":"../tsconfig.json"}]}"#,
        )
        .expect("write config with mixed-edge cycle");
        fs::create_dir_all(fixture.0.join("child")).expect("create child project");
        fs::write(fixture.0.join("child/tsconfig.json"), "{}").expect("write child config");
        assert!(matches!(
            collect_project_configs(&fixture.0, &fixture.0, &module_root, &[]),
            Err(TypeScriptProjectHostError::ConfigCycle { .. })
        ));

        fs::write(
            fixture.0.join("tsconfig.json"),
            r#"{"extends":"../outside-config.json"}"#,
        )
        .expect("write escaping extends edge");
        fs::write(
            fixture
                .0
                .parent()
                .expect("fixture parent")
                .join("outside-config.json"),
            "{}",
        )
        .expect("write outside config");
        assert!(matches!(
            collect_project_configs(&fixture.0, &fixture.0, &module_root, &[]),
            Err(TypeScriptProjectHostError::ConfigEscapesBoundary { .. })
        ));
    }

    #[test]
    fn package_config_extends_can_use_the_exact_admitted_module_root() {
        let fixture = Fixture::new();
        let app = fixture.0.join("repo/apps/web");
        let modules = fixture.0.join("repo/node_modules");
        fs::create_dir_all(&app).expect("create app package");
        install_at(&modules, "5.9.3");
        let base = modules.join("@tsconfig/node/tsconfig.json");
        fs::create_dir_all(base.parent().expect("base config parent"))
            .expect("create installed config package");
        fs::write(&base, r#"{"compilerOptions":{"strict":true}}"#)
            .expect("write installed package config");
        fs::write(app.join("tsconfig.json"), r#"{"extends":"@tsconfig/node"}"#)
            .expect("write app config");

        let workspace_root = fs::canonicalize(&app).expect("canonical selected project root");
        let module_root = fs::canonicalize(&modules).expect("canonical selected module root");
        let configs = collect_project_configs(&app, &workspace_root, &module_root, &[])
            .expect("resolve package config through exact selected module root");
        let app_config = fs::canonicalize(app.join("tsconfig.json")).expect("canonical app config");
        let base = fs::canonicalize(base).expect("canonical package config");
        let input = configs
            .iter()
            .find(|config| config.path.as_ref() == app_config)
            .expect("selected app config is retained");
        assert_eq!(input.extends.len(), 1);
        assert_eq!(input.extends[0].as_ref(), base.as_path());
        assert!(configs.iter().any(|config| config.path.as_ref() == base));
    }

    #[test]
    fn angular_build_tsconfig_is_an_explicit_program_candidate() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        fs::write(
            fixture.0.join("angular.json"),
            r#"{"projects":{"app":{"architect":{"build":{"options":{"tsConfig":"tsconfig.app.json"}}}}}}"#,
        )
        .expect("write Angular build configuration");
        fs::write(fixture.0.join("tsconfig.json"), "{\"compilerOptions\":{}}")
            .expect("write workspace default config");
        fs::write(
            fixture.0.join("tsconfig.app.json"),
            "{\"files\":[\"src/main.ts\"]}",
        )
        .expect("write selected Angular app config");
        let module_root = fs::canonicalize(modules).expect("canonical module root");
        let (selected, angular_snapshot) =
            angular_build_config_paths(&fixture.0, &module_root).expect("read Angular target");
        assert_eq!(selected.len(), 1);
        assert!(angular_snapshot.is_some());
        let configs = collect_project_configs(&fixture.0, &fixture.0, &module_root, &selected)
            .expect("capture Angular candidate and its closure");
        let app_config =
            fs::canonicalize(fixture.0.join("tsconfig.app.json")).expect("canonical app config");
        let input = configs
            .iter()
            .find(|config| config.path.as_ref() == app_config)
            .expect("app config is present");
        assert!(input.project_candidate);
        assert!(input.selected_build_config);
    }

    #[test]
    fn admitted_project_source_loader_records_and_revalidates_content_identity() {
        let fixture = Fixture::new();
        fixture.install("5.9.3");
        let ProjectTypeScriptSearch::Found(project) =
            find_project_typescript(&fixture.0).expect("discover project TypeScript")
        else {
            panic!("local TypeScript package should be found");
        };
        let node = fixture.0.join("node");
        fs::write(&node, b"node witness").expect("write fake Node bytes");
        let node = fs::canonicalize(node).expect("canonical Node");
        let compiler = project.compiler.clone();
        let module_root = project.module_root.clone();
        let package_root = fs::canonicalize(module_root.join("typescript"))
            .expect("canonical TypeScript module root");
        let witness = TypeScriptProjectWitness::capture(
            &fixture.0,
            None,
            &project,
            &compiler,
            &node,
            &module_root,
            &package_root,
            project.workspace.as_ref(),
        )
        .expect("capture project witness");
        let source = fixture.0.join("src/main.ts");
        fs::create_dir_all(source.parent().unwrap()).expect("create source directory");
        fs::write(&source, b"export const value = 1;").expect("write source bytes");
        let source = fs::canonicalize(source).expect("canonical source");
        let input = witness
            .load_source(&source)
            .expect("load source by capability");
        assert_eq!(
            input.content_id,
            ContentId::<SourceFactDomain>::from_canonical_bytes(&input.bytes)
        );
        assert!(input.path.starts_with(&fixture.0));
        witness
            .validate_current()
            .expect("unchanged source witness");

        fs::write(&source, b"export const value = 2;").expect("change loaded source");
        assert!(matches!(
            witness.validate_current(),
            Err(TypeScriptProjectHostError::WitnessChanged { .. })
        ));
        let outside = fixture.0.parent().unwrap().join(format!(
            "typescript-outside-capability-{}.ts",
            std::process::id()
        ));
        fs::write(&outside, b"export {}; ").expect("write out-of-capability file");
        assert!(matches!(
            witness.load_source(&outside),
            Err(TypeScriptProjectHostError::SourceOutsideCapability { .. })
        ));
        fs::remove_file(outside).expect("remove outside file");
    }

    #[test]
    fn source_loader_admits_pnpm_store_symlinks_and_rejects_outside_targets() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let ProjectTypeScriptSearch::Found(project) =
            find_project_typescript(&fixture.0).expect("discover project TypeScript")
        else {
            panic!("local TypeScript package should be found");
        };
        let node = fixture.0.join("node");
        fs::write(&node, b"node witness").expect("write fake Node bytes");
        let node = fs::canonicalize(node).expect("canonical Node");
        let package_root = fs::canonicalize(modules.join("typescript"))
            .expect("canonical TypeScript package root");
        let witness = TypeScriptProjectWitness::capture(
            &fixture.0,
            None,
            &project,
            &project.compiler,
            &node,
            &project.module_root,
            &package_root,
            project.workspace.as_ref(),
        )
        .expect("capture project witness");

        let package = modules.join(".pnpm/dep@1.0.0/node_modules/@scope/dep");
        fs::create_dir_all(&package).expect("create pnpm dependency package");
        fs::write(package.join("index.d.ts"), "export interface Value {}")
            .expect("write dependency declaration");
        fs::create_dir_all(modules.join("@scope")).expect("create scoped dependency alias");
        symlink(&package, modules.join("@scope/dep")).expect("link pnpm dependency");

        let dependency = witness
            .load_source(&modules.join("@scope/dep/index.d.ts"))
            .expect("admit canonical dependency under the selected pnpm store");
        assert!(dependency.path.starts_with(&project.module_root));
        assert_eq!(
            dependency.content_id,
            ContentId::<SourceFactDomain>::from_canonical_bytes(&dependency.bytes)
        );
        witness
            .validate_current()
            .expect("unchanged pnpm dependency");

        let outside = fixture.0.parent().expect("fixture parent").join(format!(
            "typescript-pnpm-outside-{}.d.ts",
            std::process::id()
        ));
        fs::write(&outside, "export {}; ").expect("write external declaration");
        fs::remove_file(modules.join("@scope/dep")).expect("remove pnpm alias");
        fs::create_dir(modules.join("@scope/dep")).expect("create dependency alias directory");
        symlink(&outside, modules.join("@scope/dep/index.d.ts")).expect("link outside declaration");
        assert!(matches!(
            witness.load_source(&modules.join("@scope/dep/index.d.ts")),
            Err(TypeScriptProjectHostError::SourceOutsideCapability { .. })
        ));
        fs::remove_file(outside).expect("remove external declaration");
    }

    #[test]
    fn resolver_capability_records_positive_negative_and_directory_witnesses() {
        let fixture = Fixture::new();
        fixture.install("5.9.3");
        let project_root = fs::canonicalize(&fixture.0).expect("canonical project root");
        fs::create_dir_all(project_root.join("src")).expect("create source directory");
        let source = project_root.join("src/main.ts");
        fs::write(&source, b"export const answer = 42;").expect("write source");
        let missing = project_root.join("src/missing.d.ts");
        let ProjectTypeScriptSearch::Found(project) =
            find_project_typescript(&project_root).expect("discover project TypeScript")
        else {
            panic!("local TypeScript package should be found");
        };
        let node = project_root.join("node");
        fs::write(&node, b"node witness").expect("write fake Node bytes");
        let node = fs::canonicalize(node).expect("canonical Node");
        let package_root = fs::canonicalize(project.module_root.join("typescript"))
            .expect("canonical TypeScript package root");
        let witness = TypeScriptProjectWitness::capture(
            &project_root,
            None,
            &project,
            &project.compiler,
            &node,
            &project.module_root,
            &package_root,
            project.workspace.as_ref(),
        )
        .expect("capture project witness");
        let mut capability = TypeScriptResolverCapability {
            witness: &witness,
            observations: ResolverObservationLedger::default(),
        };

        let loaded = capability
            .try_load_source(&source)
            .expect("read admitted source")
            .expect("source is present");
        assert_eq!(
            loaded.content_id,
            ContentId::<SourceFactDomain>::from_canonical_bytes(&loaded.bytes)
        );
        assert!(
            capability
                .try_load_source(&missing)
                .expect("observe absent declaration")
                .is_none()
        );
        assert!(
            capability
                .directory_exists(&project_root.join("src"))
                .expect("observe source directory")
        );
        let entries = capability
            .read_directory(&project_root.join("src"), &[".ts"], false, 0, 8)
            .expect("read bounded TypeScript directory");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].canonical_path.as_deref(), Some(source.as_path()));
        assert!(capability.loaded_sources().contains_key(&source));
        let observations = capability.observations();
        assert!(observations.iter().any(|observation| matches!(
            observation,
            TypeScriptResolverObservationRef::MissingPath { path, .. } if *path == missing
        )));
        assert!(observations.iter().any(|observation| matches!(
            observation,
            TypeScriptResolverObservationRef::Directory { path, .. } if *path == project_root.join("src")
        )));
        let fingerprint = capability.resolver_witness().expect("hash observations");
        assert_ne!(fingerprint, [0; 32]);
        capability
            .validate_current()
            .expect("admitted resolver observations remain current");

        fs::write(&missing, b"declare const missing: string;")
            .expect("materialize formerly missing candidate");
        assert!(matches!(
            capability.validate_current(),
            Err(TypeScriptProjectHostError::WitnessChanged { .. })
        ));
    }

    #[test]
    fn admitted_node_version_is_probed_once_and_its_binary_stays_witnessed() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new();
        let node = fixture.0.join("node");
        fs::write(
            &node,
            "#!/bin/sh\nprintf x >> \"$0.count\"\nprintf 'v22.0.0\\n'\n",
        )
        .expect("write Node probe fixture");
        fs::set_permissions(&node, fs::Permissions::from_mode(0o755))
            .expect("make Node probe fixture executable");
        let host =
            TypeScriptProjectHost::new(None, Some(node.clone()), None, None, Fixture::limits());
        let node = fs::canonicalize(node).expect("canonical Node");

        let first = host
            .admit_node_runtime(&node)
            .expect("admit and version-probe Node");
        let second = host
            .admit_node_runtime(&node)
            .expect("reuse admitted Node identity");
        assert_eq!(first.version.as_ref(), b"v22.0.0\n");
        assert_eq!(first.version, second.version);
        assert_eq!(
            fs::read(node.with_extension("count")).expect("probe count"),
            b"x"
        );

        fs::write(&node, "#!/bin/sh\nprintf 'v24.0.0\\n'\n").expect("replace selected Node binary");
        assert!(matches!(
            host.admit_node_runtime(&node),
            Err(TypeScriptProjectHostError::WitnessChanged { .. })
        ));
    }

    #[test]
    fn explicit_compiler_uses_its_matching_module_root_ahead_of_project_installation() {
        use std::os::unix::fs::PermissionsExt;

        for mutation in [
            "unchanged",
            "selected-content",
            "discovered-content",
            "selected-retarget",
            "node-replaced",
        ] {
            let fixture = Fixture::new();
            fixture.install("5.9.3");
            let explicit_modules = fixture.0.join("explicit/node_modules");
            install_at(&explicit_modules, "5.8.4");
            let compiler = fs::canonicalize(explicit_modules.join("typescript/bin/tsc"))
                .expect("canonical explicit compiler");
            let node_directory = fixture.0.join("host runtime");
            fs::create_dir_all(&node_directory).expect("create Node parent");
            let node = node_directory.join("node");
            fs::write(
                &node,
                "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf 'v22.0.0\\n'; elif [ \"$2\" = \"--version\" ]; then printf 'Version 5.8.4\\n'; else exit 9; fi\n",
            )
            .expect("write deterministic Node probe fixture");
            fs::set_permissions(&node, fs::Permissions::from_mode(0o755))
                .expect("make Node probe executable");

            let admitted = TypeScriptProjectHost::new(
                Some(compiler.clone()),
                Some(node.clone()),
                None,
                None,
                Fixture::limits(),
            )
            .admit(&fixture.0)
            .expect("admit explicit compiler and matching installation")
            .expect("local project installation is present");
            let expected_module_root =
                fs::canonicalize(&explicit_modules).expect("canonical selected module root");
            assert_eq!(admitted.compiler.as_ref(), compiler);
            assert_eq!(
                admitted.compiler_origin,
                TypeScriptSelectionOrigin::ExplicitConfiguration
            );
            assert_eq!(
                admitted.inputs().typescript_module_root,
                expected_module_root
            );
            assert_eq!(admitted.inputs().compiler_version, b"Version 5.8.4\n");
            assert_eq!(
                admitted.inputs().node_origin,
                TypeScriptSelectionOrigin::ExplicitConfiguration
            );
            match mutation {
                "unchanged" => {
                    let toolchain = admitted
                        .resolved_toolchain()
                        .expect("bind selected compiler package lease");
                    let hash_bytes_before =
                        crate::application::executable_content_hash_bytes_for_test();
                    for _ in 0..3 {
                        toolchain
                            .validate_invocation()
                            .expect("package lease checks only the same selected objects");
                    }
                    assert_eq!(
                        crate::application::executable_content_hash_bytes_for_test(),
                        hash_bytes_before,
                        "three source launches must not hash Node or the TypeScript package again",
                    );
                    admitted
                        .witness
                        .validate_current()
                        .expect("unchanged separate authorities");
                }
                "selected-content" | "discovered-content" => {
                    let root = if mutation == "selected-content" {
                        explicit_modules.clone()
                    } else {
                        fixture.0.join("node_modules")
                    };
                    let compiler_path = root.join("typescript/bin/tsc");
                    let mut changed_bytes = fs::read(&compiler_path).expect("read compiler bytes");
                    changed_bytes[0] ^= 1;
                    fs::write(&compiler_path, changed_bytes)
                        .expect("change compiler closure without replacing its file object");
                    if mutation == "selected-content" {
                        let toolchain = admitted
                            .resolved_toolchain()
                            .expect("bind selected compiler package lease");
                        toolchain.validate_invocation().expect(
                            "per-spawn lease checks the same object without a duplicate full hash",
                        );
                    }
                    assert!(matches!(
                        admitted.witness.validate_current(),
                        Err(TypeScriptProjectHostError::WitnessChanged { .. })
                    ));
                }
                "selected-retarget" => {
                    let package = explicit_modules.join("typescript");
                    let target = explicit_modules.join("retargeted-typescript");
                    fs::rename(&package, &target).expect("move selected installation");
                    symlink(&target, &package).expect("retarget selected installation");
                    assert!(matches!(
                        admitted.witness.validate_current(),
                        Err(TypeScriptProjectHostError::WitnessChanged { .. })
                    ));
                }
                "node-replaced" => {
                    let toolchain = admitted
                        .resolved_toolchain()
                        .expect("bind selected compiler package lease");
                    let moved = node.with_extension("replaced");
                    fs::rename(&node, &moved).expect("move admitted Node");
                    fs::write(&node, b"replacement Node object").expect("install replacement Node");
                    assert!(matches!(
                        toolchain.validate_invocation(),
                        Err(crate::driver::NativeInvocationError::Changed {
                            role: crate::driver::NativeInvocationFileRole::Interpreter,
                            ..
                        })
                    ));
                    assert!(matches!(
                        admitted.witness.validate_current(),
                        Err(TypeScriptProjectHostError::WitnessChanged { .. })
                    ));
                }
                _ => unreachable!(),
            }
        }
    }

    fn install_at(modules: &Path, version: &str) {
        let package = modules.join("typescript");
        fs::create_dir_all(package.join("bin")).expect("create package");
        fs::write(
            package.join("package.json"),
            format!("{{\"name\":\"typescript\",\"version\":\"{version}\"}}"),
        )
        .expect("write package manifest");
        fs::write(package.join("bin/tsc"), "#!/usr/bin/env node\n").expect("write compiler");
        fs::create_dir_all(modules.join(".bin")).expect("create package manager bin");
        symlink(package.join("bin/tsc"), modules.join(".bin/tsc")).expect("link package compiler");
    }
}
