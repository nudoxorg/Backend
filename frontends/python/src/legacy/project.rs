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

use super::{CheckerError, CheckerReport, Pyrefly, Workspace};
use crate::legacy::{DeclarationKind, Span, extract};

const CONFIG_BYTES: u64 = 1024 * 1024;

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
    Depth { observed: usize, limit: usize },
    /// The shared transaction projection work allowance is exhausted.
    #[error("type projection work exceeds {limit} nodes")]
    Work { limit: usize },
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
}

impl PythonProjectReport {
    /// Borrows the report bound to one exact source member.
    #[must_use]
    pub fn module(&self, relative_path: &str) -> Option<&CheckerReport> {
        self.modules.get(relative_path)
    }

    /// Positive and negative source/configuration probes plus executable bytes.
    #[must_use]
    pub fn witness(&self) -> &std::sync::Arc<PythonProjectWitness> {
        &self.witness
    }
}

/// Captured selected source, configuration, and executable file identities.
/// This deliberately does not certify a complete compiler dependency read-set.
#[derive(Debug)]
pub struct PythonProjectWitness {
    files: Vec<FileWitness>,
}

#[derive(Debug)]
struct FileWitness {
    path: PathBuf,
    digest: Option<(blake3::Hash, u64)>,
}

impl FileWitness {
    fn capture(path: PathBuf) -> Result<Self, CheckerError> {
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
        let current = Self::capture(self.path.clone())?;
        if self.digest != current.digest {
            return Err(project_error(
                &self.path.to_string_lossy(),
                "admitted source, configuration, or executable changed",
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
struct DirectoryWitness {
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

    fn capture_tree(root: &Path) -> Result<Vec<Self>, CheckerError> {
        let mut pending = vec![root.to_path_buf()];
        let mut result = Vec::new();
        while let Some(path) = pending.pop() {
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

impl PythonProjectWitness {
    /// Revalidates both file bytes and absence before semantic image admission.
    ///
    /// # Errors
    /// Refuses any changed selected source, configuration probe, or executable.
    pub fn validate_current(&self) -> Result<(), CheckerError> {
        for file in &self.files {
            file.validate_current()?;
        }
        Ok(())
    }
}

impl Pyrefly {
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
        if !self.program.is_absolute() {
            return Err(project_error(
                "",
                "project authority requires an explicit absolute executable",
            ));
        }
        if !self.arguments.is_empty() {
            return Err(project_error(
                "",
                "native authority requires a bare selected Pyrefly executable",
            ));
        }
        let mut witness = PythonProjectWitness {
            files: vec![FileWitness::capture(self.program.clone())?],
        };
        witness.files.push(FileWitness::capture(
            std::env::current_exe().map_err(workspace_error)?,
        )?);
        if witness.files[0].digest.is_none() {
            return Err(project_error("", "project executable is unavailable"));
        }
        let version = self.run_native_version(profile, &mirror, control)?;
        if version.as_slice() != b"pyrefly 1.2.0-dev.1\n" {
            return Err(project_error(
                "",
                "selected executable does not match pinned native Pyrefly 1.2.0-dev.1",
            ));
        }
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
        let private_tree = DirectoryWitness::capture_tree(&mirror)?;
        let modules = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
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
        witness.validate_current()?;
        for directory in private_tree {
            checkpoint(control)?;
            directory.validate_current()?;
        }
        for file in mirror_witness {
            checkpoint(control)?;
            file.validate_current()?;
        }
        checkpoint(control)?;
        Ok(PythonProjectReport {
            modules,
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
