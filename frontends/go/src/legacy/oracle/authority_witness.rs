//! Bounded Go package authority witness for module/workspace-local inputs.

use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};

use super::update_path_digest;

const GO_WORK_BYTES_LIMIT: usize = 1024 * 1024;
const GO_AUTHORITY_MANIFEST_BYTES_LIMIT: usize = 1024 * 1024;
const GO_LOCAL_TREE_ENTRY_LIMIT: usize = 32 * 1024;
const GO_LOCAL_TREE_BYTES_LIMIT: usize = 128 * 1024 * 1024;
const GO_LOCAL_TREE_FILE_BYTES_LIMIT: usize = 16 * 1024 * 1024;
const GO_LOCAL_TREE_DEPTH_LIMIT: usize = 96;
const GO_LOCAL_DIRECTIVE_LIMIT: usize = 4096;

/// Bounded identity of the nearest `go.work` selected from a package root.
/// `Disabled` means no file was found and Go's implicit workspace discovery
/// must stay off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoWorkWitness {
    /// No `go.work` exists in the package root or any ancestor.
    Disabled,
    /// The exact nearest workspace file observed at capture time.
    File(GoWorkFileWitness),
}

/// Path and bounded content identity of one selected `go.work` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoWorkFileWitness {
    path: PathBuf,
    content_digest: Option<[u8; 32]>,
    content_bytes: usize,
    complete: bool,
}

impl GoWorkWitness {
    /// Captures the nearest `go.work` above an absolute package root, or
    /// records that workspace discovery is disabled.
    pub fn capture(module_root: impl AsRef<Path>) -> Result<Self, GoWorkWitnessError> {
        let requested = module_root.as_ref();
        if !requested.is_absolute() {
            return Err(GoWorkWitnessError::RelativeRoot {
                path: requested.to_path_buf(),
            });
        }
        let root = requested
            .canonicalize()
            .map_err(|source| GoWorkWitnessError::Root {
                path: requested.to_path_buf(),
                source,
            })?;
        if !root.is_dir() {
            return Err(GoWorkWitnessError::RootNotDirectory { path: root });
        }
        let mut cursor = Some(root.as_path());
        while let Some(directory) = cursor {
            let candidate = directory.join("go.work");
            match std::fs::symlink_metadata(&candidate) {
                Ok(metadata) => {
                    let resolved =
                        candidate
                            .canonicalize()
                            .map_err(|source| GoWorkWitnessError::File {
                                path: candidate.clone(),
                                source,
                            })?;
                    if !resolved.is_file() {
                        return Err(GoWorkWitnessError::NotFile { path: resolved });
                    }
                    if metadata.file_type().is_dir() {
                        return Err(GoWorkWitnessError::NotFile { path: resolved });
                    }
                    let (content_digest, content_bytes, complete) =
                        match read_bounded_go_work(&resolved) {
                            Ok(bytes) => (Some(Sha256::digest(&bytes).into()), bytes.len(), true),
                            Err(GoWorkWitnessError::TooLarge { limit, .. }) => {
                                (None, limit.saturating_add(1), false)
                            }
                            Err(error) => return Err(error),
                        };
                    return Ok(Self::File(GoWorkFileWitness {
                        path: resolved,
                        content_digest,
                        content_bytes,
                        complete,
                    }));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(GoWorkWitnessError::File {
                        path: candidate,
                        source,
                    });
                }
            }
            cursor = directory.parent();
        }
        Ok(Self::Disabled)
    }

    /// Returns a stable path-and-content identity for local authority drift.
    #[must_use]
    pub fn identity(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"compiler.go.workspace-witness.v1\0");
        match self {
            Self::Disabled => digest.update([0]),
            Self::File(file) => {
                digest.update([1]);
                update_path_digest(&mut digest, &file.path);
                digest.update((file.content_bytes as u64).to_be_bytes());
                digest.update([u8::from(file.complete)]);
                if let Some(content_digest) = file.content_digest {
                    digest.update(content_digest);
                }
            }
        }
        digest.finalize().into()
    }

    /// Returns the admitted workspace file path, if one was present.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Disabled => None,
            Self::File(file) => Some(&file.path),
        }
    }

    /// Returns the SHA-256 digest of the captured file contents, if present.
    #[must_use]
    pub fn content_digest(&self) -> Option<[u8; 32]> {
        match self {
            Self::Disabled => None,
            Self::File(file) => file.content_digest,
        }
    }

    /// Reports whether the selected workspace file was fully captured within
    /// the configured byte bound.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        match self {
            Self::Disabled => true,
            Self::File(file) => file.complete,
        }
    }

    /// Re-captures the nearest workspace and compares its path and content.
    pub fn matches_current(
        &self,
        module_root: impl AsRef<Path>,
    ) -> Result<bool, GoWorkWitnessError> {
        Ok(*self == Self::capture(module_root)?)
    }

    pub(super) fn go_work_value(&self) -> String {
        self.path()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "off".to_owned())
    }
}

