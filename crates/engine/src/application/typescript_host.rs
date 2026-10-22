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

#[path = "typescript_host/snapshot_resolver.rs"]
mod snapshot_resolver;

use crate::application::{ToolchainProbeError, ToolchainProbeLimits};
use crate::driver::{ResolvedToolchain, ToolchainResolutionError};

pub(crate) const MAX_PROJECT_ANCESTORS: usize = 32;
const MAX_PACKAGE_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_PNPM_WORKSPACE_BYTES: usize = 64 * 1024;
pub(super) const MAX_PROJECT_CONFIG_FILES: usize = 256;
const MAX_PROJECT_CONFIG_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ANGULAR_WORKSPACE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_RESOLVED_SOURCE_FILES: usize = 4096;
const MAX_RESOLVED_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SOURCE_FILE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_TYPESCRIPT_PACKAGE_FILES: usize = super::toolchain_probe::MAX_TYPESCRIPT_MODULE_FILES;
const MAX_TYPESCRIPT_PACKAGE_ENTRIES: usize = super::toolchain_probe::MAX_TYPESCRIPT_MODULE_ENTRIES;
const MAX_TYPESCRIPT_PACKAGE_DEPTH: usize = super::toolchain_probe::MAX_TYPESCRIPT_MODULE_DEPTH;
const MAX_TYPESCRIPT_PACKAGE_BYTES: u64 = super::toolchain_probe::MAX_TYPESCRIPT_MODULE_BYTES;
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
enum ConfigDependencyKind {
    Extends,
    References,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FileIdentity {
    pub(crate) first: u64,
    pub(crate) second: super::toolchain_probe::FileObjectId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileSnapshot {
    path: Box<Path>,
    identity: FileIdentity,
    length: u64,
    digest: [u8; 32],
    content_id: ContentId<SourceFactDomain>,
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
            && self.matches_selected_package()
    }

    fn matches_selected_package(&self) -> bool {
        self.package_root.starts_with(&self.module_root)
            && fs::canonicalize(self.module_root.join("typescript"))
                .is_ok_and(|package| package.as_path() == self.package_root.as_ref())
            && is_module_tsc_script(&self.compiler_path, &self.module_root)
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
        if !self.matches_selected_package() {
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

/// Raw candidates are witnessed without granting their dependency graph authority.
/// Inherited options do not inherit project references from a base configuration.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum TypeScriptConfigDependencyAuthority {
    Unselected,
    Inheritance,
    Program,
}

#[derive(Clone, Debug)]
pub(crate) struct TypeScriptConfigInput {
    pub(crate) path: Box<Path>,
    pub(crate) bytes: Box<[u8]>,
    pub(crate) content_id: ContentId<SourceFactDomain>,
    pub(crate) identity: FileIdentity,
    pub(crate) project_candidate: bool,
    pub(crate) selected_build_config: bool,
    pub(crate) dependency_authority: TypeScriptConfigDependencyAuthority,
    pub(crate) extends: Box<[Box<Path>]>,
    pub(crate) references: Box<[Box<Path>]>,
}

#[derive(Clone, Debug)]
pub(crate) struct TypeScriptProjectInputs<'a> {
    pub(crate) package_root: &'a Path,
    pub(crate) workspace_root: &'a Path,
    pub(crate) authority: TypeScriptProjectInputAuthority<'a>,
    pub(crate) fingerprint: [u8; 32],
    pub(crate) config_candidates: &'a [TypeScriptConfigInput],
    pub(crate) selected_build_config_paths: &'a [Box<Path>],
    pub(crate) typescript_files: &'a [TypeScriptFileInput],
    witness: &'a TypeScriptProjectWitness,
}

#[derive(Clone, Debug)]
pub(crate) enum TypeScriptProjectInputAuthority<'a> {
    Native,
    CompilerApi(TypeScriptCompilerApiInputs<'a>),
}

#[derive(Clone, Debug)]
pub(crate) struct TypeScriptCompilerApiInputs<'a> {
    pub(crate) typescript_module_root: &'a Path,
    pub(crate) compiler_path: &'a Path,
    pub(crate) compiler_version: &'a [u8],
    pub(crate) compiler_origin: TypeScriptSelectionOrigin,
    pub(crate) node_path: &'a Path,
    pub(crate) node_version: &'a [u8],
    pub(crate) node_origin: TypeScriptSelectionOrigin,
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
    /// Runtime/package resources selected from the sealed standalone distribution.
    ValidatedStandalonePackage = 8,
    /// A host-installed compiler selected once and carried by the closed locald snapshot.
    InstalledHostSelection = 7,
}

impl TypeScriptProjectInputs<'_> {
    pub(crate) fn compiler_api(
        &self,
    ) -> Result<&TypeScriptCompilerApiInputs<'_>, TypeScriptProjectHostError> {
        match &self.authority {
            TypeScriptProjectInputAuthority::CompilerApi(inputs) => Ok(inputs),
            TypeScriptProjectInputAuthority::Native => {
                Err(TypeScriptProjectHostError::CompilerApiBridge {
                    message: "native TSZ authority cannot launch a Compiler API host".into(),
                })
            }
        }
    }

    pub(crate) fn validated_host(
        &self,
    ) -> Result<ValidatedTypeScriptHost<'_>, TypeScriptProjectHostError> {
        self.witness.validated()
    }
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
    /// Kind observed through `stat`, which follows a symbolic link like Node's
    /// TypeScript system adapter does while enumerating a directory.
    pub(crate) effective_kind: Option<TypeScriptDirectoryEntryKind>,
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

#[derive(Debug)]
struct CapturedPeerManifest {
    source_path: Box<Path>,
    content_id: ContentId<SourceFactDomain>,
    candidates: Box<[Box<Path>]>,
    activated: bool,
}

#[derive(Debug, Default)]
struct ResolverObservationLedger {
    loaded_sources: std::collections::BTreeMap<PathBuf, TypeScriptFileInput>,
    missing_paths: std::collections::BTreeMap<PathBuf, Option<PathBuf>>,
    directories: std::collections::BTreeMap<PathBuf, DirectorySnapshot>,
    external_negative_parents: std::collections::BTreeMap<PathBuf, RealpathSnapshot>,
    external_absent_directories: std::collections::BTreeMap<PathBuf, RealpathSnapshot>,
    captured_peer_manifests: std::collections::BTreeMap<PathBuf, CapturedPeerManifest>,
    peer_directory_candidates: std::collections::BTreeMap<PathBuf, PathBuf>,
    /// Derived target -> lexical query; the underlying realpath snapshot remains the authority.
    realpath_queries_by_target: std::collections::BTreeMap<PathBuf, PathBuf>,
    closure_replay_work_units: u64,
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
            .saturating_add(self.external_negative_parents.len())
            .saturating_add(self.external_absent_directories.len())
            .saturating_add(self.captured_peer_manifests.len())
            .saturating_add(self.peer_directory_candidates.len())
            .saturating_add(self.realpath_queries_by_target.len())
    }
}

/// Owned proof of the files, misses, directories, and realpaths read by one TypeScript program.
/// It survives the resolver capability and can be replayed against the selected project witness.
#[derive(Debug)]
pub(crate) struct TypeScriptResolverWitness {
    project_fingerprint: [u8; 32],
    digest: [u8; 32],
    observations: ResolverObservationLedger,
}

/// Borrowed proof issued only by a fresh full host validation. It is local to
/// one admission/publication boundary; leaf ledgers still validate every read.
pub(crate) struct ValidatedTypeScriptHost<'a> {
    witness: &'a TypeScriptProjectWitness,
    sources: std::collections::BTreeMap<PathBuf, FileSnapshot>,
}

impl TypeScriptResolverWitness {
    pub(crate) fn retained_bytes(&self) -> usize {
        // Admission tracks path/directory payload bytes, but not every duplicate
        // map key or tree node. Charge duplicate payload plus generous per-node
        // and per-directory-entry storage as well as all owned source bytes.
        self.observations
            .retained_metadata_bytes
            .saturating_mul(2)
            .saturating_add(self.observations.observation_count().saturating_mul(1024))
            .saturating_add(self.observations.directory_entries.saturating_mul(256))
            .saturating_add(
                usize::try_from(self.observations.loaded_source_bytes).unwrap_or(usize::MAX),
            )
    }

    pub(crate) const fn digest(&self) -> &[u8; 32] {
        &self.digest
    }

    pub(crate) fn validate_current(
        &self,
        witness: &TypeScriptProjectWitness,
    ) -> Result<(), TypeScriptProjectHostError> {
        self.validate_with_host(&witness.validated()?)
    }

    pub(crate) fn validate_with_host(
        &self,
        host: &ValidatedTypeScriptHost<'_>,
    ) -> Result<(), TypeScriptProjectHostError> {
        self.validate_with_host_controlled(host, &mut || Ok(()))
    }

    pub(crate) fn validate_with_host_controlled(
        &self,
        host: &ValidatedTypeScriptHost<'_>,
        control: &mut dyn FnMut() -> Result<(), TypeScriptProjectHostError>,
    ) -> Result<(), TypeScriptProjectHostError> {
        control()?;
        let witness = host.witness;
        if self.project_fingerprint != witness.fingerprint {
            return Err(TypeScriptProjectHostError::WitnessChanged {
                path: witness.project_root.clone(),
            });
        }
        validate_resolver_observations_with_host_controlled(
            witness,
            &self.observations,
            Some(host),
            control,
        )
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
        self.witness.record_source_snapshot(&canonical, &snapshot)?;
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
        self.index_captured_peer_manifest(&canonical)?;
        Ok(self.observations.loaded_sources.get(&canonical))
    }

