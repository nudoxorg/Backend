//! One fresh Pyrefly State transaction over the caller's complete source frontier.
//!
//! Definitions and inferred types come from the pinned native Pyrefly 1.2
//! solved State (`Require::Everything`). Byte spans are native UTF-8 coordinates.
//! Exact native module contents gate every join to the selected source.
//! No transaction or compiler read-set is reused across requests.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use backend_semantic::vocabulary::PythonVersion;

use super::{CheckerError, CheckerReport, Workspace};
use crate::legacy::{DeclarationKind, Span, extract};

const CONFIG_BYTES: u64 = 1024 * 1024;

/// The compiled native solver is a distinct producer from an external Pyrefly command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativePythonProducerIdentity([u8; 32]);

impl NativePythonProducerIdentity {
    /// Actual host producer bytes plus pinned source/manifest/policy identity.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

/// Admitted in-process Pyrefly State producer; no interpreter or external checker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativePythonProjectAuthority {
    producer: NativePythonProducerIdentity,
    timeout: std::time::Duration,
}

impl NativePythonProjectAuthority {
    /// Captures the actual executing native producer image and pinned solver receipts.
    ///
    /// # Errors
    /// Refuses unavailable producer bytes or a non-regular host executable.
    pub fn admit() -> Result<Self, CheckerError> {
        let (producer, _) = native_producer_capture()?;
        Ok(Self {
            producer,
            timeout: super::DEFAULT_TIMEOUT,
        })
    }

    /// Exact admitted native producer identity, independent of external commands.
    #[must_use]
    pub const fn producer_identity(self) -> NativePythonProducerIdentity {
        self.producer
    }

    /// Sets the deadline bound for the complete State transaction and projection.
    #[must_use]
    pub fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

fn native_producer_capture() -> Result<(NativePythonProducerIdentity, FileWitness), CheckerError> {
    let host = FileWitness::capture(std::env::current_exe().map_err(workspace_error)?)?;
    let Some((digest, size)) = host.digest else {
        return Err(project_error(
            "",
            "compiled native producer image is unavailable",
        ));
    };
    let mut identity = blake3::Hasher::new();
    identity.update(b"compiler.python.compiled-native-producer.v1\0");
    identity.update(super::PYTHON_NATIVE_PROJECT_SOURCE_REVISION.as_bytes());
    identity.update(
        blake3::hash(include_bytes!(
            "../../../../vendor/pyrefly_native_1_2/Cargo.toml"
        ))
        .as_bytes(),
    );
    identity.update(
        blake3::hash(include_bytes!(
            "../../../../vendor/pyrefly_native_1_2/NUDOX-UPSTREAM.json"
        ))
        .as_bytes(),
    );
    identity.update(b"root-isolated;fresh-state;classdef-declaration+constructor-callee;captured-candidates;depth64;work262144\0");
    identity.update(digest.as_bytes());
    identity.update(&size.to_be_bytes());
    host.validate_current()?;
    Ok((
        NativePythonProducerIdentity(*identity.finalize().as_bytes()),
        host,
    ))
}

/// Exact borrowed source member of an admitted Python package frontier.
#[derive(Clone, Copy, Debug)]
pub struct PythonProjectSource<'source> {
    /// Normalized package-relative path, retained as the original module identity.
    pub relative_path: &'source str,
    /// Exact selected UTF-8 source bytes.
    pub source: &'source str,
}

/// Cancellation and deadline shared by the entire project transaction.
#[derive(Clone, Copy, Debug)]
pub struct PythonProjectControl<'control> {
    /// Caller-owned cancellation flag.
    pub cancelled: &'control AtomicBool,
    /// Absolute deadline for snapshotting, checking, and decoding together.
    pub deadline: Instant,
}

