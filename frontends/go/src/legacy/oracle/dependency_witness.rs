//! Request-scoped identity of the dependency files selected by Go itself.

use std::{
    collections::BTreeSet,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{GoOracle, GoOracleChildEnvironment, GoWorkWitness, update_path_digest};

const FILE_COUNT_LIMIT: usize = 32 * 1024;
const FILE_BYTES_LIMIT: u64 = 16 * 1024 * 1024;
const TOTAL_BYTES_LIMIT: u64 = 128 * 1024 * 1024;
const MODULE_REPLACEMENT_LIMIT: usize = 8;

/// Closed reason that the offline Go loader could not select a complete graph.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GoDependencyClosureFailure {
    /// The bounded `go list` invocation did not complete successfully.
    Invocation,
    /// The selected Go tool emitted an invalid listing protocol.
    Protocol,
    /// Go rejected at least one package or its transitive dependency.
    PackageLoad,
    /// No package was selected, so there is no semantic authority to admit.
    EmptyGraph,
    /// At least one selected file could not be captured stably.
    IncompleteFiles,
    /// A selected path escaped the admitted module, cache, or local roots.
    UnsafePath,
    /// The selected file closure exceeded an existing witness bound.
    Limit,
}

impl GoDependencyClosureFailure {
    pub(super) const fn recovery(self) -> &'static str {
        match self {
            Self::Invocation => {
                "verify the selected Go toolchain and its invocation diagnostics, then retry"
            }
            Self::Protocol => {
                "verify that the selected Go version emits the supported package listing, then retry"
            }
            Self::PackageLoad => {
                "run `go mod download` in the project, correct any module or replacement errors, then retry"
            }
            Self::EmptyGraph => "select a project root containing Go packages, then retry",
            Self::IncompleteFiles => {
                "ensure selected dependency files are readable and stable, then retry"
            }
            Self::UnsafePath => {
                "keep selected dependency files within the admitted module, cache, or local roots, then retry"
            }
            Self::Limit => {
                "reduce the selected package or file closure to the existing witness bounds, then retry"
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct GoDependencyClosureWitness {
    environment: GoOracleChildEnvironment,
    oracle: GoOracle,
    graph: [u8; 32],
    files: Box<[DependencyFile]>,
    complete: bool,
    failure: Option<GoDependencyClosureFailure>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DependencyFile {
    path: PathBuf,
    state: DependencyFileState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DependencyFileState {
    File { bytes: u64, digest: [u8; 32] },
    Unavailable,
    Limit,
}

// Go owns this protocol. Unknown Go fields do not become semantic facts; the
// exact bounded transcript is also hashed so they cannot authorize stale reuse.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ListedPackage {
    #[serde(default)]
    dir: Option<PathBuf>,
    #[serde(default)]
    go_files: Vec<PathBuf>,
    #[serde(default)]
    compiled_go_files: Vec<PathBuf>,
    #[serde(default)]
    embed_files: Vec<PathBuf>,
    #[serde(default)]
    module: Option<ListedModule>,
    #[serde(default)]
    error: Option<serde_json::Value>,
    #[serde(default)]
    deps_errors: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ListedModule {
    #[serde(default)]
    dir: Option<PathBuf>,
    #[serde(default)]
    go_mod: Option<PathBuf>,
    #[serde(default)]
    replace: Option<Box<ListedModule>>,
}

impl GoDependencyClosureWitness {
    pub(super) fn capture(
        root: &Path,
        work: &GoWorkWitness,
        environment: &GoOracleChildEnvironment,
        oracle: GoOracle,
        roots: &[PathBuf],
        cancelled: Option<&AtomicBool>,
    ) -> Self {
        let mut command = std::process::Command::new(environment.go_executable());
        command.current_dir(root).args([
            "list",
            "-e",
            "-deps",
            "-test",
            "-json=ImportPath,Dir,GoFiles,CompiledGoFiles,EmbedFiles,Module,Error,DepsErrors",
            "./...",
        ]);
        environment.apply_to(&mut command, work, false);
        let output = match environment
            .revalidate_toolchain()
            .and_then(|()| oracle.execute_with_origin(&mut command, true, cancelled))
        {
            Ok(output) => output,
            Err(error) => {
                return Self {
                    environment: environment.clone(),
                    oracle,
                    graph: Sha256::digest(error.to_string().as_bytes()).into(),
                    files: Box::new([]),
                    complete: false,
                    failure: Some(GoDependencyClosureFailure::Invocation),
                };
            }
        };
        let graph = Sha256::digest(&output).into();
        let mut paths = BTreeSet::new();
        let mut failure = None;
        let mut count = 0usize;
        for package in serde_json::Deserializer::from_slice(&output).into_iter::<ListedPackage>() {
            let package = match package {
                Ok(package) => package,
                Err(_) => {
                    failure = Some(GoDependencyClosureFailure::Protocol);
                    break;
                }
            };
            count += 1;
            if count > FILE_COUNT_LIMIT || is_cancelled(cancelled) {
                failure = Some(GoDependencyClosureFailure::Limit);
                break;
            }
            if package.error.is_some() || !package.deps_errors.is_empty() {
                failure.get_or_insert(GoDependencyClosureFailure::PackageLoad);
            }
            if let Some(module) = package.module {
                let mut module = Some(module);
                let mut depth = 0usize;
                while let Some(current) = module {
                    depth += 1;
                    if depth > MODULE_REPLACEMENT_LIMIT {
                        failure = Some(GoDependencyClosureFailure::Protocol);
                        break;
                    }
                    if current
                        .dir
                        .as_ref()
                        .is_some_and(|path| !admitted_path(path, roots))
                    {
                        failure.get_or_insert(GoDependencyClosureFailure::UnsafePath);
                        break;
                    }
                    if let Some(path) = current.go_mod {
                        if !admitted_path(&path, roots) {
                            failure.get_or_insert(GoDependencyClosureFailure::UnsafePath);
                            break;
                        }
                        paths.insert(path);
                    }
                    module = current.replace.map(|module| *module);
                }
            }
            let Some(directory) = package.dir else {
                continue;
            };
            if !admitted_path(&directory, roots) {
                failure.get_or_insert(GoDependencyClosureFailure::UnsafePath);
                break;
            }
            // GOROOT is already admitted by complete content identity, and the
            // package's own source set is bound by its ordinary input witness.
            if directory.starts_with(environment.goroot()) {
                continue;
            }
            let go_files = if directory.starts_with(root) {
                Vec::new()
            } else {
                package
                    .go_files
                    .into_iter()
                    .chain(package.compiled_go_files)
                    .collect()
            };
            for path in go_files.into_iter().chain(package.embed_files) {
                if !clean_components(&path) {
                    failure.get_or_insert(GoDependencyClosureFailure::UnsafePath);
                    break;
                }
                let path = if path.is_absolute() {
                    path
                } else {
                    directory.join(path)
                };
                // A file selected for this package must stay in its admitted
                // directory, including after symlink resolution.
                if !admitted_path(&path, std::slice::from_ref(&directory)) {
                    failure.get_or_insert(GoDependencyClosureFailure::UnsafePath);
                    break;
                }
                paths.insert(path);
            }
            if paths.len() > FILE_COUNT_LIMIT {
                failure = Some(GoDependencyClosureFailure::Limit);
                break;
            }
        }
        if count == 0 && failure.is_none() {
            failure = Some(GoDependencyClosureFailure::EmptyGraph);
        }
        let mut total = 0u64;
        let mut files = Vec::new();
        for path in paths.into_iter().take(FILE_COUNT_LIMIT) {
            let state = capture_file(&path, roots, &mut total, cancelled);
            if failure.is_none() {
                failure = match state {
                    DependencyFileState::File { .. } => None,
                    DependencyFileState::Unavailable => {
                        Some(GoDependencyClosureFailure::IncompleteFiles)
                    }
                    DependencyFileState::Limit => Some(GoDependencyClosureFailure::Limit),
                };
            }
            files.push(DependencyFile { path, state });
        }
        let complete = failure.is_none();
        Self {
            environment: environment.clone(),
            oracle,
            graph,
            files: files.into_boxed_slice(),
            complete,
            failure,
        }
    }

    pub(super) fn environment(&self) -> &GoOracleChildEnvironment {
        &self.environment
    }

    pub(super) fn recapture(
        &self,
        root: &Path,
        work: &GoWorkWitness,
        roots: &[PathBuf],
        cancelled: Option<&AtomicBool>,
    ) -> Self {
        Self::capture(root, work, &self.environment, self.oracle, roots, cancelled)
    }

    pub(super) const fn is_complete(&self) -> bool {
        self.complete
    }

    pub(super) const fn failure(&self) -> Option<GoDependencyClosureFailure> {
        self.failure
    }

    pub(super) fn update_identity(&self, digest: &mut Sha256) {
        digest.update(b"compiler.go.selected-dependency-files.v2\0");
        update_path_digest(digest, self.environment.module_cache());
        digest.update(self.graph);
        digest.update([u8::from(self.complete)]);
        digest.update([match self.failure {
            None => 0,
            Some(GoDependencyClosureFailure::Invocation) => 1,
            Some(GoDependencyClosureFailure::Protocol) => 2,
            Some(GoDependencyClosureFailure::PackageLoad) => 3,
            Some(GoDependencyClosureFailure::EmptyGraph) => 4,
            Some(GoDependencyClosureFailure::IncompleteFiles) => 5,
            Some(GoDependencyClosureFailure::UnsafePath) => 6,
            Some(GoDependencyClosureFailure::Limit) => 7,
        }]);
        for file in &self.files {
            update_path_digest(digest, &file.path);
            match file.state {
                DependencyFileState::File {
                    bytes,
                    digest: content,
                } => {
                    digest.update([0]);
                    digest.update(bytes.to_be_bytes());
                    digest.update(content);
                }
                DependencyFileState::Unavailable => digest.update([1]),
                DependencyFileState::Limit => digest.update([2]),
            }
        }
    }
}

fn is_cancelled(cancelled: Option<&AtomicBool>) -> bool {
    cancelled.is_some_and(|token| token.load(Ordering::Acquire))
}

fn clean_components(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|part| !matches!(part, Component::ParentDir | Component::CurDir))
}

fn admitted_path(path: &Path, roots: &[PathBuf]) -> bool {
    if !path.is_absolute()
        || !clean_components(path)
        || !roots.iter().any(|root| path.starts_with(root))
    {
        return false;
    }
    path.canonicalize()
        .is_ok_and(|resolved| roots.iter().any(|root| resolved.starts_with(root)))
}

#[derive(PartialEq)]
struct FileStamp {
    identity: backend_platform::FileIdentity,
    bytes: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    changed: (i64, i64),
}

fn file_stamp(file: &std::fs::File) -> std::io::Result<FileStamp> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::other("dependency is not a regular file"));
    }
    Ok(FileStamp {
        identity: backend_platform::FileIdentity::of_file(file)?,
        bytes: metadata.len(),
        modified: metadata.modified()?,
        #[cfg(unix)]
        changed: {
            use std::os::unix::fs::MetadataExt as _;
            (metadata.ctime(), metadata.ctime_nsec())
        },
    })
}

fn capture_file(
    path: &Path,
    roots: &[PathBuf],
    total: &mut u64,
    cancelled: Option<&AtomicBool>,
) -> DependencyFileState {
    if is_cancelled(cancelled) || !admitted_path(path, roots) {
        return DependencyFileState::Unavailable;
    }
    let Ok(resolved) = path.canonicalize() else {
        return DependencyFileState::Unavailable;
    };
    let Ok(mut file) = backend_platform::durability::open_regular_file_nofollow(&resolved) else {
        return DependencyFileState::Unavailable;
    };
    let Ok(before) = file_stamp(&file) else {
        return DependencyFileState::Unavailable;
    };
    if backend_platform::FileIdentity::of_path_nofollow(&resolved).ok() != Some(before.identity) {
        return DependencyFileState::Unavailable;
    }
    let bytes = before.bytes;
    if bytes > FILE_BYTES_LIMIT || total.saturating_add(bytes) > TOTAL_BYTES_LIMIT {
        return DependencyFileState::Limit;
    }
    let mut digest = Sha256::new();
    let mut read = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if is_cancelled(cancelled) {
            return DependencyFileState::Unavailable;
        }
        let Ok(count) = file.read(&mut buffer) else {
            return DependencyFileState::Unavailable;
        };
        if count == 0 {
            break;
        }
        read = read.saturating_add(count as u64);
        if read > bytes {
            return DependencyFileState::Unavailable;
        }
        digest.update(&buffer[..count]);
    }
    if read != bytes
        || file_stamp(&file).ok().as_ref() != Some(&before)
        || path.canonicalize().ok().as_ref() != Some(&resolved)
        || !admitted_path(path, roots)
        || backend_platform::FileIdentity::of_path_nofollow(&resolved).ok() != Some(before.identity)
    {
        return DependencyFileState::Unavailable;
    }
    *total = total.saturating_add(bytes);
    DependencyFileState::File {
        bytes,
        digest: digest.finalize().into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closure_refusal_recovery_is_specific_to_the_failure() {
        let expected = [
            (
                GoDependencyClosureFailure::Invocation,
                "Invocation",
                "verify the selected Go toolchain and its invocation diagnostics, then retry",
            ),
            (
                GoDependencyClosureFailure::Protocol,
                "Protocol",
                "verify that the selected Go version emits the supported package listing, then retry",
            ),
            (
                GoDependencyClosureFailure::PackageLoad,
                "PackageLoad",
                "run `go mod download` in the project, correct any module or replacement errors, then retry",
            ),
            (
                GoDependencyClosureFailure::EmptyGraph,
                "EmptyGraph",
                "select a project root containing Go packages, then retry",
            ),
            (
                GoDependencyClosureFailure::IncompleteFiles,
                "IncompleteFiles",
                "ensure selected dependency files are readable and stable, then retry",
            ),
            (
                GoDependencyClosureFailure::UnsafePath,
                "UnsafePath",
                "keep selected dependency files within the admitted module, cache, or local roots, then retry",
            ),
            (
                GoDependencyClosureFailure::Limit,
                "Limit",
                "reduce the selected package or file closure to the existing witness bounds, then retry",
            ),
        ];
        for (failure, kind, remedy) in expected {
            let error = super::super::OracleError::DependencyClosureUnavailable { failure };
            assert_eq!(
                error.to_string(),
                format!("Go dependency closure is unavailable ({kind}); {remedy}")
            );
            assert_eq!(
                error.to_string().contains("go mod download"),
                failure == GoDependencyClosureFailure::PackageLoad
            );
        }
    }

    #[test]
    fn same_length_dependency_change_invalidates_content_witness() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("dependency.go");
        std::fs::write(&path, b"package dep\nconst Value = 1\n").unwrap();
        let before = capture_file(&path, &[root.path().canonicalize().unwrap()], &mut 0, None);
        std::fs::write(&path, b"package dep\nconst Value = 2\n").unwrap();
        let after = capture_file(&path, &[root.path().canonicalize().unwrap()], &mut 0, None);
        assert!(matches!(before, DependencyFileState::File { .. }));
        assert_ne!(before, after);
    }