    /// Classifies a path only after recording its exact lexical/realpath object.
    /// The same record is revalidated before semantic publication.
    pub(crate) fn native_path_kind(
        &mut self,
        path: &Path,
        lexical_kind: bool,
    ) -> Result<Option<TypeScriptDirectoryEntryKind>, TypeScriptProjectHostError> {
        let Some(canonical) = self.realpath(path)? else {
            return Ok(None);
        };
        let metadata = if lexical_kind {
            fs::symlink_metadata(path)
        } else {
            fs::metadata(&canonical)
        }
        .map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: path.to_path_buf().into_boxed_path(),
            source,
        })?;
        Ok(Some(if metadata.file_type().is_symlink() {
            TypeScriptDirectoryEntryKind::Symlink
        } else if metadata.is_file() {
            TypeScriptDirectoryEntryKind::RegularFile
        } else if metadata.is_dir() {
            TypeScriptDirectoryEntryKind::Directory
        } else {
            TypeScriptDirectoryEntryKind::Other
        }))
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

    /// Replays an exact negative directory result from the selected compiler.
    /// An external peer miss must be derivable from an already captured package
    /// manifest and package-directory realpath. It witnesses absence and lexical
    /// parent identity only, never a source read or positive directory capability.
    pub(crate) fn observe_absent_directory(
        &mut self,
        path: &Path,
    ) -> Result<(), TypeScriptProjectHostError> {
        let lexical = normalize_absolute_path(path).ok_or_else(|| {
            TypeScriptProjectHostError::SourceOutsideCapability {
                path: path.to_path_buf().into_boxed_path(),
            }
        })?;
        if self.witness.path_is_admitted(&lexical)
            || self.witness.external_negative_module_candidate(&lexical)
        {
            if self.directory_exists(&lexical)? {
                return Err(TypeScriptProjectHostError::WitnessChanged {
                    path: lexical.into_boxed_path(),
                });
            }
            return Ok(());
        }
        if !captured_peer_directory_candidate(&lexical, self.witness, &self.observations) {
            return Err(TypeScriptProjectHostError::SourceOutsideCapability {
                path: lexical.into_boxed_path(),
            });
        }
        let parent = capture_external_negative_parent(&lexical)?;
        if let Some(previous) = self.observations.external_absent_directories.get(&lexical) {
            if previous != &parent {
                return Err(TypeScriptProjectHostError::WitnessChanged {
                    path: lexical.into_boxed_path(),
                });
            }
            return Ok(());
        }
        // Both the typed parent proof and the miss enter the bounded ledger.
        if self.observations.observation_count().saturating_add(2) > MAX_RESOLVER_OBSERVATIONS {
            return Err(TypeScriptProjectHostError::ResolverObservationLimit {
                observed: self.observations.observation_count().saturating_add(2),
                maximum: MAX_RESOLVER_OBSERVATIONS,
            });
        }
        let cost = lexical
            .as_os_str()
            .as_encoded_bytes()
            .len()
            .saturating_mul(2)
            .saturating_add(parent.path.as_os_str().as_encoded_bytes().len())
            .saturating_add(
                parent
                    .canonical_path
                    .as_deref()
                    .map_or(0, |p| p.as_os_str().as_encoded_bytes().len()),
            );
        ensure_metadata_capacity(&self.observations, cost, &lexical)?;
        self.observations.retained_metadata_bytes += cost;
        self.observations
            .missing_paths
            .insert(lexical.clone(), None);
        self.observations
            .external_absent_directories
            .insert(lexical, parent);
        Ok(())
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

    /// Admits one exact directory enumeration observed by the TypeScript system
    /// adapter and returns its canonical directory key. No recursive walk or
    /// extension filtering is performed here.
    pub(crate) fn observe_program_directory(
        &mut self,
        path: &Path,
    ) -> Result<Option<PathBuf>, TypeScriptProjectHostError> {
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
        let metadata =
            fs::metadata(&canonical).map_err(|source| TypeScriptProjectHostError::PackagePath {
                path: canonical.clone().into_boxed_path(),
                source,
            })?;
        self.record_realpath(lexical, Some(canonical.clone()), identity)?;
        if !metadata.is_dir() {
            return Ok(None);
        }
        self.observe_directory(&canonical)?;
        Ok(Some(canonical))
    }

    pub(crate) fn observed_program_directory_entries(
        &self,
        canonical_path: &Path,
    ) -> Option<&[TypeScriptDirectoryEntry]> {
        self.observations
            .directories
            .get(canonical_path)
            .map(|snapshot| snapshot.entries.as_ref())
    }

    pub(crate) fn program_directory_entry_is_admitted(
        &self,
        entry: &TypeScriptDirectoryEntry,
    ) -> bool {
        entry
            .canonical_path
            .as_deref()
            .is_none_or(|path| self.witness.path_is_admitted(path))
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
            if self
                .observations
                .external_absent_directories
                .contains_key(path)
            {
                update_external_absent_directory_path(&mut digest, path)?;
            } else {
                update_logical_path(&mut digest, self.witness, path)?;
            }
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
        for (path, parent) in &self.observations.external_negative_parents {
            digest.update(&[5]);
            update_logical_path(&mut digest, self.witness, path)?;
            for parent_path in [Some(parent.path.as_ref()), parent.canonical_path.as_deref()] {
                if let Some(parent_path) = parent_path {
                    let bytes = parent_path.as_os_str().as_encoded_bytes();
                    update_len(&mut digest, bytes.len());
                    digest.update(bytes);
                }
            }
            if let Some(identity) = parent.identity {
                digest.update(&identity.first.to_le_bytes());
                digest.update(&identity.second.to_le_bytes());
            }
        }
        for (path, parent) in &self.observations.external_absent_directories {
            digest.update(&[6]);
            update_external_absent_directory_path(&mut digest, path)?;
            for parent_path in [Some(parent.path.as_ref()), parent.canonical_path.as_deref()] {
                if let Some(parent_path) = parent_path {
                    let bytes = parent_path.as_os_str().as_encoded_bytes();
                    update_len(&mut digest, bytes.len());
                    digest.update(bytes);
                }
            }
            if let Some(identity) = parent.identity {
                digest.update(&identity.first.to_le_bytes());
                digest.update(&identity.second.to_le_bytes());
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
                digest.update(&[match entry.effective_kind {
                    Some(TypeScriptDirectoryEntryKind::RegularFile) => 1,
                    Some(TypeScriptDirectoryEntryKind::Directory) => 2,
                    Some(TypeScriptDirectoryEntryKind::Symlink) => 3,
                    Some(TypeScriptDirectoryEntryKind::Other) => 4,
                    None => 0,
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

    pub(crate) fn seal(&mut self) -> Result<TypeScriptResolverWitness, TypeScriptProjectHostError> {
        let witness = self.witness;
        self.seal_with_host(&witness.validated()?)
    }

    pub(crate) fn seal_with_host(
        &mut self,
        host: &ValidatedTypeScriptHost<'_>,
    ) -> Result<TypeScriptResolverWitness, TypeScriptProjectHostError> {
        if !std::ptr::eq(self.witness, host.witness) {
            return Err(TypeScriptProjectHostError::WitnessChanged {
                path: self.witness.project_root.clone(),
            });
        }
        validate_resolver_observations_with_host(self.witness, &self.observations, Some(host))?;
        let digest = self.resolver_witness()?;
        Ok(TypeScriptResolverWitness {
            project_fingerprint: self.witness.fingerprint,
            digest,
            observations: std::mem::take(&mut self.observations),
        })
    }

    pub(crate) fn validate_current(&self) -> Result<(), TypeScriptProjectHostError> {
        self.witness.validate_current()?;
        validate_resolver_observations(self.witness, &self.observations)
    }

    fn capture_realpath(
        &self,
        path: &Path,
    ) -> Result<Option<(PathBuf, Option<FileIdentity>)>, TypeScriptProjectHostError> {
        capture_realpath_snapshot(path)
    }

    fn record_missing(
        &mut self,
        path: PathBuf,
        nearest_existing_parent: Option<PathBuf>,
    ) -> Result<(), TypeScriptProjectHostError> {
        if !self.witness.path_is_admitted(&path) {
            if !self.witness.external_negative_module_candidate(&path) {
                return Err(TypeScriptProjectHostError::SourceOutsideCapability {
                    path: path.into_boxed_path(),
                });
            }
            let parent = capture_external_negative_parent(&path)?;
            if let Some(previous) = self.observations.external_negative_parents.get(&path) {
                if previous != &parent {
                    return Err(TypeScriptProjectHostError::WitnessChanged {
                        path: path.into_boxed_path(),
                    });
                }
                return Ok(());
            }
            ensure_observation_capacity(&self.observations, &path)?;
            let cost = path.as_os_str().as_encoded_bytes().len()
                + parent.path.as_os_str().as_encoded_bytes().len()
                + parent
                    .canonical_path
                    .as_deref()
                    .map_or(0, |p| p.as_os_str().as_encoded_bytes().len());
            ensure_metadata_capacity(&self.observations, cost, &path)?;
            self.observations.retained_metadata_bytes += cost;
            self.observations
                .external_negative_parents
                .insert(path.clone(), parent);
            ensure_observation_capacity(&self.observations, &path)?;
            self.observations.missing_paths.insert(path, None);
            return Ok(());
        }
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

    pub(crate) const fn closure_replay_work_units(&self) -> u64 {
        self.observations.closure_replay_work_units
    }

    pub(crate) fn charge_closure_replay_work(
        &mut self,
        units: usize,
        phase: &str,
    ) -> Result<(), TypeScriptProjectHostError> {
        charge_closure_replay_work(
            &mut self.observations.closure_replay_work_units,
            units,
            phase,
        )
    }

    fn index_captured_peer_manifest(
        &mut self,
        path: &Path,
    ) -> Result<(), TypeScriptProjectHostError> {
        if path.file_name().and_then(|name| name.to_str()) != Some("package.json") {
            return Ok(());
        }
        let Some(directory) = path.parent() else {
            return Ok(());
        };
        if self
            .observations
            .captured_peer_manifests
            .contains_key(directory)
        {
            return Ok(());
        }
        self.charge_closure_replay_work(
            path.as_os_str().as_encoded_bytes().len(),
            "captured peer candidate path",
        )?;
        let Some(prefix) = directory
            .to_str()
            .and_then(typescript_peer_directory_prefix)
            .and_then(|prefix| normalize_absolute_path(Path::new(prefix)))
        else {
            return Ok(());
        };
        // Safe relative peer keys cannot escape an already admitted prefix, so
        // those manifests require no external-candidate parse or index.
        if self.witness.path_is_admitted(&prefix) {
            return Ok(());
        }
        let source = self
            .observations
            .loaded_sources
            .get(path)
            .expect("newly captured source is present");
        let content_id = source.content_id;
        let mut candidates = Vec::new();
        let mut metadata = directory
            .as_os_str()
            .as_encoded_bytes()
            .len()
            .saturating_add(path.as_os_str().as_encoded_bytes().len())
            .saturating_add(content_id.as_ref().len())
            .saturating_add(1);
        if source.bytes.len() <= MAX_PACKAGE_MANIFEST_BYTES {
            // The existing UTF-8 config reader removes one leading BOM and
            // accepts JSONC just like the selected SDK's readJson fallback.
            // Keep raw bytes/CID, and leave malformed/unsupported manifests
            // without candidates, matching readJson's empty-object fallback.
            // Charge decoding, the bounded JSONC buffers and parse BEFORE work.
            charge_closure_replay_work(
                &mut self.observations.closure_replay_work_units,
                source.bytes.len().saturating_mul(5),
                "captured peer manifest decoding",
            )?;
            if let Ok(manifest) = parse_typescript_config(path, &source.bytes)
                && let Some(peers) = manifest
                    .get("peerDependencies")
                    .and_then(serde_json::Value::as_object)
            {
                for key in peers.keys() {
                    charge_closure_replay_work(
                        &mut self.observations.closure_replay_work_units,
                        key.len().saturating_add(1),
                        "captured peer key validation",
                    )?;
                    if !valid_peer_package_key(key) {
                        continue;
                    }
                    let candidate_bytes = prefix
                        .as_os_str()
                        .as_encoded_bytes()
                        .len()
                        .saturating_add(1)
                        .saturating_add(key.len());
                    charge_closure_replay_work(
                        &mut self.observations.closure_replay_work_units,
                        candidate_bytes,
                        "captured peer candidate allocation",
                    )?;
                    metadata = metadata.saturating_add(candidate_bytes);
                    ensure_metadata_capacity(&self.observations, metadata, directory)?;
                    candidates.push(prefix.join(key).into_boxed_path());
                }
            }
        }
        ensure_peer_index_capacity(&self.observations, 1, metadata, directory)?;
        self.charge_closure_replay_work(
            path.as_os_str().as_encoded_bytes().len(),
            "captured peer manifest source key",
        )?;
        self.observations.retained_metadata_bytes += metadata;
        self.observations.captured_peer_manifests.insert(
            directory.to_path_buf(),
            CapturedPeerManifest {
                source_path: path.to_path_buf().into_boxed_path(),
                content_id,
                candidates: candidates.into_boxed_slice(),
                activated: false,
            },
        );
        self.activate_captured_peer_manifest(directory)
    }

    fn activate_captured_peer_manifest(
        &mut self,
        directory: &Path,
    ) -> Result<(), TypeScriptProjectHostError> {
        let Some(manifest) = self.observations.captured_peer_manifests.get(directory) else {
            return Ok(());
        };
        if manifest.activated || !captured_canonical_realpath(directory, &self.observations) {
            return Ok(());
        }
        let mut count = 0_usize;
        let mut metadata = 0_usize;
        for candidate in &manifest.candidates {
            let bytes = candidate
                .as_os_str()
                .as_encoded_bytes()
                .len()
                .saturating_add(directory.as_os_str().as_encoded_bytes().len());
            charge_closure_replay_work(
                &mut self.observations.closure_replay_work_units,
                bytes.saturating_add(1),
                "captured peer candidate activation",
            )?;
            if !self
                .observations
                .peer_directory_candidates
                .contains_key(candidate.as_ref())
            {
                count = count.saturating_add(1);
                metadata = metadata.saturating_add(bytes);
            }
        }
        ensure_peer_index_capacity(&self.observations, count, metadata, directory)?;
        self.observations.retained_metadata_bytes += metadata;
        let observations = &mut self.observations;
        let manifest = observations
            .captured_peer_manifests
            .get_mut(directory)
            .expect("captured manifest remains present");
        for candidate in &manifest.candidates {
            observations
                .peer_directory_candidates
                .entry(candidate.to_path_buf())
                .or_insert_with(|| directory.to_path_buf());
        }
        manifest.activated = true;
        Ok(())
    }

    fn record_realpath(
        &mut self,
        path: PathBuf,
        canonical_path: Option<PathBuf>,
        identity: Option<FileIdentity>,
    ) -> Result<(), TypeScriptProjectHostError> {
        // Index exact canonical targets once, preserving the queried lexical
        // key whose inode/target is revalidated by the ordinary realpath ledger.
        // This supports both package-manifest/realpath observation orders and
        // pnpm-style symlink roots without rescanning all realpath snapshots.
        let mut target_row = None;
        if let Some(target) = canonical_path.as_ref()
            && !self
                .observations
                .realpath_queries_by_target
                .contains_key(target)
        {
            let cost = target
                .as_os_str()
                .as_encoded_bytes()
                .len()
                .saturating_add(path.as_os_str().as_encoded_bytes().len());
            self.charge_closure_replay_work(
                cost.saturating_add(1),
                "canonical realpath index allocation",
            )?;
            let new_realpath = usize::from(!self.observations.realpaths.contains_key(&path));
            ensure_peer_index_capacity(
                &self.observations,
                1 + new_realpath,
                cost.saturating_mul(1 + new_realpath),
                &path,
            )?;
            target_row = Some((target.clone(), path.clone(), cost));
        }
        let peer_directory = canonical_path.as_ref().filter(|directory| {
            self.observations
                .captured_peer_manifests
                .get(*directory)
                .is_some_and(|manifest| !manifest.activated)
        });
        if let Some(directory) = peer_directory {
            self.charge_closure_replay_work(
                directory.as_os_str().as_encoded_bytes().len(),
                "captured peer activation path",
            )?;
        }
        let peer_directory = peer_directory.cloned();
        record_realpath_ledger(&mut self.observations, path, canonical_path, identity)?;
        if let Some((target, lexical, cost)) = target_row {
            self.observations.retained_metadata_bytes += cost;
            self.observations
                .realpath_queries_by_target
                .insert(target, lexical);
        }
        if let Some(directory) = peer_directory {
            self.activate_captured_peer_manifest(&directory)?;
        }
        Ok(())
    }

    fn observe_directory(&mut self, path: &Path) -> Result<(), TypeScriptProjectHostError> {
        // This capability serves one captured program, just as loaded_sources
        // serves retained source bytes. Repeated module probes must reuse its
        // exact first directory view rather than enumerating every sibling
        // again for each missing extension. seal_with_host freshly replays
        // every captured directory and negative edge before native admission;
        // later publication also revalidates that same sealed witness.
        // Callers still perform their current lexical/realpath admission first.
        if self.observations.directories.contains_key(path) {
            return Ok(());
        }
        let snapshot = capture_directory_snapshot(path, self.witness)?;
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

fn validate_resolver_observations(
    witness: &TypeScriptProjectWitness,
    observations: &ResolverObservationLedger,
) -> Result<(), TypeScriptProjectHostError> {
    validate_resolver_observations_with_host(witness, observations, None)
}

fn validate_resolver_observations_with_host(
    witness: &TypeScriptProjectWitness,
    observations: &ResolverObservationLedger,
    host: Option<&ValidatedTypeScriptHost<'_>>,
) -> Result<(), TypeScriptProjectHostError> {
    validate_resolver_observations_with_host_controlled(witness, observations, host, &mut || Ok(()))
}

fn validate_resolver_observations_with_host_controlled(
    witness: &TypeScriptProjectWitness,
    observations: &ResolverObservationLedger,
    host: Option<&ValidatedTypeScriptHost<'_>>,
    control: &mut dyn FnMut() -> Result<(), TypeScriptProjectHostError>,
) -> Result<(), TypeScriptProjectHostError> {
    control()?;
    if host.is_some_and(|host| !std::ptr::eq(host.witness, witness)) {
        return Err(TypeScriptProjectHostError::WitnessChanged {
            path: witness.project_root.clone(),
        });
    }
    for input in observations.loaded_sources.values() {
        control()?;
        // Common SDK/config/dynamic files were freshly checked once at this
        // boundary. New reads made after that proof still require a capture.
        let captured;
        let observed =
            if let Some(snapshot) = host.and_then(|host| host.sources.get(input.path.as_ref())) {
                snapshot
            } else {
                captured = capture_file_snapshot(&input.path, MAX_SOURCE_FILE_BYTES)?;
                &captured
            };
        if observed.identity != input.identity
            || observed.length != u64::try_from(input.bytes.len()).unwrap_or(u64::MAX)
            || observed.content_id != input.content_id
        {
            return Err(TypeScriptProjectHostError::WitnessChanged {
                path: input.path.clone(),
            });
        }
    }
    for (path, nearest_parent) in &observations.missing_paths {
        control()?;
        if !matches!(fs::symlink_metadata(path), Err(ref source) if matches!(source.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory))
        {
            return Err(TypeScriptProjectHostError::WitnessChanged {
                path: path.clone().into_boxed_path(),
            });
        }
        if let Some(parent) = nearest_parent {
            // Every recorded directory is freshly replayed once below. A module
            // resolver may probe many absent extensions in the same directory;
            // repeating its entire entry snapshot for each miss adds no proof.
            // Keep the parent edge check so a malformed ledger cannot omit it.
            if !observations.directories.contains_key(parent) {
                return Err(TypeScriptProjectHostError::WitnessChanged {
                    path: parent.clone().into_boxed_path(),
                });
            }
        }
    }
    for (path, expected) in &observations.external_negative_parents {
        control()?;
        if !witness.external_negative_module_candidate(path)
            || &capture_external_negative_parent(path)? != expected
        {
            return Err(TypeScriptProjectHostError::WitnessChanged {
                path: path.clone().into_boxed_path(),
            });
        }
    }
    for (path, expected) in &observations.external_absent_directories {
        control()?;
        if !captured_peer_directory_candidate(path, witness, observations)
            || &capture_external_negative_parent(path)? != expected
        {
            return Err(TypeScriptProjectHostError::WitnessChanged {
                path: path.clone().into_boxed_path(),
            });
        }
    }
    for (path, expected) in &observations.directories {
        control()?;
        let observed = capture_directory_snapshot_controlled(path, witness, control)?;
        if &observed != expected {
            return Err(TypeScriptProjectHostError::WitnessChanged {
                path: path.clone().into_boxed_path(),
            });
        }
    }
    for (path, expected) in &observations.realpaths {
        control()?;
        let observed = capture_realpath_snapshot(path)?;
        let canonical = observed.as_ref().map(|(path, _)| path.as_path());
        if canonical != expected.canonical_path.as_deref()
            || observed.as_ref().and_then(|(_, identity)| *identity) != expected.identity
        {
            return Err(TypeScriptProjectHostError::WitnessChanged {
                path: path.clone().into_boxed_path(),
            });
        }
    }
    control()?;
    Ok(())
}

fn capture_realpath_snapshot(
    path: &Path,
) -> Result<Option<(PathBuf, Option<FileIdentity>)>, TypeScriptProjectHostError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let identity = file_identity(&metadata, path, None).map_err(|source| {
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
        Err(source)
            if matches!(
                source.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(source) => Err(TypeScriptProjectHostError::PackagePath {
            path: path.to_path_buf().into_boxed_path(),
            source,
        }),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum TypeScriptProjectDiscoveryOrigin {
    ProjectLocal = 1,
    InstalledFallback = 2,
}

#[derive(Debug)]
pub(crate) struct TypeScriptProjectWitness {
    authority: TypeScriptWitnessAuthority,
    project_root: Box<Path>,
    home_root: Option<Box<Path>>,
    module_root: Box<Path>,
    workspace: Option<WorkspaceBoundary>,
    workspace_root: Box<Path>,
    files: Box<[FileSnapshot]>,
    typescript_files: Box<[TypeScriptFileInput]>,
    config_candidates: Box<[TypeScriptConfigInput]>,
    selected_build_config_paths: Box<[Box<Path>]>,
    config_paths: Box<[Box<Path>]>,
    loaded_source_files: std::sync::Mutex<std::collections::BTreeMap<PathBuf, FileSnapshot>>,
    fingerprint: [u8; 32],
    resource_proof: Option<crate::application::host::BundleTypeScriptResourceProof>,
}

#[derive(Debug)]
enum TypeScriptWitnessAuthority {
    Native,
    CompilerApi(Box<TypeScriptCompilerApiWitness>),
}

#[derive(Debug)]
struct TypeScriptCompilerApiWitness {
    discovery_origin: TypeScriptProjectDiscoveryOrigin,
    discovered_compiler_origin: TypeScriptSelectionOrigin,
    discovered_compiler: Box<Path>,
    discovered_module_root: Box<Path>,
    discovered_package_root: Box<Path>,
    discovered_version: Box<str>,
    compiler: Box<Path>,
    node: Box<Path>,
    package_root: Box<Path>,
    invocation_lease: TypeScriptProjectInvocationLease,
}

impl TypeScriptProjectWitness {
    /// Exact native host identity forwarded through admitted project inputs and
    /// combined with ordered TSZ environments for project execution authority.
    /// Compiler API selection and resource identities use a separate recipe.
    pub(crate) fn native_project_fingerprint(&self) -> Option<[u8; 32]> {
        (matches!(self.authority, TypeScriptWitnessAuthority::Native)
            && self.resource_proof.is_none())
        .then_some(self.fingerprint)
    }

    pub(crate) fn native_project_root(&self) -> Option<&Path> {
        matches!(self.authority, TypeScriptWitnessAuthority::Native)
            .then_some(self.project_root.as_ref())
    }

    pub(crate) fn is_native_for_root(&self, root: &Path) -> bool {
        matches!(self.authority, TypeScriptWitnessAuthority::Native)
            && self.project_root.as_ref() == root
    }

    /// Conservative payload and allocation accounting for the native-only cache.
    /// Native hosts retain no Compiler API invocation or resource-bundle payload.
    pub(crate) fn retained_native_bytes(&self) -> Option<usize> {
        let loaded_source_files = self.loaded_source_files.lock().ok()?;
        if !matches!(self.authority, TypeScriptWitnessAuthority::Native)
            || self.resource_proof.is_some()
            || !self.typescript_files.is_empty()
            || self.files.len() > MAX_PROJECT_CONFIG_FILES
            || self.config_candidates.len() > MAX_PROJECT_CONFIG_FILES
            || self.config_candidates.iter().any(|config| {
                config.extends.len() > MAX_PROJECT_ANCESTORS
                    || config.references.len() > MAX_PROJECT_ANCESTORS
            })
            || self.selected_build_config_paths.len() > MAX_PROJECT_CONFIG_FILES
            || self.config_paths.len() > MAX_PROJECT_CONFIG_FILES
            || loaded_source_files.len() > MAX_RESOLVED_SOURCE_FILES
            || self
                .workspace
                .as_ref()
                .is_some_and(|workspace| workspace.files.len() > MAX_PROJECT_CONFIG_FILES)
        {
            return None;
        }
        let path_bytes = |path: &Path| path.as_os_str().as_encoded_bytes().len();
        let snapshots = |files: &[FileSnapshot]| {
            files.iter().fold(0usize, |bytes, file| {
                bytes
                    .saturating_add(std::mem::size_of::<FileSnapshot>())
                    .saturating_add(path_bytes(&file.path))
            })
        };
        let mut bytes = std::mem::size_of::<Self>()
            .saturating_add(path_bytes(&self.project_root))
            .saturating_add(self.home_root.as_deref().map_or(0, path_bytes))
            .saturating_add(path_bytes(&self.module_root))
            .saturating_add(path_bytes(&self.workspace_root))
            .saturating_add(snapshots(&self.files));
        if let Some(workspace) = &self.workspace {
            bytes = bytes
                .saturating_add(path_bytes(&workspace.root))
                .saturating_add(snapshots(&workspace.files));
        }
        for config in &self.config_candidates {
            bytes = bytes
                .saturating_add(std::mem::size_of::<TypeScriptConfigInput>())
                .saturating_add(config.bytes.len())
                .saturating_add(path_bytes(&config.path));
            for path in config.extends.iter().chain(&config.references) {
                bytes = bytes
                    .saturating_add(std::mem::size_of::<Box<Path>>())
                    .saturating_add(path_bytes(path));
            }
        }
        for path in self
            .config_paths
            .iter()
            .chain(&self.selected_build_config_paths)
        {
            bytes = bytes
                .saturating_add(std::mem::size_of::<Box<Path>>())
                .saturating_add(path_bytes(path));
        }
        for (path, file) in loaded_source_files.iter() {
            // Charge both paths plus a conservative per-entry tree allocation.
            bytes = bytes
                .saturating_add(1024)
                .saturating_add(path.capacity())
                .saturating_add(path_bytes(&file.path))
                .saturating_add(usize::try_from(file.length).unwrap_or(usize::MAX));
        }
        Some(bytes)
    }

    /// Encodes only the exhaustively represented native host witness. The caller must
    /// store these bytes in the authenticated selected-compiler publication member.
    pub(crate) fn encode_native_snapshot(&self) -> Option<Vec<u8>> {
        self.encode_native_snapshot_with_limit(64 * 1024 * 1024)
    }

    pub(crate) fn encode_native_snapshot_with_limit(&self, max_bytes: usize) -> Option<Vec<u8>> {
        if self.retained_native_bytes()? > 64 * 1024 * 1024
            || max_bytes > 64 * 1024 * 1024
            || !matches!(self.authority, TypeScriptWitnessAuthority::Native)
            || self.resource_proof.is_some()
            || !self.typescript_files.is_empty()
        {
            return None;
        }
        let loaded_source_files = self.loaded_source_files.lock().ok()?;
        let dto = NativeTypeScriptHostSnapshotDto {
            version: 1,
            project_root: snapshot_path(&self.project_root)?,
            home_root: match self.home_root.as_deref() {
                Some(path) => SnapshotOption::Present(snapshot_path(path)?),
                None => SnapshotOption::Absent,
            },
            module_root: snapshot_path(&self.module_root)?,
            workspace: match self.workspace.as_ref() {
                Some(workspace) => SnapshotOption::Present(snapshot_workspace(workspace)?),
                None => SnapshotOption::Absent,
            },
            workspace_root: snapshot_path(&self.workspace_root)?,
            files: BoundedSnapshotVec(
                self.files
                    .iter()
                    .map(snapshot_file)
                    .collect::<Option<_>>()?,
            ),
            typescript_files: BoundedSnapshotVec(Vec::new()),
            config_candidates: BoundedSnapshotVec(
                self.config_candidates
                    .iter()
                    .map(snapshot_config)
                    .collect::<Option<_>>()?,
            ),
            selected_build_config_paths: BoundedSnapshotVec(
                self.selected_build_config_paths
                    .iter()
                    .map(|path| snapshot_path(path))
                    .collect::<Option<_>>()?,
            ),
            config_paths: BoundedSnapshotVec(
                self.config_paths
                    .iter()
                    .map(|path| snapshot_path(path))
                    .collect::<Option<_>>()?,
            ),
            loaded_source_files: BoundedSnapshotVec(
                loaded_source_files
                    .iter()
                    .map(|(path, file)| Some((snapshot_path(path)?, snapshot_file(file)?)))
                    .collect::<Option<_>>()?,
            ),
            fingerprint: self.fingerprint,
        };
        snapshot_resolver::encode_json_with_limit(&dto, max_bytes)
    }

    /// Decodes a complete versioned native host ledger. The returned opaque witness is
    /// still untrusted until `validated_controlled` replays every captured host fence.
    pub(crate) fn decode_native_snapshot(bytes: &[u8]) -> Option<Self> {
        if bytes.is_empty() || bytes.len() > 64 * 1024 * 1024 {
            return None;
        }
        let dto: NativeTypeScriptHostSnapshotDto = serde_json::from_slice(bytes).ok()?;
        if dto.version != 1
            || !dto.typescript_files.0.is_empty()
            || matches!(&dto.workspace, SnapshotOption::Present(workspace) if {
                workspace.files.0.len() > MAX_PROJECT_CONFIG_FILES
            })
        {
            return None;
        }
        let project_root = snapshot_decode_path(dto.project_root)?;
        let home_root = match dto.home_root {
            SnapshotOption::Present(path) => Some(snapshot_decode_path(path)?),
            SnapshotOption::Absent => None,
        };
        let module_root = snapshot_decode_path(dto.module_root)?;
        let workspace = match dto.workspace {
            SnapshotOption::Present(workspace) => Some(snapshot_decode_workspace(workspace)?),
            SnapshotOption::Absent => None,
        };
        let workspace_root = snapshot_decode_path(dto.workspace_root)?;
        let files = dto
            .files
            .0
            .into_iter()
            .map(snapshot_decode_file)
            .collect::<Option<Vec<_>>>()?
            .into_boxed_slice();
        let config_candidates = dto
            .config_candidates
            .0
            .into_iter()
            .map(snapshot_decode_config)
            .collect::<Option<Vec<_>>>()?
            .into_boxed_slice();
        let selected_build_config_paths = dto
            .selected_build_config_paths
            .0
            .into_iter()
            .map(snapshot_decode_path)
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .map(PathBuf::into_boxed_path)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let config_paths = dto
            .config_paths
            .0
            .into_iter()
            .map(snapshot_decode_path)
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .map(PathBuf::into_boxed_path)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let mut loaded_source_files = std::collections::BTreeMap::new();
        for (key, value) in dto.loaded_source_files.0 {
            let key = snapshot_decode_path(key)?;
            let file = snapshot_decode_file(value)?;
            if key.as_path() != file.path.as_ref()
                || file.length > MAX_SOURCE_FILE_BYTES
                || loaded_source_files.insert(key, file).is_some()
            {
                return None;
            }
        }
        if loaded_source_files
            .values()
            .try_fold(0u64, |total, file| total.checked_add(file.length))?
            > MAX_RESOLVED_SOURCE_BYTES
        {
            return None;
        }
        if module_root != workspace_root
            || workspace
                .as_ref()
                .map_or(project_root.as_path(), |workspace| workspace.root.as_ref())
                != workspace_root
            || project_root != workspace_root && !project_root.starts_with(&workspace_root)
            || files.windows(2).any(|pair| pair[0].path >= pair[1].path)
            || config_candidates
                .windows(2)
                .any(|pair| pair[0].path >= pair[1].path)
            || config_paths.len() != config_candidates.len()
            || config_candidates
                .iter()
                .zip(config_paths.iter())
                .any(|(config, path)| config.path != *path)
            || selected_build_config_paths
                .iter()
                .map(|path| path.as_ref())
                .collect::<std::collections::BTreeSet<_>>()
                != config_candidates
                    .iter()
                    .filter(|config| config.selected_build_config)
                    .map(|config| config.path.as_ref())
                    .collect::<std::collections::BTreeSet<_>>()
            || config_candidates.iter().any(|config| {
                files
                    .binary_search_by(|file| file.path.cmp(&config.path))
                    .ok()
                    .is_none_or(|index| files[index] != snapshot_from_config(config))
            })
            || workspace.as_ref().is_some_and(|workspace| {
                workspace.files.iter().any(|file| {
                    files
                        .binary_search_by(|candidate| candidate.path.cmp(&file.path))
                        .ok()
                        .is_none_or(|index| files[index] != *file)
                })
            })
            || config_candidates.iter().try_fold(0u64, |total, config| {
                total.checked_add(config.bytes.len() as u64)
            })? > MAX_PROJECT_CONFIG_BYTES
        {
            return None;
        }
        let mut fingerprint = Hasher::new();
        fingerprint.update(b"typescript.native-tsz-project.v1\0");
        fingerprint.update(backend_frontend_typescript::TSZ_NATIVE_AUTHORITY_IDENTITY);
        fingerprint.update(&witness_fingerprint(&files));
        if *fingerprint.finalize().as_bytes() != dto.fingerprint {
            return None;
        }
        let witness = Self {
            authority: TypeScriptWitnessAuthority::Native,
            project_root: project_root.into_boxed_path(),
            home_root: home_root.map(PathBuf::into_boxed_path),
            module_root: module_root.into_boxed_path(),
            workspace,
            workspace_root: workspace_root.into_boxed_path(),
            files,
            typescript_files: Box::new([]),
            config_candidates,
            selected_build_config_paths,
            config_paths,
            loaded_source_files: std::sync::Mutex::new(loaded_source_files),
            fingerprint: dto.fingerprint,
            resource_proof: None,
        };
        if witness.retained_native_bytes()? > 64 * 1024 * 1024 {
            return None;
        }
        return Some(witness);
    }

    fn selection_fingerprint(&self) -> [u8; 32] {
        let Some(proof) = &self.resource_proof else {
            return self.fingerprint;
        };
        let mut hash = Hasher::new();
        hash.update(b"compiler.typescript.selected-sdk.v1\0");
        hash.update(&self.fingerprint);
        hash.update(&proof.fingerprint());
        *hash.finalize().as_bytes()
    }

    fn invocation_lease(&self) -> &TypeScriptProjectInvocationLease {
        match &self.authority {
            TypeScriptWitnessAuthority::CompilerApi(host) => &host.invocation_lease,
            TypeScriptWitnessAuthority::Native => {
                unreachable!("native TSZ has no child invocation")
            }
        }
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
            &discovered.version,
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
        selected_package_version: &str,
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
        let typescript_files = collect_module_inputs(&package_root).map_err(|error| {
            compiler_package_admission_error(
                error,
                selected_package_version,
                backend_library::interface::TypeScriptCompilerPackageRole::SelectedCompiler,
            )
        })?;
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
                collect_module_inputs(&discovered_package_root).map_err(|error| {
                    compiler_package_admission_error(error, &discovered.version,
                        backend_library::interface::TypeScriptCompilerPackageRole::DiscoveredProjectPackage)
                })?
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
        let mut discovery_fingerprint = Hasher::new();
        discovery_fingerprint.update(b"typescript.project-discovery-origin.v1\0");
        discovery_fingerprint.update(&[discovered.discovery_origin as u8]);
        discovery_fingerprint.update(&witness_fingerprint(&files));
        let fingerprint = *discovery_fingerprint.finalize().as_bytes();
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
        let package_identity =
            file_identity(&package_metadata, &package_root, None).map_err(|source| {
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
            authority: TypeScriptWitnessAuthority::CompilerApi(Box::new(
                TypeScriptCompilerApiWitness {
                    discovery_origin: discovered.discovery_origin,
                    discovered_compiler_origin: discovered.compiler_origin,
                    discovered_compiler: discovered.compiler.to_path_buf().into_boxed_path(),
                    discovered_module_root: discovered.module_root.to_path_buf().into_boxed_path(),
                    discovered_package_root: discovered_package_root.into_boxed_path(),
                    discovered_version: discovered.version.clone().into_boxed_str(),
                    compiler: compiler.to_path_buf().into_boxed_path(),
                    node: node.to_path_buf().into_boxed_path(),
                    package_root: package_root.into_boxed_path(),
                    invocation_lease,
                },
            )),
            project_root: project_root.into_boxed_path(),
            home_root: home_root.map(|root| root.to_path_buf().into_boxed_path()),
            module_root: module_root.into_boxed_path(),
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
            resource_proof: None,
        })
    }

    #[cfg(test)]
    fn capture_native(
        project_root: &Path,
        home_root: Option<&Path>,
    ) -> Result<Self, TypeScriptProjectHostError> {
        Self::capture_native_controlled(project_root, home_root, &mut || Ok(()))
    }

    fn capture_native_controlled(
        project_root: &Path,
        home_root: Option<&Path>,
        control: &mut dyn FnMut() -> Result<(), TypeScriptProjectHostError>,
    ) -> Result<Self, TypeScriptProjectHostError> {
        control()?;
        let project_root = canonical_directory(project_root)?;
        let workspace = discover_workspace_boundary_controlled(&project_root, home_root, control)?;
        let workspace_root = workspace.as_ref().map_or_else(
            || project_root.clone(),
            |workspace| workspace.root.to_path_buf(),
        );
        // Native TSZ resolves dependencies, including hoisted node_modules, within this
        // captured workspace. No TypeScript compiler package is selected or executed.
        control()?;
        let module_root = workspace_root.clone();
        let (selected_build_config_paths, angular_snapshot) =
            angular_build_config_paths_controlled(&workspace_root, &module_root, control)?;
        let config_candidates = collect_project_configs_inner_controlled(
            &project_root,
            &workspace_root,
            &module_root,
            &selected_build_config_paths,
            false,
            control,
        )?;
        let mut files = config_candidates
            .iter()
            .map(snapshot_from_config)
            .collect::<Vec<_>>();
        if let Some(workspace) = &workspace {
            files.extend(workspace.files.iter().cloned());
        }
        if let Some(snapshot) = angular_snapshot {
            files.push(snapshot);
        }
        files.sort_unstable_by(|left, right| left.path.cmp(&right.path));
        files.dedup_by(|left, right| left.path == right.path);
        let mut hash = Hasher::new();
        hash.update(b"typescript.native-tsz-project.v1\0");
        hash.update(backend_frontend_typescript::TSZ_NATIVE_AUTHORITY_IDENTITY);
        hash.update(&witness_fingerprint(&files));
        let fingerprint = *hash.finalize().as_bytes();
        control()?;
        Ok(Self {
            authority: TypeScriptWitnessAuthority::Native,
            project_root: project_root.into_boxed_path(),
            home_root: home_root.map(|path| path.to_path_buf().into_boxed_path()),
            module_root: module_root.into_boxed_path(),
            workspace,
            workspace_root: workspace_root.into_boxed_path(),
            files: files.into_boxed_slice(),
            typescript_files: Box::new([]),
            config_paths: config_candidates
                .iter()
                .map(|config| config.path.clone())
                .collect(),
            config_candidates: config_candidates.into_boxed_slice(),
            selected_build_config_paths: selected_build_config_paths
                .into_iter()
                .map(PathBuf::into_boxed_path)
                .collect(),
            loaded_source_files: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            fingerprint,
            resource_proof: None,
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
        self.record_source_snapshot(&canonical, &snapshot)?;
        Ok(TypeScriptFileInput {
            path: canonical.into_boxed_path(),
            content_id: snapshot.content_id,
            identity: snapshot.identity,
            bytes: bytes.into_boxed_slice(),
        })
    }

    fn record_source_snapshot(
        &self,
        canonical: &Path,
        snapshot: &FileSnapshot,
    ) -> Result<(), TypeScriptProjectHostError> {
        let mut loaded = self.loaded_source_files.lock().map_err(|_| {
            TypeScriptProjectHostError::WitnessLockPoisoned {
                path: canonical.to_path_buf().into_boxed_path(),
            }
        })?;
        if let Some(previous) = loaded.get(canonical) {
            if previous != snapshot {
                return Err(TypeScriptProjectHostError::WitnessChanged {
                    path: canonical.to_path_buf().into_boxed_path(),
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
            loaded.insert(canonical.to_path_buf(), snapshot.clone());
        }
        Ok(())
    }

    fn path_is_admitted(&self, path: &Path) -> bool {
        path.starts_with(&self.workspace_root) || path.starts_with(&self.module_root)
    }

    fn external_negative_module_candidate(&self, path: &Path) -> bool {
        self.workspace_root
            .ancestors()
            .skip(1)
            .take(MAX_PROJECT_ANCESTORS)
            .any(|ancestor| {
                let modules = ancestor.join("node_modules");
                path == modules || path == modules.join("@types")
            })
    }

    fn admit_lexical_path(&self, path: &Path) -> Result<PathBuf, TypeScriptProjectHostError> {
        let Some(path) = normalize_absolute_path(path) else {
            return Err(TypeScriptProjectHostError::SourceOutsideCapability {
                path: path.to_path_buf().into_boxed_path(),
            });
        };
        if !self.path_is_admitted(&path)
            && !(self.external_negative_module_candidate(&path)
                && matches!(fs::symlink_metadata(&path), Err(ref error) if error.kind() == io::ErrorKind::NotFound))
        {
            return Err(TypeScriptProjectHostError::SourceOutsideCapability {
                path: path.into_boxed_path(),
            });
        }
        Ok(path)
    }

    pub(crate) fn validate_current(&self) -> Result<(), TypeScriptProjectHostError> {
        self.validated().map(|_| ())
    }

    pub(crate) fn validated(
        &self,
    ) -> Result<ValidatedTypeScriptHost<'_>, TypeScriptProjectHostError> {
        self.validated_controlled(&mut || Ok(()))
    }

    pub(crate) fn validated_controlled(
        &self,
        control: &mut dyn FnMut() -> Result<(), TypeScriptProjectHostError>,
    ) -> Result<ValidatedTypeScriptHost<'_>, TypeScriptProjectHostError> {
        control()?;
        if let Some(proof) = &self.resource_proof {
            proof.validate_current().map_err(bundle_admission_error)?;
        }

        let current = match &self.authority {
            TypeScriptWitnessAuthority::Native => Self::capture_native_controlled(
                &self.project_root,
                self.home_root.as_deref(),
                control,
            )?,
            TypeScriptWitnessAuthority::CompilerApi(selected) => {
                let current_project = match find_project_typescript_with_home(
                    &self.project_root,
                    self.home_root.as_deref(),
                )? {
                    ProjectTypeScriptSearch::Found(project)
                        if selected.discovery_origin
                            == TypeScriptProjectDiscoveryOrigin::ProjectLocal =>
                    {
                        project
                    }
                    ProjectTypeScriptSearch::NotFound
                        if selected.discovery_origin
                            == TypeScriptProjectDiscoveryOrigin::InstalledFallback =>
                    {
                        // Revalidate the originally selected host installation. Never select a
                        // new PATH entry or turn a vanished project installation into fallback.
                        let (module_root, version) =
                            read_typescript_module(&selected.discovered_module_root)?;
                        let compiler =
                            fs::canonicalize(&selected.discovered_compiler).map_err(|source| {
                                TypeScriptProjectHostError::PackagePath {
                                    path: selected.discovered_compiler.clone(),
                                    source,
                                }
                            })?;
                        ProjectTypeScript {
                            module_root,
                            compiler,
                            version,
                            workspace: discover_workspace_boundary(
                                &self.project_root,
                                self.home_root.as_deref(),
                            )?,
                            compiler_origin: selected.discovered_compiler_origin,
                            discovery_origin: TypeScriptProjectDiscoveryOrigin::InstalledFallback,
                        }
                    }
                    ProjectTypeScriptSearch::Found(_)
                    | ProjectTypeScriptSearch::NotFound
                    | ProjectTypeScriptSearch::Pnp(_) => {
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
                if current_project.compiler.as_path() != selected.discovered_compiler.as_ref()
                    || current_project.module_root.as_path()
                        != selected.discovered_module_root.as_ref()
                    || current_project.version.as_str() != selected.discovered_version.as_ref()
                    || package_root.as_path() != selected.discovered_package_root.as_ref()
                    || current_project.workspace != self.workspace
                {
                    return Err(TypeScriptProjectHostError::WitnessChanged {
                        path: self.project_root.to_path_buf().into_boxed_path(),
                    });
                }

                let selected_package_root = fs::canonicalize(self.module_root.join("typescript"))
                    .map_err(|source| {
                    TypeScriptProjectHostError::PackagePath {
                        path: self.module_root.join("typescript").into_boxed_path(),
                        source,
                    }
                })?;
                if selected_package_root.as_path() != selected.package_root.as_ref() {
                    return Err(TypeScriptProjectHostError::WitnessChanged {
                        path: self.module_root.join("typescript").into_boxed_path(),
                    });
                }

                Self::capture(
                    &self.project_root,
                    self.home_root.as_deref(),
                    &current_project,
                    &selected.compiler,
                    &selected.node,
                    &self.module_root,
                    &selected.package_root,
                    self.workspace.as_ref(),
                )?
            }
        };
        control()?;
        if current.files != self.files || current.fingerprint != self.fingerprint {
            let changed = first_changed_snapshot(&self.files, &current.files)
                .unwrap_or_else(|| self.project_root.to_path_buf());
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
            control()?;
            let observed = capture_file_snapshot(path, MAX_SOURCE_FILE_BYTES)?;
            if &observed != expected {
                return Err(TypeScriptProjectHostError::WitnessChanged {
                    path: path.clone().into_boxed_path(),
                });
            }
        }
        let mut sources = self
            .files
            .iter()
            .map(|snapshot| (snapshot.path.to_path_buf(), snapshot.clone()))
            .collect::<std::collections::BTreeMap<_, _>>();
        // Snapshot the same locked rows that were just validated. The token
        // cannot silently adopt a later concurrently registered file.
        sources.extend(
            loaded
                .iter()
                .map(|(path, snapshot)| (path.clone(), snapshot.clone())),
        );
        control()?;
        Ok(ValidatedTypeScriptHost {
            witness: self,
            sources,
        })
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

// TypeScript's peer lookup uses the real package directory up to the last
// "node_modules" substring (or its first eleven UTF-16 units if there is none).
// Replay only candidates whose manifest bytes and exact directory realpath have
// already entered this program's admitted ledger. No outside path is inspected
// while deriving this class, and arbitrary external typeRoots remain refused.
fn captured_peer_directory_candidate(
    candidate: &Path,
    witness: &TypeScriptProjectWitness,
    observations: &ResolverObservationLedger,
) -> bool {
    let Some(directory) = observations.peer_directory_candidates.get(candidate) else {
        return false;
    };
    let Some(manifest) = observations.captured_peer_manifests.get(directory) else {
        return false;
    };
    manifest.activated
        && witness.path_is_admitted(directory)
        && observations
            .loaded_sources
            .get(manifest.source_path.as_ref())
            .is_some_and(|source| source.content_id == manifest.content_id)
        && captured_canonical_realpath(directory, observations)
}

fn valid_peer_package_key(key: &str) -> bool {
    let valid_name = |name: &str| {
        !name.is_empty()
            && name != "."
            && name != ".."
            && name.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'~')
            })
    };
    let mut names = key.split('/');
    let Some(first) = names.next() else {
        return false;
    };
    if let Some(scope) = first.strip_prefix('@') {
        valid_name(scope) && names.next().is_some_and(valid_name) && names.next().is_none()
    } else {
        valid_name(first) && names.next().is_none()
    }
}

fn captured_canonical_realpath(directory: &Path, observations: &ResolverObservationLedger) -> bool {
    observations
        .realpath_queries_by_target
        .get(directory)
        .and_then(|lexical| observations.realpaths.get(lexical))
        .is_some_and(|proof| proof.canonical_path.as_deref() == Some(directory))
}

fn charge_closure_replay_work(
    work: &mut u64,
    units: usize,
    phase: &str,
) -> Result<(), TypeScriptProjectHostError> {
    let next = work.saturating_add(u64::try_from(units).unwrap_or(u64::MAX));
    let maximum = super::typescript_program::MAX_CLOSURE_REPLAY_WORK_UNITS;
    if next > maximum {
        return Err(TypeScriptProjectHostError::CompilerIoClosureLimit {
            phase: phase.into(),
            detail: format!("observed {next}, maximum {maximum}").into_boxed_str(),
        });
    }
    *work = next;
    Ok(())
}

fn ensure_peer_index_capacity(
    observations: &ResolverObservationLedger,
    entries: usize,
    metadata: usize,
    path: &Path,
) -> Result<(), TypeScriptProjectHostError> {
    let count = observations.observation_count().saturating_add(entries);
    if count > MAX_RESOLVER_OBSERVATIONS {
        return Err(TypeScriptProjectHostError::ResolverObservationLimit {
            observed: count,
            maximum: MAX_RESOLVER_OBSERVATIONS,
        });
    }
    ensure_metadata_capacity(observations, metadata, path)
}

fn typescript_peer_directory_prefix(directory: &str) -> Option<&str> {
    if let Some(index) = directory.rfind("node_modules") {
        return Some(&directory[..index + "node_modules".len()]);
    }
    let mut utf16_units = 0;
    for (index, character) in directory.char_indices() {
        utf16_units += character.len_utf16();
        if utf16_units == "node_modules".len() - 1 {
            return Some(&directory[..index + character.len_utf8()]);
        }
        if utf16_units > "node_modules".len() - 1 {
            // A split surrogate cannot be represented by a canonical Rust path.
            return None;
        }
    }
    Some(directory)
}

// Only absence and the nearest parent's identity are inspected outside the capability.
// This never enumerates or reads an external directory's contents.
fn capture_external_negative_parent(
    path: &Path,
) -> Result<RealpathSnapshot, TypeScriptProjectHostError> {
    if !matches!(fs::symlink_metadata(path), Err(ref error) if error.kind() == io::ErrorKind::NotFound)
    {
        return Err(TypeScriptProjectHostError::WitnessChanged {
            path: path.to_path_buf().into_boxed_path(),
        });
    }
    for parent in path.ancestors().skip(1) {
        match fs::symlink_metadata(parent) {
            Ok(_) => {
                let Some((canonical, identity)) = capture_realpath_snapshot(parent)? else {
                    return Err(TypeScriptProjectHostError::WitnessChanged {
                        path: parent.to_path_buf().into_boxed_path(),
                    });
                };
                if !fs::metadata(&canonical).is_ok_and(|metadata| metadata.is_dir()) {
                    return Err(TypeScriptProjectHostError::RegularDirectoryRequired {
                        path: canonical.into_boxed_path(),
                    });
                }
                return Ok(RealpathSnapshot {
                    path: parent.to_path_buf().into_boxed_path(),
                    canonical_path: Some(canonical.into_boxed_path()),
                    identity,
                });
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(TypeScriptProjectHostError::PackagePath {
                    path: parent.to_path_buf().into_boxed_path(),
                    source,
                });
            }
        }
    }
    Err(TypeScriptProjectHostError::SourceOutsideCapability {
        path: path.to_path_buf().into_boxed_path(),
    })
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
                    candidate = parent.parent();
                    continue;
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
            Err(source)
                if matches!(
                    source.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
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

// Count complete snapshots in real filesystem regression controls only.
#[cfg(all(test, unix))]
std::thread_local! {
    static DIRECTORY_SNAPSHOT_CAPTURES: std::cell::Cell<u64> = const {
        std::cell::Cell::new(0)
    };
}

fn capture_directory_snapshot(
    path: &Path,
    witness: &TypeScriptProjectWitness,
) -> Result<DirectorySnapshot, TypeScriptProjectHostError> {
    capture_directory_snapshot_controlled(path, witness, &mut || Ok(()))
}

fn capture_directory_snapshot_controlled<F>(
    path: &Path,
    witness: &TypeScriptProjectWitness,
    control: &mut F,
) -> Result<DirectorySnapshot, TypeScriptProjectHostError>
where
    F: FnMut() -> Result<(), TypeScriptProjectHostError> + ?Sized,
{
    control()?;
    #[cfg(all(test, unix))]
    DIRECTORY_SNAPSHOT_CAPTURES.with(|count| count.set(count.get() + 1));
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
    let identity = file_identity(&metadata, &canonical, None).map_err(|source| {
        TypeScriptProjectHostError::PackagePath {
            path: canonical.clone().into_boxed_path(),
            source,
        }
    })?;
    let mut entries = Vec::new();
    for entry in
        fs::read_dir(&canonical).map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: canonical.clone().into_boxed_path(),
            source,
        })?
    {
        control()?;
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
        let entry_identity =
            file_identity(&entry_metadata, &entry_path, None).map_err(|source| {
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
        let effective_kind = match canonical_path.as_deref() {
            Some(path) => {
                let metadata = fs::metadata(path).map_err(|source| {
                    TypeScriptProjectHostError::PackagePath {
                        path: path.to_path_buf().into_boxed_path(),
                        source,
                    }
                })?;
                Some(if metadata.is_dir() {
                    TypeScriptDirectoryEntryKind::Directory
                } else if metadata.is_file() {
                    TypeScriptDirectoryEntryKind::RegularFile
                } else {
                    TypeScriptDirectoryEntryKind::Other
                })
            }
            None => None,
        };
        entries.push(TypeScriptDirectoryEntry {
            path: entry_path.into_boxed_path(),
            canonical_path,
            kind,
            effective_kind,
            identity: entry_identity,
        });
    }
    control()?;
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

fn update_external_absent_directory_path(
    digest: &mut Hasher,
    path: &Path,
) -> Result<(), TypeScriptProjectHostError> {
    let logical = path
        .to_str()
        .ok_or_else(|| TypeScriptProjectHostError::NonPortablePath {
            path: path.to_path_buf().into_boxed_path(),
        })?;
    // External absence is location-bound, including its parent identity. It
    // cannot be mistaken for a workspace-relative positive source authority.
    digest.update(&[4]);
    update_len(digest, logical.len());
    digest.update(logical.as_bytes());
    Ok(())
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
    } else if witness.external_negative_module_candidate(path) {
        let index = witness
            .workspace_root
            .ancestors()
            .skip(1)
            .take(MAX_PROJECT_ANCESTORS)
            .position(|ancestor| {
                path == ancestor.join("node_modules")
                    || path == ancestor.join("node_modules/@types")
            })
            .expect("bounded candidate ancestor");
        digest.update(&[3]);
        update_len(digest, index);
        digest.update(&[u8::from(
            path.file_name().is_some_and(|name| name == "@types"),
        )]);
        return Ok(());
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
        content_id: input.content_id,
    }
}

fn snapshot_from_config(input: &TypeScriptConfigInput) -> FileSnapshot {
    FileSnapshot {
        path: input.path.clone(),
        identity: input.identity,
        length: u64::try_from(input.bytes.len()).unwrap_or(u64::MAX),
        digest: *blake3::hash(&input.bytes).as_bytes(),
        content_id: input.content_id,
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

fn require_compiler_api_entry(
    package_root: &Path,
    package_version: &str,
) -> Result<(), TypeScriptProjectHostError> {
    let entrypoint = package_root.join("lib/typescript.js");
    let metadata = match fs::symlink_metadata(&entrypoint) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(TypeScriptProjectHostError::CompilerApiEntryMissing {
                package_root: package_root.to_path_buf().into_boxed_path(),
                package_version: package_version.to_owned().into_boxed_str(),
                entrypoint: entrypoint.into_boxed_path(),
            });
        }
        Err(source) => {
            return Err(TypeScriptProjectHostError::PackagePath {
                path: entrypoint.into_boxed_path(),
                source,
            });
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(TypeScriptProjectHostError::ModuleEntrySymlink {
            path: entrypoint.into_boxed_path(),
        });
    }
    if !metadata.is_file() {
        return Err(TypeScriptProjectHostError::RegularFileRequired {
            path: entrypoint.into_boxed_path(),
        });
    }
    Ok(())
}

fn witness_fingerprint(files: &[FileSnapshot]) -> [u8; 32] {
    let mut digest = Hasher::new();
    digest.update(b"compiler.typescript.immutable-files.v2\0");
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
    read_regular_file_controlled(path, maximum_bytes, &mut || Ok(()))
}

fn read_regular_file_controlled(
    path: &Path,
    maximum_bytes: u64,
    control: &mut dyn FnMut() -> Result<(), TypeScriptProjectHostError>,
) -> Result<(FileSnapshot, Vec<u8>), TypeScriptProjectHostError> {
    control()?;
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
        control()?;
        File::open(path).map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: path.to_path_buf().into_boxed_path(),
            source,
        })?
    };

    control()?;
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
    control()?;
    let identity = file_identity(&before, path, Some(&file)).map_err(|source| {
        TypeScriptProjectHostError::PackagePath {
            path: path.to_path_buf().into_boxed_path(),
            source,
        }
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
    control()?;
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
    control()?;
    let after = file
        .metadata()
        .map_err(|source| TypeScriptProjectHostError::PackagePath {
            path: path.to_path_buf().into_boxed_path(),
            source,
        })?;
    control()?;
    if identity
        != file_identity(&after, path, Some(&file)).map_err(|source| {
            TypeScriptProjectHostError::PackagePath {
                path: path.to_path_buf().into_boxed_path(),
                source,
            }
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
        content_id: ContentId::<SourceFactDomain>::from_canonical_bytes(&bytes),
    };
    control()?;
    Ok((snapshot, bytes))
}

#[cfg(unix)]
fn file_identity(
    metadata: &fs::Metadata,
    _path: &Path,
    _file: Option<&fs::File>,
) -> io::Result<FileIdentity> {
    use std::os::unix::fs::MetadataExt;

    Ok(FileIdentity {
        first: metadata.dev(),
        second: metadata.ino(),
    })
}

#[cfg(windows)]
fn file_identity(
    _metadata: &fs::Metadata,
    path: &Path,
    file: Option<&fs::File>,
) -> io::Result<FileIdentity> {
    let identity = match file {
        Some(file) => backend_platform::FileIdentity::of_file(file)?,
        None => backend_platform::FileIdentity::of_path_nofollow(path)?,
    };
    let (first, second) = identity.parts();
    Ok(FileIdentity { first, second })
}

#[cfg(not(any(unix, windows)))]
fn file_identity(
    _metadata: &fs::Metadata,
    _path: &Path,
    _file: Option<&fs::File>,
) -> io::Result<FileIdentity> {
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
    let mut observed_entries = 0_usize;
    collect_regular_module_paths(
        &package_root,
        &package_root,
        0,
        &mut observed_entries,
        &mut paths,
    )?;
    paths.sort_unstable();
    if paths.len() > MAX_TYPESCRIPT_PACKAGE_FILES {
        return Err(TypeScriptProjectHostError::ModuleFileLimit {
            package_root: package_root.clone().into_boxed_path(),
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

fn compiler_package_admission_error(
    error: TypeScriptProjectHostError,
    package_version: &str,
    role: backend_library::interface::TypeScriptCompilerPackageRole,
) -> TypeScriptProjectHostError {
    if matches!(
        error,
        TypeScriptProjectHostError::ModuleFileLimit { .. }
            | TypeScriptProjectHostError::ModuleEntryLimit { .. }
            | TypeScriptProjectHostError::ModuleDirectoryDepth { .. }
            | TypeScriptProjectHostError::FileTooLarge { .. }
    ) {
        TypeScriptProjectHostError::CompilerPackageAdmission {
            package_version: package_version.into(),
            role,
            source: Box::new(error),
        }
    } else {
        error
    }
}

fn collect_project_configs(
    project_root: &Path,
    workspace_root: &Path,
    module_root: &Path,
    selected_build_configs: &[PathBuf],
) -> Result<Vec<TypeScriptConfigInput>, TypeScriptProjectHostError> {
    collect_project_configs_inner(
        project_root,
        workspace_root,
        module_root,
        selected_build_configs,
        true,
    )
}

fn collect_project_configs_inner(
    project_root: &Path,
    workspace_root: &Path,
    module_root: &Path,
    selected_build_configs: &[PathBuf],
    follow_extends: bool,
) -> Result<Vec<TypeScriptConfigInput>, TypeScriptProjectHostError> {
    collect_project_configs_inner_controlled(
        project_root,
        workspace_root,
        module_root,
        selected_build_configs,
        follow_extends,
        &mut || Ok(()),
    )
}

fn collect_project_configs_inner_controlled(
    project_root: &Path,
    workspace_root: &Path,
    module_root: &Path,
    selected_build_configs: &[PathBuf],
    follow_extends: bool,
    control: &mut dyn FnMut() -> Result<(), TypeScriptProjectHostError>,
) -> Result<Vec<TypeScriptConfigInput>, TypeScriptProjectHostError> {
    control()?;
    let project_root = canonical_directory(project_root)?;
    let workspace_root = canonical_directory(workspace_root)?;
    let module_root = canonical_directory(module_root)?;
    let mut paths = Vec::new();
    collect_tsconfig_paths_controlled(&workspace_root, &mut paths, 0, control)?;
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
        control()?;
        candidate_paths.insert(canonical_regular_config(
            path,
            &workspace_root,
            &module_root,
        )?);
    }
    let mut selected_paths = std::collections::BTreeSet::new();
    for path in selected_build_configs {
        control()?;
        selected_paths.insert(canonical_regular_config(
            path,
            &workspace_root,
            &module_root,
        )?);
    }
    // Preserve every discovered candidate's bounded bytes and identity. Selection
    // depends on paths, not successful parsing of an unrelated editor config.
    let mut captured = std::collections::BTreeMap::<PathBuf, TypeScriptConfigInput>::new();
    let mut total_bytes = 0_u64;
    for path in &candidate_paths {
        control()?;
        capture_project_config(
            path,
            &candidate_paths,
            &selected_paths,
            &mut captured,
            &mut total_bytes,
        )?;
    }
    let selection = select_project_config_with_basis(&project_root, captured.values());
    if let Some(selection) = selection {
        tracing::info!(target: "compiler.typescript_project",
            config = %selection.config.path.display(),
            basis = ?selection.basis, passed_over = selection.passed_over,
            "selected TypeScript project config");
    }
    let selected = selection.map(|selection| selection.config.path.to_path_buf());
    let mut pending = selected
        .into_iter()
        .map(|path| (path, TypeScriptConfigDependencyAuthority::Program))
        .collect::<Vec<_>>();
    while let Some((path, authority)) = pending.pop() {
        control()?;
        let canonical = canonical_regular_config(&path, &workspace_root, &module_root)?;
        capture_project_config(
            &canonical,
            &candidate_paths,
            &selected_paths,
            &mut captured,
            &mut total_bytes,
        )?;
        let config = captured
            .get_mut(&canonical)
            .expect("captured config exists");
        if config.dependency_authority >= authority {
            continue;
        }
        let value = parse_typescript_config(&canonical, &config.bytes)?;
        let extends = if follow_extends {
            config_paths_from_field_controlled(
                &value,
                &canonical,
                &project_root,
                &workspace_root,
                &module_root,
                ConfigDependencyKind::Extends,
                control,
            )?
        } else {
            Vec::new()
        };
        let references = if authority == TypeScriptConfigDependencyAuthority::Program {
            config_paths_from_field_controlled(
                &value,
                &canonical,
                &project_root,
                &workspace_root,
                &module_root,
                ConfigDependencyKind::References,
                control,
            )?
        } else {
            Vec::new()
        };
        pending.extend(
            extends
                .iter()
                .cloned()
                .map(|path| (path, TypeScriptConfigDependencyAuthority::Inheritance)),
        );
        pending.extend(
            references
                .iter()
                .cloned()
                .map(|path| (path, TypeScriptConfigDependencyAuthority::Program)),
        );
        config.dependency_authority = authority;
        config.extends = extends
            .into_iter()
            .map(PathBuf::into_boxed_path)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        config.references = references
            .into_iter()
            .map(PathBuf::into_boxed_path)
            .collect::<Vec<_>>()
            .into_boxed_slice();
    }
    control()?;
    validate_config_graph_acyclic(&captured)?;
    control()?;
    Ok(captured.into_values().collect())
}

fn capture_project_config(
    canonical: &Path,
    candidate_paths: &std::collections::BTreeSet<PathBuf>,
    selected_paths: &std::collections::BTreeSet<PathBuf>,
    captured: &mut std::collections::BTreeMap<PathBuf, TypeScriptConfigInput>,
    total_bytes: &mut u64,
) -> Result<(), TypeScriptProjectHostError> {
    if captured.contains_key(canonical) {
        return Ok(());
    }
    if captured.len() >= MAX_PROJECT_CONFIG_FILES {
        return Err(TypeScriptProjectHostError::ConfigFileLimit {
            observed: captured.len().saturating_add(1),
            maximum: MAX_PROJECT_CONFIG_FILES,
        });
    }
    let remaining = MAX_PROJECT_CONFIG_BYTES.saturating_sub(*total_bytes);
    let (snapshot, bytes) = read_regular_file(canonical, remaining)?;
    *total_bytes = total_bytes.saturating_add(snapshot.length);
    captured.insert(
        canonical.to_path_buf(),
        TypeScriptConfigInput {
            path: snapshot.path,
            content_id: ContentId::<SourceFactDomain>::from_canonical_bytes(&bytes),
            identity: snapshot.identity,
            bytes: bytes.into_boxed_slice(),
            project_candidate: candidate_paths.contains(canonical),
            selected_build_config: selected_paths.contains(canonical),
            dependency_authority: TypeScriptConfigDependencyAuthority::Unselected,
            extends: Box::new([]),
            references: Box::new([]),
        },
    );
    Ok(())
}

/// Why [`select_project_config_with_basis`] chose one config, for diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TypeScriptConfigSelectionBasis {
    /// The one config a build producer (for example `angular.json`) selected.
    ProducerSelected,
    /// Several producer-selected configs; the nearest/ranked one was chosen.
    ProducerSelectedAmong,
    /// `tsconfig.json` in the nearest directory holding project configs, which
    /// is what `tsc` and editors use for that directory.
    Conventional,
    /// The only project config in the nearest directory holding any.
    OnlyCandidate,
    /// Several named configs and no `tsconfig.json`; the ranked first was chosen.
    RankedAmong,
}

/// One deterministic config choice and how many equally eligible configs it passed over.
#[derive(Clone, Copy, Debug)]
pub(crate) struct TypeScriptConfigSelection<'a> {
    pub(crate) config: &'a TypeScriptConfigInput,
    pub(crate) basis: TypeScriptConfigSelectionBasis,
    /// Eligible configs that were not chosen.
    pub(crate) passed_over: usize,
}

/// Conventional role order for named configs sharing a directory with no
/// `tsconfig.json`: application and build configs describe the shipped program,
/// the shared `tsconfig.base.json` only carries options and ranks last.
fn config_rank(config: &TypeScriptConfigInput) -> u8 {
    match config.path.file_name().and_then(std::ffi::OsStr::to_str) {
        Some("tsconfig.json") => 0,
        Some("tsconfig.app.json") => 1,
        Some("tsconfig.build.json") => 2,
        Some("tsconfig.lib.json") => 3,
        Some("tsconfig.base.json") => 5,
        _ => 4,
    }
}

/// A producer-selected build project may leave authored package sources in the
/// conventional editor project. Admit that real independent config separately;
/// never expand the build program's roots or inherit its references. Reference
/// solutions still follow the existing, explicitly captured config DAG.
pub(super) fn supplemental_conventional_config<'a>(
    selected: &TypeScriptConfigInput,
    configs: &'a [TypeScriptConfigInput],
) -> Result<Option<&'a TypeScriptConfigInput>, TypeScriptProjectHostError> {
    if !selected.selected_build_config || !selected.references.is_empty() {
        return Ok(None);
    }
    let Some(config) = configs.iter().find(|config| {
        config.project_candidate
            && config.path != selected.path
            && config.path.parent() == selected.path.parent()
            && config
                .path
                .file_name()
                .is_some_and(|name| name == "tsconfig.json")
    }) else {
        return Ok(None);
    };
    let value = parse_typescript_config(&config.path, &config.bytes)?;
    // A nonempty or malformed reference list cannot be admitted as an
    // independent leaf from an inheritance-only candidate observation.
    if value.get("references").is_some_and(|references| {
        references
            .as_array()
            .is_none_or(|references| !references.is_empty())
    }) {
        return Ok(None);
    }
    Ok(Some(config))
}

/// Select by the admitted build target or the nearest containing config, using
/// the same path-only policy before closure capture and compiler execution.
///
/// This never refuses for ambiguity; it follows a fixed, documented order so the
/// same tree always yields the same program:
///
/// 1. A config selected by a build producer (Angular `build.options.tsConfig`).
///    With several, prefer one whose directory contains the package root
///    (nearest first), then one below it (shallowest first), then by
///    [`config_rank`] and path.
/// 2. Otherwise, among project configs whose directory contains the package
///    root, only those in the nearest such directory compete. `tsconfig.json`
///    wins there (the `tsc -p <dir>` default; a solution-style one expands to
///    its `references` as a program forest), then [`config_rank`], then path.
/// 3. No candidate selects no config (an inferred-project fallback).
pub(crate) fn select_project_config_with_basis<'a>(
    package_root: &Path,
    configs: impl IntoIterator<Item = &'a TypeScriptConfigInput>,
) -> Option<TypeScriptConfigSelection<'a>> {
    let configs = configs.into_iter().collect::<Vec<_>>();
    let mut selected = configs
        .iter()
        .copied()
        .filter(|candidate| candidate.selected_build_config)
        .collect::<Vec<_>>();
    let package_depth = package_root.components().count();
    let directory_depth = |config: &TypeScriptConfigInput| {
        config
            .path
            .parent()
            .map_or(0, |parent| parent.components().count())
    };
    match selected.len() {
        0 => {}
        1 => {
            return Some(TypeScriptConfigSelection {
                config: selected[0],
                basis: TypeScriptConfigSelectionBasis::ProducerSelected,
                passed_over: 0,
            });
        }
        _ => {
            selected.sort_by_cached_key(|config| {
                let directory = config.path.parent().unwrap_or(Path::new("/"));
                let (group, distance) = if package_root.starts_with(directory) {
                    (0_u8, package_depth.saturating_sub(directory_depth(*config)))
                } else if directory.starts_with(package_root) {
                    (1, directory_depth(*config).saturating_sub(package_depth))
                } else {
                    (2, directory_depth(*config))
                };
                (group, distance, config_rank(*config), config.path.clone())
            });
            return Some(TypeScriptConfigSelection {
                config: selected[0],
                basis: TypeScriptConfigSelectionBasis::ProducerSelectedAmong,
                passed_over: selected.len() - 1,
            });
        }
    }
    let nearest = configs
        .iter()
        .copied()
        .filter(|candidate| candidate.project_candidate)
        .filter(|candidate| {
            package_root.starts_with(candidate.path.parent().unwrap_or(Path::new("/")))
        })
        .map(directory_depth)
        .max()?;
    let mut candidates = configs
        .into_iter()
        .filter(|candidate| candidate.project_candidate)
        .filter(|candidate| {
            package_root.starts_with(candidate.path.parent().unwrap_or(Path::new("/")))
        })
        .filter(|candidate| directory_depth(*candidate) == nearest)
        .collect::<Vec<_>>();
    candidates.sort_by_cached_key(|config| (config_rank(*config), config.path.clone()));
    let first = *candidates.first()?;
    let basis = if candidates.len() == 1 {
        TypeScriptConfigSelectionBasis::OnlyCandidate
    } else if config_rank(first) == 0 {
        TypeScriptConfigSelectionBasis::Conventional
    } else {
        TypeScriptConfigSelectionBasis::RankedAmong
    };
    Some(TypeScriptConfigSelection {
        config: first,
        basis,
        passed_over: candidates.len() - 1,
    })
}

/// The config chosen by [`select_project_config_with_basis`].
pub(crate) fn select_project_config<'a>(
    package_root: &Path,
    configs: impl IntoIterator<Item = &'a TypeScriptConfigInput>,
) -> Option<&'a TypeScriptConfigInput> {
    select_project_config_with_basis(package_root, configs).map(|selection| selection.config)
}

fn validate_config_graph_acyclic(
    configs: &std::collections::BTreeMap<PathBuf, TypeScriptConfigInput>,
) -> Result<(), TypeScriptProjectHostError> {
    fn dependencies(config: &TypeScriptConfigInput, kind: ConfigDependencyKind) -> &[Box<Path>] {
        match kind {
            ConfigDependencyKind::Extends => &config.extends,
            ConfigDependencyKind::References => &config.references,
        }
    }

    fn visit(
        path: &Path,
        configs: &std::collections::BTreeMap<PathBuf, TypeScriptConfigInput>,
        kind: ConfigDependencyKind,
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
            for dependency in dependencies(config, kind) {
                visit(dependency, configs, kind, visiting, visited)?;
            }
        }
        visiting.remove(path);
        visited.insert(path.to_path_buf());
        Ok(())
    }

    for kind in [
        ConfigDependencyKind::Extends,
        ConfigDependencyKind::References,
    ] {
        let mut visiting = std::collections::BTreeSet::new();
        let mut visited = std::collections::BTreeSet::new();
        for path in configs.keys() {
            visit(path, configs, kind, &mut visiting, &mut visited)?;
        }
    }
    Ok(())
}

fn angular_build_config_paths(
    workspace_root: &Path,
    module_root: &Path,
) -> Result<(Vec<PathBuf>, Option<FileSnapshot>), TypeScriptProjectHostError> {
    angular_build_config_paths_controlled(workspace_root, module_root, &mut || Ok(()))
}

fn angular_build_config_paths_controlled(
    workspace_root: &Path,
    module_root: &Path,
    control: &mut dyn FnMut() -> Result<(), TypeScriptProjectHostError>,
) -> Result<(Vec<PathBuf>, Option<FileSnapshot>), TypeScriptProjectHostError> {
    control()?;
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
        control()?;
        for targets_key in ["architect", "targets"] {
            control()?;
            if let Some(targets) = project
                .get(targets_key)
                .and_then(serde_json::Value::as_object)
                && let Some(build) = targets.get("build")
            {
                collect_angular_tsconfig_controlled(build, &mut selected, &path, control)?;
            }
        }
    }
    let mut resolved = Vec::new();
    for raw in selected {
        control()?;
        let candidate = workspace_root.join(raw);
        resolved.push(resolve_relative_config(
            &candidate,
            &workspace_root,
            &module_root,
        )?);
    }
    resolved.sort_unstable();
    resolved.dedup();
    control()?;
    Ok((resolved, Some(snapshot)))
}

fn collect_angular_tsconfig_controlled(
    target: &serde_json::Value,
    output: &mut Vec<String>,
    workspace: &Path,
    control: &mut dyn FnMut() -> Result<(), TypeScriptProjectHostError>,
) -> Result<(), TypeScriptProjectHostError> {
    control()?;
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
            control()?;
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

fn collect_tsconfig_paths_controlled(
    directory: &Path,
    output: &mut Vec<PathBuf>,
    depth: usize,
    control: &mut dyn FnMut() -> Result<(), TypeScriptProjectHostError>,
) -> Result<(), TypeScriptProjectHostError> {
    control()?;
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
        control()?;
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
            collect_tsconfig_paths_controlled(&path, output, depth + 1, control)?;
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
    control()?;
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

fn jsonc_unicode_line_break(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\xe2\x80\xa8") || bytes.starts_with(b"\xe2\x80\xa9")
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
            while index < bytes.len()
                && !matches!(bytes[index], b'\n' | b'\r')
                && !jsonc_unicode_line_break(&bytes[index..])
            {
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
        } else if jsonc_unicode_line_break(&bytes[index..]) {
            // TypeScript's scanner treats U+2028/U+2029 as line terminators.
            // serde_json needs ASCII JSON whitespace outside strings; retain
            // byte width here while the source witness keeps the raw bytes.
            output.extend_from_slice(b"  \n");
            index += 3;
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

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(
    tag = "state",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum SnapshotOption<T> {
    Absent,
    Present(T),
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeTypeScriptHostSnapshotDto {
    version: u32,
    project_root: String,
    home_root: SnapshotOption<String>,
    module_root: String,
    workspace: SnapshotOption<NativeWorkspaceSnapshotDto>,
    workspace_root: String,
    files: BoundedSnapshotVec<NativeFileSnapshotDto, MAX_PROJECT_CONFIG_FILES>,
    typescript_files: BoundedSnapshotVec<NativeSourceSnapshotDto, 0>,
    config_candidates: BoundedSnapshotVec<NativeConfigSnapshotDto, MAX_PROJECT_CONFIG_FILES>,
    selected_build_config_paths: BoundedSnapshotVec<String, MAX_PROJECT_CONFIG_FILES>,
    config_paths: BoundedSnapshotVec<String, MAX_PROJECT_CONFIG_FILES>,
    loaded_source_files:
        BoundedSnapshotVec<(String, NativeFileSnapshotDto), MAX_RESOLVED_SOURCE_FILES>,
    fingerprint: [u8; 32],
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeFileSnapshotDto {
    path: String,
    identity_first: u64,
    identity_second: u128,
    length: u64,
    digest: [u8; 32],
    content_id: [u8; 32],
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeSourceSnapshotDto {
    path: String,
    bytes: BoundedSnapshotVec<u8, { MAX_SOURCE_FILE_BYTES as usize }>,
    content_id: [u8; 32],
    identity_first: u64,
    identity_second: u128,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeConfigSnapshotDto {
    path: String,
    bytes: BoundedSnapshotVec<u8, { MAX_PROJECT_CONFIG_BYTES as usize }>,
    content_id: [u8; 32],
    identity_first: u64,
    identity_second: u128,
    project_candidate: bool,
    selected_build_config: bool,
    dependency_authority: u8,
    extends: BoundedSnapshotVec<String, MAX_PROJECT_ANCESTORS>,
    references: BoundedSnapshotVec<String, MAX_PROJECT_ANCESTORS>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeWorkspaceSnapshotDto {
    root: String,
    files: BoundedSnapshotVec<NativeFileSnapshotDto, MAX_PROJECT_CONFIG_FILES>,
}

#[derive(Debug)]
struct BoundedSnapshotVec<T, const MAX: usize>(Vec<T>);

impl<T: serde::Serialize, const MAX: usize> serde::Serialize for BoundedSnapshotVec<T, MAX> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serde::Serialize::serialize(&self.0, serializer)
    }
}

impl<'de, T: serde::Deserialize<'de>, const MAX: usize> serde::Deserialize<'de>
    for BoundedSnapshotVec<T, MAX>
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct BoundedVisitor<T, const MAX: usize>(std::marker::PhantomData<T>);
        impl<'de, T: serde::Deserialize<'de>, const MAX: usize> serde::de::Visitor<'de>
            for BoundedVisitor<T, MAX>
        {
            type Value = BoundedSnapshotVec<T, MAX>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(formatter, "at most {MAX} bounded snapshot records")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut values = Vec::new();
                while values.len() < MAX {
                    let Some(value) = seq.next_element()? else {
                        return Ok(BoundedSnapshotVec(values));
                    };
                    values.push(value);
                }
                if seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
                    return Err(serde::de::Error::custom(
                        "snapshot record count exceeds bound",
                    ));
                }
                Ok(BoundedSnapshotVec(values))
            }
        }
        deserializer.deserialize_seq(BoundedVisitor::<T, MAX>(std::marker::PhantomData))
    }
}

fn snapshot_path(path: &Path) -> Option<String> {
    path.to_str().map(str::to_owned)
}

fn snapshot_decode_path(path: String) -> Option<PathBuf> {
    if path.is_empty() {
        return None;
    }
    let decoded = PathBuf::from(&path);
    (decoded.is_absolute() && decoded.to_str() == Some(path.as_str())).then_some(decoded)
}

fn snapshot_identity(identity: FileIdentity) -> (u64, u128) {
    (identity.first, identity.second.into())
}

fn snapshot_decode_identity(first: u64, second: u128) -> Option<FileIdentity> {
    Some(FileIdentity {
        first,
        second: second.try_into().ok()?,
    })
}

fn snapshot_file(file: &FileSnapshot) -> Option<NativeFileSnapshotDto> {
    let (identity_first, identity_second) = snapshot_identity(file.identity);
    Some(NativeFileSnapshotDto {
        path: snapshot_path(&file.path)?,
        identity_first,
        identity_second,
        length: file.length,
        digest: file.digest,
        content_id: *file.content_id.as_ref(),
    })
}

fn snapshot_decode_file(file: NativeFileSnapshotDto) -> Option<FileSnapshot> {
    Some(FileSnapshot {
        path: snapshot_decode_path(file.path)?.into_boxed_path(),
        identity: snapshot_decode_identity(file.identity_first, file.identity_second)?,
        length: file.length,
        digest: file.digest,
        content_id: ContentId::<SourceFactDomain>::try_from(file.content_id).ok()?,
    })
}

fn snapshot_source(file: &TypeScriptFileInput) -> Option<NativeSourceSnapshotDto> {
    let (identity_first, identity_second) = snapshot_identity(file.identity);
    Some(NativeSourceSnapshotDto {
        path: snapshot_path(&file.path)?,
        bytes: BoundedSnapshotVec(file.bytes.to_vec()),
        content_id: *file.content_id.as_ref(),
        identity_first,
        identity_second,
    })
}

fn snapshot_decode_source(file: NativeSourceSnapshotDto) -> Option<TypeScriptFileInput> {
    if ContentId::<SourceFactDomain>::from_canonical_bytes(&file.bytes.0).as_ref()
        != &file.content_id
    {
        return None;
    }
    Some(TypeScriptFileInput {
        path: snapshot_decode_path(file.path)?.into_boxed_path(),
        bytes: file.bytes.0.into_boxed_slice(),
        content_id: ContentId::<SourceFactDomain>::try_from(file.content_id).ok()?,
        identity: snapshot_decode_identity(file.identity_first, file.identity_second)?,
    })
}

fn snapshot_config(config: &TypeScriptConfigInput) -> Option<NativeConfigSnapshotDto> {
    let (identity_first, identity_second) = snapshot_identity(config.identity);
    Some(NativeConfigSnapshotDto {
        path: snapshot_path(&config.path)?,
        bytes: BoundedSnapshotVec(config.bytes.to_vec()),
        content_id: *config.content_id.as_ref(),
        identity_first,
        identity_second,
        project_candidate: config.project_candidate,
        selected_build_config: config.selected_build_config,
        dependency_authority: match config.dependency_authority {
            TypeScriptConfigDependencyAuthority::Unselected => 0,
            TypeScriptConfigDependencyAuthority::Inheritance => 1,
            TypeScriptConfigDependencyAuthority::Program => 2,
        },
        extends: BoundedSnapshotVec(
            config
                .extends
                .iter()
                .map(|path| snapshot_path(path))
                .collect::<Option<_>>()?,
        ),
        references: BoundedSnapshotVec(
            config
                .references
                .iter()
                .map(|path| snapshot_path(path))
                .collect::<Option<_>>()?,
        ),
    })
}

fn snapshot_decode_config(config: NativeConfigSnapshotDto) -> Option<TypeScriptConfigInput> {
    if ContentId::<SourceFactDomain>::from_canonical_bytes(&config.bytes.0).as_ref()
        != &config.content_id
    {
        return None;
    }
    Some(TypeScriptConfigInput {
        path: snapshot_decode_path(config.path)?.into_boxed_path(),
        bytes: config.bytes.0.into_boxed_slice(),
        content_id: ContentId::<SourceFactDomain>::try_from(config.content_id).ok()?,
        identity: snapshot_decode_identity(config.identity_first, config.identity_second)?,
        project_candidate: config.project_candidate,
        selected_build_config: config.selected_build_config,
        dependency_authority: match config.dependency_authority {
            0 => TypeScriptConfigDependencyAuthority::Unselected,
            1 => TypeScriptConfigDependencyAuthority::Inheritance,
            2 => TypeScriptConfigDependencyAuthority::Program,
            _ => return None,
        },
        extends: config
            .extends
            .0
            .into_iter()
            .map(snapshot_decode_path)
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .map(PathBuf::into_boxed_path)
            .collect::<Vec<_>>()
            .into_boxed_slice(),
        references: config
            .references
            .0
            .into_iter()
            .map(snapshot_decode_path)
            .collect::<Option<Vec<_>>>()?
            .into_iter()
            .map(PathBuf::into_boxed_path)
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    })
}

fn snapshot_workspace(workspace: &WorkspaceBoundary) -> Option<NativeWorkspaceSnapshotDto> {
    Some(NativeWorkspaceSnapshotDto {
        root: snapshot_path(&workspace.root)?,
        files: BoundedSnapshotVec(
            workspace
                .files
                .iter()
                .map(snapshot_file)
                .collect::<Option<_>>()?,
        ),
    })
}

fn snapshot_decode_workspace(workspace: NativeWorkspaceSnapshotDto) -> Option<WorkspaceBoundary> {
    Some(WorkspaceBoundary {
        root: snapshot_decode_path(workspace.root)?.into_boxed_path(),
        files: workspace
            .files
            .0
            .into_iter()
            .map(snapshot_decode_file)
            .collect::<Option<Vec<_>>>()?
            .into_boxed_slice(),
    })
}

#[cfg(test)]
fn config_paths_from_field(
    value: &serde_json::Value,
    source: &Path,
    project_root: &Path,
    workspace_root: &Path,
    module_root: &Path,
    kind: ConfigDependencyKind,
) -> Result<Vec<PathBuf>, TypeScriptProjectHostError> {
    config_paths_from_field_controlled(
        value,
        source,
        project_root,
        workspace_root,
        module_root,
        kind,
        &mut || Ok(()),
    )
}

fn config_paths_from_field_controlled(
    value: &serde_json::Value,
    source: &Path,
    project_root: &Path,
    workspace_root: &Path,
    module_root: &Path,
    kind: ConfigDependencyKind,
    control: &mut dyn FnMut() -> Result<(), TypeScriptProjectHostError>,
) -> Result<Vec<PathBuf>, TypeScriptProjectHostError> {
    control()?;
    let field = match kind {
        ConfigDependencyKind::Extends => "extends",
        ConfigDependencyKind::References => "references",
    };
    let Some(field_value) = value.get(field) else {
        return Ok(Vec::new());
    };
    let strings = match kind {
        ConfigDependencyKind::References => {
            let entries = field_value.as_array().ok_or_else(|| {
                TypeScriptProjectHostError::ConfigInvalid {
                    config: source.to_path_buf().into_boxed_path(),
                    message: "references must be an array".into(),
                }
            })?;
            entries
                .iter()
                .map(|entry| {
                    control()?;
                    entry
                        .get("path")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| TypeScriptProjectHostError::ConfigInvalid {
                            config: source.to_path_buf().into_boxed_path(),
                            message: "each reference must contain a string path".into(),
                        })
                })
                .collect::<Result<Vec<_>, _>>()?
        }
        ConfigDependencyKind::Extends => match field_value {
            serde_json::Value::String(value) => vec![value.as_str()],
            serde_json::Value::Array(values) => values
                .iter()
                .map(|entry| {
                    control()?;
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
        },
    };
    let mut resolved = Vec::new();
    for raw in strings {
        control()?;
        if kind == ConfigDependencyKind::References
            || raw.starts_with('.')
            || Path::new(raw).is_absolute()
        {
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
    control()?;
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
    package_root: &Path,
    directory: &Path,
    depth: usize,
    observed_entries: &mut usize,
    output: &mut Vec<PathBuf>,
) -> Result<(), TypeScriptProjectHostError> {
    if depth > MAX_TYPESCRIPT_PACKAGE_DEPTH {
        return Err(TypeScriptProjectHostError::ModuleDirectoryDepth {
            package_root: package_root.to_path_buf().into_boxed_path(),
            observed: depth,
            maximum: MAX_TYPESCRIPT_PACKAGE_DEPTH,
        });
    }
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
        *observed_entries = observed_entries.checked_add(1).ok_or_else(|| {
            TypeScriptProjectHostError::ModuleEntryLimit {
                package_root: package_root.to_path_buf().into_boxed_path(),
                observed: usize::MAX,
                maximum: MAX_TYPESCRIPT_PACKAGE_ENTRIES,
            }
        })?;
        if *observed_entries > MAX_TYPESCRIPT_PACKAGE_ENTRIES {
            return Err(TypeScriptProjectHostError::ModuleEntryLimit {
                package_root: package_root.to_path_buf().into_boxed_path(),
                observed: *observed_entries,
                maximum: MAX_TYPESCRIPT_PACKAGE_ENTRIES,
            });
        }
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
            collect_regular_module_paths(package_root, &path, depth + 1, observed_entries, output)?;
        } else if metadata.is_file() {
            let relative = path.strip_prefix(package_root).map_err(|_| {
                TypeScriptProjectHostError::WitnessChanged {
                    path: path.clone().into_boxed_path(),
                }
            })?;
            if super::toolchain_probe::typescript_module_file_is_runtime_input(relative) {
                output.push(path);
            }
        } else {
            return Err(TypeScriptProjectHostError::RegularFileRequired {
                path: path.into_boxed_path(),
            });
        }
        if output.len() > MAX_TYPESCRIPT_PACKAGE_FILES {
            return Err(TypeScriptProjectHostError::ModuleFileLimit {
                package_root: package_root.to_path_buf().into_boxed_path(),
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
    authority: TypeScriptProjectHostAuthority,
    explicit_compiler: Option<Box<Path>>,
    installed_default_compiler: Option<Box<Path>>,
    installed_default_module_root: Option<Box<Path>>,
    installed_default_origin: TypeScriptSelectionOrigin,
    node: Option<Box<Path>>,
    node_origin: Option<TypeScriptSelectionOrigin>,
    home_root: Option<Box<Path>>,
    explicit_module_root: Option<Box<Path>>,
    report_program: Option<Box<Path>>,
    bundled_application: Option<Box<Path>>,
    probe_limits: ToolchainProbeLimits,
    node_identity: std::sync::Arc<std::sync::Mutex<Option<NodeRuntimeIdentity>>>,
}

#[derive(Clone, Copy, Debug)]
enum TypeScriptProjectHostAuthority {
    Native,
    CompilerApi,
}

#[derive(Clone, Debug)]
struct NodeRuntimeIdentity {
    path: Box<Path>,
    version: Box<[u8]>,
    snapshot: FileSnapshot,
}

impl TypeScriptProjectHost {
    /// Local defaults bind directly to the linked TSZ authority. Host compiler,
    /// Node, report-helper and PATH selection cannot enter this variant.
    pub(crate) fn direct(home_root: Option<PathBuf>, probe_limits: ToolchainProbeLimits) -> Self {
        let mut host =
            Self::new_with_node_origin(None, None, None, home_root, None, None, probe_limits);
        host.authority = TypeScriptProjectHostAuthority::Native;
        host
    }

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
            authority: TypeScriptProjectHostAuthority::CompilerApi,
            explicit_compiler: explicit_compiler.map(PathBuf::into_boxed_path),
            installed_default_compiler: None,
            installed_default_module_root: None,
            installed_default_origin: TypeScriptSelectionOrigin::InstalledHostSelection,
            node: node.map(PathBuf::into_boxed_path),
            node_origin,
            home_root: home_root.map(PathBuf::into_boxed_path),
            explicit_module_root: explicit_module_root.map(PathBuf::into_boxed_path),
            report_program: report_program.map(PathBuf::into_boxed_path),
            bundled_application: None,
            probe_limits,
            node_identity: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Supplies the exact installed-host compiler pair as a fallback for packages without a
    /// project-local TypeScript installation. Project-local modules retain precedence.
    pub(crate) fn with_installed_default(
        mut self,
        compiler: Option<PathBuf>,
        module_root: Option<PathBuf>,
    ) -> Self {
        self.installed_default_compiler = compiler.map(PathBuf::into_boxed_path);
        self.installed_default_module_root = module_root.map(PathBuf::into_boxed_path);
        self
    }

    /// Tags the coherent installed fallback with the source from which its exact paths came.
    #[must_use]
    pub(crate) fn with_installed_default_origin(
        mut self,
        origin: TypeScriptSelectionOrigin,
    ) -> Self {
        self.installed_default_origin = origin;
        self
    }

    /// Keeps only the captured distribution locator. The request selects project authority
    /// before any bundle SDK bytes are hashed; no current executable or ambient path is read.
    pub(crate) fn with_bundled_application(mut self, application: Option<PathBuf>) -> Self {
        self.bundled_application = application.map(PathBuf::into_boxed_path);
        self
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
    #[cfg(test)]
    pub(crate) fn admit(
        &self,
        package_root: &Path,
    ) -> Result<Option<AdmittedTypeScriptProject>, TypeScriptProjectHostError> {
        self.admit_controlled(package_root, &mut || Ok(()))
    }

    pub(crate) fn admit_controlled(
        &self,
        package_root: &Path,
        control: &mut dyn FnMut() -> Result<(), TypeScriptProjectHostError>,
    ) -> Result<Option<AdmittedTypeScriptProject>, TypeScriptProjectHostError> {
        control()?;
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
        if matches!(self.authority, TypeScriptProjectHostAuthority::Native) {
            let witness = TypeScriptProjectWitness::capture_native_controlled(
                &project_root,
                home_root.as_deref(),
                control,
            )?;
            let _ = witness.validated_controlled(control)?;
            return Ok(Some(AdmittedTypeScriptProject::Native {
                fingerprint: witness.fingerprint,
                witness: std::sync::Arc::new(witness),
            }));
        }
        let mut resource_proof = None;
        let mut bundled_node = None;
        let mut bundled_origin = TypeScriptSelectionOrigin::ValidatedApplicationBundle;
        let project = match find_project_typescript_with_home(&project_root, home_root.as_deref())?
        {
            ProjectTypeScriptSearch::Found(project) => Some(project),
            ProjectTypeScriptSearch::NotFound => {
                match self.installed_host_project(&project_root, home_root.as_deref())? {
                    Some(project) => Some(project),
                    None if self.explicit_compiler.is_none()
                        && self.explicit_module_root.is_none()
                        && self.report_program.is_none() =>
                    {
                        match self.bundled_application.as_deref() {
                            Some(application) => {
                                let sdk = crate::application::host::bundled_typescript_sdk(
                                    Some(application),
                                    self.node.is_none(),
                                )
                                .map_err(bundle_admission_error)?;
                                match sdk {
                                    Some(sdk) => {
                                        let (module_root, version) =
                                            read_typescript_module(&sdk.module_root)?;
                                        resource_proof = Some(sdk.proof);
                                        bundled_node = sdk.node;
                                        bundled_origin = sdk.origin;
                                        Some(ProjectTypeScript {
                                            module_root,
                                            compiler: sdk.compiler,
                                            version,
                                            workspace: discover_workspace_boundary(
                                                &project_root,
                                                home_root.as_deref(),
                                            )?,
                                            discovery_origin:
                                                TypeScriptProjectDiscoveryOrigin::InstalledFallback,
                                            compiler_origin: sdk.origin,
                                        })
                                    }
                                    None => None,
                                }
                            }
                            None => None,
                        }
                    }
                    None => None,
                }
            }
            ProjectTypeScriptSearch::Pnp(marker) => {
                return Err(TypeScriptProjectHostError::YarnPnpUnsupported {
                    marker: marker.into_boxed_path(),
                });
            }
        };

        let Some(project) = project else {
            return Ok(None);
        };

        let (selected_compiler, compiler_origin) = match self.explicit_compiler.as_deref() {
            Some(compiler) => (compiler, TypeScriptSelectionOrigin::ExplicitConfiguration),
            None => (project.compiler.as_path(), project.compiler_origin),
        };
        let compiler = fs::canonicalize(selected_compiler).map_err(|source| {
            TypeScriptProjectHostError::PackagePath {
                path: selected_compiler.to_path_buf().into_boxed_path(),
                source,
            }
        })?;
        // Runtime selection is independent of package origin but is closed before probing.
        // A project SDK may use the bundled runtime without admitting the bundled package.
        let bundled_runtime;
        let (selected_node, node_origin) = match self.node.as_deref() {
            Some(node) => (
                node,
                self.node_origin
                    .unwrap_or(TypeScriptSelectionOrigin::PlatformLocation),
            ),
            None => {
                bundled_runtime = match bundled_node {
                    Some(node) => node,
                    None => {
                        let runtime = self
                            .bundled_application
                            .as_deref()
                            .map(crate::application::host::bundled_typescript_runtime)
                            .transpose()
                            .map_err(bundle_admission_error)?
                            .flatten();
                        let Some((node, origin, proof)) = runtime else {
                            return Err(TypeScriptProjectHostError::NodeUnavailable {
                                package_root: project_root.clone().into_boxed_path(),
                            });
                        };
                        resource_proof = Some(proof);
                        bundled_origin = origin;
                        node
                    }
                };
                (bundled_runtime.as_path(), bundled_origin)
            }
        };
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
        if let Some(proof) = &resource_proof {
            proof.validate_current().map_err(|source| {
                TypeScriptProjectHostError::BundledSdkAdmission {
                    path: node.clone().into_boxed_path(),
                    message: source.to_string().into_boxed_str(),
                }
            })?;
        }
        require_compiler_api_entry(&package_root, &expected_version)?;
        let node_identity = self.admit_node_runtime(&node)?;
        if let Some(expected) = resource_proof
            .as_ref()
            .and_then(|proof| proof.expected_node_version.as_deref())
        {
            let observed = std::str::from_utf8(&node_identity.version)
                .ok()
                .map(str::trim);
            if observed != Some(expected) {
                return Err(TypeScriptProjectHostError::BundledSdkAdmission {
                    path: node.clone().into_boxed_path(),
                    message: format!("selected Node version differs from its SDK receipt: expected {expected}, observed {}", bounded_text(&node_identity.version)).into_boxed_str(),
                });
            }
        }

        let mut witness = TypeScriptProjectWitness::capture_with_node_snapshot(
            &project_root,
            home_root.as_deref(),
            &project,
            &compiler,
            &node,
            &module_root,
            &package_root,
            &expected_version,
            project.workspace.as_ref(),
            Some(&node_identity.snapshot),
        )?;
        witness.resource_proof = resource_proof;
        witness.validate_current()?;
        let node_version = node_identity.version;

        let compiler_invocation = if is_module_tsc_script(&compiler, &module_root) {
            TypeScriptCompilerInvocation::PackageScript
        } else {
            TypeScriptCompilerInvocation::NativeExecutable
        };
        let version = if compiler_invocation == TypeScriptCompilerInvocation::PackageScript {
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
            witness.selection_fingerprint(),
            compiler_origin,
            node_origin,
        );
        Ok(Some(AdmittedTypeScriptProject::CompilerApi(
            AdmittedCompilerApiTypeScriptProject {
                checker,
                compiler: compiler.to_path_buf().into_boxed_path(),
                compiler_version: version,
                compiler_origin,
                compiler_invocation,
                node: node.to_path_buf().into_boxed_path(),
                node_version,
                node_origin,
                module_root: module_root.into_boxed_path(),
                fingerprint,
                witness: std::sync::Arc::new(witness),
            },
        )))
    }

    fn installed_host_project(
        &self,
        project_root: &Path,
        home_root: Option<&Path>,
    ) -> Result<Option<ProjectTypeScript>, TypeScriptProjectHostError> {
        let compiler = self
            .explicit_compiler
            .as_deref()
            .or(self.installed_default_compiler.as_deref());
        let Some(compiler) = compiler else {
            return Ok(None);
        };
        let module_root = match self
            .explicit_module_root
            .as_deref()
            .or(self.installed_default_module_root.as_deref())
        {
            Some(root) => Some(root.to_path_buf()),
            None => find_module_root_for_compiler(compiler)?,
        };
        let Some(module_root) = module_root else {
            if self.explicit_compiler.is_some() {
                return Err(TypeScriptProjectHostError::ExplicitModuleRootRequired {
                    compiler: compiler.to_path_buf().into_boxed_path(),
                });
            }
            return Ok(None);
        };
        let (module_root, version) = read_typescript_module(&module_root)?;
        let compiler = fs::canonicalize(compiler).map_err(|source| {
            TypeScriptProjectHostError::PackagePath {
                path: compiler.to_path_buf().into_boxed_path(),
                source,
            }
        })?;
        let workspace = discover_workspace_boundary(project_root, home_root)?;
        Ok(Some(ProjectTypeScript {
            module_root,
            compiler,
            version,
            workspace,
            discovery_origin: TypeScriptProjectDiscoveryOrigin::InstalledFallback,
            compiler_origin: if self.explicit_compiler.is_some() {
                TypeScriptSelectionOrigin::ExplicitConfiguration
            } else {
                self.installed_default_origin
            },
        }))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TypeScriptCompilerInvocation {
    PackageScript,
    NativeExecutable,
}

/// One project-local checker and the exact toolchain facts admitted for it.
#[derive(Debug)]
pub(crate) enum AdmittedTypeScriptProject {
    Native {
        fingerprint: [u8; 32],
        witness: std::sync::Arc<TypeScriptProjectWitness>,
    },
    CompilerApi(AdmittedCompilerApiTypeScriptProject),
}

impl AdmittedTypeScriptProject {
    pub(crate) fn resolved_toolchain(
        &self,
    ) -> Result<ResolvedToolchain<'_>, TypeScriptProjectHostError> {
        match self {
            Self::Native { .. } => Ok(ResolvedToolchain::native_typescript()),
            Self::CompilerApi(project) => project.resolved_toolchain(),
        }
    }
    pub(crate) fn checker(&self) -> Option<&ExplicitTypeScriptChecker> {
        match self {
            Self::Native { .. } => None,
            Self::CompilerApi(project) => Some(&project.checker),
        }
    }
    pub(crate) fn witness(&self) -> &std::sync::Arc<TypeScriptProjectWitness> {
        match self {
            Self::Native { witness, .. } => witness,
            Self::CompilerApi(project) => &project.witness,
        }
    }
    pub(crate) fn fingerprint(&self) -> [u8; 32] {
        match self {
            Self::Native { fingerprint, .. } => *fingerprint,
            Self::CompilerApi(project) => project.fingerprint,
        }
    }
    pub(crate) fn validate_current(&self) -> Result<(), TypeScriptProjectHostError> {
        self.witness().validate_current()
    }
    pub(crate) fn inputs(&self) -> TypeScriptProjectInputs<'_> {
        match self {
            Self::CompilerApi(project) => project.inputs(),
            Self::Native {
                fingerprint,
                witness,
            } => TypeScriptProjectInputs {
                package_root: &witness.project_root,
                workspace_root: &witness.workspace_root,
                authority: TypeScriptProjectInputAuthority::Native,
                fingerprint: *fingerprint,
                config_candidates: &witness.config_candidates,
                selected_build_config_paths: &witness.selected_build_config_paths,
                typescript_files: &[],
                witness,
            },
        }
    }
    #[cfg(test)]
    fn compiler_api_for_test(&self) -> &AdmittedCompilerApiTypeScriptProject {
        match self {
            Self::CompilerApi(project) => project,
            Self::Native { .. } => panic!("expected explicit Compiler API fixture"),
        }
    }
}

pub(crate) struct AdmittedCompilerApiTypeScriptProject {
    pub(crate) checker: ExplicitTypeScriptChecker,
    pub(crate) compiler: Box<Path>,
    pub(crate) compiler_version: Box<[u8]>,
    compiler_origin: TypeScriptSelectionOrigin,
    compiler_invocation: TypeScriptCompilerInvocation,
    node: Box<Path>,
    node_version: Box<[u8]>,
    node_origin: TypeScriptSelectionOrigin,
    module_root: Box<Path>,
    pub(crate) fingerprint: [u8; 32],
    pub(crate) witness: std::sync::Arc<TypeScriptProjectWitness>,
}

impl std::fmt::Debug for AdmittedCompilerApiTypeScriptProject {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AdmittedCompilerApiTypeScriptProject")
            .field("compiler", &self.compiler)
            .field("node", &self.node)
            .field("module_root", &self.module_root)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

impl AdmittedCompilerApiTypeScriptProject {
    pub(crate) fn resolved_toolchain(
        &self,
    ) -> Result<ResolvedToolchain<'_>, TypeScriptProjectHostError> {
        if self.compiler_invocation == TypeScriptCompilerInvocation::PackageScript {
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
            authority: TypeScriptProjectInputAuthority::CompilerApi(TypeScriptCompilerApiInputs {
                typescript_module_root: &self.module_root,
                compiler_path: &self.compiler,
                compiler_version: &self.compiler_version,
                compiler_origin: self.compiler_origin,
                node_path: &self.node,
                node_version: &self.node_version,
                node_origin: self.node_origin,
            }),
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
    discovery_origin: TypeScriptProjectDiscoveryOrigin,
    module_root: PathBuf,
    compiler: PathBuf,
    version: String,
    workspace: Option<WorkspaceBoundary>,
    compiler_origin: TypeScriptSelectionOrigin,
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
        compiler_origin: TypeScriptSelectionOrigin::ProjectLocalInstallation,
        discovery_origin: TypeScriptProjectDiscoveryOrigin::ProjectLocal,
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
    discover_workspace_boundary_controlled(root, home_root, &mut || Ok(()))
}

fn discover_workspace_boundary_controlled(
    root: &Path,
    home_root: Option<&Path>,
    control: &mut dyn FnMut() -> Result<(), TypeScriptProjectHostError>,
) -> Result<Option<WorkspaceBoundary>, TypeScriptProjectHostError> {
    control()?;
    let canonical_root =
        fs::canonicalize(root).map_err(|source| TypeScriptProjectHostError::PackageRoot {
            package_root: root.to_path_buf().into_boxed_path(),
            source,
        })?;
    control()?;
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
        control()?;
        let mut files = Vec::new();
        let manifest = ancestor.join("package.json");
        if path_exists(&manifest)? {
            control()?;
            let (snapshot, bytes) = read_regular_file_controlled(
                &manifest,
                u64::try_from(MAX_PACKAGE_MANIFEST_BYTES).unwrap_or(u64::MAX),
                control,
            )?;
            control()?;
            let parsed = serde_json::from_slice::<serde_json::Value>(&bytes);
            control()?;
            let value =
                parsed.map_err(
                    |source| TypeScriptProjectHostError::WorkspaceConfigInvalid {
                        path: manifest.clone().into_boxed_path(),
                        message: source.to_string().into_boxed_str(),
                    },
                )?;
            let workspace = has_valid_workspaces_field(&value, &manifest);
            control()?;
            if workspace? {
                files.push(snapshot);
            }
        }
        control()?;
        let pnpm = ancestor.join("pnpm-workspace.yaml");
        if path_exists(&pnpm)? {
            control()?;
            let (snapshot, bytes) = read_regular_file_controlled(
                &pnpm,
                u64::try_from(MAX_PNPM_WORKSPACE_BYTES).unwrap_or(u64::MAX),
                control,
            )?;
            control()?;
            let valid = valid_pnpm_workspace(&bytes);
            control()?;
            if !valid {
                return Err(TypeScriptProjectHostError::WorkspaceConfigInvalid {
                    path: pnpm.into_boxed_path(),
                    message: "pnpm workspace file must contain a bounded non-empty packages list"
                        .into(),
                });
            }
            files.push(snapshot);
        }
        control()?;
        if !files.is_empty() {
            return Ok(Some(WorkspaceBoundary {
                root: ancestor.to_path_buf().into_boxed_path(),
                files: files.into_boxed_slice(),
            }));
        }
    }
    control()?;
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

pub(crate) fn is_module_tsc_script(compiler: &Path, module_root: &Path) -> bool {
    let (Ok(root), Ok(package), Ok(compiler), Ok(expected)) = (
        fs::canonicalize(module_root),
        fs::canonicalize(module_root.join("typescript")),
        fs::canonicalize(compiler),
        fs::canonicalize(module_root.join("typescript/bin/tsc")),
    ) else {
        return false;
    };
    package.starts_with(root) && compiler.starts_with(package) && compiler == expected
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
fn bundle_admission_error(
    source: crate::application::host::LocalCompilerHostError,
) -> TypeScriptProjectHostError {
    match source {
        crate::application::host::LocalCompilerHostError::BundleManifest { path, message } => {
            TypeScriptProjectHostError::BundledSdkAdmission { path, message }
        }
        other => TypeScriptProjectHostError::BundledSdkAdmission {
            path: PathBuf::from("selected SDK resource").into_boxed_path(),
            message: other.to_string().into_boxed_str(),
        },
    }
}

/// Each variant's display text identifies the selected path or the bounded process failure.
/// Its field names are intentionally descriptive in the structured error chain.
#[derive(Debug, Error)]
#[allow(missing_docs)]
pub enum TypeScriptProjectHostError {
    /// A host failure paired with sanitized, project-relative public cause text.
    /// Typed classification always follows `source`; only the text is added.
    #[error("{source}")]
    Described {
        cause: backend_library::interface::TypeScriptFailureCause,
        #[source]
        source: Box<TypeScriptProjectHostError>,
    },
    #[error("TypeScript compiler package version {package_version} admission refused: {source}")]
    CompilerPackageAdmission {
        package_version: Box<str>,
        role: backend_library::interface::TypeScriptCompilerPackageRole,
        #[source]
        source: Box<TypeScriptProjectHostError>,
    },
    #[error("selected bundled TypeScript resource refused at {path:?}: {message}")]
    BundledSdkAdmission { path: Box<Path>, message: Box<str> },
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
    #[error(
        "admitted TypeScript package source {package_relative_path:?} was absent from the compiler program"
    )]
    CompilerProgramSourceMissing { package_relative_path: Box<str> },
    #[error("TypeScript compiler program admission was cancelled")]
    CompilerProgramCancelled,
    #[error("TypeScript compiler program admission exceeded its admitted deadline")]
    CompilerProgramDeadline,
    #[error("TypeScript compiler API program process failed: {source}")]
    CompilerProgramProbe {
        #[source]
        source: ToolchainProbeError,
    },
    #[error("the admitted TypeScript compiler API rejected its program input: {message}")]
    CompilerApiBridge { message: Box<str> },
    #[error(
        "selected TypeScript package {package_root:?} declares version {package_version} but has no Compiler API entrypoint at {entrypoint:?}"
    )]
    CompilerApiEntryMissing {
        package_root: Box<Path>,
        package_version: Box<str>,
        entrypoint: Box<Path>,
    },
    #[error(
        "selected TypeScript package {package_root:?} version {package_version} does not provide required Compiler API export {export:?} from {entrypoint:?}"
    )]
    CompilerApiExportMissing {
        package_root: Box<Path>,
        package_version: Box<str>,
        entrypoint: Box<Path>,
        export: Box<str>,
    },
    #[error("TypeScript compiler I/O closure exceeded its {phase} budget: {detail}")]
    CompilerIoClosureLimit { phase: Box<str>, detail: Box<str> },
    #[error("TypeScript compiler I/O closure did not match admitted directory state: {detail}")]
    CompilerIoClosureMismatch { detail: Box<str> },
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
    #[error(
        "TypeScript package {package_root:?} contains {observed} admitted compiler files, maximum is {maximum}"
    )]
    ModuleFileLimit {
        package_root: Box<Path>,
        observed: usize,
        maximum: usize,
    },
    #[error(
        "TypeScript package {package_root:?} contains {observed} filesystem entries, maximum is {maximum}"
    )]
    ModuleEntryLimit {
        package_root: Box<Path>,
        observed: usize,
        maximum: usize,
    },
    #[error(
        "TypeScript package {package_root:?} reached directory depth {observed}, maximum is {maximum}"
    )]
    ModuleDirectoryDepth {
        package_root: Box<Path>,
        observed: usize,
        maximum: usize,
    },
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
            fs::create_dir_all(package.join("lib")).expect("create compiler API package");
            fs::write(
                package.join("package.json"),
                format!("{{\"name\":\"typescript\",\"version\":\"{version}\"}}"),
            )
            .expect("write package manifest");
            fs::write(package.join("bin/tsc"), "#!/usr/bin/env node\n").expect("write compiler");
            fs::write(package.join("lib/typescript.js"), "module.exports = {};\n")
                .expect("write Compiler API fixture entry");
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
    fn native_admission_and_retained_config_replay_observe_cancellation_inside_discovery() {
        use std::sync::atomic::AtomicBool;
        let fixture = Fixture::new();
        fs::write(
            fixture.0.join("tsconfig.json"),
            r#"{"include":["src/**/*.ts"]}"#,
        )
        .unwrap();
        for directory in 0..8 {
            let path = fixture.0.join(format!("src/feature-{directory}"));
            fs::create_dir_all(&path).unwrap();
            for file in 0..4 {
                fs::write(
                    path.join(format!("file-{file}.ts")),
                    "export const value = 1;",
                )
                .unwrap();
            }
        }
        // Existing discovery exclusions must remain exclusions, including an
        // unrelated malformed package config. No Node or compiler is invoked.
        fs::create_dir_all(fixture.0.join("node_modules/irrelevant")).unwrap();
        fs::write(
            fixture.0.join("node_modules/irrelevant/tsconfig.json"),
            "not JSON",
        )
        .unwrap();
        let host = TypeScriptProjectHost::direct(None, Fixture::limits());
        let baseline = host.admit(&fixture.0).unwrap().unwrap();
        assert_eq!(
            baseline.witness().native_project_fingerprint(),
            Some(baseline.inputs().fingerprint),
            "cold replay hashes the same host identity as native project execution"
        );
        let mut full_steps = 0usize;
        let controlled = host
            .admit_controlled(&fixture.0, &mut || {
                full_steps += 1;
                Ok(())
            })
            .unwrap()
            .unwrap();
        assert_eq!(controlled.fingerprint(), baseline.fingerprint());
        assert!(
            full_steps > 32,
            "actual native discovery visits the nested filesystem frontier"
        );
        for stop_at in [full_steps / 4, full_steps * 3 / 4] {
            let cancelled = AtomicBool::new(false);
            let mut steps = 0;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let result = host.admit_controlled(&fixture.0, &mut || {
                steps += 1;
                if steps == stop_at {
                    cancelled.store(true, Ordering::Release);
                }
                super::super::typescript_program::check_admission_control(deadline, &cancelled)
            });
            assert!(matches!(
                result,
                Err(TypeScriptProjectHostError::CompilerProgramCancelled)
            ));
            assert_eq!(
                steps, stop_at,
                "no subsequent filesystem callback after cancellation"
            );
        }
        let witness = baseline.witness();
        let mut replay_steps = 0;
        witness
            .validated_controlled(&mut || {
                replay_steps += 1;
                Ok(())
            })
            .unwrap();
        assert!(
            replay_steps > 32,
            "retained native proof recaptures the same actual config frontier"
        );
        let cancelled = AtomicBool::new(false);
        let mut steps = 0;
        let stop_at = replay_steps / 2;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let result = witness.validated_controlled(&mut || {
            steps += 1;
            if steps == stop_at {
                cancelled.store(true, Ordering::Release);
            }
            super::super::typescript_program::check_admission_control(deadline, &cancelled)
        });
        assert!(matches!(
            result,
            Err(TypeScriptProjectHostError::CompilerProgramCancelled)
        ));
        assert_eq!(steps, stop_at);
        witness
            .validate_current()
            .expect("cancellation cannot mutate the retained source proof");
        fs::write(
            fixture.0.join("tsconfig.json"),
            r#"{"include":["different/**/*.ts"]}"#,
        )
        .unwrap();
        assert!(
            witness.validated_controlled(&mut || Ok(())).is_err(),
            "controlled discovery still refuses changed configuration bytes"
        );
    }

    #[test]
    fn native_workspace_ancestor_discovery_and_replay_cancel_before_later_invalid_manifest() {
        use std::sync::atomic::AtomicBool;
        let fixture = Fixture::new();
        let home = fixture.0.join("home");
        let workspace = home.join("repo");
        let app = workspace.join("apps/web/a/b/c/d/e/f");
        fs::create_dir_all(&app).unwrap();
        let manifest = workspace.join("package.json");
        let original = r#"{"name":"workspace","workspaces":["apps/*"]}"#;
        fs::write(&manifest, original).unwrap();
        // HOME is an excluded boundary even when its manifest is malformed.
        fs::write(home.join("package.json"), "not JSON").unwrap();
        let host = TypeScriptProjectHost::direct(Some(home.clone()), Fixture::limits());
        let baseline = host.admit(&app).unwrap().unwrap();
        let mut healthy_checks = 0;
        let current = host
            .admit_controlled(&app, &mut || {
                healthy_checks += 1;
                Ok(())
            })
            .unwrap()
            .unwrap();
        assert_eq!(current.fingerprint(), baseline.fingerprint());
        assert_eq!(
            current.witness().workspace.as_ref().unwrap().root.as_ref(),
            workspace
        );
        assert!(
            healthy_checks > 18,
            "real ancestor and project discovery is controlled"
        );
        fs::write(&manifest, "not JSON").unwrap();
        assert!(matches!(
            host.admit(&app),
            Err(TypeScriptProjectHostError::WorkspaceConfigInvalid { .. })
        ));
        let cancelled = AtomicBool::new(false);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut checks = 0;
        let stopped = host.admit_controlled(&app, &mut || {
            checks += 1;
            if checks == 18 {
                cancelled.store(true, Ordering::Release);
            }
            super::super::typescript_program::check_admission_control(deadline, &cancelled)
        });
        assert!(
            matches!(
                stopped,
                Err(TypeScriptProjectHostError::CompilerProgramCancelled)
            ),
            "stop in ancestor discovery before the later invalid workspace is read"
        );
        assert_eq!(checks, 18, "no later ancestor I/O after cancellation");

        let witness = baseline.witness();
        let cancelled = AtomicBool::new(false);
        let mut checks = 0;
        let stopped = witness.validated_controlled(&mut || {
            checks += 1;
            if checks == 18 {
                cancelled.store(true, Ordering::Release);
            }
            super::super::typescript_program::check_admission_control(deadline, &cancelled)
        });
        assert!(
            matches!(
                stopped,
                Err(TypeScriptProjectHostError::CompilerProgramCancelled)
            ),
            "retained native proof uses the same controlled ancestor discovery"
        );
        assert_eq!(checks, 18);
        fs::write(&manifest, original).unwrap();
        witness
            .validate_current()
            .expect("cancellation leaves original witness reusable");
        fs::write(&manifest, r#"{"name":"changed","workspaces":["apps/*"]}"#).unwrap();
        assert!(
            matches!(
                witness.validated_controlled(&mut || Ok(())),
                Err(TypeScriptProjectHostError::WitnessChanged { .. })
            ),
            "ancestor metadata is still byte-authoritative after successful discovery"
        );
    }

    #[test]
    fn controlled_workspace_marker_read_stops_before_later_metadata_refusal() {
        use std::sync::atomic::AtomicBool;
        let fixture = Fixture::new();
        let path = fixture.0.join("package.json");
        fs::write(&path, br#"{"workspaces":["apps/*"]}"#).unwrap();
        let baseline = read_regular_file(&path, 64 * 1024).unwrap();
        let controlled = read_regular_file_controlled(&path, 64 * 1024, &mut || Ok(())).unwrap();
        assert_eq!(
            controlled, baseline,
            "no-op control preserves full snapshot and bytes"
        );
        fs::write(&path, vec![b' '; 64 * 1024 + 1]).unwrap();
        assert!(matches!(
            read_regular_file(&path, 64 * 1024),
            Err(TypeScriptProjectHostError::FileTooLarge { .. })
        ));
        let cancelled = AtomicBool::new(false);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut checks = 0;
        let stopped = read_regular_file_controlled(&path, 64 * 1024, &mut || {
            checks += 1;
            if checks == 2 {
                cancelled.store(true, Ordering::Release);
            }
            super::super::typescript_program::check_admission_control(deadline, &cancelled)
        });
        assert!(
            matches!(
                stopped,
                Err(TypeScriptProjectHostError::CompilerProgramCancelled)
            ),
            "cancellation after opening wins before later file metadata refusal"
        );
        assert_eq!(checks, 2);
    }

    #[test]
    fn workspace_marker_post_parse_control_precedes_malformed_refusal() {
        use std::sync::atomic::AtomicBool;
        // Exact post-parse checkpoints in the real Unix read/validation path:
        // package JSON is poll13; its workspace-field validation and pnpm
        // validation after an absent package are each poll14.
        // The previous implementation returns ConfigInvalid before either poll.
        for (name, bytes, post_parse_poll) in [
            ("package.json", b"not JSON".as_slice(), 13),
            ("package.json", br#"{"workspaces":false}"#.as_slice(), 14),
            (
                "pnpm-workspace.yaml",
                b"packages:\n  - ../outside\n".as_slice(),
                14,
            ),
        ] {
            let fixture = Fixture::new();
            fs::write(fixture.0.join(name), bytes).unwrap();
            assert!(matches!(
                discover_workspace_boundary(&fixture.0, None),
                Err(TypeScriptProjectHostError::WorkspaceConfigInvalid { .. })
            ));
            let cancelled = AtomicBool::new(false);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let mut polls = 0;
            let stopped = discover_workspace_boundary_controlled(&fixture.0, None, &mut || {
                polls += 1;
                if polls == post_parse_poll {
                    cancelled.store(true, Ordering::Release);
                }
                super::super::typescript_program::check_admission_control(deadline, &cancelled)
            });
            assert!(
                matches!(
                    stopped,
                    Err(TypeScriptProjectHostError::CompilerProgramCancelled)
                ),
                "post-parse cancellation takes precedence over malformed {name}"
            );
            assert_eq!(
                polls, post_parse_poll,
                "no later marker I/O after post-parse stop"
            );
        }
    }

    #[test]
    fn native_config_reference_and_angular_resolution_cancel_before_later_missing_paths() {
        use std::sync::atomic::AtomicBool;
        let fixture = Fixture::new();
        let mut paths = Vec::new();
        for i in 0..8 {
            let relative = format!("configs/config-{i}.json");
            let path = fixture.0.join(&relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "{}").unwrap();
            paths.push(relative);
        }
        let source = fixture.0.join("tsconfig.json");
        let value = serde_json::json!({"references":paths.iter().map(|path|
            serde_json::json!({"path":path})).collect::<Vec<_>>()});
        let expected = paths
            .iter()
            .map(|path| fixture.0.join(path).canonicalize().unwrap())
            .collect::<Vec<_>>();
        let baseline = config_paths_from_field(
            &value,
            &source,
            &fixture.0,
            &fixture.0,
            &fixture.0,
            ConfigDependencyKind::References,
        )
        .unwrap();
        assert_eq!(baseline, expected, "reference order is retained");
        let mut healthy_steps = 0;
        let healthy = config_paths_from_field_controlled(
            &value,
            &source,
            &fixture.0,
            &fixture.0,
            &fixture.0,
            ConfigDependencyKind::References,
            &mut || {
                healthy_steps += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(healthy, expected);
        let mut invalid = value.clone();
        invalid["references"][7] = serde_json::json!(7);
        assert!(matches!(
            config_paths_from_field(
                &invalid,
                &source,
                &fixture.0,
                &fixture.0,
                &fixture.0,
                ConfigDependencyKind::References
            ),
            Err(TypeScriptProjectHostError::ConfigInvalid { .. })
        ));
        let cancelled = AtomicBool::new(false);
        let mut steps = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let stopped = config_paths_from_field_controlled(
            &invalid,
            &source,
            &fixture.0,
            &fixture.0,
            &fixture.0,
            ConfigDependencyKind::References,
            &mut || {
                steps += 1;
                if steps == paths.len() {
                    cancelled.store(true, Ordering::Release);
                }
                super::super::typescript_program::check_admission_control(deadline, &cancelled)
            },
        );
        assert!(
            matches!(
                stopped,
                Err(TypeScriptProjectHostError::CompilerProgramCancelled)
            ),
            "each reference is controlled before a later malformed entry is validated"
        );
        assert_eq!(steps, paths.len());
        let mut bad = value.clone();
        bad["references"][7]["path"] = serde_json::json!("does-not-exist.json");
        assert!(matches!(
            config_paths_from_field(
                &bad,
                &source,
                &fixture.0,
                &fixture.0,
                &fixture.0,
                ConfigDependencyKind::References
            ),
            Err(TypeScriptProjectHostError::ConfigMissing { .. })
        ));
        let cancelled = AtomicBool::new(false);
        let mut steps = 0;
        let stop_at = healthy_steps - 1;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let stopped = config_paths_from_field_controlled(
            &bad,
            &source,
            &fixture.0,
            &fixture.0,
            &fixture.0,
            ConfigDependencyKind::References,
            &mut || {
                steps += 1;
                if steps == stop_at {
                    cancelled.store(true, Ordering::Release);
                }
                super::super::typescript_program::check_admission_control(deadline, &cancelled)
            },
        );
        assert!(
            matches!(
                stopped,
                Err(TypeScriptProjectHostError::CompilerProgramCancelled)
            ),
            "cancellation at the last resolution boundary wins over the later missing path"
        );
        assert_eq!(
            steps, stop_at,
            "no subsequent reference resolution is admitted"
        );

        let configurations = paths
            .iter()
            .enumerate()
            .map(|(i, path)| (format!("config-{i}"), serde_json::json!({"tsConfig":path})))
            .collect::<serde_json::Map<_, _>>();
        let angular = serde_json::json!({"projects":{"app":{"architect":{
            "build":{"configurations":configurations}}}}});
        fs::write(
            fixture.0.join("angular.json"),
            serde_json::to_vec(&angular).unwrap(),
        )
        .unwrap();
        let baseline = angular_build_config_paths(&fixture.0, &fixture.0)
            .unwrap()
            .0;
        let mut healthy_steps = 0;
        let controlled = angular_build_config_paths_controlled(&fixture.0, &fixture.0, &mut || {
            healthy_steps += 1;
            Ok(())
        })
        .unwrap()
        .0;
        assert_eq!(controlled, baseline);
        assert_eq!(
            controlled, expected,
            "Angular path sorting and selection are unchanged"
        );
        let mut invalid = angular.clone();
        invalid["projects"]["app"]["architect"]["build"]["configurations"]["config-7"]["tsConfig"] =
            serde_json::json!(7);
        fs::write(
            fixture.0.join("angular.json"),
            serde_json::to_vec(&invalid).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            angular_build_config_paths(&fixture.0, &fixture.0),
            Err(TypeScriptProjectHostError::ConfigInvalid { .. })
        ));
        let cancelled = AtomicBool::new(false);
        let mut steps = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let stopped = angular_build_config_paths_controlled(&fixture.0, &fixture.0, &mut || {
            steps += 1;
            if steps == paths.len() {
                cancelled.store(true, Ordering::Release);
            }
            super::super::typescript_program::check_admission_control(deadline, &cancelled)
        });
        assert!(
            matches!(
                stopped,
                Err(TypeScriptProjectHostError::CompilerProgramCancelled)
            ),
            "Angular configurations are controlled before a later malformed tsConfig"
        );
        assert_eq!(steps, paths.len());
        let mut bad = angular.clone();
        bad["projects"]["app"]["architect"]["build"]["configurations"]["config-7"]["tsConfig"] =
            serde_json::json!("does-not-exist.json");
        fs::write(
            fixture.0.join("angular.json"),
            serde_json::to_vec(&bad).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            angular_build_config_paths(&fixture.0, &fixture.0),
            Err(TypeScriptProjectHostError::ConfigMissing { .. })
        ));
        let cancelled = AtomicBool::new(false);
        let mut steps = 0;
        let stop_at = healthy_steps - 1;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let stopped = angular_build_config_paths_controlled(&fixture.0, &fixture.0, &mut || {
            steps += 1;
            if steps == stop_at {
                cancelled.store(true, Ordering::Release);
            }
            super::super::typescript_program::check_admission_control(deadline, &cancelled)
        });
        assert!(
            matches!(
                stopped,
                Err(TypeScriptProjectHostError::CompilerProgramCancelled)
            ),
            "cancellation interrupts Angular selected-path resolutions before the missing path"
        );
        assert_eq!(steps, stop_at);
    }

    #[test]
    fn native_admission_uses_original_deadline_and_cancellation_precedence() {
        use std::sync::atomic::AtomicBool;
        let fixture = Fixture::new();
        let host = TypeScriptProjectHost::direct(None, Fixture::limits());
        let deadline = std::time::Instant::now();
        for cancelled in [false, true] {
            let flag = AtomicBool::new(cancelled);
            let result = host.admit_controlled(&fixture.0, &mut || {
                super::super::typescript_program::check_admission_control(deadline, &flag)
            });
            assert!(if cancelled {
                matches!(
                    result,
                    Err(TypeScriptProjectHostError::CompilerProgramCancelled)
                )
            } else {
                matches!(
                    result,
                    Err(TypeScriptProjectHostError::CompilerProgramDeadline)
                )
            });
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
    fn compiler_package_capture_excludes_root_docs_but_keeps_the_full_lib_tree() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let package = modules.join("typescript");
        fs::write(package.join("README.md"), "documentation").unwrap();
        fs::write(package.join("LICENSE.txt"), "license").unwrap();
        fs::write(package.join("lib/runtime-data.dat"), "compiler data").unwrap();
        for index in 0..300 {
            fs::write(
                package.join(format!("README-{index:03}.md")),
                "documentation",
            )
            .unwrap();
        }

        let inputs = collect_module_inputs(&package).expect("capture bounded runtime inputs");
        let paths = inputs
            .iter()
            .map(|input| input.path.strip_prefix(&package).unwrap().to_path_buf())
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec![
                PathBuf::from("bin/tsc"),
                PathBuf::from("lib/runtime-data.dat"),
                PathBuf::from("lib/typescript.js"),
                PathBuf::from("package.json"),
            ]
        );
    }

    #[test]
    fn compiler_package_file_limit_reports_selected_root_and_exact_input_count() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let package = modules.join("typescript");
        for index in 0..MAX_TYPESCRIPT_PACKAGE_FILES {
            fs::write(
                package.join(format!("lib/extra-{index:03}.js")),
                "module.exports = {};\n",
            )
            .unwrap();
        }

        let error = collect_module_inputs(&package).expect_err("runtime files exceed bound");
        assert!(matches!(
            error,
            TypeScriptProjectHostError::ModuleFileLimit {
                package_root,
                observed,
                maximum,
            } if package_root.as_ref() == package.as_path()
                && observed == MAX_TYPESCRIPT_PACKAGE_FILES + 1
                && maximum == MAX_TYPESCRIPT_PACKAGE_FILES
        ));
    }

    #[test]
    fn compiler_package_entry_limit_reports_selected_root_and_exact_entry_count() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let package = modules.join("typescript");
        for index in 0..MAX_TYPESCRIPT_PACKAGE_ENTRIES {
            fs::write(
                package.join(format!("README-{index:03}.md")),
                "documentation",
            )
            .unwrap();
        }

        let error = collect_module_inputs(&package).expect_err("package traversal exceeds bound");
        assert!(matches!(
            error,
            TypeScriptProjectHostError::ModuleEntryLimit {
                package_root,
                observed,
                maximum,
            } if package_root.as_ref() == package.as_path()
                && observed == MAX_TYPESCRIPT_PACKAGE_ENTRIES + 1
                && maximum == MAX_TYPESCRIPT_PACKAGE_ENTRIES
        ));
    }

    #[test]
    fn compiler_package_depth_refusal_retains_selected_root_and_exact_depth() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let package = modules.join("typescript");
        let mut nested = package.join("lib");
        for _ in 0..MAX_TYPESCRIPT_PACKAGE_DEPTH + 2 {
            nested.push("nested");
        }
        fs::create_dir_all(&nested).unwrap();

        let error = collect_module_inputs(&package).expect_err("package nesting exceeds bound");
        assert!(matches!(
            error,
            TypeScriptProjectHostError::ModuleDirectoryDepth {
                package_root,
                observed,
                maximum,
            } if package_root.as_ref() == package.as_path()
                && observed == MAX_TYPESCRIPT_PACKAGE_DEPTH + 1
                && maximum == MAX_TYPESCRIPT_PACKAGE_DEPTH
        ));
    }

    #[test]
    fn typescript_seven_without_api_refuses_before_host_or_bundled_fallback() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new();
        let project_modules = fixture.install("7.0.2");
        let project_package = project_modules.join("typescript");
        let project_api = project_package.join("lib/typescript.js");
        fs::remove_file(&project_api).expect("remove absent TypeScript 7 API fixture");

        let host_root = fixture.0.join("host");
        let host_modules = host_root.join("node_modules");
        install_at(&host_modules, "5.9.3");
        let node = fixture.0.join("node");
        fs::write(&node, "not executed by this capability refusal\n").unwrap();
        fs::set_permissions(&node, fs::Permissions::from_mode(0o755)).unwrap();

        let error = TypeScriptProjectHost::new(None, Some(node), None, None, Fixture::limits())
            .with_installed_default(
                Some(host_modules.join("typescript/bin/tsc")),
                Some(host_modules),
            )
            .with_bundled_application(Some(fixture.0.join("unused.app")))
            .admit(&fixture.0)
            .expect_err("a present incompatible package is not replaced by the host package");

        assert!(matches!(
            error,
            TypeScriptProjectHostError::CompilerApiEntryMissing {
                package_root,
                package_version,
                entrypoint,
            } if package_root.as_ref() == project_package.as_path()
                && package_version.as_ref() == "7.0.2"
                && entrypoint.as_ref() == project_api.as_path()
        ));
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
    fn pnpm_invocation_binds_exact_package_member_and_refuses_link_retarget() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let store = modules.join(".pnpm/typescript@5.9.3/node_modules");
        fs::create_dir_all(&store).expect("create PNPM store");
        let package = store.join("typescript");
        fs::rename(modules.join("typescript"), &package).expect("move exact package into store");
        symlink(&package, modules.join("typescript")).expect("link selected package");
        let node = fixture.0.join("node");
        fs::write(
            &node,
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf 'v22.0.0\\n'; else printf 'Version 5.9.3\\n'; fi\n",
        )
        .expect("write bounded version probe fixture");
        fs::set_permissions(&node, fs::Permissions::from_mode(0o755)).unwrap();
        let host =
            TypeScriptProjectHost::new(None, Some(node.clone()), None, None, Fixture::limits());
        let admitted = host.admit(&fixture.0).unwrap().unwrap();
        let toolchain = admitted
            .resolved_toolchain()
            .expect("bind canonical PNPM member");
        assert_eq!(
            toolchain
                .external()
                .expect("external Compiler API fixture")
                .executable(),
            package.join("bin/tsc")
        );
        assert_eq!(
            admitted
                .inputs()
                .compiler_api()
                .unwrap()
                .typescript_module_root,
            modules
        );
        let hashed_before = crate::application::executable_content_hash_bytes_for_test();
        toolchain
            .external()
            .expect("external Compiler API fixture")
            .validate_invocation()
            .expect("unchanged package objects");
        assert_eq!(
            crate::application::executable_content_hash_bytes_for_test(),
            hashed_before
        );

        let other_store = modules.join(".pnpm/another-typescript/node_modules");
        install_at(&other_store, "5.9.3");
        let other_compiler = other_store.join("typescript/bin/tsc");
        assert!(matches!(
            ResolvedToolchain::from_interpreted_script(
                NativeTool::TypeScriptCompiler,
                &node,
                &other_compiler,
                &modules,
                b"Version 5.9.3",
                b"v22.0.0",
                [0; 32],
                [0; 32],
                [0; 32],
            ),
            Err(crate::driver::ToolchainResolutionError::CompilerModuleMismatch)
        ));
        fs::remove_file(modules.join("typescript")).unwrap();
        symlink(other_store.join("typescript"), modules.join("typescript")).unwrap();
        assert!(
            toolchain
                .external()
                .expect("external Compiler API fixture")
                .validate_invocation()
                .is_err(),
            "same-byte package retarget must refuse"
        );
        assert!(
            matches!(
                admitted.resolved_toolchain(),
                Err(TypeScriptProjectHostError::ToolchainResolution {
                    source: crate::driver::ToolchainResolutionError::CompilerModuleMismatch
                })
            ),
            "an admitted script cannot drift into native executable launch"
        );
        assert!(admitted.validate_current().is_err());

        let outside = fixture.0.join("outside/node_modules");
        install_at(&outside, "5.9.3");
        fs::remove_file(modules.join("typescript")).unwrap();
        symlink(outside.join("typescript"), modules.join("typescript")).unwrap();
        assert!(!is_module_tsc_script(
            &outside.join("typescript/bin/tsc"),
            &modules
        ));
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
    fn angular_solution_references_and_child_inheritance_are_independent_graphs() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let root_bytes = br#"{
  "compilerOptions": {
    "strict": true,
    "target": "ES2022",
    "baseUrl": ".",
    "paths": {"@shared/*": ["shared/*"]}
  },
  "files": [],
  "references": [
    {"path": "./tsconfig.app.json"},
    {"path": "./tsconfig.spec.json"}
  ]
}"#;
        let app_bytes = br#"{
  "extends": "./tsconfig.json",
  "compilerOptions": {"outDir": "./out-tsc/app"},
  "files": ["src/main.ts"],
  "include": ["src/**/*.d.ts"]
}"#;
        let spec_bytes = br#"{
  "extends": "./tsconfig.json",
  "compilerOptions": {"outDir": "./out-tsc/spec"},
  "files": ["src/test.ts"],
  "include": ["src/**/*.d.ts"]
}"#;
        fs::write(
            fixture.0.join("angular.json"),
            r#"{"projects":{"app":{"architect":{"build":{"options":{"tsConfig":"tsconfig.app.json"}}}}}}"#,
        )
        .expect("write Angular build configuration");
        fs::write(fixture.0.join("tsconfig.json"), root_bytes).expect("write solution config");
        fs::write(fixture.0.join("tsconfig.app.json"), app_bytes).expect("write app config");
        fs::write(fixture.0.join("tsconfig.spec.json"), spec_bytes).expect("write spec config");
        let module_root = fs::canonicalize(modules).expect("canonical module root");
        let (selected, _) = angular_build_config_paths(&fixture.0, &module_root)
            .expect("select Angular build config");

        let configs = collect_project_configs(&fixture.0, &fixture.0, &module_root, &selected)
            .expect("capture Angular solution, build, and spec configs");
        assert_eq!(configs.len(), 3);

        let root =
            fs::canonicalize(fixture.0.join("tsconfig.json")).expect("canonical solution config");
        let app =
            fs::canonicalize(fixture.0.join("tsconfig.app.json")).expect("canonical app config");
        let spec =
            fs::canonicalize(fixture.0.join("tsconfig.spec.json")).expect("canonical spec config");
        let root_input = configs
            .iter()
            .find(|config| config.path.as_ref() == root)
            .expect("solution config is captured");
        let app_input = configs
            .iter()
            .find(|config| config.path.as_ref() == app)
            .expect("selected app config is captured");
        let spec_input = configs
            .iter()
            .find(|config| config.path.as_ref() == spec)
            .expect("spec config is captured");

        assert!(root_input.extends.is_empty());
        assert!(root_input.references.is_empty());
        assert_eq!(
            root_input.dependency_authority,
            TypeScriptConfigDependencyAuthority::Inheritance,
        );
        assert_eq!(
            app_input.dependency_authority,
            TypeScriptConfigDependencyAuthority::Program,
        );
        assert_eq!(app_input.extends.len(), 1);
        assert_eq!(app_input.extends[0].as_ref(), root.as_path());
        assert!(app_input.references.is_empty());
        assert!(spec_input.extends.is_empty());
        assert!(spec_input.references.is_empty());
        assert_eq!(
            spec_input.dependency_authority,
            TypeScriptConfigDependencyAuthority::Unselected,
        );
        assert!(app_input.selected_build_config);
        assert_eq!(root_input.bytes.as_ref(), root_bytes);
        assert_eq!(app_input.bytes.as_ref(), app_bytes);
        assert_eq!(spec_input.bytes.as_ref(), spec_bytes);
    }

    #[test]
    fn config_closure_captures_shared_diamond_nodes_once() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        fs::create_dir_all(fixture.0.join("packages/left")).expect("create left project");
        fs::create_dir_all(fixture.0.join("packages/right")).expect("create right project");
        fs::create_dir_all(fixture.0.join("configs")).expect("create shared configs");
        fs::write(
            fixture.0.join("tsconfig.json"),
            r#"{"references":[{"path":"./packages/left"},{"path":"./packages/right"}]}"#,
        )
        .expect("write solution config");
        fs::write(
            fixture.0.join("packages/left/tsconfig.json"),
            r#"{"extends":"../../configs/shared"}"#,
        )
        .expect("write left config");
        fs::write(
            fixture.0.join("packages/right/tsconfig.json"),
            r#"{"extends":"../../configs/shared"}"#,
        )
        .expect("write right config");
        fs::write(
            fixture.0.join("configs/shared.json"),
            r#"{"extends":"./base","compilerOptions":{"strict":true}}"#,
        )
        .expect("write shared config");
        fs::write(
            fixture.0.join("configs/base.json"),
            r#"{"compilerOptions":{"target":"ES2022"}}"#,
        )
        .expect("write common base config");
        let module_root = fs::canonicalize(modules).expect("canonical module root");

        let configs = collect_project_configs(&fixture.0, &fixture.0, &module_root, &[])
            .expect("capture shared inheritance and reference closure");

        assert_eq!(configs.len(), 5);
        let mut paths = configs
            .iter()
            .map(|config| config.path.as_ref())
            .collect::<Vec<_>>();
        paths.sort_unstable();
        paths.dedup();
        assert_eq!(paths.len(), configs.len());
        let shared = fs::canonicalize(fixture.0.join("configs/shared.json"))
            .expect("canonical shared config");
        let base =
            fs::canonicalize(fixture.0.join("configs/base.json")).expect("canonical base config");
        let shared_input = configs
            .iter()
            .find(|config| config.path.as_ref() == shared)
            .expect("shared config is captured once");
        assert_eq!(shared_input.extends.len(), 1);
        assert_eq!(shared_input.extends[0].as_ref(), base.as_path());
        assert_eq!(
            configs
                .iter()
                .filter(|config| config.path.as_ref() == shared.as_path())
                .count(),
            1
        );
        assert_eq!(
            configs
                .iter()
                .filter(|config| config.path.as_ref() == base.as_path())
                .count(),
            1
        );
    }

    #[test]
    fn config_graph_cycles_and_workspace_escapes_are_typed_refusals() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let module_root = fs::canonicalize(modules).expect("canonical module root");

        fs::create_dir_all(fixture.0.join("configs")).expect("create config closure");
        fs::create_dir_all(fixture.0.join("child")).expect("create child project");
        fs::write(
            fixture.0.join("tsconfig.json"),
            r#"{"extends":"./configs/base"}"#,
        )
        .expect("write root config with extends edge");
        fs::write(
            fixture.0.join("configs/base.json"),
            r#"{"extends":"../tsconfig.json"}"#,
        )
        .expect("write direct inheritance cycle");
        assert!(matches!(
            collect_project_configs(&fixture.0, &fixture.0, &module_root, &[]),
            Err(TypeScriptProjectHostError::ConfigCycle { .. })
        ));

        fs::write(
            fixture.0.join("tsconfig.json"),
            r#"{"references":[{"path":"./child"}]}"#,
        )
        .expect("write root config with project reference");
        fs::write(
            fixture.0.join("configs/base.json"),
            r#"{"compilerOptions":{"strict":true}}"#,
        )
        .expect("remove prior inheritance cycle");
        fs::write(
            fixture.0.join("child/tsconfig.json"),
            r#"{"references":[{"path":"../tsconfig.json"}]}"#,
        )
        .expect("write direct project reference cycle");
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
    fn unrelated_editor_configs_remain_raw_witnesses_without_dependency_authority() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        fs::create_dir_all(fixture.0.join("dev-docs")).expect("create editor-only docs");
        fs::create_dir_all(fixture.0.join("examples")).expect("create unrelated example");
        let root_bytes = br#"{"compilerOptions":{"strict":true},"include":["packages"]}"#;
        let docs_bytes = br#"{"extends":"@tsconfig/docusaurus/tsconfig.json"}"#;
        let malformed_bytes = br#"{"compilerOptions": }"#;
        fs::write(fixture.0.join("tsconfig.json"), root_bytes).expect("write root project");
        fs::write(fixture.0.join("dev-docs/tsconfig.json"), docs_bytes)
            .expect("write unrelated missing extends");
        fs::write(fixture.0.join("examples/tsconfig.json"), malformed_bytes)
            .expect("write unrelated malformed candidate");
        let module_root = fs::canonicalize(modules).expect("canonical module root");
        let configs = collect_project_configs(&fixture.0, &fixture.0, &module_root, &[])
            .expect("unselected editor configuration does not block the root program");
        let root = fixture.0.join("tsconfig.json");
        let selected = select_project_config(&fixture.0, &configs).expect("root program");
        assert_eq!(selected.path.as_ref(), root.as_path());
        assert_eq!(
            selected.dependency_authority,
            TypeScriptConfigDependencyAuthority::Program
        );
        assert_eq!(configs.len(), 3);
        for (relative, expected) in [
            ("dev-docs/tsconfig.json", docs_bytes.as_slice()),
            ("examples/tsconfig.json", malformed_bytes.as_slice()),
        ] {
            let path = fixture.0.join(relative);
            let config = configs
                .iter()
                .find(|config| config.path.as_ref() == path)
                .expect("unselected candidate remains captured");
            assert_eq!(config.bytes.as_ref(), expected);
            assert_eq!(
                config.content_id,
                ContentId::<SourceFactDomain>::from_canonical_bytes(expected)
            );
            assert_eq!(
                config.dependency_authority,
                TypeScriptConfigDependencyAuthority::Unselected
            );
            assert!(config.extends.is_empty());
            assert!(config.references.is_empty());
        }
    }

    #[test]
    fn selected_and_transitive_missing_config_dependencies_remain_refusals() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let module_root = fs::canonicalize(modules).expect("canonical module root");
        fs::create_dir_all(fixture.0.join("configs")).expect("create config directory");
        for bytes in [
            r#"{"extends":"@tsconfig/absent/tsconfig.json"}"#,
            r#"{"references":[{"path":"./missing-project"}]}"#,
            r#"{"extends":"./configs/base"}"#,
            r#"{"references":[{"path":"./configs/base.json"}]}"#,
        ] {
            fs::write(fixture.0.join("tsconfig.json"), bytes).expect("write selected dependency");
            fs::write(
                fixture.0.join("configs/base.json"),
                r#"{"extends":"./absent"}"#,
            )
            .expect("write missing transitive inheritance");
            assert!(matches!(
                collect_project_configs(&fixture.0, &fixture.0, &module_root, &[]),
                Err(TypeScriptProjectHostError::ConfigMissing { .. })
            ));
        }
    }

    #[test]
    fn angular_build_does_not_inherit_solution_references_or_select_a_broken_spec() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let module_root = fs::canonicalize(modules).expect("canonical module root");
        fs::write(fixture.0.join("angular.json"),
            r#"{"projects":{"app":{"architect":{"build":{"options":{"tsConfig":"tsconfig.app.json"}}}}}}"#)
            .expect("write selected Angular build");
        fs::write(
            fixture.0.join("tsconfig.app.json"),
            r#"{"extends":"./tsconfig.json"}"#,
        )
        .expect("write selected app inheritance");
        fs::write(
            fixture.0.join("tsconfig.json"),
            r#"{"references":[{"path":"./missing-spec"}]}"#,
        )
        .expect("write non-inherited solution references");
        fs::write(fixture.0.join("tsconfig.spec.json"), r#"{"extends": 17}"#)
            .expect("write unselected invalid spec configuration");
        let (selected, _) =
            angular_build_config_paths(&fixture.0, &module_root).expect("selected app path");
        let configs = collect_project_configs(&fixture.0, &fixture.0, &module_root, &selected)
            .expect("only actual app authority is expanded");
        let selected = select_project_config(&fixture.0, &configs).expect("app");
        assert!(selected.path.ends_with("tsconfig.app.json"));
        assert_eq!(
            selected.dependency_authority,
            TypeScriptConfigDependencyAuthority::Program
        );
        assert!(selected.references.is_empty());
        let root = fixture.0.join("tsconfig.json");
        let base = configs
            .iter()
            .find(|config| config.path.as_ref() == root)
            .expect("base");
        assert_eq!(
            base.dependency_authority,
            TypeScriptConfigDependencyAuthority::Inheritance
        );
        assert!(base.references.is_empty());
        assert!(
            matches!(
                collect_project_configs(&fixture.0, &fixture.0, &module_root, &[root]),
                Err(TypeScriptProjectHostError::ConfigMissing { .. })
            ),
            "the same root references are strict when the root is selected as a program"
        );
    }

    #[test]
    fn inherited_config_upgrades_to_program_when_reached_by_an_actual_reference() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let module_root = fs::canonicalize(modules).expect("canonical module root");
        fs::create_dir_all(fixture.0.join("left")).expect("create left project");
        fs::create_dir_all(fixture.0.join("right")).expect("create right project");
        fs::create_dir_all(fixture.0.join("configs")).expect("create inheritance");
        fs::write(
            fixture.0.join("tsconfig.json"),
            r#"{"references":[{"path":"./left"},{"path":"./right"}]}"#,
        )
        .expect("write solution");
        fs::write(
            fixture.0.join("right/tsconfig.json"),
            r#"{"extends":"../configs/base.json"}"#,
        )
        .expect("base is inherited first by LIFO traversal");
        fs::write(
            fixture.0.join("left/tsconfig.json"),
            r#"{"references":[{"path":"../configs/base.json"}]}"#,
        )
        .expect("later actual reference");
        fs::write(
            fixture.0.join("configs/base.json"),
            r#"{"references":[{"path":"./missing"}]}"#,
        )
        .expect("base's own reference");
        assert!(
            matches!(
                collect_project_configs(&fixture.0, &fixture.0, &module_root, &[]),
                Err(TypeScriptProjectHostError::ConfigMissing { .. })
            ),
            "program authority must upgrade the previously inherited base and check its references"
        );
    }

    fn selected_name(configs: &[TypeScriptConfigInput], package_root: &Path) -> String {
        select_project_config(package_root, configs)
            .and_then(|config| config.path.file_name())
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
    }

    #[test]
    fn conventional_tsconfig_wins_over_named_siblings_without_refusing() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let module_root = fs::canonicalize(modules).expect("canonical module root");
        fs::write(fixture.0.join("tsconfig.json"), "{}").expect("write conventional config");
        fs::write(fixture.0.join("tsconfig.app.json"), "{}").expect("write named sibling");
        fs::write(fixture.0.join("tsconfig.node.json"), "{}").expect("write named sibling");
        let configs = collect_project_configs(&fixture.0, &fixture.0, &module_root, &[])
            .expect("equally near configs are not ambiguous");
        let selection = select_project_config_with_basis(&fixture.0, &configs).expect("selection");
        assert_eq!(selection.config.path.file_name().unwrap(), "tsconfig.json");
        assert_eq!(
            selection.basis,
            TypeScriptConfigSelectionBasis::Conventional
        );
        assert_eq!(selection.passed_over, 2);
        for name in ["tsconfig.app.json", "tsconfig.node.json"] {
            let sibling = configs
                .iter()
                .find(|config| config.path.ends_with(name))
                .expect("sibling stays witnessed");
            assert_eq!(
                sibling.dependency_authority,
                TypeScriptConfigDependencyAuthority::Unselected
            );
        }
    }

    #[test]
    fn named_configs_without_a_conventional_default_choose_by_documented_rank() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let module_root = fs::canonicalize(modules).expect("canonical module root");
        for (names, expected) in [
            (
                vec!["tsconfig.server.json", "tsconfig.react.json"],
                "tsconfig.react.json",
            ),
            (
                vec!["tsconfig.base.json", "tsconfig.spec.json"],
                "tsconfig.spec.json",
            ),
            (
                vec!["tsconfig.lib.json", "tsconfig.build.json"],
                "tsconfig.build.json",
            ),
            (
                vec![
                    "tsconfig.zeta.json",
                    "tsconfig.app.json",
                    "tsconfig.build.json",
                ],
                "tsconfig.app.json",
            ),
            (vec!["tsconfig.base.json"], "tsconfig.base.json"),
        ] {
            let package = fixture.0.join(expected.replace('.', "-"));
            fs::create_dir_all(&package).expect("create case directory");
            for name in &names {
                fs::write(package.join(name), "{}").expect("write named config");
            }
            let configs = collect_project_configs(&package, &fixture.0, &module_root, &[])
                .expect("named variants never refuse the project");
            assert_eq!(selected_name(&configs, &package), expected, "{names:?}");
            // Selection is a pure function of paths: the same tree yields the same choice.
            let mut reversed = configs.clone();
            reversed.reverse();
            assert_eq!(selected_name(&reversed, &package), expected, "{names:?}");
        }
    }

    #[test]
    fn several_producer_selected_configs_degrade_to_the_nearest_ranked_one() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let module_root = fs::canonicalize(modules).expect("canonical module root");
        fs::create_dir_all(fixture.0.join("projects/lib")).expect("create lib");
        fs::create_dir_all(fixture.0.join("projects/app")).expect("create app");
        fs::write(
            fixture.0.join("angular.json"),
            r#"{"projects":{
                "lib":{"architect":{"build":{"options":{"tsConfig":"projects/lib/tsconfig.lib.json"}}}},
                "app":{"architect":{"build":{"options":{"tsConfig":"projects/app/tsconfig.app.json"}}}},
                "root":{"architect":{"build":{"options":{"tsConfig":"tsconfig.app.json"}}}}}}"#,
        )
        .expect("write multi-project Angular workspace");
        for relative in [
            "tsconfig.json",
            "tsconfig.app.json",
            "projects/lib/tsconfig.lib.json",
            "projects/app/tsconfig.app.json",
        ] {
            fs::write(fixture.0.join(relative), "{}").expect("write config");
        }
        let (selected, _) =
            angular_build_config_paths(&fixture.0, &module_root).expect("read Angular targets");
        assert_eq!(selected.len(), 3);
        let configs = collect_project_configs(&fixture.0, &fixture.0, &module_root, &selected)
            .expect("more than one selected build config is not a refusal");
        let selection = select_project_config_with_basis(&fixture.0, &configs).expect("selection");
        assert_eq!(
            selection.basis,
            TypeScriptConfigSelectionBasis::ProducerSelectedAmong
        );
        assert_eq!(selection.passed_over, 2);
        assert_eq!(
            selection.config.path.as_ref(),
            module_path(&fixture.0, "tsconfig.app.json")
        );
        assert_eq!(
            selection.config.dependency_authority,
            TypeScriptConfigDependencyAuthority::Program
        );
        let ignored = configs
            .iter()
            .find(|config| config.path.ends_with("projects/lib/tsconfig.lib.json"))
            .expect("other selected build config stays witnessed");
        assert_eq!(
            ignored.dependency_authority,
            TypeScriptConfigDependencyAuthority::Unselected
        );

        // Only nested selected builds: the shallowest wins, then rank, then path.
        fs::remove_file(fixture.0.join("tsconfig.app.json")).expect("remove root app config");
        fs::write(
            fixture.0.join("angular.json"),
            r#"{"projects":{
                "lib":{"architect":{"build":{"options":{"tsConfig":"projects/lib/tsconfig.lib.json"}}}},
                "app":{"architect":{"build":{"options":{"tsConfig":"projects/app/tsconfig.app.json"}}}}}}"#,
        )
        .expect("rewrite workspace");
        let (selected, _) =
            angular_build_config_paths(&fixture.0, &module_root).expect("read Angular targets");
        let configs = collect_project_configs(&fixture.0, &fixture.0, &module_root, &selected)
            .expect("capture nested selected configs");
        assert_eq!(selected_name(&configs, &fixture.0), "tsconfig.app.json");
    }

    fn module_path(root: &Path, relative: &str) -> PathBuf {
        fs::canonicalize(root.join(relative)).expect("canonical config")
    }

    #[test]
    fn nearest_directory_wins_and_ancestor_conventional_config_is_not_ambiguous() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let module_root = fs::canonicalize(modules).expect("canonical module root");
        let package = fixture.0.join("packages/a");
        fs::create_dir_all(&package).expect("create package");
        fs::write(fixture.0.join("tsconfig.json"), "{}").expect("write root config");
        fs::write(fixture.0.join("tsconfig.base.json"), "{}").expect("write shared base");
        fs::write(
            package.join("tsconfig.json"),
            r#"{"extends":"../../tsconfig.base.json"}"#,
        )
        .expect("write package config");
        let configs = collect_project_configs(&package, &fixture.0, &module_root, &[])
            .expect("monorepo package");
        assert_eq!(
            select_project_config(&package, &configs)
                .unwrap()
                .path
                .as_ref(),
            module_path(&package, "tsconfig.json")
        );
        let base = configs
            .iter()
            .find(|config| config.path.ends_with("tsconfig.base.json"))
            .expect("extended base is captured");
        assert_eq!(
            base.dependency_authority,
            TypeScriptConfigDependencyAuthority::Inheritance
        );
        // Indexing the repository root selects its own conventional config.
        let configs = collect_project_configs(&fixture.0, &fixture.0, &module_root, &[])
            .expect("repository root");
        assert_eq!(
            select_project_config(&fixture.0, &configs)
                .unwrap()
                .path
                .as_ref(),
            module_path(&fixture.0, "tsconfig.json")
        );
    }

    #[test]
    fn solution_style_config_expands_directory_and_file_references() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let module_root = fs::canonicalize(modules).expect("canonical module root");
        fs::create_dir_all(fixture.0.join("packages/lib")).expect("create referenced directory");
        fs::write(
            fixture.0.join("tsconfig.json"),
            r#"{"files":[],"references":[
                {"path":"./tsconfig.app.json"},
                {"path":"./tsconfig.node.json"},
                {"path":"./packages/lib"}]}"#,
        )
        .expect("write solution config");
        fs::write(
            fixture.0.join("tsconfig.app.json"),
            r#"{"extends":"./tsconfig.base.json","include":["src"]}"#,
        )
        .expect("write app");
        fs::write(
            fixture.0.join("tsconfig.node.json"),
            r#"{"include":["vite.config.ts"]}"#,
        )
        .expect("write node");
        fs::write(
            fixture.0.join("tsconfig.base.json"),
            r#"{"compilerOptions":{"strict":true}}"#,
        )
        .expect("write base");
        fs::write(
            fixture.0.join("packages/lib/tsconfig.json"),
            r#"{"include":["src"]}"#,
        )
        .expect("write directory-referenced project");
        let configs = collect_project_configs(&fixture.0, &fixture.0, &module_root, &[])
            .expect("solution style layout");
        let root = select_project_config(&fixture.0, &configs).expect("solution root");
        assert_eq!(root.path.as_ref(), module_path(&fixture.0, "tsconfig.json"));
        assert_eq!(root.references.len(), 3);
        for relative in [
            "tsconfig.app.json",
            "tsconfig.node.json",
            "packages/lib/tsconfig.json",
        ] {
            let referenced = configs
                .iter()
                .find(|config| config.path.as_ref() == module_path(&fixture.0, relative))
                .expect("referenced project captured");
            assert_eq!(
                referenced.dependency_authority,
                TypeScriptConfigDependencyAuthority::Program,
                "{relative}"
            );
            assert!(
                root.references
                    .iter()
                    .any(|reference| **reference == *referenced.path)
            );
        }
        let base = configs
            .iter()
            .find(|config| config.path.ends_with("tsconfig.base.json"))
            .expect("inherited base captured");
        assert_eq!(
            base.dependency_authority,
            TypeScriptConfigDependencyAuthority::Inheritance
        );
    }

    #[test]
    fn solution_reference_cycles_remain_typed_refusals() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let module_root = fs::canonicalize(modules).expect("canonical module root");
        fs::write(
            fixture.0.join("tsconfig.json"),
            r#"{"files":[],"references":[{"path":"./tsconfig.app.json"}]}"#,
        )
        .expect("write solution");
        fs::write(
            fixture.0.join("tsconfig.app.json"),
            r#"{"include":["src"],"references":[{"path":"./tsconfig.json"}]}"#,
        )
        .expect("write cycle back to the solution");
        assert!(matches!(
            collect_project_configs(&fixture.0, &fixture.0, &module_root, &[]),
            Err(TypeScriptProjectHostError::ConfigCycle { .. })
        ));
    }

    #[test]
    fn unselected_candidate_changes_additions_and_removals_invalidate_the_project_witness() {
        for mutation in ["content", "addition", "removal"] {
            let fixture = Fixture::new();
            fixture.install("5.9.3");
            fs::write(fixture.0.join("tsconfig.json"), "{}").expect("write selected root");
            fs::create_dir_all(fixture.0.join("dev-docs")).expect("create docs");
            let docs = fixture.0.join("dev-docs/tsconfig.json");
            fs::write(&docs, r#"{"extends":"@tsconfig/absent"}"#).expect("raw unselected docs");
            let ProjectTypeScriptSearch::Found(project) =
                find_project_typescript(&fixture.0).expect("discover project TypeScript")
            else {
                panic!("project installation required")
            };
            let node = fixture.0.join("node");
            fs::write(&node, b"node witness").expect("write identity-only Node bytes");
            let package_root = fs::canonicalize(project.module_root.join("typescript"))
                .expect("canonical compiler package");
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
            .expect("capture witness without running Node or compiler");
            witness
                .validate_current()
                .expect("unchanged raw candidate is witnessed");
            match mutation {
                "content" => fs::write(&docs, r#"{"extends":"@tsconfig/changed-absent"}"#)
                    .expect("change raw docs"),
                "addition" => {
                    fs::create_dir_all(fixture.0.join("examples")).expect("create example");
                    fs::write(
                        fixture.0.join("examples/tsconfig.json"),
                        "invalid raw config",
                    )
                    .expect("add unselected candidate");
                }
                "removal" => fs::remove_file(&docs).expect("remove unselected candidate"),
                _ => unreachable!(),
            }
            assert!(
                matches!(
                    witness.validate_current(),
                    Err(TypeScriptProjectHostError::WitnessChanged { .. })
                ),
                "unselected {mutation} still changes the admission witness"
            );
        }
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
    fn validated_host_proof_is_exact_and_fresh_at_each_forest_boundary() {
        let fixture = Fixture::new();
        fixture.install("5.9.3");
        let ProjectTypeScriptSearch::Found(project) = find_project_typescript(&fixture.0).unwrap()
        else {
            panic!("project");
        };
        let node = fixture.0.join("node");
        fs::write(&node, b"identity-only host control; never executed").unwrap();
        let package_root = fs::canonicalize(project.module_root.join("typescript")).unwrap();
        let capture = || {
            TypeScriptProjectWitness::capture(
                &fixture.0,
                None,
                &project,
                &project.compiler,
                &node,
                &project.module_root,
                &package_root,
                project.workspace.as_ref(),
            )
            .unwrap()
        };
        let witness = capture();
        let source = fixture.0.join("main.ts");
        fs::write(&source, b"export const value = 1;").unwrap();
        let source = fs::canonicalize(source).unwrap();
        let mut first = TypeScriptResolverCapability {
            witness: &witness,
            observations: ResolverObservationLedger::default(),
        };
        first.try_load_source(&source).unwrap().unwrap();
        let proof = witness.validated().unwrap();
        assert!(
            proof.sources.contains_key(&source),
            "registered reads enter one freshly validated index"
        );
        let other = capture();
        assert!(
            first.seal_with_host(&other.validated().unwrap()).is_err(),
            "same physical inputs cannot substitute another witness object"
        );
        let sealed = first.seal_with_host(&proof).unwrap();
        sealed.validate_with_host(&proof).unwrap();
        fs::write(&source, b"export const value = 2;").unwrap();
        assert!(
            witness.validated().is_err(),
            "next stage/publish boundary recaptures every shared file"
        );
        assert!(sealed.validate_current(&witness).is_err());
    }

    #[test]
    fn native_host_snapshot_codec_is_complete_strict_and_replayable_after_decode() {
        let fixture = Fixture::new();
        fs::write(
            fixture.0.join("package.json"),
            r#"{"name":"snapshot-fixture"}"#,
        )
        .unwrap();
        let witness = TypeScriptProjectWitness::capture_native(&fixture.0, None).unwrap();
        let encoded = witness
            .encode_native_snapshot()
            .expect("encode native host");
        let decoded = TypeScriptProjectWitness::decode_native_snapshot(&encoded)
            .expect("decode complete host witness");
        assert_eq!(
            decoded.encode_native_snapshot().as_deref(),
            Some(encoded.as_slice())
        );
        decoded
            .validate_current()
            .expect("replay host witness against current files");

        let mut corrupt = encoded.clone();
        corrupt[0] ^= 0x20;
        assert!(TypeScriptProjectWitness::decode_native_snapshot(&corrupt).is_none());

        let mut incomplete: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        incomplete
            .as_object_mut()
            .unwrap()
            .remove("config_candidates");
        assert!(
            TypeScriptProjectWitness::decode_native_snapshot(
                &serde_json::to_vec(&incomplete).unwrap()
            )
            .is_none()
        );
    }

    #[test]
    fn native_directory_snapshot_keeps_full_targets_and_detects_atomic_symlink_replacement() {
        let fixture = Fixture::new();
        let outside = Fixture::new();
        let directory = fixture.0.join("entries");
        fs::create_dir_all(directory.join("subdir")).unwrap();
        fs::write(directory.join("plain.ts"), "export const value = 1;").unwrap();
        fs::write(outside.0.join("external.ts"), "export const outside = 1;").unwrap();
        symlink("plain.ts", directory.join("file-link.ts")).unwrap();
        symlink("subdir", directory.join("directory-link")).unwrap();
        symlink("absent.ts", directory.join("dangling.ts")).unwrap();
        symlink(outside.0.join("external.ts"), directory.join("outside.ts")).unwrap();
        let witness = TypeScriptProjectWitness::capture_native(&fixture.0, None).unwrap();
        DIRECTORY_SNAPSHOT_CAPTURES.with(|count| count.set(0));
        let snapshot = capture_directory_snapshot(&directory, &witness).unwrap();
        assert_eq!(
            DIRECTORY_SNAPSHOT_CAPTURES.with(|count| count.replace(0)),
            1
        );
        for entry in snapshot.entries.iter() {
            let canonical = fs::canonicalize(&entry.path)
                .ok()
                .map(PathBuf::into_boxed_path);
            assert_eq!(
                entry.canonical_path, canonical,
                "exact target identity: {:?}",
                entry.path
            );
            let effective = canonical.as_deref().map(|path| {
                let metadata = fs::metadata(path).unwrap();
                if metadata.is_dir() {
                    TypeScriptDirectoryEntryKind::Directory
                } else if metadata.is_file() {
                    TypeScriptDirectoryEntryKind::RegularFile
                } else {
                    TypeScriptDirectoryEntryKind::Other
                }
            });
            assert_eq!(entry.effective_kind, effective);
        }
        let mut capability = TypeScriptResolverCapability {
            witness: &witness,
            observations: ResolverObservationLedger::default(),
        };
        assert!(
            matches!(
                capability.try_load_source(&directory.join("outside.ts")),
                Err(TypeScriptProjectHostError::SourceOutsideCapability { .. })
            ),
            "a captured link never admits its outside target"
        );
        symlink("plain.ts", directory.join("new-link.ts")).unwrap();
        assert_ne!(
            capture_directory_snapshot(&directory, &witness).unwrap(),
            snapshot,
            "new children still change the full directory witness"
        );
        fs::remove_file(directory.join("new-link.ts")).unwrap();
        fs::remove_file(directory.join("file-link.ts")).unwrap();
        symlink("subdir", directory.join("file-link.ts")).unwrap();
        assert_ne!(
            capture_directory_snapshot(&directory, &witness).unwrap(),
            snapshot,
            "retargeted symlinks still change kind and canonical target"
        );
        let before_replacement = capture_directory_snapshot(&directory, &witness).unwrap();
        symlink(
            outside.0.join("external.ts"),
            directory.join("replacement.tmp"),
        )
        .unwrap();
        fs::rename(
            directory.join("replacement.tmp"),
            directory.join("plain.ts"),
        )
        .unwrap();
        let after_replacement = capture_directory_snapshot(&directory, &witness).unwrap();
        assert_ne!(
            after_replacement, before_replacement,
            "atomic replacement of a regular child with a persistent symlink changes the witness"
        );
        let replaced = after_replacement
            .entries
            .iter()
            .find(|entry| entry.path.as_ref() == directory.join("plain.ts").as_path())
            .unwrap();
        assert_eq!(replaced.kind, TypeScriptDirectoryEntryKind::Symlink);
        assert_eq!(
            replaced.canonical_path.as_deref(),
            Some(outside.0.join("external.ts").as_path())
        );
        assert!(
            matches!(
                capability.try_load_source(&directory.join("plain.ts")),
                Err(TypeScriptProjectHostError::SourceOutsideCapability { .. })
            ),
            "a replaced child cannot admit its outside target"
        );
    }

    #[test]
    fn native_negative_resolution_replays_each_exact_parent_directory_once() {
        let fixture = Fixture::new();
        let directory = fixture.0.join("node_modules");
        for index in 0..32 {
            fs::create_dir_all(directory.join(format!("package-{index}"))).unwrap();
        }
        let witness = TypeScriptProjectWitness::capture_native(&fixture.0, None).unwrap();
        let mut capability = TypeScriptResolverCapability {
            witness: &witness,
            observations: ResolverObservationLedger::default(),
        };
        for index in 0..24 {
            assert!(
                capability
                    .try_load_source(&directory.join(format!("absent-{index}.d.ts")))
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(capability.observations.missing_paths.len(), 24);
        assert_eq!(capability.observations.directories.len(), 1);
        let fingerprint = capability.resolver_witness().unwrap();
        DIRECTORY_SNAPSHOT_CAPTURES.with(|count| count.set(0));
        capability.validate_current().unwrap();
        assert_eq!(
            DIRECTORY_SNAPSHOT_CAPTURES.with(|count| count.replace(0)),
            1,
            "24 distinct misses sharing32siblings require one complete fresh parent replay"
        );
        assert_eq!(
            capability.resolver_witness().unwrap(),
            fingerprint,
            "the same exact witness digest and observation recipe remain authoritative"
        );
        fs::create_dir(directory.join("new-package")).unwrap();
        assert!(
            matches!(
                capability.validate_current(),
                Err(TypeScriptProjectHostError::WitnessChanged { .. })
            ),
            "a changed parent still refuses while all original misses remain absent"
        );
        fs::remove_dir(directory.join("new-package")).unwrap();
        fs::write(directory.join("absent-0.d.ts"), "export {};").unwrap();
        assert!(
            matches!(
                capability.validate_current(),
                Err(TypeScriptProjectHostError::WitnessChanged { .. })
            ),
            "a materialized negative candidate still refuses independently"
        );
        fs::remove_file(directory.join("absent-0.d.ts")).unwrap();
        capability.observations.directories.clear();
        assert!(
            matches!(
                capability.validate_current(),
                Err(TypeScriptProjectHostError::WitnessChanged { .. })
            ),
            "a forged parent edge without its directory proof never admits"
        );
    }

    #[test]
    fn native_directory_capture_reuses_one_view_for_repeated_resolution_probes() {
        let fixture = Fixture::new();
        let directory = fixture.0.join("modules");
        fs::create_dir(&directory).unwrap();
        for index in 0..32 {
            fs::write(directory.join(format!("sibling-{index}.ts")), "export {};").unwrap();
        }
        let witness = TypeScriptProjectWitness::capture_native(&fixture.0, None).unwrap();
        let mut capability = TypeScriptResolverCapability {
            witness: &witness,
            observations: ResolverObservationLedger::default(),
        };
        DIRECTORY_SNAPSHOT_CAPTURES.with(|count| count.set(0));
        for index in 0..24 {
            assert!(
                capability
                    .try_load_source(&directory.join(format!("missing-{index}.ts")))
                    .unwrap()
                    .is_none()
            );
            assert!(capability.directory_exists(&directory).unwrap());
            assert_eq!(
                capability.observe_program_directory(&directory).unwrap(),
                Some(directory.clone())
            );
            assert_eq!(
                capability
                    .observed_program_directory_entries(&directory)
                    .unwrap()
                    .len(),
                32
            );
        }
        assert_eq!(
            DIRECTORY_SNAPSHOT_CAPTURES.with(|count| count.replace(0)),
            1,
            "24 distinct misses and48 repeated positive directory probes share one captured view"
        );
        assert_eq!(capability.observations.missing_paths.len(), 24);
        let digest = capability.resolver_witness().unwrap();
        let sealed = capability.seal().unwrap();
        assert_eq!(
            DIRECTORY_SNAPSHOT_CAPTURES.with(|count| count.replace(0)),
            1,
            "the complete current directory is still replayed at the admission seal"
        );
        assert_eq!(*sealed.digest(), digest, "same exact observation recipe");
        sealed.validate_current(&witness).unwrap();
    }

    #[test]
    fn native_cached_directory_rejects_new_child_before_admission_seal() {
        let fixture = Fixture::new();
        let directory = fixture.0.join("modules");
        fs::create_dir(&directory).unwrap();
        let witness = TypeScriptProjectWitness::capture_native(&fixture.0, None).unwrap();
        let mut capability = TypeScriptResolverCapability {
            witness: &witness,
            observations: ResolverObservationLedger::default(),
        };
        capability.observe_program_directory(&directory).unwrap();
        fs::write(directory.join("new.ts"), "export const newValue = 1;").unwrap();
        capability.observe_program_directory(&directory).unwrap();
        assert!(
            capability
                .observed_program_directory_entries(&directory)
                .unwrap()
                .is_empty(),
            "one program continues to read its immutable first capture"
        );
        assert!(
            matches!(
                capability.seal(),
                Err(TypeScriptProjectHostError::WitnessChanged { .. })
            ),
            "a new child cannot cross the fresh native admission seal"
        );
    }

    #[test]
    fn native_cached_directory_rejects_materialized_negative_before_admission_seal() {
        let fixture = Fixture::new();
        let directory = fixture.0.join("modules");
        fs::create_dir(&directory).unwrap();
        let missing = directory.join("missing.ts");
        let witness = TypeScriptProjectWitness::capture_native(&fixture.0, None).unwrap();
        let mut capability = TypeScriptResolverCapability {
            witness: &witness,
            observations: ResolverObservationLedger::default(),
        };
        assert!(capability.try_load_source(&missing).unwrap().is_none());
        fs::write(&missing, "export const appeared = 1;").unwrap();
        assert!(capability.directory_exists(&directory).unwrap());
        assert!(
            matches!(
                capability.seal(),
                Err(TypeScriptProjectHostError::WitnessChanged { .. })
            ),
            "a formerly missing module must still refuse independently of the cached directory"
        );
    }

    #[test]
    fn native_cached_directory_keeps_current_alias_capability_check() {
        let fixture = Fixture::new();
        let outside = Fixture::new();
        let directory = fixture.0.join("modules");
        fs::create_dir(&directory).unwrap();
        let witness = TypeScriptProjectWitness::capture_native(&fixture.0, None).unwrap();
        let mut capability = TypeScriptResolverCapability {
            witness: &witness,
            observations: ResolverObservationLedger::default(),
        };
        capability.observe_program_directory(&directory).unwrap();
        fs::rename(&directory, fixture.0.join("original-modules")).unwrap();
        symlink(&outside.0, &directory).unwrap();
        assert!(
            matches!(
                capability.observe_program_directory(&directory),
                Err(TypeScriptProjectHostError::SourceOutsideCapability { .. })
            ),
            "per-request current realpath admission precedes the captured-directory lookup"
        );
        assert!(capability.seal().is_err());
    }

    #[test]
    fn native_cached_directory_rejects_atomic_child_symlink_replacement_at_seal() {
        let fixture = Fixture::new();
        let outside = Fixture::new();
        let directory = fixture.0.join("modules");
        fs::create_dir(&directory).unwrap();
        let child = directory.join("value.ts");
        fs::write(&child, "export const value = 1;").unwrap();
        fs::write(outside.0.join("other.ts"), "export const other = 1;").unwrap();
        let witness = TypeScriptProjectWitness::capture_native(&fixture.0, None).unwrap();
        let mut capability = TypeScriptResolverCapability {
            witness: &witness,
            observations: ResolverObservationLedger::default(),
        };
        capability.observe_program_directory(&directory).unwrap();
        let replacement = directory.join("replacement");
        symlink(outside.0.join("other.ts"), &replacement).unwrap();
        fs::rename(&replacement, &child).unwrap();
        capability.observe_program_directory(&directory).unwrap();
        assert!(
            matches!(
                capability.try_load_source(&child),
                Err(TypeScriptProjectHostError::SourceOutsideCapability { .. })
            ),
            "cached entries cannot grant an outside source after atomic replacement"
        );
        assert!(
            capability.seal().is_err(),
            "unchanged full child snapshots still detect replacement"
        );
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
        let closure = capability
            .seal()
            .expect("retain the exact resolver read closure");
        assert_eq!(closure.digest(), &fingerprint);
        closure
            .validate_current(&witness)
            .expect("sealed resolver observations remain current");

        let newly_enumerated = project_root.join("src/added.ts");
        fs::write(&newly_enumerated, b"export const added = true;")
            .expect("add a new directory result");
        assert!(matches!(
            closure.validate_current(&witness),
            Err(TypeScriptProjectHostError::WitnessChanged { .. })
        ));
        fs::remove_file(&newly_enumerated).expect("restore witnessed directory set");
        closure
            .validate_current(&witness)
            .expect("restored directory result matches the sealed witness");

        fs::write(&source, b"export const answer = 43;").expect("change captured source bytes");
        assert!(matches!(
            closure.validate_current(&witness),
            Err(TypeScriptProjectHostError::WitnessChanged { .. })
        ));
        fs::write(&source, b"export const answer = 42;").expect("restore captured source bytes");
        closure
            .validate_current(&witness)
            .expect("restored source bytes match the sealed witness");

        fs::write(&missing, b"declare const missing: string;")
            .expect("materialize formerly missing candidate");
        assert!(matches!(
            closure.validate_current(&witness),
            Err(TypeScriptProjectHostError::WitnessChanged { .. })
        ));
    }

    #[test]
    fn resolver_deduplicates_symlinked_source_bytes_but_binds_each_alias() {
        let fixture = Fixture::new();
        let modules = fixture.install("5.9.3");
        let source_dir = fixture.0.join("src");
        fs::create_dir_all(&source_dir).expect("create source directory");
        let source = source_dir.join("shared.d.ts");
        fs::write(&source, b"export interface Shared {}").expect("write shared source");
        let first_alias = source_dir.join("first.d.ts");
        let second_alias = source_dir.join("second.d.ts");
        symlink(&source, &first_alias).expect("create first source alias");
        symlink(&source, &second_alias).expect("create second source alias");

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
        let mut capability = TypeScriptResolverCapability {
            witness: &witness,
            observations: ResolverObservationLedger::default(),
        };

        let admitted = capability
            .try_load_source(&source)
            .expect("load canonical source")
            .expect("canonical source exists");
        let content_id = admitted.content_id;
        for alias in [&first_alias, &second_alias] {
            let aliased = capability
                .try_load_source(alias)
                .expect("load symlinked source")
                .expect("alias resolves to source");
            assert_eq!(aliased.content_id, content_id);
            assert_eq!(aliased.path.as_ref(), fs::canonicalize(&source).unwrap());
            assert!(capability.observations.realpaths.contains_key(alias));
        }
        assert_eq!(
            capability.loaded_sources().len(),
            1,
            "canonical source bytes enter the project closure only once"
        );
        assert_eq!(
            capability.observations.realpaths.len(),
            3,
            "canonical path and both lexical aliases remain separately witnessed"
        );
        capability
            .seal()
            .expect("seal exact canonical source plus lexical alias observations")
            .validate_current(&witness)
            .expect("the deduplicated source closure revalidates");
    }

    #[test]
    fn resolver_capability_witnesses_only_absent_ancestor_node_modules_candidates() {
        let outer = Fixture::new();
        let project_root = outer.0.join("workspace/app");
        fs::create_dir_all(&project_root).expect("create project root");
        let package_fixture = Fixture(project_root.clone());
        package_fixture.install("5.9.3");
        let modules = fs::canonicalize(project_root.join("node_modules"))
            .expect("canonical local module root");
        let ProjectTypeScriptSearch::Found(project) =
            find_project_typescript(&project_root).expect("discover local compiler")
        else {
            panic!("local compiler should be selected");
        };
        let node = project_root.join("node");
        fs::write(&node, b"node witness").expect("write Node witness");
        let node = fs::canonicalize(node).expect("canonical Node");
        let package_root = fs::canonicalize(modules.join("typescript"))
            .expect("canonical TypeScript package root");
        let witness = TypeScriptProjectWitness::capture(
            &project_root,
            None,
            &project,
            &project.compiler,
            &node,
            &modules,
            &package_root,
            project.workspace.as_ref(),
        )
        .expect("capture project witness");
        let mut capability = TypeScriptResolverCapability {
            witness: &witness,
            observations: ResolverObservationLedger::default(),
        };
        let ancestor_modules = outer.0.join("workspace/node_modules");
        assert!(
            !capability
                .directory_exists(&ancestor_modules)
                .expect("observe bounded absent ancestor module root")
        );
        assert!(
            capability
                .read_directory(&ancestor_modules, &[".d.ts"], true, 8, 64)
                .expect("enumerate absent ancestor candidate")
                .is_empty()
        );
        assert!(
            capability
                .resolver_witness()
                .expect("fingerprint negative candidate")
                != [0; 32]
        );
        capability
            .validate_current()
            .expect("negative ancestor candidate remains absent");

        fs::create_dir_all(&ancestor_modules).expect("materialize ancestor module root");
        assert!(matches!(
            capability.validate_current(),
            Err(TypeScriptProjectHostError::WitnessChanged { .. })
        ));
    }

    #[test]
    fn negative_directory_replay_preserves_external_source_refusal_and_parent_witness() {
        let outer = Fixture::new();
        let project_root = outer.0.join("peer-probesnode_modules-suffix/workspace/app");
        fs::create_dir_all(&project_root).expect("create project root");
        let package_fixture = Fixture(project_root.clone());
        let modules = package_fixture.install("5.9.3");
        fs::write(project_root.join("package.json"),
            br#"{"peerDependencies":{"react":"*","react-dom":"*","@scope/peer":"*","../foreign":"*"}}"#)
            .expect("write witnessed workspace peers");
        let ProjectTypeScriptSearch::Found(project) =
            find_project_typescript(&project_root).expect("discover local compiler")
        else {
            panic!("local compiler should be selected");
        };
        let node = project_root.join("node");
        fs::write(&node, b"node witness").expect("write Node witness");
        let package_root = fs::canonicalize(modules.join("typescript"))
            .expect("canonical TypeScript package root");
        let witness = TypeScriptProjectWitness::capture(
            &project_root,
            None,
            &project,
            &project.compiler,
            &node,
            &modules,
            &package_root,
            project.workspace.as_ref(),
        )
        .expect("capture project witness");
        let parent = outer.0.join("peer-probesnode_modules");
        fs::create_dir(&parent).expect("create external probe parent");
        let candidate = parent.join("react");
        let mut capability = TypeScriptResolverCapability {
            witness: &witness,
            observations: ResolverObservationLedger::default(),
        };
        assert!(matches!(
            capability.directory_exists(&candidate),
            Err(TypeScriptProjectHostError::SourceOutsideCapability { .. })
        ));
        assert!(
            matches!(
                capability.observe_absent_directory(&candidate),
                Err(TypeScriptProjectHostError::SourceOutsideCapability { .. })
            ),
            "a reported miss alone carries no peer lookup authority"
        );
        capability
            .try_load_source(&project_root.join("package.json"))
            .expect("capture admitted peer manifest");
        assert_eq!(capability.observations.captured_peer_manifests.len(), 1);
        assert!(
            capability.observations.peer_directory_candidates.is_empty(),
            "parsed peers stay inactive until the exact directory realpath is captured"
        );
        assert!(
            matches!(
                capability.observe_absent_directory(&candidate),
                Err(TypeScriptProjectHostError::SourceOutsideCapability { .. })
            ),
            "manifest bytes alone do not prove the compiler's real package directory"
        );
        capability
            .realpath(&project_root)
            .expect("capture selected package realpath");
        let unrelated = parent.join("unselected-type-root");
        assert!(matches!(
            capability.observe_absent_directory(&unrelated),
            Err(TypeScriptProjectHostError::SourceOutsideCapability { .. })
        ));
        fs::create_dir(&unrelated).expect("materialize unrelated external type root");
        assert!(
            matches!(
                capability.observe_absent_directory(&unrelated),
                Err(TypeScriptProjectHostError::SourceOutsideCapability { .. })
            ),
            "external type-root presence cannot change refusal into acceptance"
        );
        fs::remove_dir(&unrelated).expect("remove unrelated type root");
        assert!(
            matches!(
                capability.observe_absent_directory(&parent.join("../foreign")),
                Err(TypeScriptProjectHostError::SourceOutsideCapability { .. })
            ),
            "malformed peer names cannot escape the derived peer base"
        );
        capability
            .observe_absent_directory(&candidate)
            .expect("replay source-derived compiler negative peer observation");
        let digest = capability
            .resolver_witness()
            .expect("hash external absence");
        capability
            .observe_absent_directory(&candidate)
            .expect("repeated observation retains the same proof");
        assert_eq!(capability.resolver_witness().unwrap(), digest);
        assert_eq!(capability.observations.external_absent_directories.len(), 1);
        let indexed_work = capability.closure_replay_work_units();
        capability
            .try_load_source(&project_root.join("package.json"))
            .expect("reuse captured manifest without reparsing");
        capability
            .realpath(&project_root)
            .expect("reuse captured exact directory realpath");
        capability
            .observe_absent_directory(&candidate)
            .expect("reuse indexed peer candidate");
        assert_eq!(
            capability.closure_replay_work_units(),
            indexed_work,
            "repeated manifest/realpath/miss observations perform no repeated index work"
        );
        assert!(capability.observations.directories.is_empty());
        assert_eq!(
            capability.loaded_sources().len(),
            1,
            "only admitted manifest bytes"
        );
        assert!(matches!(
            capability.try_load_source(&candidate.join("index.d.ts")),
            Err(TypeScriptProjectHostError::SourceOutsideCapability { .. })
        ));
        assert!(matches!(
            capability.realpath(&candidate),
            Err(TypeScriptProjectHostError::SourceOutsideCapability { .. })
        ));
        assert!(matches!(
            capability.read_directory(&candidate, &[".d.ts"], false, 0, 8),
            Err(TypeScriptProjectHostError::SourceOutsideCapability { .. })
        ));
        let mut other = TypeScriptResolverCapability {
            witness: &witness,
            observations: ResolverObservationLedger::default(),
        };
        other
            .realpath(&project_root)
            .expect("capture package realpath before its manifest");
        other
            .try_load_source(&project_root.join("package.json"))
            .expect("later manifest capture activates the same candidate index");
        assert!(!other.observations.peer_directory_candidates.is_empty());
        other
            .observe_absent_directory(&parent.join("react-dom"))
            .expect("observe a distinct absent peer directory");
        assert_ne!(other.resolver_witness().unwrap(), digest);
        fs::create_dir(parent.join("@scope")).expect("create external peer scope parent");
        other
            .observe_absent_directory(&parent.join("@scope/peer"))
            .expect("a captured scoped package peer derives the same bounded candidate class");
        let closure = capability.seal().expect("seal the typed negative closure");
        closure
            .validate_current(&witness)
            .expect("absence remains current");

        let moved_parent = outer.0.join("prior-peer-probes");
        fs::rename(&parent, &moved_parent).expect("move witnessed parent");
        fs::create_dir(&parent).expect("replace external parent identity");
        assert!(matches!(
            closure.validate_current(&witness),
            Err(TypeScriptProjectHostError::WitnessChanged { .. })
        ));
        fs::remove_dir(&parent).expect("remove replacement parent");
        fs::rename(&moved_parent, &parent).expect("restore witnessed parent");
        closure
            .validate_current(&witness)
            .expect("exact parent restored");
        fs::create_dir(&candidate).expect("materialize negative directory");
        fs::write(candidate.join("index.d.ts"), b"export interface Foreign {}")
            .expect("write external source");
        assert!(matches!(
            closure.validate_current(&witness),
            Err(TypeScriptProjectHostError::WitnessChanged { .. })
        ));
        assert!(other.observe_absent_directory(&candidate).is_err());
        assert!(matches!(
            other.directory_exists(&candidate),
            Err(TypeScriptProjectHostError::SourceOutsideCapability { .. })
        ));
        assert!(matches!(
            other.try_load_source(&candidate.join("index.d.ts")),
            Err(TypeScriptProjectHostError::SourceOutsideCapability { .. })
        ));
        fs::remove_dir_all(&candidate).expect("remove external candidate");
        symlink(parent.join("missing-target"), &candidate).expect("create dangling external link");
        assert!(other.observe_absent_directory(&candidate).is_err());
        assert_eq!(other.loaded_sources().len(), 1);

        fs::remove_file(&candidate).expect("remove dangling candidate link");
        let target = outer.0.join("symlink-parent-target");
        fs::rename(&parent, &target).expect("move parent to the symlink target");
        symlink(&target, &parent).expect("create lexical parent symlink");
        let mut linked = TypeScriptResolverCapability {
            witness: &witness,
            observations: ResolverObservationLedger::default(),
        };
        linked
            .try_load_source(&project_root.join("package.json"))
            .expect("capture peers for the symlink parent proof");
        linked
            .realpath(&project_root)
            .expect("capture package realpath");
        linked
            .observe_absent_directory(&candidate)
            .expect("capture missing peer through link");
        let proof = linked
            .observations
            .external_absent_directories
            .get(&candidate)
            .unwrap();
        assert_eq!(proof.canonical_path.as_deref(), Some(target.as_path()));
        assert_eq!(
            proof.identity,
            Some(file_identity(&fs::symlink_metadata(&parent).unwrap(), &parent, None).unwrap()),
            "proof captures the lexical link inode"
        );
        let linked = linked.seal().expect("seal lexical parent-link proof");
        linked
            .validate_current(&witness)
            .expect("unchanged link and missing peer");

        let previous_target = outer.0.join("previous-symlink-parent-target");
        fs::rename(&target, &previous_target).expect("move canonical target directory");
        fs::create_dir(&target).expect("replace target at the same canonical path");
        linked.validate_current(&witness).expect(
            "same lexical link and canonical path retain an absence proof, not target-inode proof",
        );
        fs::create_dir(&candidate).expect("materialize peer inside replacement target");
        assert!(matches!(
            linked.validate_current(&witness),
            Err(TypeScriptProjectHostError::WitnessChanged { .. })
        ));
        fs::remove_dir(&candidate).expect("restore candidate absence");
        fs::remove_dir(&target).expect("remove replacement target");
        fs::rename(&previous_target, &target).expect("restore original canonical target");

        let retarget = outer.0.join("different-symlink-parent-target");
        fs::create_dir(&retarget).expect("create distinct target");
        fs::remove_file(&parent).expect("remove lexical link");
        symlink(&retarget, &parent).expect("retarget lexical link");
        assert!(
            matches!(
                linked.validate_current(&witness),
                Err(TypeScriptProjectHostError::WitnessChanged { .. })
            ),
            "canonical target-path changes invalidate the parent proof"
        );
        fs::remove_file(&parent).expect("remove retargeted lexical link");
        fs::rename(&target, &parent).expect("replace the link with a regular directory");
        assert!(
            matches!(
                linked.validate_current(&witness),
                Err(TypeScriptProjectHostError::WitnessChanged { .. })
            ),
            "replacing the lexical symlink invalidates its inode/canonical-path proof"
        );
    }

    #[test]
    fn negative_directory_replay_obeys_existing_metadata_bound_without_partial_proof() {
        let outer = Fixture::new();
        let project_root = outer.0.join("peer-probesnode_modules-suffix/workspace/app");
        fs::create_dir_all(&project_root).expect("create project root");
        let fixture = Fixture(project_root.clone());
        let modules = fixture.install("5.9.3");
        fs::write(
            project_root.join("package.json"),
            br#"{"peerDependencies":{"react":"*"}}"#,
        )
        .expect("write peers");
        let ProjectTypeScriptSearch::Found(project) =
            find_project_typescript(&project_root).expect("discover local compiler")
        else {
            panic!("local compiler should be selected");
        };
        let node = project_root.join("node");
        fs::write(&node, b"node witness").expect("write Node witness");
        let package_root = fs::canonicalize(modules.join("typescript"))
            .expect("canonical TypeScript package root");
        let witness = TypeScriptProjectWitness::capture(
            &project_root,
            None,
            &project,
            &project.compiler,
            &node,
            &modules,
            &package_root,
            project.workspace.as_ref(),
        )
        .expect("capture project witness");
        let parent = outer.0.join("peer-probesnode_modules");
        fs::create_dir(&parent).expect("create external peer parent");
        let candidate = parent.join("react");
        let mut exhausted = TypeScriptResolverCapability {
            witness: &witness,
            observations: ResolverObservationLedger {
                closure_replay_work_units:
                    super::super::typescript_program::MAX_CLOSURE_REPLAY_WORK_UNITS,
                ..ResolverObservationLedger::default()
            },
        };
        assert!(matches!(
            exhausted.try_load_source(&project_root.join("package.json")),
            Err(TypeScriptProjectHostError::CompilerIoClosureLimit { .. })
        ));
        assert!(exhausted.observations.captured_peer_manifests.is_empty());
        assert!(exhausted.observations.peer_directory_candidates.is_empty());
        assert!(
            matches!(
                exhausted.observe_absent_directory(&candidate),
                Err(TypeScriptProjectHostError::SourceOutsideCapability { .. })
            ),
            "index work exhaustion cannot leave a partial external candidate grant"
        );
        let mut replay_first = TypeScriptResolverCapability {
            witness: &witness,
            observations: ResolverObservationLedger::default(),
        };
        replay_first
            .charge_closure_replay_work(
                usize::try_from(
                    super::super::typescript_program::MAX_CLOSURE_REPLAY_WORK_UNITS - 1,
                )
                .unwrap(),
                "preceding ordinary replay",
            )
            .expect("charge ordinary work before indexing");
        assert!(matches!(
            replay_first.try_load_source(&project_root.join("package.json")),
            Err(TypeScriptProjectHostError::CompilerIoClosureLimit { .. })
        ));
        assert!(replay_first.observations.captured_peer_manifests.is_empty());
        assert!(
            replay_first
                .observations
                .peer_directory_candidates
                .is_empty()
        );
        let mut capability = TypeScriptResolverCapability {
            witness: &witness,
            observations: ResolverObservationLedger::default(),
        };
        capability
            .try_load_source(&project_root.join("package.json"))
            .expect("capture admitted peer manifest");
        capability
            .realpath(&project_root)
            .expect("capture package realpath");
        let indexing_work = capability.closure_replay_work_units();
        assert!(indexing_work > 0);
        let remaining =
            super::super::typescript_program::MAX_CLOSURE_REPLAY_WORK_UNITS - indexing_work;
        capability
            .charge_closure_replay_work(
                usize::try_from(remaining).unwrap(),
                "following ordinary replay",
            )
            .expect("exact shared remaining work fits");
        assert!(
            matches!(
                capability.charge_closure_replay_work(1, "one additional replay unit"),
                Err(TypeScriptProjectHostError::CompilerIoClosureLimit { .. })
            ),
            "index work cannot leave a separate fresh ordinary replay allowance"
        );
        let cost = candidate.as_os_str().as_encoded_bytes().len() * 2
            + parent.as_os_str().as_encoded_bytes().len() * 2;
        capability.observations.retained_metadata_bytes = MAX_RESOLVER_METADATA_BYTES - cost + 1;
        let missing_before = capability.observations.missing_paths.len();
        assert!(
            matches!(
                capability.observe_absent_directory(&candidate),
                Err(TypeScriptProjectHostError::ResolverMetadataLimit { .. })
            ),
            "both retained candidate keys must be charged before publication"
        );
        assert!(
            capability
                .observations
                .external_absent_directories
                .is_empty()
        );
        assert_eq!(capability.observations.missing_paths.len(), missing_before);
        capability.observations.retained_metadata_bytes = MAX_RESOLVER_METADATA_BYTES - cost;
        capability
            .observe_absent_directory(&candidate)
            .expect("exact remaining byte bound fits");
        assert_eq!(
            capability.observations.retained_metadata_bytes,
            MAX_RESOLVER_METADATA_BYTES
        );
    }

    #[test]
    fn captured_peer_manifest_preserves_bom_jsonc_and_symlink_realpath_orders() {
        use std::os::unix::fs::symlink;

        let outer = Fixture::new();
        let project_root = outer.0.join("peer-probesnode_modules-suffix/workspace/app");
        fs::create_dir_all(&project_root).expect("create project root");
        let fixture = Fixture(project_root.clone());
        let modules = fixture.install("5.9.3");
        let ProjectTypeScriptSearch::Found(project) =
            find_project_typescript(&project_root).expect("discover local compiler")
        else {
            panic!("local compiler should be selected");
        };
        let node = project_root.join("node");
        fs::write(&node, b"node witness").expect("write Node witness");
        let package_root = fs::canonicalize(modules.join("typescript"))
            .expect("canonical TypeScript package root");
        let witness = TypeScriptProjectWitness::capture(
            &project_root,
            None,
            &project,
            &project.compiler,
            &node,
            &modules,
            &package_root,
            project.workspace.as_ref(),
        )
        .expect("capture project witness");
        let parent = outer.0.join("peer-probesnode_modules");
        fs::create_dir(&parent).expect("create external peer parent");
        let candidate = parent.join("react");
        let directory = project_root.join("packages/peer-owner");
        fs::create_dir_all(&directory).expect("create canonical package root");
        let raw = b"\xef\xbb\xbf{\n// captured peer declaration\r\"peerDependencies\": {\"react\": \"*\",},\n}";
        let manifest_path = directory.join("package.json");
        fs::write(&manifest_path, raw).expect("write BOM-prefixed JSONC package manifest");
        let alias = modules.join("peer-owner");
        symlink(&directory, &alias).expect("create pnpm-style package directory alias");
        let inactive_directory = project_root.join("packages/unqueried-peer-owner");
        fs::create_dir_all(&inactive_directory).expect("create unqueried workspace package");
        fs::write(
            inactive_directory.join("package.json"),
            br#"{"peerDependencies":{"react":"*"}}"#,
        )
        .expect("write same pending peer");
        let mut manifest_first = TypeScriptResolverCapability {
            witness: &witness,
            observations: ResolverObservationLedger::default(),
        };
        manifest_first
            .try_load_source(&inactive_directory.join("package.json"))
            .expect("capture a prior manifest without its directory realpath");
        assert!(
            !manifest_first
                .observations
                .captured_peer_manifests
                .get(&inactive_directory)
                .unwrap()
                .activated
        );
        assert!(
            manifest_first
                .observations
                .peer_directory_candidates
                .is_empty(),
            "an inactive same-candidate owner does not reserve the active candidate key"
        );
        let source = manifest_first
            .try_load_source(&alias.join("package.json"))
            .expect("capture admitted aliased package")
            .expect("manifest exists");
        assert_eq!(
            source.content_id,
            ContentId::<SourceFactDomain>::from_canonical_bytes(raw)
        );
        assert_ne!(
            source.content_id,
            ContentId::<SourceFactDomain>::from_canonical_bytes(&raw[3..]),
            "the compiler text BOM removal does not normalize durable raw source identity"
        );
        assert!(
            manifest_first
                .observations
                .peer_directory_candidates
                .is_empty()
        );
        manifest_first
            .realpath(&alias)
            .expect("capture lexical alias to canonical package root");
        assert!(
            !manifest_first
                .observations
                .realpaths
                .contains_key(&directory),
            "activation uses the actual alias observation, not an invented self-realpath"
        );
        assert_eq!(
            manifest_first
                .observations
                .realpath_queries_by_target
                .get(&directory),
            Some(&alias)
        );
        manifest_first.observe_absent_directory(&candidate).expect(
            "BOM/CR-terminated JSONC peers activate from the exact captured canonical target",
        );
        assert_eq!(
            manifest_first
                .observations
                .peer_directory_candidates
                .get(&candidate),
            Some(&directory),
            "the actually queried package activates despite an earlier pending same-candidate manifest"
        );
        manifest_first
            .realpath(&inactive_directory)
            .expect("later activate the other exact owner");
        assert!(
            manifest_first
                .observations
                .captured_peer_manifests
                .get(&inactive_directory)
                .unwrap()
                .activated
        );
        assert_eq!(
            manifest_first
                .observations
                .peer_directory_candidates
                .get(&candidate),
            Some(&directory),
            "two activated owners retain the first sufficient captured proof without rescanning"
        );
        let used = manifest_first.closure_replay_work_units();
        manifest_first
            .try_load_source(&alias.join("package.json"))
            .expect("reuse raw source");
        manifest_first
            .realpath(&alias)
            .expect("reuse exact alias proof");
        assert_eq!(
            manifest_first.closure_replay_work_units(),
            used,
            "duplicate package/alias observations do not parse or index again"
        );
        let first = manifest_first
            .seal()
            .expect("seal manifest-first alias witness");
        first
            .validate_current(&witness)
            .expect("unchanged raw bytes and alias revalidate");

        let mut realpath_first = TypeScriptResolverCapability {
            witness: &witness,
            observations: ResolverObservationLedger::default(),
        };
        realpath_first
            .realpath(&alias)
            .expect("capture alias before manifest");
        realpath_first
            .try_load_source(&alias.join("package.json"))
            .expect("later raw manifest activates recorded canonical target");
        realpath_first
            .observe_absent_directory(&candidate)
            .expect("same derived peer miss");
        realpath_first
            .seal()
            .expect("seal realpath-first alias witness")
            .validate_current(&witness)
            .expect("both observation orders revalidate");

        for (name, separator) in [
            ("line-separator-peer", "\u{2028}"),
            ("paragraph-separator-peer", "\u{2029}"),
        ] {
            let unicode_directory = project_root.join("packages").join(name);
            fs::create_dir_all(&unicode_directory).expect("create Unicode JSONC package root");
            let raw = format!(
                "\u{feff}{{// declared peer{separator}\"peerDependencies\":{{\"{name}\":\"*\"}}}}"
            );
            let path = unicode_directory.join("package.json");
            fs::write(&path, raw.as_bytes()).expect("write raw Unicode line-break manifest");
            let mut unicode = TypeScriptResolverCapability {
                witness: &witness,
                observations: ResolverObservationLedger::default(),
            };
            let source = unicode
                .try_load_source(&path)
                .expect("capture Unicode JSONC manifest")
                .expect("manifest exists");
            assert_eq!(
                source.content_id,
                ContentId::<SourceFactDomain>::from_canonical_bytes(raw.as_bytes()),
                "Unicode whitespace transformation does not alter raw source identity"
            );
            unicode
                .realpath(&unicode_directory)
                .expect("capture exact Unicode peer owner");
            unicode
                .observe_absent_directory(&parent.join(name))
                .expect("TypeScript Unicode line terminator leaves following peer key visible");
            unicode
                .seal()
                .expect("seal Unicode JSONC proof")
                .validate_current(&witness)
                .expect("raw Unicode bytes and directory proof revalidate");
        }

        let malformed_directory = project_root.join("packages/malformed-peer-owner");
        fs::create_dir_all(&malformed_directory).expect("create malformed peer root");
        fs::write(
            malformed_directory.join("package.json"),
            br#"{"peerDependencies":{"broken-peer":"*"},"#,
        )
        .expect("write malformed manifest");
        let mut malformed = TypeScriptResolverCapability {
            witness: &witness,
            observations: ResolverObservationLedger::default(),
        };
        malformed
            .try_load_source(&malformed_directory.join("package.json"))
            .expect("retain exact malformed raw source without deriving candidates");
        malformed
            .realpath(&malformed_directory)
            .expect("capture malformed package directory");
        assert!(
            matches!(
                malformed.observe_absent_directory(&parent.join("broken-peer")),
                Err(TypeScriptProjectHostError::SourceOutsideCapability { .. })
            ),
            "malformed package fallback cannot fabricate a peer candidate"
        );

        fs::remove_file(&alias).expect("remove witnessed alias");
        symlink(&malformed_directory, &alias).expect("retarget alias to another admitted package");
        assert!(
            matches!(
                first.validate_current(&witness),
                Err(TypeScriptProjectHostError::WitnessChanged { .. })
            ),
            "retargeting the lexical alias invalidates the canonical-target proof"
        );
    }

    #[test]
    fn peer_directory_prefix_matches_typescript_utf16_substring_without_path_allowlist() {
        assert_eq!(
            typescript_peer_directory_prefix(
                "/work/sol61-ts-app-proof/current71-excal-01/excalidraw/packages/excalidraw"
            ),
            Some("/work/sol61")
        );
        assert_eq!(typescript_peer_directory_prefix("/"), Some("/"));
        assert_eq!(typescript_peer_directory_prefix("/short"), Some("/short"));
        assert_eq!(
            typescript_peer_directory_prefix("/owned/node_modules-shadow/package"),
            Some("/owned/node_modules")
        );
        assert_eq!(
            typescript_peer_directory_prefix("/abc/😀defghijk"),
            Some("/abc/😀defg")
        );
        assert_eq!(
            typescript_peer_directory_prefix("/abcdefghi😀/package"),
            None,
            "do not admit an unrepresentable split UTF-16 surrogate"
        );
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
            assert_eq!(admitted.compiler_api_for_test().compiler.as_ref(), compiler);
            assert_eq!(
                admitted.compiler_api_for_test().compiler_origin,
                TypeScriptSelectionOrigin::ExplicitConfiguration
            );
            assert_eq!(
                admitted
                    .inputs()
                    .compiler_api()
                    .unwrap()
                    .typescript_module_root,
                expected_module_root
            );
            assert_eq!(
                admitted.inputs().compiler_api().unwrap().compiler_version,
                b"Version 5.8.4\n"
            );
            assert_eq!(
                admitted.inputs().compiler_api().unwrap().node_origin,
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
                            .external()
                            .expect("external Compiler API fixture")
                            .validate_invocation()
                            .expect("package lease checks only the same selected objects");
                    }
                    assert_eq!(
                        crate::application::executable_content_hash_bytes_for_test(),
                        hash_bytes_before,
                        "three source launches must not hash Node or the TypeScript package again",
                    );
                    admitted
                        .witness()
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
                        toolchain.external().expect("external Compiler API fixture").validate_invocation().expect(
                            "per-spawn lease checks the same object without a duplicate full hash",
                        );
                    }
                    assert!(matches!(
                        admitted.witness().validate_current(),
                        Err(TypeScriptProjectHostError::WitnessChanged { .. })
                    ));
                }
                "selected-retarget" => {
                    let package = explicit_modules.join("typescript");
                    let target = explicit_modules.join("retargeted-typescript");
                    fs::rename(&package, &target).expect("move selected installation");
                    symlink(&target, &package).expect("retarget selected installation");
                    assert!(matches!(
                        admitted.witness().validate_current(),
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
                        toolchain
                            .external()
                            .expect("external Compiler API fixture")
                            .validate_invocation(),
                        Err(crate::driver::NativeInvocationError::Changed {
                            role: crate::driver::NativeInvocationFileRole::Interpreter,
                            ..
                        })
                    ));
                    assert!(matches!(
                        admitted.witness().validate_current(),
                        Err(TypeScriptProjectHostError::WitnessChanged { .. })
                    ));
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn installed_host_typescript_admits_packages_without_local_modules() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new();
        let host_root = fixture.0.join("host");
        let modules = host_root.join("node_modules");
        install_at(&modules, "5.9.3");
        let compiler = fs::canonicalize(modules.join("typescript/bin/tsc"))
            .expect("canonical installed-host compiler");
        let workspace = fixture.0.join("workspace");
        fs::create_dir_all(&workspace).expect("create project without local TypeScript");
        let node = host_root.join("node");
        fs::write(
            &node,
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then printf 'v22.0.0\n'; elif [ "$2" = "--version" ]; then printf 'Version 5.9.3\n'; else exit 9; fi
"#,
        )
        .expect("write deterministic Node probe fixture");
        fs::set_permissions(&node, fs::Permissions::from_mode(0o755))
            .expect("make Node probe executable");

        let admitted = TypeScriptProjectHost::new(None, Some(node), None, None, Fixture::limits())
            .with_installed_default(Some(compiler.clone()), Some(modules.clone()))
            .admit(&workspace)
            .expect("admit captured installed-host TypeScript")
            .expect("installed host compiler is usable without a project-local copy");

        assert_eq!(admitted.compiler_api_for_test().compiler.as_ref(), compiler);
        assert_eq!(
            admitted.compiler_api_for_test().compiler_origin,
            TypeScriptSelectionOrigin::InstalledHostSelection
        );
        assert_eq!(
            admitted
                .inputs()
                .compiler_api()
                .unwrap()
                .typescript_module_root,
            fs::canonicalize(modules).expect("canonical TypeScript module root")
        );
        assert_eq!(
            admitted.inputs().compiler_api().unwrap().compiler_version,
            b"Version 5.9.3\n"
        );
    }

    #[test]
    fn project_local_typescript_keeps_precedence_over_installed_host_default() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = Fixture::new();
        let host_root = fixture.0.join("host");
        let host_modules = host_root.join("node_modules");
        install_at(&host_modules, "5.9.3");
        let workspace = fixture.0.join("workspace");
        fs::create_dir_all(&workspace).expect("create project root");
        let local_modules = workspace.join("node_modules");
        install_at(&local_modules, "5.8.4");
        let node = host_root.join("node");
        fs::create_dir_all(&host_root).expect("create host tools directory");
        fs::write(
            &node,
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then printf 'v22.0.0\n'; elif [ "$2" = "--version" ]; then printf 'Version 5.8.4\n'; else exit 9; fi
"#,
        )
        .expect("write deterministic Node probe fixture");
        fs::set_permissions(&node, fs::Permissions::from_mode(0o755))
            .expect("make Node probe executable");
        let installed_compiler = fs::canonicalize(host_modules.join("typescript/bin/tsc"))
            .expect("canonical installed-host compiler");

        let admitted = TypeScriptProjectHost::new(None, Some(node), None, None, Fixture::limits())
            .with_installed_default(Some(installed_compiler), Some(host_modules))
            .admit(&workspace)
            .expect("admit project-local TypeScript ahead of host default")
            .expect("project-local compiler is usable");

        assert_eq!(
            admitted.compiler_api_for_test().compiler_origin,
            TypeScriptSelectionOrigin::ProjectLocalInstallation
        );
        assert_eq!(
            admitted
                .inputs()
                .compiler_api()
                .unwrap()
                .typescript_module_root,
            fs::canonicalize(local_modules).expect("canonical project-local module root")
        );
        assert_eq!(
            admitted.inputs().compiler_api().unwrap().compiler_version,
            b"Version 5.8.4\n"
        );
    }

    #[test]
    fn project_sdk_and_selected_node_do_not_admit_an_unused_bundle() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = Fixture::new();
        let workspace = fixture.0.join("project");
        let modules = workspace.join("node_modules");
        install_at(&modules, "5.7.2");
        let host_modules = fixture.0.join("host/node_modules");
        install_at(&host_modules, "5.9.3");
        let node = fixture.0.join("explicit-node");
        fs::write(&node, r#"#!/bin/sh
if [ "$1" = "--version" ]; then printf 'v24.21.0\n'; elif [ "$2" = "--version" ]; then printf 'Version 5.7.2\n'; else exit 9; fi
"#).expect("write admitted Node probe");
        fs::set_permissions(&node, fs::Permissions::from_mode(0o755)).unwrap();
        let node = fs::canonicalize(node).unwrap();
        // No receipt exists. If bundle fallback is evaluated, this control cannot admit.
        let bundle_application = fixture.0.join("unused.app/Contents/MacOS/backend-mcp");
        let admitted =
            TypeScriptProjectHost::new(None, Some(node.clone()), None, None, Fixture::limits())
                .with_installed_default(
                    Some(host_modules.join("typescript/bin/tsc")),
                    Some(host_modules),
                )
                .with_bundled_application(Some(bundle_application))
                .admit(&workspace)
                .expect("project authority precedes all bundle work")
                .unwrap();
        assert_eq!(
            admitted.inputs().compiler_api().unwrap().compiler_origin,
            TypeScriptSelectionOrigin::ProjectLocalInstallation
        );
        assert_eq!(
            admitted.inputs().compiler_api().unwrap().compiler_version,
            b"Version 5.7.2\n"
        );
        assert_eq!(
            admitted.inputs().compiler_api().unwrap().node_version,
            b"v24.21.0\n"
        );
        assert_eq!(admitted.inputs().compiler_api().unwrap().node_path, node);
        assert!(admitted.witness().resource_proof.is_none());
        let expected = Checker::default()
            .with_node(node, fs::canonicalize(modules).unwrap())
            .unwrap();
        assert_eq!(
            admitted
                .compiler_api_for_test()
                .checker
                .local_configuration_fingerprint(),
            expected.local_configuration_fingerprint(),
            "the report recipe uses exactly the selected Node and project Compiler API"
        );
        admitted
            .resolved_toolchain()
            .unwrap()
            .external()
            .expect("external Compiler API fixture")
            .validate_invocation()
            .unwrap();
    }

    fn installed_fallback_witness(fixture: &Fixture) -> TypeScriptProjectWitness {
        let project_root = fixture.0.join("project");
        fs::create_dir_all(&project_root).expect("create package root");
        let module_root = fixture.0.join("host-modules");
        install_at(&module_root, "5.9.3");
        let compiler = fs::canonicalize(module_root.join("typescript/bin/tsc")).unwrap();
        let node = fixture.0.join("host-node");
        fs::write(&node, b"captured node executable").unwrap();
        let package_root = fs::canonicalize(module_root.join("typescript")).unwrap();
        let project = ProjectTypeScript {
            module_root: module_root.clone(),
            compiler: compiler.clone(),
            version: "5.9.3".into(),
            workspace: None,
            compiler_origin: TypeScriptSelectionOrigin::InstalledHostSelection,
            discovery_origin: TypeScriptProjectDiscoveryOrigin::InstalledFallback,
        };
        TypeScriptProjectWitness::capture(
            &project_root,
            None,
            &project,
            &compiler,
            &node,
            &module_root,
            &package_root,
            None,
        )
        .expect("capture exact host fallback")
    }

    #[test]
    fn absent_ancestor_types_witness_refuses_materialized_types_or_parent() {
        for materialize_types in [false, true] {
            let outer = Fixture::new();
            let project = outer.0.join("isolated-project");
            fs::create_dir_all(&project).expect("private project");
            let fixture = Fixture(project);
            let mut witness = installed_fallback_witness(&fixture);
            witness.workspace_root = fs::canonicalize(&fixture.0).unwrap().into_boxed_path();
            let candidate = outer.0.join("node_modules/@types");
            assert!(!witness.path_is_admitted(&candidate));
            let mut capability = TypeScriptResolverCapability {
                witness: &witness,
                observations: ResolverObservationLedger::default(),
            };
            assert!(
                !capability
                    .directory_exists(&candidate)
                    .expect("bounded negative @types probe")
            );
            assert!(
                capability
                    .read_directory(&candidate, &[".d.ts"], false, 0, 64)
                    .unwrap()
                    .is_empty()
            );
            assert_ne!(capability.resolver_witness().unwrap(), [0; 32]);
            capability
                .validate_current()
                .expect("absence and parent remain unchanged");
            fs::create_dir_all(if materialize_types {
                candidate.as_path()
            } else {
                candidate.parent().unwrap()
            })
            .unwrap();
            assert!(matches!(
                capability.validate_current(),
                Err(TypeScriptProjectHostError::WitnessChanged { .. })
            ));
            if materialize_types {
                assert!(matches!(
                    witness.admit_lexical_path(&candidate),
                    Err(TypeScriptProjectHostError::SourceOutsideCapability { .. })
                ));
            }
        }
    }

    #[test]
    fn installed_fallback_witness_revalidates_without_project_sdk_and_refuses_new_local_sdk() {
        let fixture = Fixture::new();
        let witness = installed_fallback_witness(&fixture);
        witness
            .validate_current()
            .expect("unchanged installed fallback remains valid");
        let project_modules = fixture.0.join("project/node_modules");
        install_at(&project_modules, "5.9.3");
        assert!(
            matches!(
                witness.validate_current(),
                Err(TypeScriptProjectHostError::WitnessChanged { .. })
            ),
            "a newly present project installation requires a new selection even at the same version"
        );
    }

    #[test]
    fn installed_fallback_witness_refuses_changed_or_deleted_host_sdk_and_node() {
        for mutation in ["node", "sdk-version", "sdk-deleted", "compiler"] {
            let fixture = Fixture::new();
            let witness = installed_fallback_witness(&fixture);
            witness
                .validate_current()
                .expect("each independent baseline is valid");
            match mutation {
                "node" => {
                    fs::write(fixture.0.join("host-node"), b"changed node executable").unwrap()
                }
                "sdk-version" => fs::write(
                    fixture.0.join("host-modules/typescript/package.json"),
                    r#"{"name":"typescript","version":"5.8.4"}"#,
                )
                .unwrap(),
                "sdk-deleted" => {
                    fs::remove_dir_all(fixture.0.join("host-modules/typescript")).unwrap()
                }
                "compiler" => fs::write(
                    fixture.0.join("host-modules/typescript/bin/tsc"),
                    b"changed compiler entry",
                )
                .unwrap(),
                _ => unreachable!(),
            }
            assert!(
                witness.validate_current().is_err(),
                "independent {mutation} corruption must refuse"
            );
        }
    }

    #[test]
    fn project_local_witness_cannot_become_installed_fallback_when_local_sdk_disappears() {
        let fixture = Fixture::new();
        let _host_witness = installed_fallback_witness(&fixture);
        let project_root = fixture.0.join("project");
        let modules = project_root.join("node_modules");
        install_at(&modules, "5.9.3");
        let ProjectTypeScriptSearch::Found(project) =
            find_project_typescript(&project_root).unwrap()
        else {
            panic!("actual project installation required");
        };
        let node = fixture.0.join("host-node");
        let package_root = fs::canonicalize(modules.join("typescript")).unwrap();
        let witness = TypeScriptProjectWitness::capture(
            &project_root,
            None,
            &project,
            &project.compiler,
            &node,
            &modules,
            &package_root,
            project.workspace.as_ref(),
        )
        .unwrap();
        witness
            .validate_current()
            .expect("unchanged local selection");
        fs::remove_dir_all(modules).unwrap();
        assert!(
            matches!(
                witness.validate_current(),
                Err(TypeScriptProjectHostError::WitnessChanged { .. })
            ),
            "a disappeared project installation does not inherit the valid host installation"
        );
    }

    fn install_at(modules: &Path, version: &str) {
        let package = modules.join("typescript");
        fs::create_dir_all(package.join("bin")).expect("create package");
        fs::create_dir_all(package.join("lib")).expect("create Compiler API package");
        fs::write(
            package.join("package.json"),
            format!("{{\"name\":\"typescript\",\"version\":\"{version}\"}}"),
        )
        .expect("write package manifest");
        fs::write(package.join("bin/tsc"), "#!/usr/bin/env node\n").expect("write compiler");
        fs::write(package.join("lib/typescript.js"), "module.exports = {};\n")
            .expect("write Compiler API fixture entry");
        fs::create_dir_all(modules.join(".bin")).expect("create package manager bin");
        symlink(package.join("bin/tsc"), modules.join(".bin/tsc")).expect("link package compiler");
    }
}