/// Finite native type decoding refusal; never substituted with `Any`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PythonTypeProjectionFault {
    /// Nested constructors exceed the retained output depth contract.
    #[error("type depth {observed} exceeds {limit}")]
    Depth {
        /// Depth of the rejected borrowed constructor.
        observed: usize,
        /// Maximum admitted structural output depth.
        limit: usize,
    },
    /// The shared transaction projection work allowance is exhausted.
    #[error("type projection work exceeds {limit} nodes")]
    Work {
        /// Maximum scheduled native nodes shared by the transaction.
        limit: usize,
    },
    /// A borrowed native type refers to an active ancestor.
    #[error("native type graph contains a cycle")]
    Cycle,
}

/// Exact selected declaration reached by a native Pyrefly reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionTarget {
    /// Original package-relative target module path.
    pub relative_path: Box<str>,
    /// Explicit package lineage selected by the application owner.
    pub package: Box<str>,
    /// Ruff-confirmed identifier span inside the target's digest-validated bytes.
    pub name_span: Span,
    /// Native resolved module plus the exact declaration's lexical scope.
    pub qualified_name: Box<str>,
    /// Native selected-source manifest, digest and declaration extent, encoded by shared IR admission.
    pub source_coordinate: Box<str>,
    /// Identifier spelling confirmed against the target's exact source slice.
    pub name: Box<str>,
    /// Exact syntax declaration kind of the validated target.
    pub kind: DeclarationKind,
    /// Whether the target is in the module carrying this report.
    pub same_module: bool,
}

/// Reports from one completed project transaction, keyed by original module path.
#[derive(Debug)]
pub struct PythonProjectReport {
    modules: BTreeMap<Box<str>, CheckerReport>,
    witness: std::sync::Arc<PythonProjectWitness>,
    diagnostics: Box<[PythonProjectDiagnostic]>,
    coverage_gaps: Box<[PythonProjectCoverageGap]>,
}

/// A dependency operation that the finite native mirror does not certify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PythonProjectCoverageGapKind {
    /// A runtime import primitive may choose modules beyond syntactic imports.
    DynamicImport,
    /// A runtime enumeration primitive may inspect installed module membership.
    ModuleEnumeration,
    /// A wildcard import depends on the resolved module's exported names.
    WildcardImport,
    /// The native solver could not resolve this import binding.
    UnavailableImport,
    /// A compiled-extension candidate exists, but has no captured Python authority.
    UnavailableCompiledImport,
}

/// Typed partial dependency coverage with its original captured source range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonProjectCoverageGap {
    /// Original package-relative source module.
    pub relative_path: Box<str>,
    /// UTF-8 range of the dependency operation.
    pub span: Span,
    /// Exact unsupported or unavailable dependency operation family.
    pub kind: PythonProjectCoverageGapKind,
}

/// An exact selected-source native diagnostic, retained independently of type facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonProjectDiagnostic {
    /// Original package-relative module.
    pub relative_path: Box<str>,
    /// Native UTF-8 byte range inside the captured source.
    pub span: Span,
    /// Native diagnostic family, including `missing-import` and `untyped-import`.
    pub kind: Box<str>,
    /// Native severity label.
    pub severity: Box<str>,
    /// Exact native diagnostic description.
    pub message: Box<str>,
}

impl PythonProjectReport {
    /// Partial dependency coverage remains explicit even when native types exist.
    #[must_use]
    pub fn coverage_gaps(&self) -> &[PythonProjectCoverageGap] {
        &self.coverage_gaps
    }
    /// Retains native selected-source diagnostics, including unavailable imports.
    #[must_use]
    pub fn diagnostics(&self) -> &[PythonProjectDiagnostic] {
        &self.diagnostics
    }

    /// Borrows the report bound to one exact source member.
    #[must_use]
    pub fn module(&self, relative_path: &str) -> Option<&CheckerReport> {
        self.modules.get(relative_path)
    }

    /// Positive and negative source/configuration/import probes plus native producer bytes.
    #[must_use]
    pub fn witness(&self) -> &std::sync::Arc<PythonProjectWitness> {
        &self.witness
    }
}