/// Typed failures while capturing or revalidating the selected Go workspace.
#[derive(Debug, thiserror::Error)]
pub enum GoWorkWitnessError {
    /// Relative package roots cannot select a stable workspace.
    #[error("Go package root is not absolute: {path:?}")]
    RelativeRoot { path: PathBuf },
    /// The selected package root could not be canonicalized.
    #[error("cannot canonicalize Go package root {path:?}: {source}")]
    Root {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The selected package root is not a directory.
    #[error("Go package root is not a directory: {path:?}")]
    RootNotDirectory { path: PathBuf },
    /// The nearest workspace file cannot be read.
    #[error("cannot read Go workspace file {path:?}: {source}")]
    File {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A `go.work` entry exists but does not resolve to a regular file.
    #[error("Go workspace entry is not a regular file: {path:?}")]
    NotFile { path: PathBuf },
    /// A workspace file exceeds the bounded witness size.
    #[error("Go workspace file exceeds {limit} bytes: {path:?}")]
    TooLarge { path: PathBuf, limit: usize },
}

fn read_bounded_go_work(path: &Path) -> Result<Vec<u8>, GoWorkWitnessError> {
    use std::io::Read;

    let file = std::fs::File::open(path).map_err(|source| GoWorkWitnessError::File {
        path: path.to_path_buf(),
        source,
    })?;
    let mut bytes = Vec::new();
    file.take((GO_WORK_BYTES_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| GoWorkWitnessError::File {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() > GO_WORK_BYTES_LIMIT {
        return Err(GoWorkWitnessError::TooLarge {
            path: path.to_path_buf(),
            limit: GO_WORK_BYTES_LIMIT,
        });
    }
    Ok(bytes)
}

/// Complete bounded witness of Go package authority's local filesystem
/// inputs. An incomplete witness permits a fresh local oracle run, but must
/// never authorize remote placement or semantic reuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoPackageAuthorityWitness {
    package_root: PathBuf,
    workspace: GoWorkWitness,
    module_manifests: Box<[GoManifestWitness]>,
    workspace_sum: GoManifestWitness,
    local_trees: Box<[GoLocalTreeWitness]>,
    local_only_reasons: Box<[GoLocalOnlyReason]>,
    complete: bool,
}

/// The kind of Go directive that selected a local filesystem target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GoFilesystemTargetKind {
    /// A `replace` in the selected module's `go.mod`.
    ModuleReplacement,
    /// A `replace` in the selected `go.work`.
    WorkspaceReplacement,
    /// A module selected by a `use` directive in `go.work`.
    WorkspaceUse,
    /// A `replace` in a module selected by `go.work use`.
    WorkspaceModuleReplacement,
}

/// Stable reason why Go authority must execute on the local host.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum GoLocalOnlyReason {
    /// Go selected a host-local workspace file.
    SelectedWorkspace { path: PathBuf },
    /// A Go directive refers to a local filesystem target.
    FilesystemTarget {
        kind: GoFilesystemTargetKind,
        manifest: PathBuf,
        target: PathBuf,
    },
    /// A witness bound was exceeded; local compilation is still permitted,
    /// but the tree is not a complete cache or placement input.
    WitnessLimitExceeded { path: PathBuf, limit: &'static str },
    /// A path selected by a local directive could not be safely captured.
    UnsupportedFilesystemTarget { manifest: PathBuf, target: PathBuf },
    /// A manifest contained a local directive the bounded parser could not
    /// interpret, so its filesystem closure cannot be exported.
    UnparsedManifest { path: PathBuf },
    /// The bounded walk deliberately skipped a potentially relevant tree.
    SkippedDirectory { path: PathBuf },
    /// An entry could not be read while capturing a local target tree.
    UnreadableTreeEntry { path: PathBuf },
}

impl GoLocalOnlyReason {
    /// Stable machine-readable code suitable for capability reports.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::SelectedWorkspace { .. } => "go.workspace.local.v1",
            Self::FilesystemTarget {
                kind: GoFilesystemTargetKind::ModuleReplacement,
                ..
            } => "go.module_replace.local.v1",
            Self::FilesystemTarget {
                kind: GoFilesystemTargetKind::WorkspaceReplacement,
                ..
            } => "go.workspace_replace.local.v1",
            Self::FilesystemTarget {
                kind: GoFilesystemTargetKind::WorkspaceUse,
                ..
            } => "go.workspace_use.local.v1",
            Self::FilesystemTarget {
                kind: GoFilesystemTargetKind::WorkspaceModuleReplacement,
                ..
            } => "go.workspace_module_replace.local.v1",
            Self::WitnessLimitExceeded { .. } => "go.authority_witness.limit_exceeded.v1",
            Self::UnsupportedFilesystemTarget { .. } => {
                "go.authority_witness.unsupported_target.v1"
            }
            Self::UnparsedManifest { .. } => "go.authority_witness.unparsed_manifest.v1",
            Self::SkippedDirectory { .. } => "go.authority_witness.skipped_directory.v1",
            Self::UnreadableTreeEntry { .. } => "go.authority_witness.unreadable_entry.v1",
        }
    }

    /// Returns the path that caused this local-only reason.
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::SelectedWorkspace { path }
            | Self::WitnessLimitExceeded { path, .. }
            | Self::UnparsedManifest { path }
            | Self::SkippedDirectory { path }
            | Self::UnreadableTreeEntry { path } => path,
            Self::FilesystemTarget { target, .. }
            | Self::UnsupportedFilesystemTarget { target, .. } => target,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GoManifestWitness {
    path: Option<PathBuf>,
    state: GoManifestState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GoManifestState {
    NotApplicable,
    Absent,
    File {
        digest: Option<[u8; 32]>,
        bytes: u64,
        complete: bool,
    },
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GoLocalTreeWitness {
    root: PathBuf,
    entries: Box<[GoLocalTreeEntry]>,
    bytes: u64,
    complete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GoLocalTreeEntry {
    relative_path: PathBuf,
    kind: GoLocalTreeEntryKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GoLocalTreeEntryKind {
    Directory,
    File {
        bytes: u64,
        digest: Option<[u8; 32]>,
    },
    Symlink {
        target: PathBuf,
        bytes: Option<u64>,
        digest: Option<[u8; 32]>,
    },
}

#[derive(Debug, Default)]
struct GoLocalTreeBudget {
    entries: usize,
    bytes: u64,
    exhausted: bool,
}

#[derive(Debug)]
struct GoLocalTreeCapture {
    witness: GoLocalTreeWitness,
    incomplete_reasons: Vec<GoLocalOnlyReason>,
    root_available: bool,
}

impl GoPackageAuthorityWitness {
    /// Captures Go manifests, selected workspace state, local directive
    /// targets, and bounded target-tree identities for one package root.
    pub fn capture(package_root: impl AsRef<Path>) -> Result<Self, GoPackageAuthorityWitnessError> {
        capture_go_package_authority_witness(package_root.as_ref())
    }

    /// Returns the path-independent identity for fully portable closure and a
    /// path-bound identity whenever local-only inputs were found.
    #[must_use]
    pub fn identity(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"compiler.go.package-authority-witness.v1\0");
        digest.update(self.workspace.identity());
        update_manifest_identity(
            &mut digest,
            &self.module_manifests,
            self.requires_local_execution(),
        );
        update_manifest_identity(
            &mut digest,
            std::slice::from_ref(&self.workspace_sum),
            self.requires_local_execution(),
        );
        for tree in &self.local_trees {
            update_path_digest(&mut digest, &tree.root);
            digest.update([u8::from(tree.complete)]);
            digest.update(tree.bytes.to_be_bytes());
            digest.update((tree.entries.len() as u64).to_be_bytes());
            for entry in &tree.entries {
                update_path_digest(&mut digest, &entry.relative_path);
                match &entry.kind {
                    GoLocalTreeEntryKind::Directory => digest.update([0]),
                    GoLocalTreeEntryKind::File {
                        bytes,
                        digest: content,
                    } => {
                        digest.update([1]);
                        digest.update(bytes.to_be_bytes());
                        if let Some(content) = content {
                            digest.update(content);
                        }
                    }
                    GoLocalTreeEntryKind::Symlink {
                        target,
                        bytes,
                        digest: content,
                    } => {
                        digest.update([2]);
                        update_path_digest(&mut digest, target);
                        if let Some(bytes) = bytes {
                            digest.update(bytes.to_be_bytes());
                        }
                        if let Some(content) = content {
                            digest.update(content);
                        }
                    }
                }
            }
        }
        for reason in &self.local_only_reasons {
            digest.update(reason.code().as_bytes());
            digest.update([0]);
            update_path_digest(&mut digest, reason.path());
        }
        digest.finalize().into()
    }

    /// Returns the selected workspace file used for the `GOWORK` child value.
    #[must_use]
    pub const fn go_work_witness(&self) -> &GoWorkWitness {
        &self.workspace
    }

    /// Returns every reason this package must stay on its local host.
    #[must_use]
    pub fn local_only_reasons(&self) -> &[GoLocalOnlyReason] {
        &self.local_only_reasons
    }

    /// Returns the first stable local-only reason, if any.
    #[must_use]
    pub fn local_only_reason(&self) -> Option<&GoLocalOnlyReason> {
        self.local_only_reasons.first()
    }

    /// Reports whether any local path or incomplete closure requires local
    /// execution.
    #[must_use]
    pub fn requires_local_execution(&self) -> bool {
        !self.local_only_reasons.is_empty() || !self.complete
    }

    /// Reports whether the complete set of authority inputs was bounded and
    /// captured. Incomplete witnesses are local-only and non-reusable.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.complete
    }

    /// Re-captures the filesystem closure and compares every witnessed input.
    /// An incomplete witness can still authorize a fresh local run after its
    /// captured directives and workspace selection have been revalidated, but
    /// it must not be used for semantic reuse.
    pub fn matches_current(
        &self,
        package_root: impl AsRef<Path>,
    ) -> Result<bool, GoPackageAuthorityWitnessError> {
        Ok(*self == Self::capture(package_root)?)
    }
}

/// Typed root-selection failure while capturing a Go package authority witness.
#[derive(Debug, thiserror::Error)]
pub enum GoPackageAuthorityWitnessError {
    /// The selected workspace could not be resolved.
    #[error(transparent)]
    Workspace(#[from] GoWorkWitnessError),
}

#[derive(Debug)]
struct CapturedGoManifest {
    witness: GoManifestWitness,
    path: Option<PathBuf>,
    module_directory: Option<PathBuf>,
    content: Option<Vec<u8>>,
}

impl GoManifestWitness {
    fn is_complete(&self) -> bool {
        match &self.state {
            GoManifestState::NotApplicable | GoManifestState::Absent => true,
            GoManifestState::File { complete, .. } => *complete,
            GoManifestState::Unavailable => false,
        }
    }

    fn is_file(&self) -> bool {
        matches!(&self.state, GoManifestState::File { .. })
    }
}

fn capture_nearest_go_mod(root: &Path) -> CapturedGoManifest {
    let mut cursor = Some(root);
    while let Some(directory) = cursor {
        let candidate = directory.join("go.mod");
        match std::fs::symlink_metadata(&candidate) {
            Ok(_) => return capture_manifest_file(&candidate, Some(directory)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return CapturedGoManifest {
                    witness: GoManifestWitness {
                        path: Some(candidate.clone()),
                        state: GoManifestState::Unavailable,
                    },
                    path: Some(candidate.clone()),
                    module_directory: Some(directory.to_path_buf()),
                    content: None,
                };
            }
        }
        cursor = directory.parent();
    }
    CapturedGoManifest {
        witness: GoManifestWitness {
            path: None,
            state: GoManifestState::Absent,
        },
        path: None,
        module_directory: None,
        content: None,
    }
}

fn capture_exact_go_mod(directory: &Path) -> CapturedGoManifest {
    let mut captured = capture_manifest_file(&directory.join("go.mod"), Some(directory));
    if !captured.witness.is_file() {
        // A `use` target is required to name a module root. Preserve its
        // negative go.mod observation so creating that file changes witness
        // identity before the oracle is entered.
        captured.module_directory = Some(directory.to_path_buf());
    }
    captured
}

fn capture_manifest_file(path: &Path, module_directory: Option<&Path>) -> CapturedGoManifest {
    let unavailable = |candidate: PathBuf| CapturedGoManifest {
        witness: GoManifestWitness {
            path: Some(candidate.clone()),
            state: GoManifestState::Unavailable,
        },
        path: Some(candidate),
        module_directory: module_directory.map(Path::to_path_buf),
        content: None,
    };
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return CapturedGoManifest {
                witness: GoManifestWitness {
                    path: Some(path.to_path_buf()),
                    state: GoManifestState::Absent,
                },
                path: None,
                module_directory: module_directory.map(Path::to_path_buf),
                content: None,
            };
        }
        Err(_) => return unavailable(path.to_path_buf()),
    };
    if metadata.file_type().is_dir() {
        return unavailable(path.to_path_buf());
    }
    let resolved = match path.canonicalize() {
        Ok(resolved) => resolved,
        Err(_) => return unavailable(path.to_path_buf()),
    };
    let mut file = match std::fs::File::open(&resolved) {
        Ok(file) => file,
        Err(_) => return unavailable(resolved),
    };
    let file_bytes = match file.metadata() {
        Ok(metadata) => metadata.len(),
        Err(_) => return unavailable(resolved),
    };
    let mut bytes = Vec::new();
    if std::io::Read::by_ref(&mut file)
        .take((GO_AUTHORITY_MANIFEST_BYTES_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return unavailable(resolved);
    }
    let complete = bytes.len() <= GO_AUTHORITY_MANIFEST_BYTES_LIMIT
        && file_bytes <= GO_AUTHORITY_MANIFEST_BYTES_LIMIT as u64;
    let content_digest = Some(Sha256::digest(&bytes).into());
    CapturedGoManifest {
        witness: GoManifestWitness {
            path: Some(resolved.clone()),
            state: GoManifestState::File {
                digest: content_digest,
                bytes: file_bytes,
                complete,
            },
        },
        path: Some(resolved),
        module_directory: module_directory.map(Path::to_path_buf),
        content: complete.then_some(bytes),
    }
}

fn add_module_replacement_targets(
    targets: &mut Vec<GoLocalDirectiveTarget>,
    reasons: &mut Vec<GoLocalOnlyReason>,
    manifest: &Path,
    module_directory: &Path,
    replacements: Vec<String>,
    kind: GoFilesystemTargetKind,
) {
    for replacement in replacements {
        let path = resolve_go_local_path(module_directory, &replacement);
        targets.push(GoLocalDirectiveTarget {
            kind,
            manifest: manifest.to_path_buf(),
            path: path.clone(),
        });
        reasons.push(GoLocalOnlyReason::FilesystemTarget {
            kind,
            manifest: manifest.to_path_buf(),
            target: path,
        });
    }
}

fn workspace_replacement_targets(
    manifest: &Path,
    base: &Path,
    replacements: Vec<String>,
) -> Vec<GoLocalDirectiveTarget> {
    replacements
        .into_iter()
        .map(|replacement| GoLocalDirectiveTarget {
            kind: GoFilesystemTargetKind::WorkspaceReplacement,
            manifest: manifest.to_path_buf(),
            path: resolve_go_local_path(base, &replacement),
        })
        .collect()
}

fn resolve_go_local_path(base: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    normalize_absolute_path(&joined)
}

fn normalize_absolute_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn parse_go_directives(
    bytes: &[u8],
    manifest_kind: GoDirectiveManifestKind,
) -> Result<GoDirectiveSet, ()> {
    let text = std::str::from_utf8(bytes).map_err(|_| ())?;
    let mut directives = GoDirectiveSet::default();
    let mut block = None;
    let mut directive_count = 0usize;
    for line in text.lines() {
        let tokens = tokenize_go_manifest_line(line)?;
        if tokens.is_empty() {
            continue;
        }
        if let Some(active) = block {
            let closes = tokens.first().is_some_and(|token| token == ")");
            let entry = if closes { &tokens[1..] } else { &tokens[..] };
            if !entry.is_empty() {
                parse_go_manifest_entry(&mut directives, active, entry)?;
                directive_count = directive_count.saturating_add(1);
            }
            if closes {
                block = None;
            }
        } else {
            let is_use = manifest_kind == GoDirectiveManifestKind::Workspace
                && tokens.first().is_some_and(|token| token == "use");
            let is_replace = tokens.first().is_some_and(|token| token == "replace");
            let active = if is_use {
                Some(GoManifestBlock::Use)
            } else if is_replace {
                Some(GoManifestBlock::Replace)
            } else {
                None
            };
            let Some(active) = active else {
                continue;
            };
            let rest = &tokens[1..];
            if rest.first().is_some_and(|token| token == "(") {
                block = Some(active);
                let end = rest.iter().position(|token| token == ")");
                let body_end = end.unwrap_or(rest.len());
                let entries = &rest[1..body_end];
                if !entries.is_empty() {
                    parse_go_manifest_entry(&mut directives, active, entries)?;
                    directive_count = directive_count.saturating_add(1);
                }
                if end.is_some() {
                    block = None;
                }
            } else {
                parse_go_manifest_entry(&mut directives, active, rest)?;
                directive_count = directive_count.saturating_add(1);
            }
        }
        if directive_count > GO_LOCAL_DIRECTIVE_LIMIT {
            return Err(());
        }
    }
    if block.is_some() {
        return Err(());
    }
    Ok(directives)
}

#[derive(Debug, Clone, Copy)]
enum GoManifestBlock {
    Use,
    Replace,
}

fn parse_go_manifest_entry(
    directives: &mut GoDirectiveSet,
    block: GoManifestBlock,
    tokens: &[String],
) -> Result<(), ()> {
    match block {
        GoManifestBlock::Use => {
            if tokens.len() != 1 {
                return Err(());
            }
            directives.uses.push(tokens[0].clone());
        }
        GoManifestBlock::Replace => {
            let arrow = tokens.iter().position(|token| token == "=>").ok_or(())?;
            if arrow == 0 || !(arrow + 2 == tokens.len() || arrow + 3 == tokens.len()) {
                return Err(());
            }
            let target = tokens[arrow + 1].clone();
            if is_local_go_path(&target) {
                if arrow + 2 != tokens.len() {
                    // Local replacements cannot include a module version.
                    return Err(());
                }
                directives.replacements.push(target);
            }
        }
    }
    Ok(())
}

fn tokenize_go_manifest_line(line: &str) -> Result<Vec<String>, ()> {
    let bytes = line.as_bytes();
    let mut tokens = Vec::new();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        match bytes[cursor] {
            byte if byte.is_ascii_whitespace() => cursor += 1,
            b'/' if bytes.get(cursor + 1) == Some(&b'/') => break,
            b'(' | b')' => {
                tokens.push(String::from(char::from(bytes[cursor])));
                cursor += 1;
            }
            b'=' if bytes.get(cursor + 1) == Some(&b'>') => {
                tokens.push("=>".to_owned());
                cursor += 2;
            }
            b'"' => {
                let start = cursor;
                cursor += 1;
                let mut escaped = false;
                while cursor < bytes.len() {
                    let byte = bytes[cursor];
                    cursor += 1;
                    if escaped {
                        escaped = false;
                    } else if byte == b'\\' {
                        escaped = true;
                    } else if byte == b'"' {
                        break;
                    }
                }
                if bytes.get(cursor.saturating_sub(1)) != Some(&b'"') {
                    return Err(());
                }
                tokens.push(serde_json::from_str(&line[start..cursor]).map_err(|_| ())?);
            }
            b'`' => {
                let start = cursor + 1;
                cursor += 1;
                while cursor < bytes.len() && bytes[cursor] != b'`' {
                    cursor += 1;
                }
                if cursor == bytes.len() {
                    return Err(());
                }
                tokens.push(line[start..cursor].to_owned());
                cursor += 1;
            }
            b';' => return Err(()),
            _ => {
                let start = cursor;
                while cursor < bytes.len()
                    && !bytes[cursor].is_ascii_whitespace()
                    && !matches!(bytes[cursor], b'(' | b')' | b';')
                    && !(bytes[cursor] == b'/' && bytes.get(cursor + 1) == Some(&b'/'))
                    && !(bytes[cursor] == b'=' && bytes.get(cursor + 1) == Some(&b'>'))
                {
                    cursor += 1;
                }
                if start == cursor {
                    return Err(());
                }
                tokens.push(line[start..cursor].to_owned());
            }
        }
    }
    Ok(tokens)
}

fn is_local_go_path(value: &str) -> bool {
    let path = Path::new(value);
    path.is_absolute()
        || value == "."
        || value == ".."
        || value.starts_with("./")
        || value.starts_with("../")
        || value.starts_with(".\\")
        || value.starts_with("..\\")
}

fn capture_local_tree(path: &Path, budget: &mut GoLocalTreeBudget) -> GoLocalTreeCapture {
    let requested_root = normalize_absolute_path(path);
    let mut entries = Vec::new();
    let mut incomplete_reasons = Vec::new();
    let root = match path.canonicalize() {
        Ok(root) if root.is_dir() => root,
        _ => {
            incomplete_reasons.push(GoLocalOnlyReason::UnreadableTreeEntry {
                path: requested_root.clone(),
            });
            return GoLocalTreeCapture {
                witness: GoLocalTreeWitness {
                    root: requested_root,
                    entries: Box::new([]),
                    bytes: 0,
                    complete: false,
                },
                incomplete_reasons,
                root_available: false,
            };
        }
    };
    if budget.exhausted {
        incomplete_reasons.push(GoLocalOnlyReason::WitnessLimitExceeded {
            path: root.clone(),
            limit: "aggregate local tree entries or bytes",
        });
        return GoLocalTreeCapture {
            witness: GoLocalTreeWitness {
                root,
                entries: Box::new([]),
                bytes: 0,
                complete: false,
            },
            incomplete_reasons,
            root_available: true,
        };
    }

    let before_bytes = budget.bytes;
    let complete = walk_local_tree(
        &root,
        Path::new(""),
        0,
        budget,
        &mut entries,
        &mut incomplete_reasons,
    );
    entries.sort_by(|left: &GoLocalTreeEntry, right| left.relative_path.cmp(&right.relative_path));
    GoLocalTreeCapture {
        witness: GoLocalTreeWitness {
            root,
            entries: entries.into_boxed_slice(),
            bytes: budget.bytes.saturating_sub(before_bytes),
            complete,
        },
        incomplete_reasons,
        root_available: true,
    }
}

fn walk_local_tree(
    path: &Path,
    relative_path: &Path,
    depth: usize,
    budget: &mut GoLocalTreeBudget,
    entries: &mut Vec<GoLocalTreeEntry>,
    incomplete_reasons: &mut Vec<GoLocalOnlyReason>,
) -> bool {
    if budget.exhausted {
        incomplete_reasons.push(GoLocalOnlyReason::WitnessLimitExceeded {
            path: path.to_path_buf(),
            limit: "aggregate local tree entries or bytes",
        });
        return false;
    }
    if depth > GO_LOCAL_TREE_DEPTH_LIMIT {
        budget.exhausted = true;
        incomplete_reasons.push(GoLocalOnlyReason::WitnessLimitExceeded {
            path: path.to_path_buf(),
            limit: "local tree depth",
        });
        return false;
    }
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => {
            incomplete_reasons.push(GoLocalOnlyReason::UnreadableTreeEntry {
                path: path.to_path_buf(),
            });
            return false;
        }
    };
    if metadata.file_type().is_symlink() {
        if budget.entries >= GO_LOCAL_TREE_ENTRY_LIMIT {
            budget.exhausted = true;
            incomplete_reasons.push(GoLocalOnlyReason::WitnessLimitExceeded {
                path: path.to_path_buf(),
                limit: "local tree entries",
            });
            return false;
        }
        budget.entries += 1;
        let target = match path.canonicalize() {
            Ok(target) => target,
            Err(_) => {
                let target = std::fs::read_link(path).unwrap_or_default();
                entries.push(GoLocalTreeEntry {
                    relative_path: relative_path.to_path_buf(),
                    kind: GoLocalTreeEntryKind::Symlink {
                        target,
                        bytes: None,
                        digest: None,
                    },
                });
                incomplete_reasons.push(GoLocalOnlyReason::UnreadableTreeEntry {
                    path: path.to_path_buf(),
                });
                return false;
            }
        };
        let target_metadata = match std::fs::metadata(&target) {
            Ok(metadata) => metadata,
            Err(_) => {
                incomplete_reasons.push(GoLocalOnlyReason::UnreadableTreeEntry {
                    path: path.to_path_buf(),
                });
                return false;
            }
        };
        if target_metadata.is_dir() {
            entries.push(GoLocalTreeEntry {
                relative_path: relative_path.to_path_buf(),
                kind: GoLocalTreeEntryKind::Symlink {
                    target,
                    bytes: None,
                    digest: None,
                },
            });
            incomplete_reasons.push(GoLocalOnlyReason::UnreadableTreeEntry {
                path: path.to_path_buf(),
            });
            return false;
        }
        if !target_metadata.is_file() {
            entries.push(GoLocalTreeEntry {
                relative_path: relative_path.to_path_buf(),
                kind: GoLocalTreeEntryKind::Symlink {
                    target,
                    bytes: None,
                    digest: None,
                },
            });
            incomplete_reasons.push(GoLocalOnlyReason::UnreadableTreeEntry {
                path: path.to_path_buf(),
            });
            return false;
        }
        let mut file = match std::fs::File::open(&target) {
            Ok(file) => file,
            Err(_) => {
                incomplete_reasons.push(GoLocalOnlyReason::UnreadableTreeEntry {
                    path: path.to_path_buf(),
                });
                return false;
            }
        };
        let length = target_metadata.len();
        if length > GO_LOCAL_TREE_FILE_BYTES_LIMIT as u64
            || budget.bytes.saturating_add(length) > GO_LOCAL_TREE_BYTES_LIMIT as u64
        {
            budget.exhausted = true;
            entries.push(GoLocalTreeEntry {
                relative_path: relative_path.to_path_buf(),
                kind: GoLocalTreeEntryKind::Symlink {
                    target,
                    bytes: Some(length),
                    digest: None,
                },
            });
            incomplete_reasons.push(GoLocalOnlyReason::WitnessLimitExceeded {
                path: path.to_path_buf(),
                limit: "local tree file bytes",
            });
            return false;
        }
        let mut bytes = Vec::with_capacity(length as usize);
        if std::io::Read::by_ref(&mut file)
            .take((GO_LOCAL_TREE_FILE_BYTES_LIMIT + 1) as u64)
            .read_to_end(&mut bytes)
            .is_err()
        {
            incomplete_reasons.push(GoLocalOnlyReason::UnreadableTreeEntry {
                path: path.to_path_buf(),
            });
            return false;
        }
        if bytes.len() as u64 != length {
            incomplete_reasons.push(GoLocalOnlyReason::UnreadableTreeEntry {
                path: path.to_path_buf(),
            });
            return false;
        }
        budget.bytes = budget.bytes.saturating_add(length);
        budget.entries = budget.entries.saturating_add(1);
        entries.push(GoLocalTreeEntry {
            relative_path: relative_path.to_path_buf(),
            kind: GoLocalTreeEntryKind::Symlink {
                target,
                bytes: Some(length),
                digest: Some(Sha256::digest(&bytes).into()),
            },
        });
        return true;
    }
    if metadata.is_file() {
        let length = metadata.len();
        if length > GO_LOCAL_TREE_FILE_BYTES_LIMIT as u64
            || budget.bytes.saturating_add(length) > GO_LOCAL_TREE_BYTES_LIMIT as u64
            || budget.entries >= GO_LOCAL_TREE_ENTRY_LIMIT
        {
            budget.exhausted = true;
            entries.push(GoLocalTreeEntry {
                relative_path: relative_path.to_path_buf(),
                kind: GoLocalTreeEntryKind::File {
                    bytes: length,
                    digest: None,
                },
            });
            incomplete_reasons.push(GoLocalOnlyReason::WitnessLimitExceeded {
                path: path.to_path_buf(),
                limit: "local tree file bytes",
            });
            return false;
        }
        let mut file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(_) => {
                incomplete_reasons.push(GoLocalOnlyReason::UnreadableTreeEntry {
                    path: path.to_path_buf(),
                });
                return false;
            }
        };
        let mut bytes = Vec::with_capacity(length as usize);
        if std::io::Read::by_ref(&mut file)
            .take((GO_LOCAL_TREE_FILE_BYTES_LIMIT + 1) as u64)
            .read_to_end(&mut bytes)
            .is_err()
            || bytes.len() as u64 != length
        {
            incomplete_reasons.push(GoLocalOnlyReason::UnreadableTreeEntry {
                path: path.to_path_buf(),
            });
            return false;
        }
        if budget.entries >= GO_LOCAL_TREE_ENTRY_LIMIT {
            budget.exhausted = true;
            incomplete_reasons.push(GoLocalOnlyReason::WitnessLimitExceeded {
                path: path.to_path_buf(),
                limit: "local tree entries",
            });
            return false;
        }
        budget.entries += 1;
        budget.bytes = budget.bytes.saturating_add(length);
        entries.push(GoLocalTreeEntry {
            relative_path: relative_path.to_path_buf(),
            kind: GoLocalTreeEntryKind::File {
                bytes: length,
                digest: Some(Sha256::digest(&bytes).into()),
            },
        });
        return true;
    }
    if !metadata.is_dir() {
        incomplete_reasons.push(GoLocalOnlyReason::UnreadableTreeEntry {
            path: path.to_path_buf(),
        });
        return false;
    }
    if budget.entries >= GO_LOCAL_TREE_ENTRY_LIMIT {
        budget.exhausted = true;
        incomplete_reasons.push(GoLocalOnlyReason::WitnessLimitExceeded {
            path: path.to_path_buf(),
            limit: "local tree entries",
        });
        return false;
    }
    budget.entries += 1;
    entries.push(GoLocalTreeEntry {
        relative_path: relative_path.to_path_buf(),
        kind: GoLocalTreeEntryKind::Directory,
    });

    let mut read_errors = false;
    let remaining_entries = GO_LOCAL_TREE_ENTRY_LIMIT.saturating_sub(budget.entries);
    let mut children = match std::fs::read_dir(path) {
        Ok(children) => {
            let mut entries = Vec::new();
            for child in children {
                match child {
                    Ok(child) if entries.len() < remaining_entries => entries.push(child),
                    Ok(_) => {
                        budget.exhausted = true;
                        incomplete_reasons.push(GoLocalOnlyReason::WitnessLimitExceeded {
                            path: path.to_path_buf(),
                            limit: "local tree directory entries",
                        });
                        return false;
                    }
                    Err(_) => {
                        read_errors = true;
                        incomplete_reasons.push(GoLocalOnlyReason::UnreadableTreeEntry {
                            path: path.to_path_buf(),
                        });
                    }
                }
            }
            entries
        }
        Err(_) => {
            incomplete_reasons.push(GoLocalOnlyReason::UnreadableTreeEntry {
                path: path.to_path_buf(),
            });
            return false;
        }
    };
    children.sort_by_key(std::fs::DirEntry::file_name);
    let mut complete = !read_errors;
    for child in children {
        let name = child.file_name();
        if is_nonsemantic_vcs_directory(&name) {
            continue;
        }
        let child_path = child.path();
        let child_relative = relative_path.join(&name);
        if child.file_type().is_ok_and(|kind| kind.is_dir())
            && is_generated_or_go_ignored_directory(&name)
        {
            complete = false;
            incomplete_reasons.push(GoLocalOnlyReason::SkippedDirectory { path: child_path });
            continue;
        }
        let child_complete = walk_local_tree(
            &child.path(),
            &child_relative,
            depth.saturating_add(1),
            budget,
            entries,
            incomplete_reasons,
        );
        complete &= child_complete;
        if budget.exhausted {
            break;
        }
    }
    complete
}

fn is_nonsemantic_vcs_directory(name: &std::ffi::OsStr) -> bool {
    matches!(name.to_str(), Some(".git" | ".hg" | ".svn"))
}

fn is_generated_or_go_ignored_directory(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(
            "target"
                | "dist"
                | "build"
                | "out"
                | "coverage"
                | "node_modules"
                | ".cache"
                | ".venv"
                | "venv"
                | "testdata"
        )
    ) || name
        .to_str()
        .is_some_and(|name| name.starts_with('_') || name.starts_with('.'))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GoDirectiveManifestKind {
    Module,
    Workspace,
}

#[derive(Debug, Clone)]
struct GoLocalDirectiveTarget {
    kind: GoFilesystemTargetKind,
    manifest: PathBuf,
    path: PathBuf,
}

#[derive(Debug, Default)]
struct GoDirectiveSet {
    uses: Vec<String>,
    replacements: Vec<String>,
}

fn capture_go_package_authority_witness(
    requested: &Path,
) -> Result<GoPackageAuthorityWitness, GoPackageAuthorityWitnessError> {
    if !requested.is_absolute() {
        return Err(GoWorkWitnessError::RelativeRoot {
            path: requested.to_path_buf(),
        }
        .into());
    }
    let package_root = requested
        .canonicalize()
        .map_err(|source| GoWorkWitnessError::Root {
            path: requested.to_path_buf(),
            source,
        })?;
    if !package_root.is_dir() {
        return Err(GoWorkWitnessError::RootNotDirectory { path: package_root }.into());
    }

    let workspace = GoWorkWitness::capture(&package_root)?;
    let mut complete = workspace.is_complete();
    let mut reasons = Vec::new();
    if let Some(path) = workspace.path() {
        reasons.push(GoLocalOnlyReason::SelectedWorkspace {
            path: path.to_path_buf(),
        });
        if !workspace.is_complete() {
            reasons.push(GoLocalOnlyReason::WitnessLimitExceeded {
                path: path.to_path_buf(),
                limit: "go.work bytes",
            });
        }
    }

    let mut manifests = Vec::new();
    let mut targets = Vec::new();
    let mut parsed_module_directories = std::collections::BTreeSet::new();

    let selected_module = capture_nearest_go_mod(&package_root);
    let mut selected_module_directory = None;
    if let Some(module_directory) = selected_module.module_directory.clone() {
        selected_module_directory = Some(module_directory.clone());
        parsed_module_directories.insert(module_directory.clone());
        let module_file_complete =
            selected_module.witness.is_file() && selected_module.witness.is_complete();
        complete &= module_file_complete;
        if !module_file_complete {
            reasons.push(GoLocalOnlyReason::UnparsedManifest {
                path: selected_module
                    .path
                    .clone()
                    .unwrap_or_else(|| module_directory.join("go.mod")),
            });
        }
        if let Some(content) = selected_module.content.as_deref() {
            match parse_go_directives(content, GoDirectiveManifestKind::Module) {
                Ok(directives) => {
                    let manifest_path = selected_module
                        .path
                        .clone()
                        .unwrap_or_else(|| module_directory.join("go.mod"));
                    add_module_replacement_targets(
                        &mut targets,
                        &mut reasons,
                        &manifest_path,
                        &module_directory,
                        directives.replacements,
                        GoFilesystemTargetKind::ModuleReplacement,
                    );
                }
                Err(()) => {
                    complete = false;
                    reasons.push(GoLocalOnlyReason::UnparsedManifest {
                        path: selected_module
                            .path
                            .clone()
                            .unwrap_or_else(|| module_directory.join("go.mod")),
                    });
                }
            }
        }
        manifests.push(selected_module.witness);
        let module_sum =
            capture_manifest_file(&module_directory.join("go.sum"), Some(&module_directory));
        complete &= module_sum.witness.is_complete();
        if !module_sum.witness.is_complete() {
            reasons.push(GoLocalOnlyReason::UnparsedManifest {
                path: module_directory.join("go.sum"),
            });
        }
        manifests.push(module_sum.witness);
    }

    let workspace_file = workspace
        .path()
        .map(|path| capture_manifest_file(path, None));
    let workspace_sum = if let Some(path) = workspace.path() {
        let sum_path = path.with_file_name("go.work.sum");
        let captured = capture_manifest_file(&sum_path, None);
        complete &= captured.witness.is_complete();
        if !captured.witness.is_complete() {
            reasons.push(GoLocalOnlyReason::WitnessLimitExceeded {
                path: sum_path,
                limit: "go.work.sum bytes or readability",
            });
        }
        captured.witness
    } else {
        GoManifestWitness {
            path: None,
            state: GoManifestState::NotApplicable,
        }
    };

    if let Some(workspace_file) = workspace_file {
        complete &= workspace_file.witness.is_complete();
        if !workspace_file.witness.is_complete() {
            reasons.push(GoLocalOnlyReason::UnparsedManifest {
                path: workspace_file
                    .path
                    .clone()
                    .unwrap_or_else(|| workspace.path().expect("captured workspace").to_path_buf()),
            });
        }
        if !workspace_file.witness.is_file() {
            complete = false;
            reasons.push(GoLocalOnlyReason::UnparsedManifest {
                path: workspace.path().expect("captured workspace").to_path_buf(),
            });
        }
        if let Some(content) = workspace_file.content.as_deref() {
            match parse_go_directives(content, GoDirectiveManifestKind::Workspace) {
                Ok(directives) => {
                    let manifest = workspace_file
                        .path
                        .as_deref()
                        .unwrap_or_else(|| workspace.path().expect("captured workspace"));
                    for value in directives.uses {
                        let path = resolve_go_local_path(
                            workspace_file
                                .module_directory
                                .as_deref()
                                .unwrap_or_else(|| {
                                    manifest.parent().unwrap_or_else(|| Path::new("/"))
                                }),
                            &value,
                        );
                        targets.push(GoLocalDirectiveTarget {
                            kind: GoFilesystemTargetKind::WorkspaceUse,
                            manifest: manifest.to_path_buf(),
                            path: path.clone(),
                        });
                        reasons.push(GoLocalOnlyReason::FilesystemTarget {
                            kind: GoFilesystemTargetKind::WorkspaceUse,
                            manifest: manifest.to_path_buf(),
                            target: path.clone(),
                        });
                        let module_manifest = capture_exact_go_mod(&path);
                        let module_file_complete = module_manifest.witness.is_file()
                            && module_manifest.witness.is_complete();
                        complete &= module_file_complete;
                        if !module_file_complete {
                            reasons.push(GoLocalOnlyReason::UnsupportedFilesystemTarget {
                                manifest: manifest.to_path_buf(),
                                target: path.clone(),
                            });
                        }
                        if let Some(module_directory) = module_manifest.module_directory.clone() {
                            if parsed_module_directories.insert(module_directory.clone()) {
                                if let Some(module_content) = module_manifest.content.as_deref() {
                                    match parse_go_directives(
                                        module_content,
                                        GoDirectiveManifestKind::Module,
                                    ) {
                                        Ok(module_directives) => {
                                            let manifest_path = module_manifest
                                                .path
                                                .clone()
                                                .unwrap_or_else(|| module_directory.join("go.mod"));
                                            add_module_replacement_targets(
                                                &mut targets,
                                                &mut reasons,
                                                &manifest_path,
                                                &module_directory,
                                                module_directives.replacements,
                                                GoFilesystemTargetKind::WorkspaceModuleReplacement,
                                            );
                                        }
                                        Err(()) => {
                                            complete = false;
                                            reasons.push(GoLocalOnlyReason::UnparsedManifest {
                                                path: module_manifest.path.clone().unwrap_or_else(
                                                    || module_directory.join("go.mod"),
                                                ),
                                            });
                                        }
                                    }
                                }
                                manifests.push(module_manifest.witness.clone());
                                let module_sum = capture_manifest_file(
                                    &module_directory.join("go.sum"),
                                    Some(&module_directory),
                                );
                                complete &= module_sum.witness.is_complete();
                                if !module_sum.witness.is_complete() {
                                    reasons.push(GoLocalOnlyReason::UnparsedManifest {
                                        path: module_directory.join("go.sum"),
                                    });
                                }
                                manifests.push(module_sum.witness);
                            }
                        }
                    }
                    let replacement_targets = workspace_replacement_targets(
                        manifest,
                        manifest.parent().unwrap_or_else(|| Path::new("/")),
                        directives.replacements,
                    );
                    for target in replacement_targets {
                        reasons.push(GoLocalOnlyReason::FilesystemTarget {
                            kind: target.kind,
                            manifest: target.manifest.clone(),
                            target: target.path.clone(),
                        });
                        targets.push(target);
                    }
                }
                Err(()) => {
                    complete = false;
                    reasons.push(GoLocalOnlyReason::UnparsedManifest {
                        path: workspace_file.path.clone().unwrap_or_else(|| {
                            workspace.path().expect("captured workspace").to_path_buf()
                        }),
                    });
                }
            }
        }
    }

    // A package outside a module still gets a negative module-manifest
    // witness; this prevents a newly-created ancestor go.mod from silently
    // changing Go's package selection after queue admission.
    if selected_module_directory.is_none() {
        manifests.push(GoManifestWitness {
            path: None,
            state: GoManifestState::Absent,
        });
    }

    let mut unique_targets = BTreeMap::<PathBuf, GoLocalDirectiveTarget>::new();
    for target in targets {
        unique_targets.entry(target.path.clone()).or_insert(target);
    }
    let mut tree_budget = GoLocalTreeBudget::default();
    let mut local_trees = Vec::new();
    for target in unique_targets.into_values() {
        let tree = capture_local_tree(&target.path, &mut tree_budget);
        complete &= tree.witness.complete;
        if !tree.root_available {
            reasons.push(GoLocalOnlyReason::UnsupportedFilesystemTarget {
                manifest: target.manifest.clone(),
                target: target.path.clone(),
            });
        }
        for reason in tree.incomplete_reasons.iter().cloned() {
            reasons.push(reason);
        }
        local_trees.push(tree.witness);
    }

    manifests.sort_by(|left, right| left.path.cmp(&right.path));
    manifests.dedup();
    local_trees.sort_by(|left, right| left.root.cmp(&right.root));
    reasons.sort();
    reasons.dedup();

    Ok(GoPackageAuthorityWitness {
        package_root,
        workspace,
        module_manifests: manifests.into_boxed_slice(),
        workspace_sum,
        local_trees: local_trees.into_boxed_slice(),
        local_only_reasons: reasons.into_boxed_slice(),
        complete,
    })
}

fn update_manifest_identity(
    digest: &mut Sha256,
    manifests: &[GoManifestWitness],
    path_bound: bool,
) {
    for manifest in manifests {
        match &manifest.state {
            GoManifestState::NotApplicable => digest.update([0]),
            GoManifestState::Absent => {
                digest.update([1]);
                if path_bound {
                    if let Some(path) = &manifest.path {
                        update_path_digest(digest, path);
                    }
                }
            }
            GoManifestState::File {
                digest: content_digest,
                bytes,
                complete,
            } => {
                digest.update([2, u8::from(*complete)]);
                digest.update(bytes.to_be_bytes());
                if let Some(content_digest) = content_digest {
                    digest.update(content_digest);
                }
                if path_bound {
                    if let Some(path) = &manifest.path {
                        update_path_digest(digest, path);
                    }
                }
            }
            GoManifestState::Unavailable => {
                digest.update([3]);
                if let Some(path) = &manifest.path {
                    update_path_digest(digest, path);
                }
            }
        }
    }
}
