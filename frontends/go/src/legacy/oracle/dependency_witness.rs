//! Request-scoped identity of the dependency files selected by Go itself.

use std::{
    collections::BTreeSet,
    io::Read,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{GoOracle, GoOracleChildEnvironment, GoWorkWitness, update_path_digest};

const FILE_COUNT_LIMIT: usize = 32 * 1024;
const FILE_BYTES_LIMIT: u64 = 16 * 1024 * 1024;
const TOTAL_BYTES_LIMIT: u64 = 128 * 1024 * 1024;

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
        cancelled: Option<&std::sync::atomic::AtomicBool>,
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
        let output = match oracle.execute_with_origin(&mut command, true, cancelled) {
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
            if package.error.is_some() || !package.deps_errors.is_empty() {
                failure = Some(GoDependencyClosureFailure::PackageLoad);
            }
            if let Some(module) = package.module {
                let mut module = Some(module);
                while let Some(current) = module {
                    if let Some(path) = current.go_mod {
                        paths.insert(path);
                    }
                    module = current.replace.map(|module| *module);
                }
            }
            let Some(directory) = package.dir else {
                continue;
            };
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
                paths.insert(if path.is_absolute() {
                    path
                } else {
                    directory.join(path)
                });
            }
            if paths.len() > FILE_COUNT_LIMIT {
                break;
            }
        }
        if count == 0 && failure.is_none() {
            failure = Some(GoDependencyClosureFailure::EmptyGraph);
        }
        let mut complete = failure.is_none() && paths.len() <= FILE_COUNT_LIMIT;
        let mut total = 0u64;
        let mut files = Vec::new();
        for path in paths.into_iter().take(FILE_COUNT_LIMIT) {
            let state = capture_file(&path, &mut total);
            complete &= matches!(state, DependencyFileState::File { .. });
            files.push(DependencyFile { path, state });
        }
        Self {
            environment: environment.clone(),
            oracle,
            graph,
            files: files.into_boxed_slice(),
            complete,
            failure,
        }
    }

    pub(super) fn recapture(&self, root: &Path, work: &GoWorkWitness) -> Self {
        Self::capture(root, work, &self.environment, self.oracle, None)
    }

    pub(super) const fn is_complete(&self) -> bool {
        self.complete
    }

    pub(super) const fn failure(&self) -> Option<GoDependencyClosureFailure> {
        self.failure
    }

    pub(super) fn update_identity(&self, digest: &mut Sha256) {
        digest.update(b"compiler.go.selected-dependency-files.v1\0");
        update_path_digest(digest, self.environment.module_cache());
        digest.update(self.graph);
        digest.update([u8::from(self.complete)]);
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

fn capture_file(path: &Path, total: &mut u64) -> DependencyFileState {
    if !path.is_absolute() {
        return DependencyFileState::Unavailable;
    }
    let Ok(resolved) = path.canonicalize() else {
        return DependencyFileState::Unavailable;
    };
    let Ok(mut file) = backend_platform::durability::open_regular_file_nofollow(&resolved) else {
        return DependencyFileState::Unavailable;
    };
    let Ok(metadata) = file.metadata() else {
        return DependencyFileState::Unavailable;
    };
    if !metadata.is_file() {
        return DependencyFileState::Unavailable;
    }
    let bytes = metadata.len();
    if bytes > FILE_BYTES_LIMIT || total.saturating_add(bytes) > TOTAL_BYTES_LIMIT {
        return DependencyFileState::Limit;
    }
    let mut digest = Sha256::new();
    let mut read = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
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
    if read != bytes {
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
    fn same_length_dependency_change_invalidates_content_witness() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("dependency.go");
        std::fs::write(&path, b"package dep\nconst Value = 1\n").unwrap();
        let before = capture_file(&path, &mut 0);
        std::fs::write(&path, b"package dep\nconst Value = 2\n").unwrap();
        let after = capture_file(&path, &mut 0);
        assert!(matches!(before, DependencyFileState::File { .. }));
        assert_ne!(before, after);
    }

    #[test]
    fn missing_dependency_and_later_setup_have_different_witnesses() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("dependency.go");
        assert_eq!(
            capture_file(&path, &mut 0),
            DependencyFileState::Unavailable
        );
        std::fs::write(&path, b"package dep\n").unwrap();
        assert!(matches!(
            capture_file(&path, &mut 0),
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
        assert_eq!(capture_file(&path, &mut 0), DependencyFileState::Limit);
        std::fs::write(&path, b"package dep\n").unwrap();
        let mut total = TOTAL_BYTES_LIMIT;
        assert_eq!(capture_file(&path, &mut total), DependencyFileState::Limit);
        assert_eq!(total, TOTAL_BYTES_LIMIT);
    }

    #[cfg(unix)]
    #[test]
    fn dependency_directory_is_refused_as_a_source_file() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            capture_file(root.path(), &mut 0),
            DependencyFileState::Unavailable
        );
    }
}