/// Captured selected source, configuration, candidates, and native producer identities.
/// This deliberately does not certify a complete compiler dependency read-set.
#[derive(Debug)]
pub struct PythonProjectWitness {
    files: Vec<FileWitness>,
    fingerprint: PythonProjectFingerprint,
    candidates: Vec<CandidateWitness>,
    frontier: Vec<SourceDirectoryWitness>,
}

/// Exact host-local transaction identity for source/configuration/producer facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PythonProjectFingerprint([u8; 32]);

impl PythonProjectFingerprint {
    /// The domain-separated captured project digest; no completeness promotion.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug)]
struct FileWitness {
    path: PathBuf,
    digest: Option<(blake3::Hash, u64)>,
}

impl FileWitness {
    fn capture(path: PathBuf) -> Result<Self, CheckerError> {
        Self::capture_controlled(path, None)
    }

    fn capture_controlled(
        path: PathBuf,
        control: Option<PythonProjectControl<'_>>,
    ) -> Result<Self, CheckerError> {
        if let Some(control) = control {
            checkpoint(control)?;
        }
        let digest = match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() || metadata.file_type().is_symlink() => {
                let target = std::fs::metadata(&path).map_err(workspace_error)?;
                if !target.is_file() {
                    return Err(project_error(
                        &path.to_string_lossy(),
                        "witness target is not a regular file",
                    ));
                }
                let mut file = std::fs::File::open(&path).map_err(workspace_error)?;
                let mut hasher = blake3::Hasher::new();
                let mut bytes = [0u8; 65536];
                let mut size = 0u64;
                loop {
                    if let Some(control) = control {
                        checkpoint(control)?;
                    }
                    let count =
                        std::io::Read::read(&mut file, &mut bytes).map_err(workspace_error)?;
                    if count == 0 {
                        break;
                    }
                    hasher.update(&bytes[..count]);
                    size = size
                        .checked_add(count as u64)
                        .ok_or_else(|| project_error("", "witness extent overflow"))?;
                }
                Some((hasher.finalize(), size))
            }
            Ok(_) => {
                return Err(project_error(
                    &path.to_string_lossy(),
                    "witness is not a file",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(workspace_error(error)),
        };
        Ok(Self { path, digest })
    }

    fn validate_current(&self) -> Result<(), CheckerError> {
        self.validate_controlled(None)
    }

    fn validate_controlled(
        &self,
        control: Option<PythonProjectControl<'_>>,
    ) -> Result<(), CheckerError> {
        let current = Self::capture_controlled(self.path.clone(), control)?;
        if self.digest != current.digest {
            return Err(project_error(
                &self.path.to_string_lossy(),
                "admitted source, configuration, or executable changed",
            ));
        }
        Ok(())
    }
}

/// Candidate membership is captured independently of any solver filesystem read.
#[derive(Debug)]
pub(super) struct CandidateWitness {
    path: PathBuf,
    kind: Option<bool>,
}

impl CandidateWitness {
    pub(super) fn capture(path: PathBuf) -> Result<Self, CheckerError> {
        let kind = match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => Some(false),
            Ok(metadata) if metadata.is_dir() => Some(true),
            Ok(_) => return Err(CheckerError::UncapturedDependency { path }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => return Err(workspace_error(source)),
        };
        Ok(Self { path, kind })
    }

    pub(super) fn is_present(&self) -> bool {
        self.kind.is_some()
    }

    fn validate_current(&self) -> Result<(), CheckerError> {
        if Self::capture(self.path.clone())?.kind != self.kind {
            return Err(project_error(
                &self.path.to_string_lossy(),
                "configured internal import candidate membership changed",
            ));
        }
        Ok(())
    }

    fn fingerprint(&self, identity: &mut blake3::Hasher) {
        hash_field(identity, self.path.as_os_str().as_encoded_bytes());
        identity.update(&[match self.kind {
            None => 0,
            Some(false) => 1,
            Some(true) => 2,
        }]);
    }
}

#[derive(Debug)]
pub(super) struct DirectoryWitness {
    path: PathBuf,
    children: Vec<(std::ffi::OsString, bool)>,
}

impl DirectoryWitness {
    fn capture(path: &Path) -> Result<Self, CheckerError> {
        let mut children = Vec::new();
        for entry in std::fs::read_dir(path).map_err(workspace_error)? {
            let entry = entry.map_err(workspace_error)?;
            let kind = entry.file_type().map_err(workspace_error)?;
            if !kind.is_file() && !kind.is_dir() {
                return Err(project_error(
                    "",
                    "private source tree contains a symlink or special file",
                ));
            }
            children.push((entry.file_name(), kind.is_dir()));
        }
        children.sort();
        Ok(Self {
            path: path.to_path_buf(),
            children,
        })
    }

    pub(super) fn capture_tree(
        root: &Path,
        control: PythonProjectControl<'_>,
    ) -> Result<Vec<Self>, CheckerError> {
        let mut pending = vec![root.to_path_buf()];
        let mut result = Vec::new();
        while let Some(path) = pending.pop() {
            checkpoint(control)?;
            let directory = Self::capture(&path)?;
            pending.extend(
                directory
                    .children
                    .iter()
                    .filter(|(_, directory)| *directory)
                    .map(|(name, _)| path.join(name)),
            );
            result.push(directory);
        }
        Ok(result)
    }

    fn validate_current(&self) -> Result<(), CheckerError> {
        if Self::capture(&self.path)?.children != self.children {
            return Err(project_error(
                "",
                "private source/configuration/dependency directory entries changed",
            ));
        }
        Ok(())
    }
}

/// Source-capture exclusions, matching the Python producer example and the
/// discovery owner's generated/cache directory policy. An explicitly configured
/// root is inspected even when its own name is excluded; exclusions affect its
/// descendants only.
#[must_use]
pub fn is_ignored_python_source_directory(name: &std::ffi::OsStr) -> bool {
    backend_discovery::is_hard_ignored_directory(name) || name == ".local"
}

/// Exact original directory membership for the configured finite module roots.
/// Non-source files are not read; namespace directories and Python candidates
/// are included so both omissions and later newly-present modules are detected.
#[derive(Debug)]
pub(super) struct SourceDirectoryWitness {
    path: PathBuf,
    children: Option<Vec<(std::ffi::OsString, bool)>>,
}

impl SourceDirectoryWitness {
    fn capture(path: PathBuf, control: PythonProjectControl<'_>) -> Result<Self, CheckerError> {
        checkpoint(control)?;
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.is_dir() => {
                return Err(CheckerError::UncapturedDependency { path });
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    path,
                    children: None,
                });
            }
            Err(error) => return Err(workspace_error(error)),
        }
        let directory = match std::fs::read_dir(&path) {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self {
                    path,
                    children: None,
                });
            }
            Err(error) => return Err(workspace_error(error)),
        };
        let mut children = Vec::new();
        for entry in directory {
            checkpoint(control)?;
            let entry = entry.map_err(workspace_error)?;
            let name = entry.file_name();
            if is_ignored_python_source_directory(&name) {
                continue;
            }
            let kind = entry.file_type().map_err(workspace_error)?;
            let python = entry
                .path()
                .extension()
                .is_some_and(|ext| ext == "py" || ext == "pyi");
            if kind.is_symlink() {
                if python || std::fs::metadata(entry.path()).is_ok_and(|target| target.is_dir()) {
                    return Err(CheckerError::UncapturedDependency { path: entry.path() });
                }
                continue;
            }
            if python && !kind.is_file() {
                return Err(CheckerError::UncapturedDependency { path: entry.path() });
            }
            if kind.is_dir() || python {
                children.push((name, kind.is_dir()));
            }
        }
        children.sort();
        Ok(Self {
            path,
            children: Some(children),
        })
    }

    pub(super) fn capture_frontier(
        roots: impl IntoIterator<Item = PathBuf>,
        original: &Path,
        mirror: &Path,
        source_path: &str,
        control: PythonProjectControl<'_>,
    ) -> Result<Vec<Self>, CheckerError> {
        let mut pending = roots.into_iter().collect::<Vec<_>>();
        let mut captured = BTreeMap::new();
        while let Some(path) = pending.pop() {
            checkpoint(control)?;
            if captured.contains_key(&path) {
                continue;
            }
            let witness = Self::capture(path.clone(), control)?;
            if let Some(children) = &witness.children {
                let relative = path
                    .strip_prefix(original)
                    .map_err(|_| CheckerError::UncapturedDependency { path: path.clone() })?;
                std::fs::create_dir_all(mirror.join(relative)).map_err(workspace_error)?;
                for (name, directory) in children {
                    checkpoint(control)?;
                    let child = path.join(name);
                    if *directory {
                        pending.push(child);
                    } else if !mirror.join(relative).join(name).is_file() {
                        return Err(CheckerError::IncompleteSourceFrontier {
                            source_path: source_path.into(),
                            module: child
                                .strip_prefix(original)
                                .expect("captured root")
                                .to_string_lossy()
                                .into_owned()
                                .into_boxed_str(),
                            candidate: child,
                        });
                    }
                }
            }
            captured.insert(path, witness);
        }
        Ok(captured.into_values().collect())
    }

    pub(super) fn validate_current(
        &self,
        control: PythonProjectControl<'_>,
    ) -> Result<(), CheckerError> {
        if Self::capture(self.path.clone(), control)?.children != self.children {
            return Err(project_error(
                &self.path.to_string_lossy(),
                "configured Python source directory membership changed",
            ));
        }
        Ok(())
    }

    fn fingerprint(&self, identity: &mut blake3::Hasher) {
        hash_field(identity, self.path.as_os_str().as_encoded_bytes());
        match &self.children {
            None => {
                identity.update(&[0]);
            }
            Some(children) => {
                identity.update(&[1]);
                identity.update(&(children.len() as u64).to_be_bytes());
                for (name, directory) in children {
                    hash_field(identity, name.as_encoded_bytes());
                    identity.update(&[u8::from(*directory)]);
                }
            }
        }
    }
}