    #[test]
    fn missing_dependency_and_later_setup_have_different_witnesses() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("dependency.go");
        assert_eq!(
            capture_file(&path, &[root.path().canonicalize().unwrap()], &mut 0, None),
            DependencyFileState::Unavailable
        );
        std::fs::write(&path, b"package dep\n").unwrap();
        assert!(matches!(
            capture_file(&path, &[root.path().canonicalize().unwrap()], &mut 0, None),
            DependencyFileState::File { .. }
        ));
    }

    #[test]
    fn oversized_dependency_never_becomes_a_complete_witness() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("large.go");
        std::fs::File::create(&path)
            .unwrap()
            .set_len(FILE_BYTES_LIMIT + 1)
            .unwrap();
        assert_eq!(
            capture_file(&path, &[root.path().canonicalize().unwrap()], &mut 0, None),
            DependencyFileState::Limit
        );
        std::fs::write(&path, b"package dep\n").unwrap();
        let mut total = TOTAL_BYTES_LIMIT;
        assert_eq!(
            capture_file(
                &path,
                &[root.path().canonicalize().unwrap()],
                &mut total,
                None
            ),
            DependencyFileState::Limit
        );
        assert_eq!(total, TOTAL_BYTES_LIMIT);
    }

    #[cfg(unix)]
    #[test]
    fn dependency_directory_is_refused_as_a_source_file() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            capture_file(
                root.path(),
                &[root.path().canonicalize().unwrap()],
                &mut 0,
                None
            ),
            DependencyFileState::Unavailable
        );
    }

    #[test]
    fn dependency_path_cannot_escape_the_admitted_root() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let path = outside.path().join("dependency.go");
        std::fs::write(&path, b"package outside\n").unwrap();
        let roots = [root.path().canonicalize().unwrap()];
        assert!(!admitted_path(&path, &roots));
        assert_eq!(
            capture_file(&path, &roots, &mut 0, None),
            DependencyFileState::Unavailable
        );
        assert!(!clean_components(Path::new("../dependency.go")));
        assert!(!clean_components(Path::new("/admitted/../outside.go")));
    }

    #[cfg(unix)]
    #[test]
    fn dependency_symlink_cannot_escape_the_admitted_root() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let source = outside.path().join("dependency.go");
        std::fs::write(&source, b"package outside\n").unwrap();
        let link = root.path().join("dependency.go");
        std::os::unix::fs::symlink(&source, &link).unwrap();
        assert_eq!(
            capture_file(&link, &[root.path().canonicalize().unwrap()], &mut 0, None),
            DependencyFileState::Unavailable
        );
    }

    #[test]
    fn cancelled_dependency_read_does_not_admit_content() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("dependency.go");
        std::fs::write(&path, b"package dep\n").unwrap();
        assert_eq!(
            capture_file(
                &path,
                &[root.path().canonicalize().unwrap()],
                &mut 0,
                Some(&AtomicBool::new(true))
            ),
            DependencyFileState::Unavailable
        );
    }
}