impl PythonProjectWitness {
    /// Binds exact source/configuration probes, effective scope, and producer bytes.
    #[must_use]
    pub const fn fingerprint(&self) -> PythonProjectFingerprint {
        self.fingerprint
    }

    /// Revalidates both file bytes and absence before semantic image admission.
    ///
    /// # Errors
    /// Refuses any changed selected source, configuration probe, or executable.
    pub fn validate_current(&self, control: PythonProjectControl<'_>) -> Result<(), CheckerError> {
        checkpoint(control)?;
        for directory in &self.frontier {
            directory.validate_current(control)?;
        }
        for candidate in &self.candidates {
            checkpoint(control)?;
            candidate.validate_current()?;
        }
        for file in &self.files {
            checkpoint(control)?;
            file.validate_controlled(Some(control))?;
        }
        checkpoint(control)
    }
}

impl NativePythonProjectAuthority {
    /// Checks every selected module in one fresh native State transaction.
    ///
    /// Selected sources and configuration files are captured in a private mirror
    /// before the native transaction starts. Its original relative paths are retained; its
    /// digest-validated native declaration spans join only to the supplied bytes.
    /// Filesystem dependencies/configuration outside that mirror are refused;
    /// bundled stubs are immutable native producer data. No cross-call reuse or
    /// complete public binding claim follows from this bounded source contract.
    ///
    /// # Errors
    /// Returns exact snapshot, source-digest, schema, process, cancellation, or
    /// deadline failure. An incomplete report never becomes a syntax-only success.
    pub fn analyze_project(
        &self,
        package_root: &Path,
        package_name: &str,
        sources: &[PythonProjectSource<'_>],
        profile: PythonVersion,
        control: PythonProjectControl<'_>,
    ) -> Result<PythonProjectReport, CheckerError> {
        let control = PythonProjectControl {
            deadline: control.deadline.min(
                Instant::now()
                    .checked_add(self.timeout)
                    .unwrap_or(control.deadline),
            ),
            ..control
        };
        checkpoint(control)?;
        if !package_root.is_absolute() || !package_root.is_dir() {
            return Err(CheckerError::PackageRoot {
                path: package_root.to_path_buf(),
            });
        }
        let workspace = Workspace::create()?;
        let mirror = workspace.path.join("project");
        std::fs::create_dir_all(&mirror).map_err(workspace_error)?;
        let mut paths = BTreeSet::new();
        let mut directories = BTreeSet::from([PathBuf::new()]);
        let mut facts = BTreeMap::new();
        let (current_producer, host_witness) = native_producer_capture()?;
        if current_producer != self.producer {
            return Err(CheckerError::NativeProducerIdentity {
                expected: self.producer.as_bytes(),
                observed: current_producer.as_bytes(),
            });
        }
        let mut witness = PythonProjectWitness {
            files: vec![host_witness],
            fingerprint: PythonProjectFingerprint([0; 32]),
            candidates: Vec::new(),
            frontier: Vec::new(),
        };
        let mut mirror_witness = Vec::new();
        for source in sources {
            checkpoint(control)?;
            let path = Path::new(source.relative_path);
            if source.relative_path.is_empty()
                || source.relative_path.contains('\\')
                || path.is_absolute()
                || !path
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
                || path.components().collect::<PathBuf>().as_os_str() != path.as_os_str()
                || !paths.insert(source.relative_path)
            {
                return Err(project_error(
                    source.relative_path,
                    "invalid or duplicate source path",
                ));
            }
            let syntax = extract(source.source.as_bytes(), profile).map_err(|source_error| {
                CheckerError::ProjectSyntax {
                    path: PathBuf::from(source.relative_path),
                    source: Box::new(source_error),
                }
            })?;
            let target = mirror.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(workspace_error)?;
            }
            std::fs::write(&target, source.source).map_err(workspace_error)?;
            mirror_witness.push(FileWitness::capture(target)?);
            let original = FileWitness::capture(package_root.join(path))?;
            if let Some((digest, size)) = original.digest
                && (digest != blake3::hash(source.source.as_bytes())
                    || size != source.source.len() as u64)
            {
                return Err(project_error(
                    source.relative_path,
                    "selected source differs from original file",
                ));
            }
            witness.files.push(original);
            facts.insert(source.relative_path, syntax);
            let mut parent = path.parent();
            while let Some(directory) = parent {
                directories.insert(directory.to_path_buf());
                parent = directory.parent();
            }
        }
        if paths.is_empty() {
            return Err(project_error("", "empty source frontier"));
        }
        // Capture both present and absent configuration probes before invoking
        // Pyrefly. No upward config search can reach the caller's live ancestors.
        for directory in directories {
            for name in [
                "pyrefly.toml",
                ".pyrefly.toml",
                "pyproject.toml",
                "mypy.ini",
                "pyrightconfig.json",
                "setup.cfg",
            ] {
                checkpoint(control)?;
                let relative = directory.join(name);
                let original = package_root.join(&relative);
                let original_witness = match std::fs::symlink_metadata(&original) {
                    Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                        if metadata.len() > CONFIG_BYTES {
                            return Err(project_error(
                                &relative.to_string_lossy(),
                                "configuration exceeds capture bound",
                            ));
                        }
                        let bytes = std::fs::read(&original).map_err(workspace_error)?;
                        if bytes.len() as u64 > CONFIG_BYTES {
                            return Err(project_error(
                                &relative.to_string_lossy(),
                                "configuration changed beyond capture bound",
                            ));
                        }
                        let digest = Some((blake3::hash(&bytes), bytes.len() as u64));
                        let target = mirror.join(&relative);
                        if let Some(parent) = target.parent() {
                            std::fs::create_dir_all(parent).map_err(workspace_error)?;
                        }
                        std::fs::write(&target, &bytes).map_err(workspace_error)?;
                        mirror_witness.push(FileWitness {
                            path: target,
                            digest,
                        });
                        FileWitness {
                            path: original,
                            digest,
                        }
                    }
                    Ok(_) => {
                        return Err(project_error(
                            &relative.to_string_lossy(),
                            "configuration is not a regular captured file",
                        ));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => FileWitness {
                        path: original,
                        digest: None,
                    },
                    Err(error) => return Err(workspace_error(error)),
                };
                original_witness.validate_current()?;
                witness.files.push(original_witness);
            }
        }
        if !mirror.join("pyrefly.toml").is_file()
            && !mirror.join(".pyrefly.toml").is_file()
            && !mirror.join("pyproject.toml").is_file()
        {
            std::fs::write(
                mirror.join("pyrefly.toml"),
                r#"project-includes = ["**/*.py", "**/*.pyi"]
"#,
            )
            .map_err(workspace_error)?;
            mirror_witness.push(FileWitness::capture(mirror.join("pyrefly.toml"))?);
        }
        let native = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::project_native::analyze(
                &mirror,
                package_root,
                package_name,
                sources,
                &facts,
                profile,
                control,
            )
        }))
        .map_err(|_| CheckerError::ProjectPanic)??;
        let mut identity = blake3::Hasher::new();
        identity.update(b"compiler.python.captured-project.v1\0");
        identity.update(&self.producer.as_bytes());
        identity.update(&native.configuration_fingerprint);
        hash_field(&mut identity, package_name.as_bytes());
        hash_field(&mut identity, super::profile_tag(profile).as_bytes());
        let mut selected = sources.iter().collect::<Vec<_>>();
        selected.sort_by_key(|source| source.relative_path);
        for source in selected {
            identity.update(b"selected-source\0");
            hash_field(&mut identity, source.relative_path.as_bytes());
            hash_field(&mut identity, source.source.as_bytes());
        }
        let mut probes = witness.files.iter().collect::<Vec<_>>();
        probes.sort_by_key(|file| &file.path);
        for file in probes {
            identity.update(b"original-file-probe\0");
            hash_field(&mut identity, file.path.as_os_str().as_encoded_bytes());
            match file.digest {
                Some((digest, size)) => {
                    identity.update(&[1]);
                    identity.update(digest.as_bytes());
                    identity.update(&size.to_be_bytes());
                }
                None => {
                    identity.update(&[0]);
                }
            }
        }
        witness.candidates = native.candidates;
        for candidate in &witness.candidates {
            identity.update(b"internal-candidate-membership\0");
            candidate.fingerprint(&mut identity);
        }
        witness.frontier = native.frontier;
        for directory in &witness.frontier {
            identity.update(b"configured-source-directory-membership\0");
            directory.fingerprint(&mut identity);
        }
        witness.fingerprint = PythonProjectFingerprint(*identity.finalize().as_bytes());
        witness.validate_current(control)?;
        for directory in native.mirror_tree {
            checkpoint(control)?;
            directory.validate_current()?;
        }
        for file in mirror_witness {
            checkpoint(control)?;
            file.validate_current()?;
        }
        checkpoint(control)?;
        Ok(PythonProjectReport {
            modules: native.modules,
            diagnostics: native.diagnostics.into_boxed_slice(),
            coverage_gaps: native.coverage_gaps.into_boxed_slice(),
            witness: std::sync::Arc::new(witness),
        })
    }
}

fn workspace_error(source: std::io::Error) -> CheckerError {
    CheckerError::Workspace { source }
}

pub(super) fn project_error(path: &str, message: &str) -> CheckerError {
    CheckerError::ProjectReport {
        path: PathBuf::from(path),
        message: message.to_owned(),
    }
}

pub(super) fn checkpoint(control: PythonProjectControl<'_>) -> Result<(), CheckerError> {
    if control.cancelled.load(Ordering::Acquire) {
        return Err(CheckerError::Cancelled { phase: "project" });
    }
    if Instant::now() >= control.deadline {
        return Err(CheckerError::Deadline { phase: "project" });
    }
    Ok(())
}

fn hash_field(identity: &mut blake3::Hasher, bytes: &[u8]) {
    identity.update(&(bytes.len() as u64).to_be_bytes());
    identity.update(bytes);
}
