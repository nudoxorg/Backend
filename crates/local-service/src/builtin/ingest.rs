//! Deterministic, parallel, content-versioned filesystem ingestion.

use super::source_budget::{SourceAdmissionLedger, SourceAdmissionLimits, SourceAdmissionPolicy};
use super::source_frontier;
use backend_compile::{
    DeclarationKind, InputContentSchema, SourceExcerpt, SourceLanguage, SyntaxFrontend, typed_of,
};
use backend_engine::{
    ProductSourceFileFactsUpdate, ProductSourceRecord, ProductSourceRelation, Relation,
    SourceUnavailableReason, build_product_source_file_facts, product_source_file_key,
};
use backend_library::{
    DiscoveryPolicy, EntryKind, discover_source_entries, source_selection_policy,
};
use backend_semantic::vocabulary::{
    CSharpVersion, CStandard, CxxStandard, GoVersion, JavaRelease, Language, LanguageProfile,
    PythonVersion, RustEdition, TypeScriptSource,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, mpsc};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};
use unicode_normalization::UnicodeNormalization;

// One relation row can never exceed the canonical node capacity, so the
// per-record ceiling is that bound rather than an independent number that
// would admit records the tree must later reject.
const MAX_ENCODED_RECORD_BYTES: usize = ProductSourceRecord::ROW_VALUE_CAPACITY;
const MAX_DIRECTORY_ENTRIES: usize = 100_000;
const MAX_DISCOVERY_ENTRIES: usize = 500_000;
const MAX_COMPILER_WORKSPACE_FILE_BYTES: usize = 64 * 1024 * 1024;
const MAX_COMPILER_WORKSPACE_STREAM_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_COMPILER_WORKSPACE_CHUNK_BYTES: usize = 1024 * 1024;
#[cfg(windows)]
const COMPILER_WORKSPACE_POLICY_IDENTITY: &str = "nudox.compiler-workspace.v1/gitignore+generated-defaults.v1;ignore-case=insensitive;portable-case-collision=reject;symlink=reject";
#[cfg(not(windows))]
const COMPILER_WORKSPACE_POLICY_IDENTITY: &str = "nudox.compiler-workspace.v1/gitignore+generated-defaults.v1;ignore-case=sensitive;portable-case-collision=reject;symlink=reject";
const MAX_COMPILER_CONFIGURATION_FILES_PER_LANGUAGE: usize = 4_096;
pub(super) const MAX_COMPILER_CONFIGURATION_FILE_BYTES: usize = 8 * 1024 * 1024;
const MAX_COMPILER_CONFIGURATION_BYTES_PER_LANGUAGE: usize = 32 * 1024 * 1024;
const RESULT_QUEUE_PER_WORKER: usize = 1;
pub(super) const INDEX_SCAN_CANCELLED: &str = "index scan cancelled";

fn check_scan_cancellation(cancellation: Option<&AtomicBool>) -> Result<(), String> {
    if cancellation.is_some_and(|cancellation| cancellation.load(Ordering::Acquire)) {
        Err(INDEX_SCAN_CANCELLED.to_owned())
    } else {
        Ok(())
    }
}

fn absolute_path_spelling(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|current| current.join(path))
            .map_err(|error| format!("resolve project path spelling: {error}"))
    }
}

/// One complete, deterministic project scan before comparison with the
/// selected versioned relation.
pub(super) struct IndexSnapshot {
    pub(super) source_version: [u8; 32],
    pub(super) files: Vec<([u8; 32], ProductSourceRecord)>,
    /// Complete file-facts updates produced before compact source-row retention.
    pub(super) source_facts: Vec<ProductSourceFileFactsUpdate>,
    /// Full persisted fact-page charges, including unchanged frontier files.
    pub(super) encoded_fact_bytes: Vec<([u8; 32], usize)>,
    pub(super) compiler_sources: Vec<CompilerSourceHandle>,
    pub(super) reused_compiler_files: Vec<ReusedCompilerFile>,
    pub(super) compiler_configuration: CompilerConfigurationSnapshot,
    pub(super) revision_fence: CompilerRevisionFence,
    /// Source bytes actually read by this scan. This is distinct from the
    /// byte-length budget, which is also charged for files admitted from a
    /// verified frontier.
    pub(super) source_bytes_read: usize,
    /// Limits pinned for source admission, compiler rereads, and warm-cache
    /// identity during this scan's downstream compilation.
    pub(super) source_admission_policy: SourceAdmissionPolicy,
}

/// Captured tree and file metadata used to reject a compiler result if known
/// source/configuration paths change after scanning. Source contents are
/// separately re-admitted at the compiler boundary before native execution.
#[derive(Clone)]
pub(super) struct CompilerRevisionFence {
    /// Absolute spelling supplied when this scan began, before symlink
    /// resolution. Retaining it lets later fences notice a retargeted alias.
    requested_root: PathBuf,
    /// Canonical root pinned when the scan began.
    root: PathBuf,
    directories: Vec<(PathBuf, Option<FileSystemRevision>)>,
    pub(super) files: Vec<CompilerFileRevision>,
}

impl CompilerRevisionFence {
    /// Canonical root pinned by this source scan. Filesystem reads that happen
    /// after admission must use this path rather than resolving the caller's
    /// original spelling again.
    pub(super) fn canonical_root(&self) -> &Path {
        &self.root
    }

    /// Opens the exact root admitted by this scan after checking both the
    /// original path spelling and the root directory's captured identity.
    fn open_bound_root(&self, requested_root: &Path) -> Result<ProjectRoot, String> {
        let requested_spelling = absolute_path_spelling(requested_root)?;
        if requested_spelling != self.requested_root {
            return Err("project root spelling changed after source admission".to_owned());
        }
        let resolved = self
            .requested_root
            .canonicalize()
            .map_err(|error| format!("resolve admitted project root: {error}"))?;
        if resolved != self.root {
            return Err("project root alias now resolves to a different directory".to_owned());
        }
        let expected_revision = self
            .directories
            .iter()
            .find(|(relative, _)| relative.as_os_str().is_empty())
            .and_then(|(_, revision)| *revision)
            .ok_or_else(|| "admitted project root has no captured revision".to_owned())?;
        let capability = ProjectRoot::open(&self.root)?;
        if capability.revision()? != expected_revision {
            return Err("project root directory changed after source admission".to_owned());
        }
        Ok(capability)
    }
}

#[derive(Clone)]
pub(super) struct CompilerFileRevision {
    pub(super) relative_path: PathBuf,
    pub(super) metadata: Option<FileSystemRevision>,
}

/// Entry kind admitted into a complete compiler workspace inventory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CompilerWorkspaceEntryKind {
    /// The workspace root or one admitted directory.
    Directory,
    /// An admitted regular file, including a zero-byte file.
    File,
}

/// One path in a complete, normalized compiler workspace inventory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CompilerWorkspaceEntry {
    /// Portable root-relative path using `/`; the root itself is `""`.
    pub(super) path: String,
    /// Directory or regular-file kind. Links and special nodes fail admission.
    pub(super) kind: CompilerWorkspaceEntryKind,
    /// Exact file size; directories carry `None`.
    pub(super) byte_length: Option<u64>,
    revision: FileSystemRevision,
}

/// A complete admissible workspace view with a confined read capability and
/// an exact full-entry revalidation fence.
pub(super) struct CompilerWorkspaceSnapshot {
    requested_root: PathBuf,
    root: PathBuf,
    root_capability: ProjectRoot,
    policy: DiscoveryPolicy,
    entries: Vec<CompilerWorkspaceEntry>,
    fence_digest: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FileSystemRevision {
    pub(super) length: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    unix: (u64, u64, i64, i64),
    #[cfg(windows)]
    windows: backend_platform::win32::project_fs::FileRevision,
}

/// Bounded configuration and lockfile read set admitted by project discovery.
#[derive(Default)]
pub(super) struct CompilerConfigurationSnapshot {
    pub(super) files: Vec<CompilerConfigurationFile>,
    pub(super) complete_languages: BTreeSet<Language>,
}

/// One bounded, confined read of a compiler configuration input.
#[derive(Clone)]
pub(super) struct CompilerConfigurationFile {
    pub(super) language: Language,
    pub(super) relative_path: PathBuf,
    pub(super) content: [u8; 32],
}

/// Compact source capability retained between baseline parsing and compiler
/// admission. The compiler rereads this path through `ProjectRoot` and checks
/// the exact identity before constructing the contiguous text it needs.
#[derive(Clone)]
pub(super) struct CompilerSourceHandle {
    pub(super) relative_path: String,
    pub(super) profile: LanguageProfile,
    pub(super) content: [u8; 32],
    /// Exact source-fact identity persisted with the corresponding source row.
    pub(super) source_fact_identity: SourceFactIdentity,
}

/// UTF-8 source admitted for one exact semantic authority slot.
#[derive(Clone)]
pub(super) struct CompilerSource {
    pub(super) relative_path: String,
    pub(super) profile: LanguageProfile,
    pub(super) source: String,
    pub(super) content: [u8; 32],
    /// Exact source-fact identity revalidated during compiler admission.
    pub(super) source_fact_identity: SourceFactIdentity,
}

/// A file whose prior analysis is still exact, so the scan keeps its content
/// hash and drops the compiler text until a sibling change forces a compile.
#[derive(Clone)]
pub(super) struct ReusedCompilerFile {
    pub(super) relative_path: String,
    pub(super) profile: LanguageProfile,
    pub(super) content: [u8; 32],
    /// Exact source-fact identity copied from the validated source row.
    pub(super) source_fact_identity: SourceFactIdentity,
}

/// Domain-typed identity of the canonical bytes consumed by semantic compilers.
pub(super) type SourceFactIdentity = backend_version::ContentId<backend_version::SourceFactDomain>;

struct ScannedFile {
    relative: String,
    key: [u8; 32],
    record: ProductSourceRecord,
    /// Source size charged against the project's bounded ingest budget.
    source_bytes: usize,
    /// Bytes actually returned by a source read in this scan.
    source_bytes_read: usize,
    encoded_record_bytes: usize,
    encoded_fact_bytes: usize,
    /// `None` when the claiming frontend has no semantic profile for this
    /// extension: the file is still a project row, it simply carries nothing
    /// a compiler could be asked to analyse.
    compiler_source: Option<CompilerSourceHandle>,
    reused_compiler: Option<ReusedCompilerFile>,
    source_facts: Option<ProductSourceFileFactsUpdate>,
}

/// An opened project directory capability. Source reads resolve every path
/// component relative to this descriptor with symlink following disabled, so
/// a concurrently modified checkout cannot redirect ingestion outside the
/// admitted project after discovery.
struct ProjectRoot {
    #[cfg(unix)]
    directory: fs::File,
    #[cfg(windows)]
    directory: backend_platform::win32::project_fs::ProjectRoot,
    #[cfg(not(any(unix, windows)))]
    canonical: PathBuf,
}

impl ProjectRoot {
    fn open(path: &Path) -> Result<Self, String> {
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags, open};
            let directory = open(
                path,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map(fs::File::from)
            .map_err(|error| format!("open project directory {}: {error}", path.display()))?;
            Ok(Self { directory })
        }
        #[cfg(windows)]
        {
            let directory = backend_platform::win32::project_fs::ProjectRoot::open(path)
                .map_err(|error| format!("open project directory {}: {error}", path.display()))?;
            Ok(Self { directory })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Ok(Self {
                canonical: path.to_path_buf(),
            })
        }
    }

    fn revision(&self) -> Result<FileSystemRevision, String> {
        #[cfg(unix)]
        let metadata = self
            .directory
            .metadata()
            .map_err(|error| format!("stat opened workspace root: {error}"))?;
        #[cfg(windows)]
        {
            return self
                .directory
                .revision()
                .map(file_system_revision_from_windows)
                .map_err(|error| format!("stat opened workspace root: {error}"));
        }
        #[cfg(not(any(unix, windows)))]
        let metadata = fs::symlink_metadata(&self.canonical)
            .map_err(|error| format!("stat workspace root: {error}"))?;
        #[cfg(not(windows))]
        Ok(file_system_revision_from_metadata(&metadata))
    }

    /// Opens one project-relative path, following no symlink at any step.
    ///
    /// Each component is resolved against the previously opened directory
    /// descriptor, so a checkout mutated during ingestion cannot redirect the
    /// read outside the admitted project.
    #[cfg(unix)]
    fn open_confined(&self, relative: &Path) -> Result<fs::File, SourceFault> {
        use rustix::fs::{Mode, OFlags, openat};
        let escaped = || SourceFault::Fatal("source path escaped its project root".to_owned());
        let components = relative.components().collect::<Vec<_>>();
        let (last, parents) = components.split_last().ok_or_else(escaped)?;
        let mut directory = self
            .directory
            .try_clone()
            .map_err(|error| SourceFault::Fatal(error.to_string()))?;
        for component in parents {
            let std::path::Component::Normal(name) = component else {
                return Err(escaped());
            };
            directory = openat(
                &directory,
                *name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map(fs::File::from)
            .map_err(|error| SourceFault::from_open(error.kind()))?;
        }
        let std::path::Component::Normal(name) = last else {
            return Err(escaped());
        };
        openat(
            &directory,
            *name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(fs::File::from)
        .map_err(|error| SourceFault::from_open(error.kind()))
    }

    fn read(&self, relative: &Path, maximum_bytes: usize) -> Result<Vec<u8>, SourceFault> {
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(SourceFault::Fatal(
                "source path is not a confined relative path".to_owned(),
            ));
        }
        #[cfg(unix)]
        {
            read_bounded(self.open_confined(relative)?, maximum_bytes)
        }
        #[cfg(windows)]
        {
            let segments = windows_relative_segments(relative).map_err(SourceFault::Fatal)?;
            let file = self
                .directory
                .open_file_read(&segments)
                .map_err(|error| SourceFault::from_open(error.kind()))?;
            read_bounded(file, maximum_bytes)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let path = self.canonical.join(relative);
            let canonical = path
                .canonicalize()
                .map_err(|error| SourceFault::from_open(error.kind()))?;
            if !canonical.starts_with(&self.canonical) {
                return Err(SourceFault::Fatal(
                    "source path escaped its project root".to_owned(),
                ));
            }
            let file = std::fs::File::open(canonical)
                .map_err(|error| SourceFault::from_open(error.kind()))?;
            read_bounded(file, maximum_bytes)
        }
    }

    fn read_compiler_configuration(
        &self,
        relative: &Path,
        maximum_bytes: usize,
    ) -> Result<Vec<u8>, SourceFault> {
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(SourceFault::Fatal(
                "compiler configuration path is not confined".to_owned(),
            ));
        }
        #[cfg(unix)]
        {
            read_bounded(self.open_confined(relative)?, maximum_bytes)
        }
        #[cfg(windows)]
        {
            let segments = windows_relative_segments(relative).map_err(SourceFault::Fatal)?;
            let file = self
                .directory
                .open_file_read(&segments)
                .map_err(|error| SourceFault::from_open(error.kind()))?;
            read_bounded(file, maximum_bytes)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let path = self.canonical.join(relative);
            let canonical = path
                .canonicalize()
                .map_err(|error| SourceFault::from_open(error.kind()))?;
            if !canonical.starts_with(&self.canonical) {
                return Err(SourceFault::Fatal(
                    "compiler configuration escaped its project root".to_owned(),
                ));
            }
            let file = std::fs::File::open(canonical)
                .map_err(|error| SourceFault::from_open(error.kind()))?;
            read_bounded(file, maximum_bytes)
        }
    }

    fn open_workspace_file(&self, relative: &Path) -> Result<fs::File, String> {
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err("workspace path is not a confined relative path".to_owned());
        }
        #[cfg(unix)]
        {
            self.open_confined(relative).map_err(|error| match error {
                SourceFault::Fatal(detail) => detail,
                SourceFault::Unavailable(_)
                | SourceFault::UnavailableRead(_, _)
                | SourceFault::Vanished => "workspace file changed or became unreadable".to_owned(),
            })
        }
        #[cfg(windows)]
        {
            let segments = windows_relative_segments(relative)?;
            self.directory
                .open_file_read(&segments)
                .map_err(|error| format!("open workspace file: {error}"))
        }
        #[cfg(not(any(unix, windows)))]
        {
            let path = self.canonical.join(relative);
            let canonical = path
                .canonicalize()
                .map_err(|error| format!("open workspace file: {error}"))?;
            if !canonical.starts_with(&self.canonical) || canonical != path {
                return Err("workspace path escaped or aliased its project root".to_owned());
            }
            fs::File::open(canonical).map_err(|error| format!("open workspace file: {error}"))
        }
    }

    #[cfg(windows)]
    fn revision_relative(&self, relative: &Path) -> Result<FileSystemRevision, String> {
        if relative.as_os_str().is_empty() {
            return self.revision();
        }
        let segments = windows_relative_segments(relative)?;
        self.directory
            .revision_relative(&segments)
            .map(file_system_revision_from_windows)
            .map_err(|error| format!("stat opened workspace entry: {error}"))
    }
}

#[cfg(windows)]
fn windows_relative_segments(relative: &Path) -> Result<Vec<&str>, String> {
    let mut segments = Vec::new();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err("project path contains a non-normal component".to_owned());
        };
        let name = name
            .to_str()
            .ok_or_else(|| "project path component is not valid UTF-8".to_owned())?;
        if name.is_empty() || name.contains(['/', '\\']) {
            return Err("project path component is invalid".to_owned());
        }
        segments.push(name);
    }
    if segments.is_empty() {
        return Err("project path is empty".to_owned());
    }
    Ok(segments)
}

#[cfg(windows)]
fn open_canonical_project_root(root: &Path) -> Result<(PathBuf, ProjectRoot), String> {
    let requested = ProjectRoot::open(root)?;
    let requested_revision = requested.revision()?;
    let canonical = root
        .canonicalize()
        .map_err(|error| format!("resolve project root {}: {error}", root.display()))?;
    let opened = ProjectRoot::open(&canonical)?;
    if opened.revision()? != requested_revision {
        return Err("project root changed while it was being opened".to_owned());
    }
    Ok((canonical, opened))
}

impl CompilerWorkspaceSnapshot {
    fn root_binding_is_current(&self) -> bool {
        let requested_resolves_to_root =
            self.requested_root.canonicalize().ok().as_deref() == Some(self.root.as_path());
        let expected_revision = self
            .entries
            .iter()
            .find(|entry| entry.path.is_empty())
            .map(|entry| entry.revision);
        let opened_revision = ProjectRoot::open(&self.root)
            .and_then(|root| root.revision())
            .ok();
        requested_resolves_to_root
            && expected_revision.is_some()
            && opened_revision == expected_revision
    }

    /// Opens and inventories every path admitted by the versioned workspace
    /// policy. The root itself is represented by an empty relative path.
    pub(super) fn open(root: &Path) -> Result<Self, String> {
        Self::open_with_cancellation(root, None)
    }

    pub(super) fn open_with_cancellation(
        root: &Path,
        cancellation: Option<&AtomicBool>,
    ) -> Result<Self, String> {
        check_scan_cancellation(cancellation)?;
        let requested_root = absolute_path_spelling(root)?;
        #[cfg(not(windows))]
        {
            let requested_metadata = fs::symlink_metadata(&requested_root).map_err(|error| {
                format!(
                    "stat compiler workspace {}: {error}",
                    requested_root.display()
                )
            })?;
            if requested_metadata.file_type().is_symlink() {
                return Err("compiler workspace root cannot be a symlink".to_owned());
            }
        }
        #[cfg(windows)]
        let (root, root_capability) = open_canonical_project_root(&requested_root)?;
        #[cfg(not(windows))]
        let root = requested_root.canonicalize().map_err(|error| {
            format!(
                "open compiler workspace {}: {error}",
                requested_root.display()
            )
        })?;
        #[cfg(not(windows))]
        if !root.is_dir() {
            return Err(format!(
                "compiler workspace {} is not a directory",
                root.display()
            ));
        }
        let policy = source_selection_policy();
        #[cfg(not(windows))]
        let root_capability = ProjectRoot::open(&root)?;
        let entries =
            capture_compiler_workspace_entries(&root, &policy, &root_capability, cancellation)?;
        check_scan_cancellation(cancellation)?;
        if entries.first().map(|entry| entry.revision) != Some(root_capability.revision()?) {
            return Err("compiler workspace root changed during inventory capture".to_owned());
        }
        #[cfg(windows)]
        if open_canonical_project_root(&root)?.1.revision()? != root_capability.revision()? {
            return Err("compiler workspace root changed during inventory capture".to_owned());
        }
        let fence_digest =
            compiler_workspace_fence_digest(&entries, COMPILER_WORKSPACE_POLICY_IDENTITY);
        Ok(Self {
            requested_root,
            root,
            root_capability,
            policy,
            entries,
            fence_digest,
        })
    }

    /// Stable versioned identity for the exact ignore and generated-folder
    /// rules used to construct this snapshot.
    #[must_use]
    pub(super) const fn policy_identity(&self) -> &'static str {
        COMPILER_WORKSPACE_POLICY_IDENTITY
    }

    /// Returns the complete sorted inventory, including the root entry.
    #[must_use]
    pub(super) fn entries(&self) -> &[CompilerWorkspaceEntry] {
        &self.entries
    }

    /// Absolute spelling supplied for this workspace. Durable compiler work
    /// retains it so later recovery can detect if an alias resolves elsewhere;
    /// filesystem reads still use the canonical root capability.
    #[must_use]
    pub(super) fn root_path(&self) -> &Path {
        &self.requested_root
    }

    /// Digest pairing compiler authority evidence with this captured complete
    /// filesystem fence. Callers must still use [`Self::revalidate`] before
    /// selecting outputs.
    #[must_use]
    pub(super) const fn fence_digest(&self) -> [u8; 32] {
        self.fence_digest
    }

    /// Reads one captured regular file through the root capability, bounded by
    /// both the caller's limit and the captured length.
    pub(super) fn read_file(&self, relative: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
        if max_bytes > MAX_COMPILER_WORKSPACE_FILE_BYTES {
            return Err("workspace read bound exceeds the per-file limit".to_owned());
        }
        let expected_length = self.captured_file_length(relative)?;
        let expected_length_usize = usize::try_from(expected_length)
            .map_err(|_| "workspace file length exceeds this target".to_owned())?;
        if expected_length_usize > max_bytes {
            return Err("workspace file exceeds the requested read bound".to_owned());
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(expected_length_usize)
            .map_err(|_| "workspace read allocation exceeds memory".to_owned())?;
        let _ = self.stream_file(relative, max_bytes as u64, 64 * 1024, |chunk| {
            bytes.extend_from_slice(chunk);
            Ok(())
        })?;
        Ok(bytes)
    }

    /// Streams a captured regular file using bounded memory while pinning the
    /// opened file identity through EOF. Consumers should stage side effects
    /// until this method returns successfully because a mutation can be found
    /// after earlier chunks have already been delivered.
    pub(super) fn stream_file(
        &self,
        relative: &str,
        max_bytes: u64,
        chunk_bytes: usize,
        mut consume: impl FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<u64, String> {
        if max_bytes > MAX_COMPILER_WORKSPACE_STREAM_BYTES {
            return Err("workspace stream bound exceeds the per-file limit".to_owned());
        }
        if chunk_bytes == 0 || chunk_bytes > MAX_COMPILER_WORKSPACE_CHUNK_BYTES {
            return Err("workspace stream chunk size is outside the allowed bound".to_owned());
        }
        let expected_length = self.captured_file_length(relative)?;
        if expected_length > max_bytes {
            return Err("workspace file exceeds the requested stream bound".to_owned());
        }
        let entry = &self.entries[self
            .entries
            .binary_search_by(|entry| entry.path.as_str().cmp(relative))
            .map_err(|_| "workspace path is not in the captured inventory".to_owned())?];
        let mut file = self
            .root_capability
            .open_workspace_file(Path::new(relative))?;
        if file_system_revision_from_open_file(&file)? != entry.revision {
            return Err("workspace file changed after inventory capture".to_owned());
        }
        let mut buffer = Vec::new();
        buffer
            .try_reserve_exact(chunk_bytes)
            .map_err(|_| "workspace stream buffer allocation failed".to_owned())?;
        buffer.resize(chunk_bytes, 0);
        let mut total = 0_u64;
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|error| format!("read workspace file: {error}"))?;
            if read == 0 {
                break;
            }
            total = total
                .checked_add(u64::try_from(read).map_err(|_| "workspace read overflow".to_owned())?)
                .ok_or_else(|| "workspace read length overflow".to_owned())?;
            if total > expected_length || total > max_bytes {
                return Err("workspace file grew beyond its captured length".to_owned());
            }
            consume(&buffer[..read])?;
        }
        if total != expected_length || file_system_revision_from_open_file(&file)? != entry.revision
        {
            return Err("workspace file changed while it was streamed".to_owned());
        }
        Ok(total)
    }

    fn captured_file_length(&self, relative: &str) -> Result<u64, String> {
        let position = self
            .entries
            .binary_search_by(|entry| entry.path.as_str().cmp(relative))
            .map_err(|_| "workspace path is not in the captured inventory".to_owned())?;
        let entry = &self.entries[position];
        if entry.kind != CompilerWorkspaceEntryKind::File {
            return Err("workspace path does not name a regular file".to_owned());
        }
        entry
            .byte_length
            .ok_or_else(|| "workspace file has no captured length".to_owned())
    }

    /// Checks the exact complete path, kind, and filesystem-revision fence.
    /// `Ok(false)` means the admitted workspace changed; traversal failures
    /// remain errors so callers cannot mistake an incomplete scan for fresh.
    pub(super) fn revalidate(&self) -> Result<bool, String> {
        self.revalidate_with_cancellation(None)
    }

    pub(super) fn revalidate_with_cancellation(
        &self,
        cancellation: Option<&AtomicBool>,
    ) -> Result<bool, String> {
        check_scan_cancellation(cancellation)?;
        if !self.root_binding_is_current() {
            return Ok(false);
        }
        #[cfg(windows)]
        if open_canonical_project_root(&self.root)?.1.revision()?
            != self.root_capability.revision()?
        {
            return Ok(false);
        }
        let current = capture_compiler_workspace_entries(
            &self.root,
            &self.policy,
            &self.root_capability,
            cancellation,
        )?;
        #[cfg(windows)]
        if open_canonical_project_root(&self.root)?.1.revision()?
            != self.root_capability.revision()?
        {
            return Ok(false);
        }
        Ok(current == self.entries
            && compiler_workspace_fence_digest(&current, self.policy_identity())
                == self.fence_digest
            && current.first().map(|entry| entry.revision)
                == Some(self.root_capability.revision()?)
            && self.root_binding_is_current())
    }
}

fn compiler_workspace_fence_digest(
    entries: &[CompilerWorkspaceEntry],
    policy_identity: &str,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    #[cfg(windows)]
    hasher.update(b"backend.compiler-workspace-fence.v2\0");
    #[cfg(not(windows))]
    hasher.update(b"backend.compiler-workspace-fence.v1\0");
    hasher.update(policy_identity.as_bytes());
    hasher.update(&[0]);
    for entry in entries {
        hasher.update(
            &u64::try_from(entry.path.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        hasher.update(entry.path.as_bytes());
        hasher.update(&[match entry.kind {
            CompilerWorkspaceEntryKind::Directory => 0,
            CompilerWorkspaceEntryKind::File => 1,
        }]);
        match entry.byte_length {
            Some(length) => {
                hasher.update(&[1]);
                hasher.update(&length.to_be_bytes());
            }
            None => {
                hasher.update(&[0]);
            }
        };
        hasher.update(&entry.revision.length.to_be_bytes());
        match entry.revision.modified {
            Some(modified) => {
                hasher.update(&[1]);
                match modified.duration_since(UNIX_EPOCH) {
                    Ok(duration) => {
                        hasher.update(&[0]);
                        hasher.update(&duration.as_nanos().to_be_bytes());
                    }
                    Err(error) => {
                        hasher.update(&[1]);
                        hasher.update(&error.duration().as_nanos().to_be_bytes());
                    }
                }
            }
            None => {
                hasher.update(&[0]);
            }
        };
        #[cfg(unix)]
        {
            for value in [
                entry.revision.unix.0,
                entry.revision.unix.1,
                entry.revision.unix.2 as u64,
                entry.revision.unix.3 as u64,
            ] {
                hasher.update(&value.to_be_bytes());
            }
        }
        #[cfg(windows)]
        {
            hasher.update(&entry.revision.windows.volume_serial_number.to_be_bytes());
            hasher.update(&entry.revision.windows.file_id);
            hasher.update(&entry.revision.windows.change_time.to_be_bytes());
            hasher.update(&entry.revision.windows.last_write_time.to_be_bytes());
            hasher.update(&[u8::from(entry.revision.windows.is_directory)]);
            hasher.update(&entry.revision.windows.number_of_links.to_be_bytes());
        }
    }
    *hasher.finalize().as_bytes()
}

fn capture_compiler_workspace_entries(
    root: &Path,
    policy: &DiscoveryPolicy,
    _root_capability: &ProjectRoot,
    cancellation: Option<&AtomicBool>,
) -> Result<Vec<CompilerWorkspaceEntry>, String> {
    let mut entries = Vec::new();
    #[cfg(windows)]
    let root_revision = _root_capability.revision()?;
    #[cfg(not(windows))]
    let root_revision = file_system_revision(root)
        .ok_or_else(|| "compiler workspace root metadata is unavailable".to_owned())?;
    entries.push(CompilerWorkspaceEntry {
        path: String::new(),
        kind: CompilerWorkspaceEntryKind::Directory,
        byte_length: None,
        revision: root_revision,
    });
    let mut budget = DiscoveryBudget::default();
    let mut directory_widths = BTreeMap::<String, usize>::new();
    let mut case_keys = BTreeSet::new();
    let mut canonical_paths = BTreeSet::new();
    for result in policy.clone().walk_workspace_entries(root) {
        check_scan_cancellation(cancellation)?;
        let discovered = result.map_err(|error| error.to_string())?;
        if discovered.path() == root {
            continue;
        }
        let relative = discovered
            .path()
            .strip_prefix(root)
            .map_err(|_| "workspace entry escaped its root".to_owned())?;
        let relative = normalized_workspace_path(relative)?;
        let parent = relative.rsplit_once('/').map_or("", |(parent, _)| parent);
        let width = directory_widths.entry(parent.to_owned()).or_default();
        budget.admit_entry(*width)?;
        *width = width.saturating_add(1);
        if !case_keys.insert(relative.to_lowercase()) {
            return Err(format!(
                "workspace contains a case-colliding path: {relative}"
            ));
        }
        let path = root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
        let canonical = path
            .canonicalize()
            .map_err(|error| format!("resolve workspace entry {}: {error}", path.display()))?;
        if canonical != path || !canonical.starts_with(root) || !canonical_paths.insert(canonical) {
            return Err(format!("workspace contains a path alias: {relative}"));
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("stat workspace entry {}: {error}", path.display()))?;
        let kind = match discovered.kind() {
            EntryKind::Directory if metadata.is_dir() => CompilerWorkspaceEntryKind::Directory,
            EntryKind::File if metadata.is_file() => CompilerWorkspaceEntryKind::File,
            EntryKind::Symlink => {
                return Err(format!("workspace contains a symlink: {relative}"));
            }
            EntryKind::Other => {
                return Err(format!(
                    "workspace contains a special filesystem entry: {relative}"
                ));
            }
            _ => {
                return Err(format!(
                    "workspace entry kind changed during discovery: {relative}"
                ));
            }
        };
        #[cfg(windows)]
        let revision = _root_capability.revision_relative(Path::new(&relative))?;
        #[cfg(not(windows))]
        let revision = file_system_revision(&path)
            .ok_or_else(|| format!("workspace metadata is unavailable: {relative}"))?;
        #[cfg(windows)]
        if revision.windows.is_directory != (kind == CompilerWorkspaceEntryKind::Directory) {
            return Err(format!(
                "workspace entry kind changed during capture: {relative}"
            ));
        }
        let byte_length = match kind {
            CompilerWorkspaceEntryKind::Directory => None,
            CompilerWorkspaceEntryKind::File => Some(revision.length),
        };
        entries.push(CompilerWorkspaceEntry {
            path: relative,
            kind,
            byte_length,
            revision,
        });
    }
    check_scan_cancellation(cancellation)?;
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(entries)
}

fn normalized_workspace_path(relative: &Path) -> Result<String, String> {
    let mut components = Vec::new();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            return Err("workspace path contains a non-normal component".to_owned());
        };
        let component = component
            .to_str()
            .ok_or_else(|| "workspace path is not valid UTF-8".to_owned())?;
        if component.is_empty()
            || component.contains(['/', '\\'])
            || component.chars().any(char::is_control)
            || component.nfc().collect::<String>() != component
        {
            return Err(format!("workspace path is not normalized: {component:?}"));
        }
        components.push(component);
    }
    if components.is_empty() {
        return Err("workspace root path must use the empty relative path".to_owned());
    }
    Ok(components.join("/"))
}

/// Reads one already-opened regular file within the per-file byte bound.
///
/// Every failure here is a fact about the file rather than the project, so it
/// is classified rather than propagated as a scan error.
fn read_bounded(mut file: fs::File, maximum_bytes: usize) -> Result<Vec<u8>, SourceFault> {
    let metadata = file
        .metadata()
        .map_err(|_| SourceFault::Unavailable(SourceUnavailableReason::Unreadable))?;
    if !metadata.is_file() {
        return Err(SourceFault::Vanished);
    }
    let capacity = usize::try_from(metadata.len())
        .ok()
        .filter(|length| *length <= maximum_bytes)
        .ok_or(SourceFault::Unavailable(SourceUnavailableReason::TooLarge))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| SourceFault::Fatal("source allocation exceeds memory".to_owned()))?;
    let bound = u64::try_from(maximum_bytes)
        .map_err(|_| SourceFault::Fatal("input bound exceeds this target".to_owned()))?;
    if file
        .by_ref()
        .take(bound.saturating_add(1))
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Err(SourceFault::UnavailableRead(
            SourceUnavailableReason::Unreadable,
            bytes.len(),
        ));
    }
    if bytes.len() > maximum_bytes {
        return Err(SourceFault::UnavailableRead(
            SourceUnavailableReason::TooLarge,
            bytes.len(),
        ));
    }
    Ok(bytes)
}

/// What one file's failure means for the project scan.
///
/// Only a failure that invalidates the whole scan may stop it.  A file that
/// cannot be read, decoded, or parsed is a fact about that file: it keeps its
/// place in the project frontier with a typed reason, and every other file
/// still indexes.  Before this split, one unreadable byte anywhere made a
/// large checkout permanently unindexable.
#[derive(Clone, Debug, Eq, PartialEq)]
enum SourceFault {
    /// The file disappeared between discovery and read; drop it silently
    /// because the project no longer contains it.
    Vanished,
    /// The file is present but nothing could be extracted from it.
    Unavailable(SourceUnavailableReason),
    /// The file was read, but its bytes could not be extracted. Retain the
    /// number of bytes actually returned by the bounded read for measurement.
    UnavailableRead(SourceUnavailableReason, usize),
    /// The scan itself cannot continue.
    Fatal(String),
}

impl SourceFault {
    fn from_open(kind: std::io::ErrorKind) -> Self {
        match kind {
            std::io::ErrorKind::NotFound => Self::Vanished,
            _ => Self::Unavailable(SourceUnavailableReason::Unreadable),
        }
    }
}

#[derive(Default)]
struct DiscoveryBudget {
    entries: usize,
}

impl DiscoveryBudget {
    fn admit_entry(&mut self, directory_entries: usize) -> Result<(), String> {
        if directory_entries >= MAX_DIRECTORY_ENTRIES {
            return Err("directory exceeds the bounded discovery width".to_owned());
        }
        self.entries = self
            .entries
            .checked_add(1)
            .ok_or_else(|| "project discovery entry count overflow".to_owned())?;
        if self.entries > MAX_DISCOVERY_ENTRIES {
            return Err("project contains too many filesystem entries".to_owned());
        }
        Ok(())
    }
}

struct ProductFrontend {
    baseline: SyntaxFrontend,
    producer_version: [u8; 32],
}

struct FrontendSet {
    values: [ProductFrontend; 7],
}

impl FrontendSet {
    fn build() -> Result<Self, String> {
        let baselines = [
            backend_frontend_rust::syntax_frontend(),
            backend_frontend_python::syntax_frontend(),
            backend_frontend_typescript::syntax_frontend(),
            backend_frontend_go::syntax_frontend(),
            backend_frontend_java::syntax_frontend(),
            backend_frontend_csharp::syntax_frontend(),
            backend_frontend_clang::syntax_frontend(),
        ]
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
        let values = baselines
            .into_iter()
            .map(|baseline| {
                let producer_version = producer_version(&baseline);
                ProductFrontend {
                    baseline,
                    producer_version,
                }
            })
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| "frontend registry has the wrong cardinality".to_owned())?;
        Ok(Self { values })
    }

    fn for_path(&self, path: &Path) -> Option<&ProductFrontend> {
        self.values
            .iter()
            .find(|frontend| frontend.baseline.supports_path(path))
    }

    fn for_language(&self, language: SourceLanguage) -> Option<&ProductFrontend> {
        self.values
            .iter()
            .find(|frontend| frontend.language() == language)
    }
}

impl ProductFrontend {
    fn language(&self) -> SourceLanguage {
        self.baseline.language()
    }

    const fn analysis_version(&self) -> [u8; 32] {
        self.producer_version
    }
}

pub(super) fn semantic_capabilities(
    baseline: &backend_engine::CapabilityInventory,
    compiler: &backend_engine::application::LocalCompilerClient,
) -> Result<backend_engine::CapabilityInventory, String> {
    let frontends = FrontendSet::build()?;
    let target = backend_engine::CapabilityTarget::Native {
        os: native_os(),
        architecture: native_architecture(),
    };
    let mut rows = baseline
        .as_slice()
        .iter()
        .map(|status| match status.family() {
            backend_engine::CapabilityFamily::LanguageOracle { profile, task } => {
                let compiler_capability = compiler.capabilities().for_profile(profile);
                return match compiler_capability.state() {
                    backend_engine::application::LocalCompilerCapabilityState::Probing => {
                        backend_engine::CapabilityStatus::probing(status.id(), status.family())
                    }
                    backend_engine::application::LocalCompilerCapabilityState::ProbeFailed => {
                        backend_engine::CapabilityStatus::unavailable(
                            status.id(),
                            status.family(),
                            backend_engine::CapabilityUnavailable::ProbeFailed,
                        )
                    }
                    backend_engine::application::LocalCompilerCapabilityState::Unavailable => {
                        *status
                    }
                    backend_engine::application::LocalCompilerCapabilityState::Ready => {
                        let Some(manifest) = compiler_capability.manifest() else {
                            return *status;
                        };
                        let (Some(toolchain_identity), Some(package_authority)) = (
                            compiler_capability.toolchain_identity(),
                            compiler_capability.local_authority_fingerprint(),
                        ) else {
                            return *status;
                        };
                        let package_authority =
                            backend_engine::PackageAuthorityIdentity::LocalConfiguration(
                                package_authority,
                            );
                        let toolchain = compiler_capability.toolchain();
                        let protocol_abi = backend_compile::NATIVE_PAYLOAD_VERSION;
                        backend_engine::CapabilityStatus::observed(
                            status.id(),
                            status.family(),
                            manifest,
                            target,
                            protocol_abi,
                            backend_engine::CapabilityAuthority::Compiler {
                                recipe: backend_engine::compiler_authority_recipe(
                                    manifest,
                                    profile,
                                    task,
                                    protocol_abi,
                                    toolchain,
                                    toolchain_identity,
                                    package_authority,
                                ),
                                toolchain,
                                toolchain_identity,
                                package_authority,
                            },
                            backend_engine::CapabilityLifecycle::Active,
                        )
                    }
                };
            }
            backend_engine::CapabilityFamily::StructuralFrontend { profile } => {
                let manifest = frontends
                    .for_language(profile.language())
                    .map(ProductFrontend::analysis_version);
                let Some(manifest) = manifest else {
                    return *status;
                };
                backend_engine::CapabilityStatus::observed(
                    status.id(),
                    status.family(),
                    manifest,
                    target,
                    backend_compile::NATIVE_PAYLOAD_VERSION,
                    backend_engine::CapabilityAuthority::Structural { producer: manifest },
                    backend_engine::CapabilityLifecycle::Ready,
                )
            }
            backend_engine::CapabilityFamily::Embedding { .. } => *status,
        })
        .collect::<Vec<_>>();
    rows.sort_unstable_by_key(backend_engine::CapabilityStatus::id);
    backend_engine::CapabilityInventory::try_new(rows).map_err(|error| error.to_string())
}

const fn native_os() -> u8 {
    if cfg!(target_os = "macos") {
        1
    } else if cfg!(target_os = "linux") {
        2
    } else if cfg!(target_os = "windows") {
        3
    } else {
        0
    }
}

const fn native_architecture() -> u8 {
    if cfg!(target_arch = "aarch64") {
        1
    } else if cfg!(target_arch = "x86_64") {
        2
    } else {
        0
    }
}

fn producer_version(frontend: &SyntaxFrontend) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.product-structural-baseline.v1\0");
    hasher.update(&frontend.producer().to_bytes());
    *hasher.finalize().as_bytes()
}

fn frontends() -> Result<&'static FrontendSet, String> {
    static FRONTENDS: OnceLock<FrontendSet> = OnceLock::new();
    if let Some(frontends) = FRONTENDS.get() {
        return Ok(frontends);
    }
    let candidate = FrontendSet::build()?;
    let _ = FRONTENDS.set(candidate);
    FRONTENDS
        .get()
        .ok_or_else(|| "frontend registry failed to initialize".to_owned())
}

fn scan_source_paths(
    root: &Path,
    root_capability: &ProjectRoot,
    paths: &[PathBuf],
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    frontends: &FrontendSet,
    delta: Option<&source_frontier::SourceDelta>,
    source_policy: SourceAdmissionPolicy,
    cancellation: Option<&AtomicBool>,
) -> Result<Vec<ScannedFile>, String> {
    let workers = source_policy.worker_count(
        thread::available_parallelism().map_or(1, usize::from),
        paths.len(),
    );
    // The stop flag is owned by the caller of the scoped workers, so its
    // borrow outlives every join without shared ownership or a mutex.
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        let queue = workers
            .checked_mul(RESULT_QUEUE_PER_WORKER)
            .ok_or_else(|| "source result queue width overflow".to_owned())?;
        let (sender, receiver) = mpsc::sync_channel(queue);
        let mut handles = Vec::with_capacity(workers);
        let mut acknowledgements = Vec::with_capacity(workers);
        // Stride adjacent path positions across workers. Contiguous chunks
        // plus ordered acknowledgements would leave all but the first chunk
        // idle; this bounded window permits each worker to progress while
        // retaining at most one unacknowledged result per worker.
        for worker_index in 0..workers {
            let root = root;
            let root_capability = root_capability;
            let sender = sender.clone();
            let cancellation = cancellation;
            let stop = &stop;
            let (acknowledge, acknowledged) = mpsc::sync_channel::<bool>(1);
            acknowledgements.push(acknowledge);
            handles.push(scope.spawn(move || {
                for (path_index, path) in
                    paths.iter().enumerate().skip(worker_index).step_by(workers)
                {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        scan_source_path(
                            root,
                            root_capability,
                            path,
                            project,
                            reusable,
                            frontends,
                            delta,
                            source_policy,
                            cancellation,
                        )
                    }))
                    .unwrap_or_else(|_| Err("source analysis worker panicked".to_owned()));
                    let failed = result.is_err();
                    if sender.send((path_index, worker_index, result)).is_err() {
                        break;
                    }
                    if !acknowledged.recv().unwrap_or(false) || failed {
                        break;
                    }
                }
            }));
        }
        drop(sender);
        let mut output =
            Vec::with_capacity(paths.len().min(source_policy.limits().max_project_records));
        let mut failure = None;
        let mut pending = BTreeMap::<usize, (usize, Result<Option<ScannedFile>, String>)>::new();
        let mut next_path_index = 0_usize;
        let mut budget = SourceAdmissionLedger::default();
        for (path_index, worker_index, result) in receiver {
            if failure.is_some() {
                let _ = acknowledgements[worker_index].send(false);
                continue;
            }
            if let Err(error) = check_scan_cancellation(cancellation) {
                failure = Some(error);
                stop.store(true, Ordering::Release);
                let _ = acknowledgements[worker_index].send(false);
                for (_, (pending_worker, _)) in std::mem::take(&mut pending) {
                    let _ = acknowledgements[pending_worker].send(false);
                }
                continue;
            }
            pending.insert(path_index, (worker_index, result));
            loop {
                let Some((pending_worker, result)) = pending.remove(&next_path_index) else {
                    break;
                };
                // Cancellation may arrive while an earlier path is admitted.
                // Poll each result already held in the ordered window too.
                match check_scan_cancellation(cancellation).and_then(|()| result) {
                    Ok(Some(file)) => {
                        let charge = source_policy.admit_actual_file(file.source_bytes).and_then(
                            |admitted| {
                                budget.admit_with_fact_pages(
                                    source_policy,
                                    admitted,
                                    file.encoded_record_bytes,
                                    file.encoded_fact_bytes,
                                    false,
                                )
                            },
                        );
                        match charge {
                            Ok(()) => output.push(file),
                            Err(refusal) => failure = Some(refusal.to_string()),
                        }
                    }
                    Ok(None) => {}
                    Err(error) => failure = Some(error),
                }
                if failure.is_some() {
                    stop.store(true, Ordering::Release);
                    let _ = acknowledgements[pending_worker].send(false);
                    for (_, (waiting_worker, _)) in std::mem::take(&mut pending) {
                        let _ = acknowledgements[waiting_worker].send(false);
                    }
                    break;
                }
                if acknowledgements[pending_worker].send(true).is_err() {
                    failure =
                        Some("source analysis worker stopped before acknowledgement".to_owned());
                    stop.store(true, Ordering::Release);
                    for (_, (waiting_worker, _)) in std::mem::take(&mut pending) {
                        let _ = acknowledgements[waiting_worker].send(false);
                    }
                    break;
                }
                next_path_index = next_path_index
                    .checked_add(1)
                    .ok_or_else(|| "source path index overflow".to_owned())?;
            }
        }
        for handle in handles {
            if handle.join().is_err() && failure.is_none() {
                failure = Some("source analysis worker panicked".to_owned());
            }
        }
        if failure.is_none() && next_path_index != paths.len() {
            failure = Some("source analysis workers did not account for every path".to_owned());
        }
        if let Some(error) = failure {
            return Err(error);
        }
        output.sort_by(|left, right| left.relative.cmp(&right.relative));
        Ok(output)
    })
}

fn scan_source_path(
    root: &Path,
    root_capability: &ProjectRoot,
    path: &Path,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    frontends: &FrontendSet,
    delta: Option<&source_frontier::SourceDelta>,
    source_policy: SourceAdmissionPolicy,
    cancellation: Option<&AtomicBool>,
) -> Result<Option<ScannedFile>, String> {
    check_scan_cancellation(cancellation)?;
    if let Some(cached) = delta
        .filter(|delta| delta.is_current())
        .and_then(|delta| delta.unchanged.get(path))
    {
        let cached_length_admitted = usize::try_from(cached.revision.length)
            .ok()
            .is_some_and(|length| source_policy.admit_actual_file(length).is_ok());
        if cached_length_admitted {
            let record = reusable
                .get(&cached.key)
                .ok_or_else(|| "source frontier lost its exact CAS row".to_owned())?;
            let source_fact_identity = record
                .file_fields()
                .and_then(|fields| fields.source_identity)
                .ok_or_else(|| {
                    "source frontier row lost its exact source-fact identity".to_owned()
                })?;
            let profile = profile_fault(frontends, path).map_err(|fault| match fault {
                SourceFault::Unavailable(_)
                | SourceFault::UnavailableRead(_, _)
                | SourceFault::Vanished => "source frontier profile changed".to_owned(),
                SourceFault::Fatal(error) => error,
            })?;
            let source_bytes = usize::try_from(cached.revision.length)
                .map_err(|_| "source byte length exceeds this target".to_owned())?;
            let mut file = reused_scanned_file(
                cached.relative_path.clone(),
                cached.key,
                record.clone(),
                source_bytes,
                0,
                cached.content,
                profile,
                source_fact_identity,
                Some(cached.encoded_record_bytes),
            )?;
            file.encoded_fact_bytes = cached.encoded_fact_bytes;
            return Ok(Some(file));
        }
    }
    scan_file(
        root,
        root_capability,
        path,
        project,
        reusable,
        frontends,
        source_policy,
    )
}

/// Recovers one declaration excerpt from its already indexed source file.
///
/// This only serves bytes that still match the indexed source identity. It
/// reads through the confined project-root capability and reruns the admitted
/// baseline parser, retaining its declaration matching and excerpt bounds.
pub(super) fn recover_indexed_excerpt(
    root: &Path,
    path: &str,
    expected_source: backend_version::ContentId<backend_version::SourceFactDomain>,
    language: SourceLanguage,
    label: &str,
    line: u32,
    kind: Option<DeclarationKind>,
) -> Option<SourceExcerpt> {
    if path.is_empty() || path.contains('\\') {
        return None;
    }
    let relative = Path::new(path);
    if relative
        .components()
        .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return None;
    }
    let source_policy = SourceAdmissionPolicy::from_environment().ok()?;
    let bytes = ProjectRoot::open(root)
        .ok()?
        .read(relative, source_policy.limits().max_file_source_bytes)
        .ok()?;
    source_policy.admit_actual_file(bytes.len()).ok()?;
    if backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(&bytes)
        != expected_source
    {
        return None;
    }
    let frontend = frontends().ok()?.for_language(language)?;
    let analysis = frontend.baseline.analyze(relative, &bytes).ok()?;
    let name = label.rsplit("::").next()?;
    let mut matches = analysis.declarations().iter().filter(|declaration| {
        declaration.location().path() == path
            && declaration.location().start_line() == line
            && declaration.name() == name
            && kind.is_none_or(|kind| declaration.kind() == kind)
    });
    let declaration = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    match declaration.source_excerpt() {
        SourceExcerpt::Captured { .. } => Some(declaration.source_excerpt().clone()),
        SourceExcerpt::NotCaptured | SourceExcerpt::NotHydrated | SourceExcerpt::Unconfigured => {
            None
        }
    }
}

/// Reads supported sources and reuses prior analyses behind exact content and
/// producer-version fences.
pub(super) fn scan_project(
    coordinate: &str,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
) -> Result<IndexSnapshot, String> {
    scan_project_with_configuration_policy(
        coordinate,
        project,
        reusable,
        source_selection_policy(),
        true,
    )
}

/// Scans one project for compiler lanes that cannot prove a complete input
/// read set. Bounded configuration observations still contribute to freshness;
/// observing them does not prove dynamic or negative reads or authorize reuse.
pub(super) fn scan_project_for_unproven_authorities(
    coordinate: &str,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
) -> Result<IndexSnapshot, String> {
    scan_project_with_configuration_policy(
        coordinate,
        project,
        reusable,
        source_selection_policy(),
        true,
    )
}

/// Cancellable variant used by asynchronous local index jobs. Cancellation is
/// checked during directory discovery, source analysis, and configuration
/// capture so a large project scan can stop without cancelling compiler work.
pub(super) fn scan_project_for_unproven_authorities_cancellable(
    coordinate: &str,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    cancellation: &AtomicBool,
) -> Result<IndexSnapshot, String> {
    let source_policy = SourceAdmissionPolicy::from_environment()?;
    check_scan_cancellation(Some(cancellation))?;
    scan_project_with_configuration_policy_attempt(
        coordinate,
        project,
        reusable,
        source_selection_policy(),
        true,
        true,
        Some(cancellation),
        source_policy,
    )
}

/// Reads supported sources under one shared discovery policy.
///
/// Keeping the policy as an argument makes the same selection contract usable
/// by local ingest and future package/archive graph adapters.  The default
/// entry point above preserves the production behavior for existing callers.
pub(super) fn scan_project_with_policy(
    coordinate: &str,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    discovery: DiscoveryPolicy,
) -> Result<IndexSnapshot, String> {
    scan_project_with_configuration_policy(coordinate, project, reusable, discovery, true)
}

fn scan_project_with_configuration_policy(
    coordinate: &str,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    discovery: DiscoveryPolicy,
    capture_configuration_contents: bool,
) -> Result<IndexSnapshot, String> {
    let source_policy = SourceAdmissionPolicy::from_environment()?;
    scan_project_with_configuration_policy_attempt(
        coordinate,
        project,
        reusable,
        discovery,
        capture_configuration_contents,
        true,
        None,
        source_policy,
    )
}

fn scan_project_with_configuration_policy_attempt(
    coordinate: &str,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    discovery: DiscoveryPolicy,
    capture_configuration_contents: bool,
    allow_frontier_reuse: bool,
    cancellation: Option<&AtomicBool>,
    source_policy: SourceAdmissionPolicy,
) -> Result<IndexSnapshot, String> {
    check_scan_cancellation(cancellation)?;
    let requested_root = absolute_path_spelling(Path::new(coordinate))?;
    let root = requested_root
        .canonicalize()
        .map_err(|error| format!("open project {coordinate}: {error}"))?;
    if !root.is_dir() {
        return Err(format!("project {} is not a directory", root.display()));
    }
    let root_capability = ProjectRoot::open(&root)?;
    let frontends = frontends()?;
    let frontier_policy_allowed = discovery == source_selection_policy();
    let mut project_paths = project_paths_with_policy(
        &root,
        &root_capability,
        Some(frontends),
        discovery.clone(),
        cancellation,
    )?;
    check_scan_cancellation(cancellation)?;
    let mut paths = std::mem::take(&mut project_paths.sources);
    paths.sort();
    let directories = std::mem::take(&mut project_paths.directories);
    let (local_policy_paths, mut local_policy_paths_complete) =
        match source_frontier::source_policy_candidates(&root, &directories) {
            Some(paths) => (paths, true),
            None => (Vec::new(), false),
        };
    let mut file_revisions = BTreeMap::<PathBuf, CompilerFileRevision>::new();
    for path in &paths {
        let relative = path
            .strip_prefix(&root)
            .map_err(|_| "discovered source escaped its project root".to_owned())?
            .to_path_buf();
        file_revisions.insert(
            relative.clone(),
            CompilerFileRevision {
                relative_path: relative,
                metadata: project_paths.file_revisions.get(path).copied().flatten(),
            },
        );
    }
    for (_, path) in &project_paths.configurations {
        let relative = path
            .strip_prefix(&root)
            .map_err(|_| "discovered compiler configuration escaped its project root".to_owned())?
            .to_path_buf();
        file_revisions
            .entry(relative.clone())
            .or_insert_with(|| CompilerFileRevision {
                relative_path: relative,
                metadata: project_paths.file_revisions.get(path).copied().flatten(),
            });
    }
    for (path, metadata) in &project_paths.file_revisions {
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(source_frontier::is_policy_file_name)
        {
            let relative = path
                .strip_prefix(&root)
                .map_err(|_| "discovered policy file escaped its project root".to_owned())?
                .to_path_buf();
            file_revisions
                .entry(relative.clone())
                .or_insert_with(|| CompilerFileRevision {
                    relative_path: relative,
                    metadata: *metadata,
                });
        }
    }
    for path in &local_policy_paths {
        let relative = path
            .strip_prefix(&root)
            .map_err(|_| "discovered policy file escaped its project root".to_owned())?
            .to_path_buf();
        let metadata = file_system_revision(path);
        local_policy_paths_complete &= metadata.is_some();
        file_revisions
            .entry(relative.clone())
            .or_insert_with(|| CompilerFileRevision {
                relative_path: relative,
                metadata,
            });
    }
    let revision_fence = CompilerRevisionFence {
        requested_root,
        root: root.clone(),
        directories: directories.clone(),
        files: file_revisions.values().cloned().collect(),
    };
    let frontier_sources = paths
        .iter()
        .filter_map(|path| {
            let frontend = frontends.for_path(path)?;
            let relative = path
                .strip_prefix(&root)
                .ok()?
                .to_str()?
                .replace(std::path::MAIN_SEPARATOR, "/");
            Some(source_frontier::SourcePath {
                absolute: path.clone(),
                relative,
                language: frontend.language(),
                analysis: frontend.analysis_version(),
                revision: project_paths.file_revisions.get(path).copied().flatten(),
            })
        })
        .collect::<Vec<_>>();
    let mut configuration_paths = project_paths
        .configurations
        .iter()
        .map(|(_, path)| path.clone())
        .collect::<Vec<_>>();
    configuration_paths.extend(project_paths.file_revisions.keys().filter_map(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(source_frontier::is_policy_file_name)
            .then(|| path.clone())
    }));
    configuration_paths.sort();
    configuration_paths.dedup();
    let frontier_paths_complete = frontier_sources.len() == paths.len();
    let wanted_paths = frontier_paths_complete
        .then(|| {
            source_frontier::source_wanted_paths(&root, &frontier_sources, &configuration_paths)
        })
        .flatten();
    let git_before =
        (frontier_policy_allowed && frontier_paths_complete && local_policy_paths_complete)
            .then(|| {
                wanted_paths.as_ref().and_then(|wanted| {
                    source_frontier::git_source_state(&root, wanted, &local_policy_paths)
                })
            })
            .flatten();
    let source_delta = if frontier_policy_allowed && allow_frontier_reuse {
        git_before.as_ref().and_then(|state| {
            source_frontier::source_delta(
                &root,
                project,
                &frontier_sources,
                &configuration_paths,
                directories.iter().all(|(_, revision)| revision.is_some()),
                reusable,
                state,
                source_policy.identity(),
                |path| frontends.for_path(path).is_some(),
            )
        })
    } else {
        None
    };
    preflight_source_bytes(&paths, source_policy, cancellation)?;
    let mut scanned = scan_source_paths(
        &root,
        &root_capability,
        &paths,
        project,
        reusable,
        frontends,
        source_delta.as_ref(),
        source_policy,
        cancellation,
    )?;
    let source_bytes_read = scanned
        .iter()
        .map(|file| file.source_bytes_read)
        .sum::<usize>();
    let cache_witness = git_before;
    if source_delta.is_some() {
        #[cfg(test)]
        source_frontier::trigger_source_frontier_race(&root);
        let after = wanted_paths.as_ref().and_then(|wanted| {
            source_frontier::git_source_state(&root, wanted, &local_policy_paths)
        });
        let stable = cache_witness
            .as_ref()
            .zip(after.as_ref())
            .is_some_and(|(before, after)| {
                source_frontier::git_states_same_during_scan(before, after)
            });
        if !stable
            || !compiler_revision_is_current_with_cancellation(&revision_fence, cancellation)?
        {
            let mut full = scan_project_with_configuration_policy_attempt(
                coordinate,
                project,
                reusable,
                discovery,
                capture_configuration_contents,
                false,
                cancellation,
                source_policy,
            )?;
            full.source_bytes_read = full.source_bytes_read.saturating_add(source_bytes_read);
            return Ok(full);
        }
    }
    scanned.sort_by(|left, right| left.relative.cmp(&right.relative));

    let mut source = blake3::Hasher::new();
    source.update(b"backend.project-snapshot.v2\0");
    let mut files = Vec::with_capacity(scanned.len());
    let mut source_facts = Vec::with_capacity(scanned.len());
    let mut encoded_fact_bytes = Vec::with_capacity(scanned.len());
    let mut compiler_sources = Vec::with_capacity(scanned.len());
    let mut reused_compiler_files = Vec::with_capacity(scanned.len());
    for scanned in scanned {
        let ScannedFile {
            key,
            record,
            compiler_source,
            reused_compiler,
            source_facts: facts,
            source_bytes_read: _,
            encoded_fact_bytes: fact_bytes,
            ..
        } = scanned;
        let file = record
            .file_fields()
            .ok_or_else(|| "frontend produced a non-file record".to_owned())?;
        source.update(&key);
        source.update(&file.content_version);
        source.update(&file.analysis_version);
        encoded_fact_bytes.push((key, fact_bytes));
        files.push((key, record));
        source_facts.extend(facts);
        compiler_sources.extend(compiler_source);
        reused_compiler_files.extend(reused_compiler);
    }
    files.sort_by_key(|(key, _)| *key);
    encoded_fact_bytes.sort_unstable_by_key(|(key, _)| *key);
    let relevant_languages = compiler_sources
        .iter()
        .map(|source| source.profile.language())
        .chain(
            reused_compiler_files
                .iter()
                .map(|source| source.profile.language()),
        )
        .collect();
    let compiler_configuration = if capture_configuration_contents {
        read_compiler_configuration_snapshot(
            &root,
            &root_capability,
            project_paths.configurations,
            project_paths.incomplete_configurations,
            relevant_languages,
            cancellation,
        )?
    } else {
        CompilerConfigurationSnapshot::default()
    };
    let snapshot = IndexSnapshot {
        source_version: *source.finalize().as_bytes(),
        files,
        source_facts,
        encoded_fact_bytes,
        compiler_sources,
        reused_compiler_files,
        compiler_configuration,
        revision_fence,
        source_bytes_read,
        source_admission_policy: source_policy,
    };
    if let (Some(before), Some(wanted)) = (cache_witness.as_ref(), wanted_paths.as_ref()) {
        let after = source_frontier::git_source_state(&root, wanted, &local_policy_paths);
        if after.as_ref().is_some_and(|after| {
            source_frontier::git_states_same_during_scan(before, after)
                && compiler_revision_is_current(&snapshot.revision_fence).unwrap_or(false)
        }) {
            source_frontier::remember_snapshot_frontier(&root, project, &snapshot, before);
        }
    }
    Ok(snapshot)
}

/// Revalidates the scanner's source/configuration revision without a second
/// recursive discovery pass or another source-content read.
pub(super) fn compiler_revision_is_current(fence: &CompilerRevisionFence) -> Result<bool, String> {
    compiler_revision_is_current_with_cancellation(fence, None)
}

pub(super) fn compiler_revision_is_current_with_cancellation(
    fence: &CompilerRevisionFence,
    cancellation: Option<&AtomicBool>,
) -> Result<bool, String> {
    if fence.open_bound_root(&fence.requested_root).is_err() {
        return Ok(false);
    }
    #[cfg(windows)]
    {
        let root_capability = match ProjectRoot::open(&fence.root) {
            Ok(root) => root,
            Err(_) => return Ok(false),
        };
        for (relative, expected) in &fence.directories {
            check_scan_cancellation(cancellation)?;
            if expected.is_none() || root_capability.revision_relative(relative).ok() != *expected {
                return Ok(false);
            }
        }
        for file in &fence.files {
            check_scan_cancellation(cancellation)?;
            if file.metadata.is_none()
                || root_capability.revision_relative(&file.relative_path).ok() != file.metadata
            {
                return Ok(false);
            }
        }
        Ok(fence.open_bound_root(&fence.requested_root).is_ok())
    }
    #[cfg(not(windows))]
    {
        for (relative, expected) in &fence.directories {
            check_scan_cancellation(cancellation)?;
            let path = if relative.as_os_str().is_empty() {
                fence.root.clone()
            } else {
                fence.root.join(relative)
            };
            if expected.is_none() || file_system_revision(&path) != *expected {
                return Ok(false);
            }
        }
        for file in &fence.files {
            check_scan_cancellation(cancellation)?;
            let path = fence.root.join(&file.relative_path);
            if file.metadata.is_none() || file_system_revision(&path) != file.metadata {
                return Ok(false);
            }
        }
        Ok(fence.open_bound_root(&fence.requested_root).is_ok())
    }
}

#[cfg(not(windows))]
fn file_system_revision(path: &Path) -> Option<FileSystemRevision> {
    let metadata = fs::symlink_metadata(path).ok()?;
    Some(file_system_revision_from_metadata(&metadata))
}

/// Reads the revision of the object `path` names, walking every component
/// relative to a held handle. A reparse point anywhere, including the final
/// component, makes the revision unavailable rather than describing its target.
#[cfg(windows)]
fn file_system_revision(path: &Path) -> Option<FileSystemRevision> {
    backend_platform::win32::project_fs::revision_of_path(path)
        .ok()
        .map(file_system_revision_from_windows)
}

#[cfg(not(windows))]
fn file_system_revision_from_metadata(metadata: &fs::Metadata) -> FileSystemRevision {
    #[cfg(unix)]
    let unix = {
        use std::os::unix::fs::MetadataExt as _;
        (
            metadata.dev(),
            metadata.ino(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        )
    };
    FileSystemRevision {
        length: metadata.len(),
        modified: metadata.modified().ok(),
        #[cfg(unix)]
        unix,
    }
}

#[cfg(windows)]
fn file_system_revision_from_windows(
    revision: backend_platform::win32::project_fs::FileRevision,
) -> FileSystemRevision {
    FileSystemRevision {
        length: revision.length,
        modified: None,
        windows: revision,
    }
}

fn file_system_revision_from_open_file(file: &fs::File) -> Result<FileSystemRevision, String> {
    #[cfg(unix)]
    {
        let metadata = file
            .metadata()
            .map_err(|error| format!("stat opened workspace file: {error}"))?;
        Ok(file_system_revision_from_metadata(&metadata))
    }
    #[cfg(windows)]
    {
        backend_platform::win32::project_fs::revision_for_file(file)
            .map(file_system_revision_from_windows)
            .map_err(|error| format!("stat opened workspace file: {error}"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let metadata = file
            .metadata()
            .map_err(|error| format!("stat opened workspace file: {error}"))?;
        Ok(file_system_revision_from_metadata(&metadata))
    }
}

fn supported_paths(root: &Path, frontends: &FrontendSet) -> Result<Vec<PathBuf>, String> {
    supported_paths_with_policy(root, frontends, source_selection_policy())
}

fn supported_paths_with_policy(
    root: &Path,
    frontends: &FrontendSet,
    discovery: DiscoveryPolicy,
) -> Result<Vec<PathBuf>, String> {
    let root_capability = ProjectRoot::open(root)?;
    Ok(
        project_paths_with_policy(root, &root_capability, Some(frontends), discovery, None)?
            .sources,
    )
}

struct ProjectPaths {
    sources: Vec<PathBuf>,
    configurations: Vec<(Language, PathBuf)>,
    directories: Vec<(PathBuf, Option<FileSystemRevision>)>,
    file_revisions: BTreeMap<PathBuf, Option<FileSystemRevision>>,
    incomplete_configurations: BTreeSet<Language>,
}

fn project_paths_with_policy(
    root: &Path,
    _root_capability: &ProjectRoot,
    frontends: Option<&FrontendSet>,
    discovery: DiscoveryPolicy,
    cancellation: Option<&AtomicBool>,
) -> Result<ProjectPaths, String> {
    let mut sources = Vec::new();
    let mut configurations = Vec::new();
    #[cfg(windows)]
    let root_revision = Some(_root_capability.revision()?);
    #[cfg(not(windows))]
    let root_revision = file_system_revision(root);
    let mut directories = vec![(PathBuf::new(), root_revision)];
    let mut file_revisions = BTreeMap::new();
    let mut configuration_counts = BTreeMap::<Language, usize>::new();
    let mut incomplete_configurations = BTreeSet::new();
    let mut budget = DiscoveryBudget::default();
    let mut current_directory = None;
    let mut directory_entries = 0_usize;
    for entry in discover_source_entries(root, discovery) {
        check_scan_cancellation(cancellation)?;
        let entry = entry.map_err(|error| error.to_string())?;
        if entry.path() == root {
            continue;
        }
        let parent = entry.path().parent().unwrap_or(root);
        if current_directory.as_deref() != Some(parent) {
            current_directory = Some(parent.to_owned());
            directory_entries = 0;
        }
        budget
            .admit_entry(directory_entries)
            .map_err(|error| format!("{error}: {}", entry.path().display()))?;
        directory_entries = directory_entries.saturating_add(1);
        if entry.is_file() {
            let mut revision_path = entry
                .path()
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(source_frontier::is_policy_file_name);
            if frontends.is_some_and(|frontends| frontends.for_path(entry.path()).is_some()) {
                if sources.len() >= ProductSourceRecord::MAX_PROJECT_FILES {
                    return Err("project contains too many supported source files".to_owned());
                }
                sources.push(entry.path().to_owned());
                revision_path = true;
            }
            if let Some(language) = compiler_configuration_language(entry.path()) {
                revision_path = true;
                let count = configuration_counts.entry(language).or_default();
                if *count >= MAX_COMPILER_CONFIGURATION_FILES_PER_LANGUAGE {
                    incomplete_configurations.insert(language);
                } else {
                    configurations.push((language, entry.path().to_owned()));
                }
                *count = count.saturating_add(1);
            }
            if revision_path {
                let _relative = entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|_| "discovered file escaped its project root".to_owned())?;
                #[cfg(windows)]
                let revision = _root_capability.revision_relative(_relative).ok();
                #[cfg(not(windows))]
                let revision = file_system_revision(entry.path());
                file_revisions.insert(entry.path().to_owned(), revision);
            }
        } else if entry.is_directory() {
            let relative = entry
                .path()
                .strip_prefix(root)
                .map_err(|_| "discovered directory escaped its project root".to_owned())?
                .to_path_buf();
            #[cfg(windows)]
            let revision = _root_capability.revision_relative(&relative).ok();
            #[cfg(not(windows))]
            let revision = file_system_revision(entry.path());
            directories.push((relative, revision));
        }
    }
    Ok(ProjectPaths {
        sources,
        configurations,
        directories,
        file_revisions,
        incomplete_configurations,
    })
}

fn read_compiler_configuration_snapshot(
    root: &Path,
    root_capability: &ProjectRoot,
    configurations: Vec<(Language, PathBuf)>,
    incomplete: BTreeSet<Language>,
    relevant_languages: BTreeSet<Language>,
    cancellation: Option<&AtomicBool>,
) -> Result<CompilerConfigurationSnapshot, String> {
    let mut complete_languages = relevant_languages
        .into_iter()
        .filter(|language| !incomplete.contains(language))
        .collect::<BTreeSet<_>>();
    let mut used_bytes = BTreeMap::<Language, usize>::new();
    let mut files = Vec::with_capacity(configurations.len());
    for (language, path) in configurations {
        check_scan_cancellation(cancellation)?;
        if !complete_languages.contains(&language) {
            continue;
        }
        let relative_path = path
            .strip_prefix(root)
            .map_err(|_| "compiler configuration escaped its project root".to_owned())?;
        let used = used_bytes.get(&language).copied().unwrap_or_default();
        let remaining = MAX_COMPILER_CONFIGURATION_BYTES_PER_LANGUAGE.saturating_sub(used);
        let maximum = MAX_COMPILER_CONFIGURATION_FILE_BYTES.min(remaining);
        let Ok(bytes) = root_capability.read_compiler_configuration(relative_path, maximum) else {
            complete_languages.remove(&language);
            continue;
        };
        *used_bytes.entry(language).or_default() = used.saturating_add(bytes.len());
        files.push(CompilerConfigurationFile {
            language,
            relative_path: relative_path.to_path_buf(),
            content: typed_of::<InputContentSchema>(&bytes).to_bytes(),
        });
    }
    files.sort_by(|left, right| {
        (left.language, &left.relative_path).cmp(&(right.language, &right.relative_path))
    });
    Ok(CompilerConfigurationSnapshot {
        files,
        complete_languages,
    })
}

fn compiler_languages() -> [Language; 7] {
    [
        Language::Rust,
        Language::TypeScript,
        Language::Python,
        Language::Go,
        Language::Java,
        Language::CSharp,
        Language::Clang,
    ]
}

fn compiler_configuration_language(path: &Path) -> Option<Language> {
    let name = path.file_name()?.to_str()?;
    if matches!(
        name,
        "Cargo.toml" | "Cargo.lock" | "rust-toolchain" | "rust-toolchain.toml" | "build.rs"
    ) || matches!(name, "config" | "config.toml")
        && path
            .parent()
            .is_some_and(|parent| parent.ends_with(".cargo"))
    {
        return Some(Language::Rust);
    }
    if matches!(
        name,
        "package.json"
            | "package-lock.json"
            | "npm-shrinkwrap.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "bun.lock"
            | "bun.lockb"
            | ".npmrc"
            | ".yarnrc.yml"
            | "deno.json"
            | "deno.jsonc"
    ) || ["tsconfig", "jsconfig"]
        .iter()
        .any(|prefix| name.starts_with(prefix) && name.ends_with(".json"))
        || [
            "vite.config.",
            "webpack.config.",
            "rollup.config.",
            "esbuild.config.",
            "babel.config.",
            "jest.config.",
            "vitest.config.",
        ]
        .iter()
        .any(|prefix| name.starts_with(prefix))
        || name.starts_with(".babelrc")
    {
        return Some(Language::TypeScript);
    }
    if matches!(
        name,
        "pyproject.toml"
            | "Pipfile"
            | "Pipfile.lock"
            | "poetry.lock"
            | "uv.lock"
            | "setup.cfg"
            | "setup.py"
            | "tox.ini"
            | "pyrefly.toml"
            | "pyrightconfig.json"
            | "mypy.ini"
    ) || name.starts_with("requirements") && (name.ends_with(".txt") || name.ends_with(".in"))
    {
        return Some(Language::Python);
    }
    if matches!(name, "go.mod" | "go.sum" | "go.work" | "go.work.sum")
        || name == "modules.txt"
            && path
                .parent()
                .is_some_and(|parent| parent.ends_with("vendor"))
    {
        return Some(Language::Go);
    }
    if matches!(
        name,
        "pom.xml"
            | "build.gradle"
            | "build.gradle.kts"
            | "settings.gradle"
            | "settings.gradle.kts"
            | "gradle.lockfile"
            | "gradle.properties"
            | "gradle-wrapper.properties"
    ) {
        return Some(Language::Java);
    }
    let lower = name.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "directory.build.props"
            | "directory.build.targets"
            | "directory.packages.props"
            | "packages.lock.json"
            | "nuget.config"
            | "global.json"
    ) || lower.ends_with(".csproj")
        || lower.ends_with(".sln")
        || lower.ends_with(".slnx")
    {
        return Some(Language::CSharp);
    }
    if matches!(
        name,
        "compile_commands.json"
            | "compile_flags.txt"
            | ".clang-tidy"
            | "CMakeLists.txt"
            | "Makefile"
    ) {
        return Some(Language::Clang);
    }
    None
}

/// Charges the aggregate source budget before any file is read.
///
/// A file that is oversized or unreadable is not a reason to refuse the
/// project: it is charged as zero bytes here and reported per file by
/// [`scan_file`].  Only the aggregate budget, which protects the process
/// rather than describing a file, can still stop the scan.
fn preflight_source_bytes(
    paths: &[PathBuf],
    source_policy: SourceAdmissionPolicy,
    cancellation: Option<&AtomicBool>,
) -> Result<(), String> {
    let mut total = 0_usize;
    for path in paths {
        check_scan_cancellation(cancellation)?;
        let Ok(metadata) = fs::metadata(path) else {
            continue;
        };
        let Ok(length) = usize::try_from(metadata.len()) else {
            continue;
        };
        if length > source_policy.limits().max_file_source_bytes {
            continue;
        }
        total = total
            .checked_add(length)
            .ok_or_else(|| "source byte count overflow".to_owned())?;
    }
    source_policy
        .admit_preflight(total, paths.len())
        .map_err(|refusal| refusal.to_string())
}

/// Scans one discovered file into a row, or into a typed per-file fault.
///
/// `Ok(None)` means the file vanished between discovery and read, so the
/// project no longer contains it and the frontier simply omits it.
fn scan_file(
    root: &Path,
    root_capability: &ProjectRoot,
    path: &Path,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    frontends: &FrontendSet,
    source_policy: SourceAdmissionPolicy,
) -> Result<Option<ScannedFile>, String> {
    match scan_one(
        root,
        root_capability,
        path,
        project,
        reusable,
        frontends,
        source_policy,
    ) {
        Ok(file) => Ok(Some(file)),
        Err(SourceFault::Vanished) => Ok(None),
        Err(SourceFault::Fatal(message)) => {
            Err(format!("read source {}: {message}", path.display()))
        }
        Err(SourceFault::Unavailable(reason)) => {
            unavailable_file(root, path, project, frontends, reason, 0).map(Some)
        }
        Err(SourceFault::UnavailableRead(reason, bytes_read)) => {
            unavailable_file(root, path, project, frontends, reason, bytes_read).map(Some)
        }
    }
}

/// Builds the row for a file whose contents could not be extracted.
fn unavailable_file(
    root: &Path,
    path: &Path,
    project: [u8; 32],
    frontends: &FrontendSet,
    reason: SourceUnavailableReason,
    source_bytes_read: usize,
) -> Result<ScannedFile, String> {
    let relative = relative_coordinate(root, path)?;
    let frontend = frontends
        .for_path(path)
        .ok_or_else(|| "unsupported source language".to_owned())?;
    let key = product_source_file_key(project, &relative);
    let record = ProductSourceRecord::file_unavailable(
        project,
        relative.clone(),
        frontend.language(),
        frontend.analysis_version(),
        reason,
    )?;
    let mut encoded = Vec::new();
    ProductSourceRelation::encode_value(&record, &mut encoded);
    Ok(ScannedFile {
        // An unavailable source must not be turned into an empty compiler
        // artifact.  The relation row carries the typed terminal and the
        // semantic lane must retain partial coverage until a later scan can
        // read the real bytes.
        compiler_source: None,
        reused_compiler: None,
        relative,
        key,
        record,
        source_bytes: 0,
        source_bytes_read,
        encoded_record_bytes: encoded.len(),
        encoded_fact_bytes: 0,
        source_facts: None,
    })
}

fn relative_coordinate(root: &Path, path: &Path) -> Result<String, String> {
    Ok(path
        .strip_prefix(root)
        .map_err(|_| "source path escaped its project root".to_owned())?
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/"))
}

fn scan_one(
    root: &Path,
    root_capability: &ProjectRoot,
    path: &Path,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    frontends: &FrontendSet,
    source_policy: SourceAdmissionPolicy,
) -> Result<ScannedFile, SourceFault> {
    let relative_path = path
        .strip_prefix(root)
        .map_err(|_| SourceFault::Fatal("source path escaped its project root".to_owned()))?;
    let bytes =
        root_capability.read(relative_path, source_policy.limits().max_file_source_bytes)?;
    let source_bytes = bytes.len();
    source_policy.admit_actual_file(source_bytes).map_err(|_| {
        SourceFault::UnavailableRead(SourceUnavailableReason::TooLarge, source_bytes)
    })?;
    let relative = relative_path
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    let frontend = frontends
        .for_path(path)
        .ok_or_else(|| SourceFault::Fatal("unsupported source language".to_owned()))?;
    let profile = match profile_fault(frontends, path) {
        Ok(profile) => profile,
        Err(SourceFault::Unavailable(reason)) => {
            return Err(SourceFault::UnavailableRead(reason, bytes.len()));
        }
        Err(error) => return Err(error),
    };
    let key = product_source_file_key(project, &relative);
    let content = typed_of::<InputContentSchema>(&bytes).to_bytes();
    let analysis = frontend.analysis_version();
    let reusable_record = reusable.get(&key).filter(|record| {
        record.file_fields().is_some_and(|fields| {
            fields.project == project
                && fields.path == relative
                && fields.language == frontend.language()
                && fields.content_version == content
                // An unavailable row stores a zero content version because its
                // bytes were never admitted. A real digest is never that value,
                // and this fence keeps such a row from becoming compiler input.
                && fields.content_version != [0; 32]
                && fields.analysis_version == analysis
                && fields.source_identity.is_some()
        })
    });

    // Move the bounded read buffer into its UTF-8 owner to avoid a second
    // per-file source allocation. The scan-wide source, retained-text, row,
    // and in-flight budgets are separate policy limits.
    let source = String::from_utf8(bytes).map_err(|error| {
        SourceFault::UnavailableRead(SourceUnavailableReason::NotText, error.as_bytes().len())
    })?;
    // Structural parsing is an explicit baseline projection for local browsing.
    // Package semantics are compiled and published by the engine application module.
    let analyzed = frontend
        .baseline
        .analyze(Path::new(&relative), source.as_bytes())
        .map_err(|_| {
            SourceFault::UnavailableRead(SourceUnavailableReason::Unparsed, source_bytes)
        })?;
    debug_assert_eq!(analyzed.language(), frontend.language());
    debug_assert_eq!(analyzed.content().to_bytes(), content);
    // Facts are produced from the full frontend result before the compact
    // source relation applies its bounded-row retention policy. A reused
    // compact source row is never treated as a complete facts source.
    let source_fact_identity = SourceFactIdentity::from_canonical_bytes(source.as_bytes());
    let source_facts = build_product_source_file_facts(
        project,
        relative.clone(),
        analyzed.language(),
        analyzed.content().to_bytes(),
        analysis,
        source_fact_identity,
        analyzed.declarations(),
    )
    .map_err(SourceFault::Fatal)?;
    // A real source file routinely extracts more detail than one canonical
    // relation row can carry: `memchr 2.8.3` alone produces a 65 686 byte row
    // for `src/arch/x86_64/avx2/memchr.rs` against a 65 464 byte capacity.
    // Constructing through the capacity-aware path sheds derived detail in a
    // fixed order and records how far it had to go, instead of failing the
    // whole project when the relation delta is later prepared.
    let record = if let Some(record) = reusable_record {
        record.clone()
    } else {
        // Bind identity before compaction: its final wire format also encodes
        // containment per declaration, so reserving only a header is insufficient.
        ProductSourceRecord::identified_file_within_row_capacity(
            project,
            relative.clone(),
            analyzed.language(),
            analyzed.content().to_bytes(),
            analysis,
            analyzed.declarations().clone(),
            source_fact_identity,
        )
        .map_err(SourceFault::Fatal)?
    };
    if reusable_record.is_some() {
        let mut scanned = reused_scanned_file(
            relative,
            key,
            record,
            source_bytes,
            source_bytes,
            content,
            profile,
            source_fact_identity,
            None,
        )
        .map_err(SourceFault::Fatal)?;
        scanned.encoded_fact_bytes = source_facts.encoded_bytes();
        scanned.source_facts = Some(source_facts);
        return Ok(scanned);
    }
    scanned_file(
        relative,
        key,
        record,
        source_facts,
        source_bytes,
        source_bytes,
        path,
        profile,
        content,
        source_fact_identity,
    )
    .map_err(SourceFault::Fatal)
}

fn scanned_file(
    relative: String,
    key: [u8; 32],
    record: ProductSourceRecord,
    source_facts: ProductSourceFileFactsUpdate,
    source_bytes: usize,
    source_bytes_read: usize,
    path: &Path,
    profile: LanguageProfile,
    content: [u8; 32],
    source_fact_identity: SourceFactIdentity,
) -> Result<ScannedFile, String> {
    finish_scanned_file(
        relative.clone(),
        key,
        record,
        source_bytes,
        source_bytes_read,
        path,
        Some(CompilerSourceHandle {
            profile,
            relative_path: relative,
            content,
            source_fact_identity,
        }),
        None,
        None,
        Some(source_facts),
    )
}

fn reused_scanned_file(
    relative: String,
    key: [u8; 32],
    record: ProductSourceRecord,
    source_bytes: usize,
    source_bytes_read: usize,
    content: [u8; 32],
    profile: LanguageProfile,
    source_fact_identity: SourceFactIdentity,
    encoded_record_bytes: Option<usize>,
) -> Result<ScannedFile, String> {
    let path = PathBuf::from(&relative);
    finish_scanned_file(
        relative.clone(),
        key,
        record,
        source_bytes,
        source_bytes_read,
        &path,
        None,
        Some(ReusedCompilerFile {
            profile,
            relative_path: relative,
            content,
            source_fact_identity,
        }),
        encoded_record_bytes,
        None,
    )
}

fn finish_scanned_file(
    relative: String,
    key: [u8; 32],
    record: ProductSourceRecord,
    source_bytes: usize,
    source_bytes_read: usize,
    path: &Path,
    compiler_source: Option<CompilerSourceHandle>,
    reused_compiler: Option<ReusedCompilerFile>,
    known_encoded_record_bytes: Option<usize>,
    source_facts: Option<ProductSourceFileFactsUpdate>,
) -> Result<ScannedFile, String> {
    let encoded_record_bytes = if let Some(size) = known_encoded_record_bytes {
        size
    } else {
        let mut encoded = Vec::new();
        ProductSourceRelation::encode_value(&record, &mut encoded);
        encoded.len()
    };
    if encoded_record_bytes > MAX_ENCODED_RECORD_BYTES {
        return Err(format!(
            "source projection {} exceeds the {} byte record limit",
            path.display(),
            MAX_ENCODED_RECORD_BYTES
        ));
    }
    Ok(ScannedFile {
        compiler_source,
        reused_compiler,
        relative,
        key,
        record,
        source_bytes,
        source_bytes_read,
        encoded_record_bytes,
        encoded_fact_bytes: source_facts
            .as_ref()
            .map_or(0, ProductSourceFileFactsUpdate::encoded_bytes),
        source_facts,
    })
}

/// Reopens fresh and reused source handles through [`ProjectRoot`]. Both paths
/// must still hash to the identity admitted during scanning before contiguous
/// UTF-8 strings are created for the compiler boundary. The immutable policy
/// comes from the same scan that created these handles; it is never re-read
/// from the environment midway through an index attempt.
pub(super) fn admit_compiler_sources_for_scan(
    root: &Path,
    revision_fence: &CompilerRevisionFence,
    fresh: Vec<CompilerSourceHandle>,
    reused: Vec<ReusedCompilerFile>,
    source_policy: SourceAdmissionPolicy,
    cancellation: Option<&AtomicBool>,
) -> Result<Vec<CompilerSource>, String> {
    check_scan_cancellation(cancellation)?;
    let capability = revision_fence.open_bound_root(root)?;
    let admitted = materialize_compiler_sources(
        &capability,
        fresh,
        reused,
        source_policy,
        cancellation,
        |_| {},
    )?;
    if !compiler_revision_is_current_with_cancellation(revision_fence, cancellation)? {
        return Err(
            "project source root changed during compiler-source materialization".to_owned(),
        );
    }
    check_scan_cancellation(cancellation)?;
    Ok(admitted)
}

#[cfg(test)]
pub(super) fn admit_compiler_sources_with_policy(
    root: &Path,
    fresh: Vec<CompilerSourceHandle>,
    reused: Vec<ReusedCompilerFile>,
    source_policy: SourceAdmissionPolicy,
) -> Result<Vec<CompilerSource>, String> {
    let canonical = root
        .canonicalize()
        .map_err(|error| format!("open project {}: {error}", root.display()))?;
    if !canonical.is_dir() {
        return Err(format!(
            "project {} is not a directory",
            canonical.display()
        ));
    }
    let capability = ProjectRoot::open(&canonical)?;
    materialize_compiler_sources(&capability, fresh, reused, source_policy, None, |_| {})
}

fn materialize_compiler_sources(
    capability: &ProjectRoot,
    fresh: Vec<CompilerSourceHandle>,
    reused: Vec<ReusedCompilerFile>,
    source_policy: SourceAdmissionPolicy,
    cancellation: Option<&AtomicBool>,
    mut after_source: impl FnMut(usize),
) -> Result<Vec<CompilerSource>, String> {
    let mut pending = BTreeMap::<String, (LanguageProfile, [u8; 32], SourceFactIdentity)>::new();
    for source in fresh {
        let path = source.relative_path.clone();
        if pending
            .insert(
                path.clone(),
                (source.profile, source.content, source.source_fact_identity),
            )
            .is_some()
        {
            return Err(format!("compiler source {path} was admitted twice"));
        }
    }
    for source in reused {
        let path = source.relative_path.clone();
        if pending
            .insert(
                path.clone(),
                (source.profile, source.content, source.source_fact_identity),
            )
            .is_some()
        {
            return Err(format!("compiler source {path} was admitted twice"));
        }
    }
    let mut admitted = Vec::with_capacity(pending.len());
    let mut retained_bytes = 0_usize;
    for (path, (profile, expected_content, expected_source_fact_identity)) in pending {
        check_scan_cancellation(cancellation)?;
        let bytes = capability
            .read(
                Path::new(&path),
                source_policy.limits().max_file_source_bytes,
            )
            .map_err(|error| reread_fault(&path, error))?;
        check_scan_cancellation(cancellation)?;
        source_policy
            .admit_actual_file(bytes.len())
            .map_err(|refusal| refusal.to_string())?;
        let content = typed_of::<InputContentSchema>(&bytes).to_bytes();
        if content != expected_content {
            return Err(format!(
                "source {path} changed after its content hash was admitted"
            ));
        }
        let source_fact_identity = SourceFactIdentity::from_canonical_bytes(&bytes);
        if source_fact_identity != expected_source_fact_identity {
            return Err(format!(
                "source {path} changed after its source-fact identity was admitted"
            ));
        }
        retained_bytes = source_policy
            .admit_retained_total(retained_bytes, bytes.len())
            .map_err(|refusal| refusal.to_string())?;
        check_scan_cancellation(cancellation)?;
        let source =
            String::from_utf8(bytes).map_err(|_| format!("source {path} is no longer UTF-8"))?;
        admitted.push(CompilerSource {
            relative_path: path,
            profile,
            source,
            content: expected_content,
            source_fact_identity,
        });
        after_source(admitted.len());
    }
    Ok(admitted)
}

/// Profiles that still have compiler input in this scan.
///
/// The set is taken from every fresh and reused file, before unchanged
/// profiles are dropped from the compile. A selected publication whose
/// profile is absent from this set no longer has source.
pub(super) fn live_compiler_profiles(
    source_root: &Path,
    fresh: &[CompilerSourceHandle],
    reused: &[ReusedCompilerFile],
) -> BTreeSet<LanguageProfile> {
    let mut live = BTreeSet::new();
    for source in fresh {
        live.insert(compilation_profile(
            source_root,
            &source.relative_path,
            source.profile,
        ));
    }
    for source in reused {
        live.insert(compilation_profile(
            source_root,
            &source.relative_path,
            source.profile,
        ));
    }
    live
}

/// Selected profiles that this scan no longer contains.
pub(super) fn retired_selected_profiles(
    selected: &[LanguageProfile],
    live: &BTreeSet<LanguageProfile>,
) -> BTreeSet<LanguageProfile> {
    selected
        .iter()
        .copied()
        .filter(|profile| !live.contains(profile))
        .collect()
}

/// The paths whose compiler text is still part of this scan.
pub(super) fn present_compiler_paths(
    fresh: &[CompilerSourceHandle],
    reused: &[ReusedCompilerFile],
) -> BTreeSet<String> {
    let mut present = BTreeSet::new();
    present.extend(fresh.iter().map(|source| source.relative_path.clone()));
    present.extend(reused.iter().map(|source| source.relative_path.clone()));
    present
}

/// Profiles that lost a previously compiled file.
///
/// A zero content version was never compiler input. A path that is still
/// fresh or reused is still part of its profile.
pub(super) fn lost_compiler_profiles(
    source_root: &Path,
    previous: &BTreeMap<[u8; 32], ProductSourceRecord>,
    present: &BTreeSet<String>,
) -> Result<BTreeSet<LanguageProfile>, String> {
    let mut lost = BTreeSet::new();
    for record in previous.values() {
        let Some(fields) = record.file_fields() else {
            continue;
        };
        if fields.content_version == [0; 32] || present.contains(fields.path) {
            continue;
        }
        let Some(profile) = source_profile(Path::new(fields.path))? else {
            continue;
        };
        lost.insert(compilation_profile(source_root, fields.path, profile));
    }
    Ok(lost)
}

/// Keeps compiler inputs whose profile must be compiled again.
///
/// A profile is compiled when one of its files is fresh or a previous file
/// of that profile disappeared. Every other profile is left out, so its text
/// is not read again and its package compiler is not invoked.
pub(super) fn select_compiler_inputs(
    source_root: &Path,
    fresh: Vec<CompilerSourceHandle>,
    reused: Vec<ReusedCompilerFile>,
    lost: &BTreeSet<LanguageProfile>,
) -> (Vec<CompilerSourceHandle>, Vec<ReusedCompilerFile>) {
    let mut dirty = lost.clone();
    let mut classified = Vec::with_capacity(fresh.len());
    for source in fresh {
        let profile = compilation_profile(source_root, &source.relative_path, source.profile);
        dirty.insert(profile);
        classified.push((source, profile));
    }
    let fresh = classified
        .into_iter()
        .filter(|(_, profile)| dirty.contains(profile))
        .map(|(source, _)| source)
        .collect();
    let reused = reused
        .into_iter()
        .filter(|source| {
            dirty.contains(&compilation_profile(
                source_root,
                &source.relative_path,
                source.profile,
            ))
        })
        .collect();
    (fresh, reused)
}

/// Selects the exact profile one source is compiled under.
///
/// A file's extension names its language, but a Rust file's edition is a
/// fact of the crate that owns it: the authority checks it against Cargo's
/// own metadata and refuses a mismatch. Compiling every `.rs` file as edition
/// 2024 therefore sent every 2015, 2018, and 2021 crate (most of crates.io)
/// to a terminal `ProjectAuthority` failure and a structural-only answer. The
/// edition is read from the nearest `Cargo.toml` with a `[package]` table
/// between the file and the source root, exactly as Cargo resolves it.
pub(super) fn compilation_profile(
    source_root: &Path,
    relative_path: &str,
    profile: LanguageProfile,
) -> LanguageProfile {
    let LanguageProfile::Rust(_) = profile else {
        return profile;
    };
    let mut directory = source_root.join(relative_path);
    while directory.pop() && directory.starts_with(source_root) {
        let manifest = directory.join("Cargo.toml");
        let declares_package = fs::read_to_string(&manifest)
            .is_ok_and(|contents| contents.lines().any(|line| line.trim() == "[package]"));
        if declares_package {
            // An unreadable or unknown edition keeps the default profile; the
            // authority then reports the exact mismatch as a typed terminal
            // rather than this scan failing the whole package.
            return backend_frontend_rust::legacy::manifest_edition(&directory)
                .map_or(profile, LanguageProfile::Rust);
        }
    }
    profile
}

fn reread_fault(path: &str, fault: SourceFault) -> String {
    match fault {
        SourceFault::Vanished => {
            format!("source {path} vanished after its content hash was admitted")
        }
        SourceFault::Unavailable(reason) => format!(
            "source {path} became unavailable ({}) after its content hash was admitted",
            reason.name()
        ),
        SourceFault::UnavailableRead(reason, _) => format!(
            "source {path} became unavailable ({}) after its content hash was admitted",
            reason.name()
        ),
        SourceFault::Fatal(message) => format!("reread source {path}: {message}"),
    }
}

/// Derives one file's semantic profile from the frontend that claimed it.
///
/// Discovery admits a file because some frontend claims its extension, so the
/// claimed set is the only table that decides which extensions exist.  This
/// function therefore never repeats that list: it asks the frontend set which
/// language owns the path and then picks a dialect, which is the one question
/// the extension still answers for the two languages that have two.
///
/// `None` means the path has no profile — either no frontend claims it, or the
/// language's dialects do not cover this extension.  Callers treat that as one
/// unavailable file, never as a broken project.
///
/// # Errors
/// Returns an error only when the frontend registry itself cannot be built.
pub(super) fn source_profile(path: &Path) -> Result<Option<LanguageProfile>, String> {
    Ok(profile_of(frontends()?, path))
}

/// Resolves a discovered path's profile, or the typed fault its absence is.
///
/// A claimed extension with no semantic profile is one odd file, not a broken
/// project: it is reported per file as
/// [`SourceUnavailableReason::Unparsed`] — nothing could be extracted — and
/// the scan continues.  Returning [`SourceFault::Fatal`] here is what made a
/// single `postcss.config.js` sink an entire checkout.
fn profile_fault(frontends: &FrontendSet, path: &Path) -> Result<LanguageProfile, SourceFault> {
    profile_of(frontends, path).ok_or(SourceFault::Unavailable(SourceUnavailableReason::Unparsed))
}

/// Resolves a path's profile against an explicit frontend set.
fn profile_of(frontends: &FrontendSet, path: &Path) -> Option<LanguageProfile> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)?;
    let language = frontends.for_path(path)?.language();
    dialect(language, &extension)
}

/// Returns the key the view uses to pair a publication with its files.
///
/// A path names its language and dialect, never a Rust crate's edition: the
/// edition belongs to the crate and is chosen when it is compiled. A
/// publication compiled as edition 2021 must still be recognized as the
/// semantic answer for the `.rs` files [`source_profile`] keys as the default
/// edition, so every Rust edition shares one lane key.
pub(super) const fn lane_profile(profile: LanguageProfile) -> LanguageProfile {
    match profile {
        LanguageProfile::Rust(_) => LanguageProfile::Rust(RustEdition::Rust2024),
        other => other,
    }
}

/// Selects the dialect a claimed extension names inside one language.
///
/// Only TypeScript and Clang carry two dialects; every other language has a
/// single profile, so its extensions cannot disagree.
fn dialect(language: SourceLanguage, extension: &str) -> Option<LanguageProfile> {
    Some(match language {
        SourceLanguage::Rust => LanguageProfile::Rust(RustEdition::Rust2024),
        SourceLanguage::Python => LanguageProfile::Python(PythonVersion::Python314),
        SourceLanguage::Go => LanguageProfile::Go(GoVersion::Go125),
        SourceLanguage::Java => LanguageProfile::Java(JavaRelease::Java25),
        SourceLanguage::CSharp => LanguageProfile::CSharp(CSharpVersion::CSharp14),
        SourceLanguage::TypeScript => LanguageProfile::TypeScript(match extension {
            "tsx" | "jsx" => TypeScriptSource::Tsx,
            _ => TypeScriptSource::TypeScript,
        }),
        SourceLanguage::Clang => match extension {
            "c" | "h" | "m" => LanguageProfile::C(CStandard::C23),
            "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" | "mm" => {
                LanguageProfile::Cxx(CxxStandard::Cxx23)
            }
            _ => return None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn cancellable_project_scan_stops_before_opening_the_workspace() {
        let cancellation = AtomicBool::new(true);
        let result = scan_project_for_unproven_authorities_cancellable(
            "workspace-cancelled-before-open",
            [17; 32],
            &BTreeMap::new(),
            &cancellation,
        );
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("a pre-cancelled scan must not open the workspace"),
        };
        assert_eq!(error, INDEX_SCAN_CANCELLED);
    }

    #[test]
    fn structural_frontend_inventory_has_exactly_one_parser_per_language() -> Result<(), String> {
        let frontends = FrontendSet::build()?;
        for language in SourceLanguage::ALL {
            let frontend = frontends
                .for_language(language)
                .ok_or_else(|| format!("missing {language:?} parser capability"))?;
            if frontend.analysis_version() == [0; 32] {
                return Err(format!(
                    "{language:?} parser has an empty producer identity"
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn discovery_budget_is_checked_before_an_entry_is_retained() {
        let mut global = DiscoveryBudget {
            entries: MAX_DISCOVERY_ENTRIES,
        };
        assert!(global.admit_entry(0).is_err());

        let mut directory = DiscoveryBudget::default();
        assert!(directory.admit_entry(MAX_DIRECTORY_ENTRIES).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn opened_project_root_rejects_symlink_substitution() -> Result<(), String> {
        use std::os::unix::fs::symlink;

        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        let unique = format!(
            "backend-confined-ingest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let scratch = Scratch(std::env::temp_dir().join(unique));
        let project = scratch.0.join("project");
        let outside = scratch.0.join("outside.rs");
        fs::create_dir_all(&project).map_err(|error| error.to_string())?;
        fs::write(&outside, b"pub fn secret() {}").map_err(|error| error.to_string())?;
        let root = ProjectRoot::open(&project)?;

        symlink(&outside, project.join("victim.rs")).map_err(|error| error.to_string())?;
        let maximum = SourceAdmissionLimits::default().max_file_source_bytes;
        assert!(root.read(Path::new("victim.rs"), maximum).is_err());

        fs::create_dir(project.join("real")).map_err(|error| error.to_string())?;
        fs::write(project.join("real/lib.rs"), b"pub fn safe() {}")
            .map_err(|error| error.to_string())?;
        // A confined read still succeeds; a substituted symlink is refused
        // rather than admitted, and the refusal is now a typed per-file fault.
        assert_eq!(
            root.read(Path::new("real/lib.rs"), maximum)
                .map_err(|error| format!("{error:?}"))?,
            b"pub fn safe() {}"
        );
        fs::remove_dir_all(project.join("real")).map_err(|error| error.to_string())?;
        symlink(&scratch.0, project.join("real")).map_err(|error| error.to_string())?;
        assert!(root.read(Path::new("real/outside.rs"), maximum).is_err());
        Ok(())
    }

    #[test]
    fn every_language_lane_uses_its_grammar_tags() -> Result<(), String> {
        let fixtures = [
            ("fixture.rs", "pub fn ferris() {}", "ferris"),
            ("fixture.py", "def monty():\n    pass", "monty"),
            ("fixture.ts", "export function turing() {}", "turing"),
            ("fixture.go", "func gopher() {}", "gopher"),
            ("fixture.java", "public class Duke {}", "Duke"),
            ("fixture.cs", "public class Anders {}", "Anders"),
            ("fixture.cpp", "struct Bjarne {};", "Bjarne"),
        ];
        for (path, source, expected) in fixtures {
            let frontend = frontends()?
                .for_path(Path::new(path))
                .ok_or_else(|| format!("missing frontend for {path}"))?;
            let found = frontend
                .baseline
                .analyze(Path::new(path), source.as_bytes())
                .map_err(|error| error.to_string())?;
            assert!(
                found
                    .declarations()
                    .iter()
                    .any(|item| item.name() == expected),
                "{path}: expected {expected}, found {:?}",
                found
                    .declarations()
                    .iter()
                    .map(|item| (item.kind(), item.name()))
                    .collect::<Vec<_>>()
            );
        }
        Ok(())
    }

    #[test]
    fn scanned_files_persist_the_semantic_source_content_identity() -> Result<(), String> {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let unique = format!(
            "backend-source-identity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let scratch = Scratch(std::env::temp_dir().join(unique));
        fs::create_dir_all(&scratch.0).map_err(|error| error.to_string())?;
        let source = b"pub fn ferris() {}";
        fs::write(scratch.0.join("lib.rs"), source).map_err(|error| error.to_string())?;
        let scan = scan_project(
            scratch.0.to_str().ok_or("non-UTF-8 scratch path")?,
            [9; 32],
            &BTreeMap::new(),
        )?;
        let fields = scan.files[0]
            .1
            .file_fields()
            .ok_or("expected a source file record")?;
        // The persisted identity must be exactly the construction the engine's
        // semantic compiler derives from the same bytes, so a view comparing
        // image provenance against file records compares like with like.
        let expected =
            backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                source,
            );
        assert_eq!(fields.source_identity, Some(expected));
        assert_ne!(
            fields.source_identity,
            Some(backend_version::ContentId::<
                backend_version::SourceFactDomain,
            >::from_canonical_bytes(b"edited in place")),
            "a distinct byte stream must not share the file's identity"
        );
        Ok(())
    }

    #[test]
    fn unchanged_files_reuse_the_exact_declaration_allocation() -> Result<(), String> {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        let unique = format!(
            "backend-syntax-reuse-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let scratch = Scratch(std::env::temp_dir().join(unique));
        fs::create_dir_all(&scratch.0).map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("lib.rs"), b"pub fn ferris() {}")
            .map_err(|error| error.to_string())?;
        let project = [7; 32];
        let first = scan_project(
            scratch.0.to_str().ok_or("non-UTF-8 scratch path")?,
            project,
            &BTreeMap::new(),
        )?;
        let reusable = first.files.iter().cloned().collect::<BTreeMap<_, _>>();
        let second = scan_project(
            scratch.0.to_str().ok_or("non-UTF-8 scratch path")?,
            project,
            &reusable,
        )?;
        let first_declarations = first.files[0]
            .1
            .file_fields()
            .ok_or("expected first source file")?
            .declarations;
        let second_declarations = second.files[0]
            .1
            .file_fields()
            .ok_or("expected second source file")?
            .declarations;
        assert_eq!(first.source_version, second.source_version);
        assert!(Arc::ptr_eq(first_declarations, second_declarations));
        assert!(
            second.compiler_sources.is_empty(),
            "an unchanged scan must drop compiler text"
        );
        assert_eq!(second.reused_compiler_files.len(), 1);
        Ok(())
    }

    fn retained_compiler_handle_bytes(scan: &IndexSnapshot) -> usize {
        scan.compiler_sources
            .iter()
            .map(|source| {
                std::mem::size_of::<CompilerSourceHandle>() + source.relative_path.capacity()
            })
            .sum()
    }

    fn declarations_for<'a>(
        scan: &'a IndexSnapshot,
        path: &str,
    ) -> Result<&'a Arc<[backend_engine::SourceDeclaration]>, String> {
        scan.files
            .iter()
            .find_map(|(_, record)| {
                let fields = record.file_fields()?;
                (fields.path == path).then_some(fields.declarations)
            })
            .ok_or_else(|| format!("missing source file {path}"))
    }

    #[test]
    fn reused_scan_drops_compiler_text_until_a_sibling_edit() -> Result<(), String> {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let unique = format!(
            "backend-ingest-delta-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let scratch = Scratch(std::env::temp_dir().join(unique));
        fs::create_dir_all(&scratch.0).map_err(|error| error.to_string())?;
        let root = scratch.0.to_str().ok_or("non-UTF-8 scratch path")?;
        let kept = "pub fn kept() {}";
        let edited_v1 = "pub fn edited() {}";
        let edited_v2 = "pub fn edited() { let _ = 1; }";
        let edited_v3 = "pub fn edited() { let _ = 3; }";
        let kept_v4 = "pub fn kept() { let _ = 4; }";
        fs::write(scratch.0.join("kept.rs"), kept).map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("edited.rs"), edited_v1).map_err(|error| error.to_string())?;
        let project = [11; 32];

        let cold = scan_project(root, project, &BTreeMap::new())?;
        assert_eq!(
            cold.compiler_sources
                .iter()
                .map(|source| source.relative_path.as_str())
                .collect::<Vec<_>>(),
            ["edited.rs", "kept.rs"]
        );
        assert!(cold.reused_compiler_files.is_empty());
        let cold_retained = retained_compiler_handle_bytes(&cold);
        assert!(cold_retained < edited_v1.len() + kept.len());

        let reusable = cold.files.iter().cloned().collect::<BTreeMap<_, _>>();
        let warm = scan_project(root, project, &reusable)?;
        let warm_retained = retained_compiler_handle_bytes(&warm);
        assert_eq!(warm_retained, 0);
        assert_eq!(warm.reused_compiler_files.len(), 2);
        assert_eq!(cold.source_version, warm.source_version);
        assert!(Arc::ptr_eq(
            declarations_for(&cold, "kept.rs")?,
            declarations_for(&warm, "kept.rs")?,
        ));
        assert!(Arc::ptr_eq(
            declarations_for(&cold, "edited.rs")?,
            declarations_for(&warm, "edited.rs")?,
        ));

        fs::write(scratch.0.join("edited.rs"), edited_v2).map_err(|error| error.to_string())?;
        let delta = scan_project(root, project, &reusable)?;
        assert_eq!(delta.compiler_sources.len(), 1);
        assert_eq!(delta.compiler_sources[0].relative_path, "edited.rs");
        assert_eq!(
            delta.compiler_sources[0].content,
            typed_of::<InputContentSchema>(edited_v2.as_bytes()).to_bytes()
        );
        assert_eq!(
            delta.compiler_sources[0].source_fact_identity,
            SourceFactIdentity::from_canonical_bytes(edited_v2.as_bytes())
        );
        assert_eq!(delta.reused_compiler_files.len(), 1);
        assert_eq!(delta.reused_compiler_files[0].relative_path, "kept.rs");
        assert_eq!(
            delta.reused_compiler_files[0].content,
            typed_of::<InputContentSchema>(kept.as_bytes()).to_bytes()
        );
        assert_eq!(
            delta.reused_compiler_files[0].source_fact_identity,
            SourceFactIdentity::from_canonical_bytes(kept.as_bytes())
        );
        assert!(Arc::ptr_eq(
            declarations_for(&warm, "kept.rs")?,
            declarations_for(&delta, "kept.rs")?,
        ));
        assert!(!Arc::ptr_eq(
            declarations_for(&warm, "edited.rs")?,
            declarations_for(&delta, "edited.rs")?,
        ));

        // Fresh and reused handles both reopen through the root capability,
        // compare the exact admitted content identity, then build text.
        let admitted = admit_compiler_sources_with_policy(
            scratch.0.as_path(),
            delta.compiler_sources.clone(),
            delta.reused_compiler_files.clone(),
            delta.source_admission_policy,
        )?;
        assert_eq!(
            admitted
                .iter()
                .map(|source| (source.relative_path.as_str(), source.source.as_str()))
                .collect::<Vec<_>>(),
            [("edited.rs", edited_v2), ("kept.rs", kept)]
        );
        assert_eq!(
            admitted[0].source_fact_identity,
            SourceFactIdentity::from_canonical_bytes(edited_v2.as_bytes())
        );
        assert_eq!(
            admitted[1].source_fact_identity,
            SourceFactIdentity::from_canonical_bytes(kept.as_bytes())
        );
        drop(admitted);

        fs::write(scratch.0.join("edited.rs"), edited_v3).map_err(|error| error.to_string())?;
        let Err(error) = admit_compiler_sources_with_policy(
            scratch.0.as_path(),
            delta.compiler_sources.clone(),
            delta.reused_compiler_files.clone(),
            delta.source_admission_policy,
        ) else {
            return Err("a changed fresh file was compiled".to_owned());
        };
        assert!(
            error.contains("edited.rs") && error.contains("changed after its content hash"),
            "{error}"
        );

        let delta_fresh_bytes = retained_compiler_handle_bytes(&delta);
        fs::write(scratch.0.join("edited.rs"), edited_v2).map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("kept.rs"), kept_v4).map_err(|error| error.to_string())?;
        let Err(error) = admit_compiler_sources_with_policy(
            scratch.0.as_path(),
            delta.compiler_sources,
            delta.reused_compiler_files,
            delta.source_admission_policy,
        ) else {
            return Err("a changed reused file was compiled".to_owned());
        };
        assert!(
            error.contains("kept.rs") && error.contains("changed after its content hash"),
            "{error}"
        );
        assert!(
            !error.contains("let _ = 4"),
            "the refusal must not echo the torn bytes: {error}"
        );
        eprintln!(
            "ingest_delta files=2 cold_source_handle_bytes={cold_retained} warm_source_handle_bytes={warm_retained} delta_source_handle_bytes={delta_fresh_bytes} delta_reused=1"
        );
        Ok(())
    }

    #[test]
    fn unavailable_bytes_are_not_reused_as_compiler_input() -> Result<(), String> {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let unique = format!(
            "backend-ingest-unavailable-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let scratch = Scratch(std::env::temp_dir().join(unique));
        fs::create_dir_all(&scratch.0).map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("lib.rs"), [0xff, 0xfe, b'n', b'o'])
            .map_err(|error| error.to_string())?;
        let root = scratch.0.to_str().ok_or("non-UTF-8 scratch path")?;
        let project = [12; 32];
        let first = scan_project(root, project, &BTreeMap::new())?;
        assert!(first.compiler_sources.is_empty());
        assert!(first.reused_compiler_files.is_empty());
        assert_eq!(first.files.len(), 1);
        let reusable = first.files.iter().cloned().collect::<BTreeMap<_, _>>();
        let second = scan_project(root, project, &reusable)?;
        assert!(second.compiler_sources.is_empty());
        assert!(second.reused_compiler_files.is_empty());
        Ok(())
    }

    #[test]
    fn admit_rejects_a_path_present_in_both_lists() -> Result<(), String> {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let unique = format!(
            "backend-ingest-duplicate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let scratch = Scratch(std::env::temp_dir().join(unique));
        fs::create_dir_all(&scratch.0).map_err(|error| error.to_string())?;
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let policy = SourceAdmissionPolicy::new(SourceAdmissionLimits::default())
            .map_err(|error| error.to_string())?;
        let source = b"pub fn a() {}";
        let source_fact_identity = SourceFactIdentity::from_canonical_bytes(source);
        let Err(error) = admit_compiler_sources_with_policy(
            scratch.0.as_path(),
            vec![CompilerSourceHandle {
                relative_path: "a.rs".to_owned(),
                profile,
                content: typed_of::<InputContentSchema>(source).to_bytes(),
                source_fact_identity,
            }],
            vec![ReusedCompilerFile {
                relative_path: "a.rs".to_owned(),
                profile,
                content: [9; 32],
                source_fact_identity,
            }],
            policy,
        ) else {
            return Err("one path was admitted as both fresh and reused".to_owned());
        };
        assert!(
            error.contains("a.rs") && error.contains("admitted twice"),
            "{error}"
        );
        Ok(())
    }

    #[test]
    fn indexed_source_excerpt_recovers_only_exact_confined_bytes() -> Result<(), String> {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        let unique = format!(
            "backend-indexed-source-recovery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let scratch = Scratch(std::env::temp_dir().join(unique));
        let root = scratch.0.join("pkg");
        fs::create_dir_all(root.join("src")).map_err(|error| error.to_string())?;
        let source = b"pub fn recovered() { 7 }\n";
        fs::write(root.join("src/value.rs"), source).map_err(|error| error.to_string())?;
        let identity =
            backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                source,
            );
        let label = "pkg:cargo/example@1.0.0::recovered";
        let recover = |path: &str, line, kind| {
            recover_indexed_excerpt(
                &root,
                path,
                identity,
                SourceLanguage::Rust,
                label,
                line,
                kind,
            )
        };
        assert!(matches!(
            recover("src/value.rs", 1, Some(DeclarationKind::Function)),
            Some(SourceExcerpt::Captured {
                ref text,
                extent: backend_compile::SourceExcerptExtent::Complete,
            })
                if text.as_ref() == "pub fn recovered() { 7 }"
        ));
        assert!(recover("src/value.rs", 2, Some(DeclarationKind::Function)).is_none());
        assert!(recover("src/value.rs", 1, Some(DeclarationKind::Struct)).is_none());
        assert!(recover("../outside.rs", 1, Some(DeclarationKind::Function)).is_none());

        fs::write(root.join("src/value.rs"), b"pub fn recovered() { 8 }\n")
            .map_err(|error| error.to_string())?;
        assert!(recover("src/value.rs", 1, Some(DeclarationKind::Function)).is_none());
        fs::remove_file(root.join("src/value.rs")).map_err(|error| error.to_string())?;
        assert!(recover("src/value.rs", 1, Some(DeclarationKind::Function)).is_none());

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let outside = scratch.0.join("outside.rs");
            fs::write(&outside, source).map_err(|error| error.to_string())?;
            symlink(&outside, root.join("src/escape.rs")).map_err(|error| error.to_string())?;
            assert!(recover("src/escape.rs", 1, Some(DeclarationKind::Function)).is_none());
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn admit_refuses_a_reused_file_replaced_by_a_symlink() -> Result<(), String> {
        use std::os::unix::fs::symlink;

        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let unique = format!(
            "backend-ingest-symlink-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let scratch = Scratch(std::env::temp_dir().join(unique));
        let project = scratch.0.join("project");
        fs::create_dir_all(&project).map_err(|error| error.to_string())?;
        let outside = scratch.0.join("outside.rs");
        fs::write(&outside, b"pub fn secret() {}").map_err(|error| error.to_string())?;
        fs::write(project.join("kept.rs"), b"pub fn kept() {}")
            .map_err(|error| error.to_string())?;
        let root = project.to_str().ok_or("non-UTF-8 scratch path")?;
        let key = [13; 32];
        let cold = scan_project(root, key, &BTreeMap::new())?;
        let reusable = cold.files.iter().cloned().collect::<BTreeMap<_, _>>();
        let warm = scan_project(root, key, &reusable)?;
        assert_eq!(warm.reused_compiler_files.len(), 1);
        fs::remove_file(project.join("kept.rs")).map_err(|error| error.to_string())?;
        symlink(&outside, project.join("kept.rs")).map_err(|error| error.to_string())?;
        let Err(error) = admit_compiler_sources_with_policy(
            project.as_path(),
            Vec::new(),
            warm.reused_compiler_files,
            warm.source_admission_policy,
        ) else {
            return Err("a symlinked reused file was admitted".to_owned());
        };
        assert!(error.contains("kept.rs"), "{error}");
        assert!(!error.contains("secret"), "{error}");
        Ok(())
    }

    fn scratch_dir(label: &str) -> Result<PathBuf, String> {
        let unique = format!(
            "backend-ingest-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let directory = std::env::temp_dir().join(unique);
        fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        Ok(directory)
    }

    #[cfg(unix)]
    struct GitScratch(PathBuf);

    #[cfg(unix)]
    impl Drop for GitScratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    fn git_test_command(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .map_err(|error| format!("could not run Git test helper: {error}"))?;
        if !output.status.success() {
            return Err(format!("Git test helper failed: git {}", args.join(" ")));
        }
        Ok(output.stdout)
    }

    #[cfg(unix)]
    fn git_test_repo(label: &str, files: &[(&str, &[u8])]) -> Result<Option<GitScratch>, String> {
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            return Ok(None);
        }
        let repository = GitScratch(scratch_dir(label)?);
        let root = &repository.0;
        git_test_command(root, &["init", "-q"])?;
        git_test_command(
            root,
            &["config", "user.email", "frontier-test@example.invalid"],
        )?;
        git_test_command(root, &["config", "user.name", "Source Frontier Test"])?;
        for (relative, bytes) in files {
            let path = root.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            fs::write(path, bytes).map_err(|error| error.to_string())?;
        }
        git_test_command(root, &["add", "-A"])?;
        git_test_command(root, &["commit", "-m", "initial source snapshot"])?;
        Ok(Some(repository))
    }

    #[cfg(unix)]
    fn git_test_commit(root: &Path, message: &str) -> Result<(), String> {
        git_test_command(root, &["add", "-A"])?;
        git_test_command(root, &["commit", "-m", message])?;
        Ok(())
    }

    #[cfg(unix)]
    fn git_project_key(root: &Path) -> [u8; 32] {
        *blake3::hash(root.as_os_str().as_encoded_bytes()).as_bytes()
    }

    fn reusable_rows(snapshot: &IndexSnapshot) -> BTreeMap<[u8; 32], ProductSourceRecord> {
        snapshot.files.iter().cloned().collect()
    }

    fn scan_with_source_policy(
        root: &Path,
        project: [u8; 32],
        reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
        source_policy: SourceAdmissionPolicy,
    ) -> Result<IndexSnapshot, String> {
        let root = root.to_str().ok_or("non-UTF-8 Git path")?;
        scan_project_with_configuration_policy_attempt(
            root,
            project,
            reusable,
            source_selection_policy(),
            true,
            true,
            None,
            source_policy,
        )
    }

    #[cfg(unix)]
    #[test]
    fn git_frontier_warm_scan_reads_no_source_bytes_with_ignored_build_output() -> Result<(), String>
    {
        let Some(repository) = git_test_repo(
            "frontier-ignored-build",
            &[
                (".gitignore", b"/target/\n"),
                ("lib.rs", b"pub fn stable() -> u8 { 7 }\n"),
            ],
        )?
        else {
            return Ok(());
        };
        let root = &repository.0;
        let project = git_project_key(root);
        git_test_command(root, &["config", "core.fsmonitor", "true"])?;
        let source_bytes = b"pub fn stable() -> u8 { 7 }\n";
        fs::create_dir_all(root.join("target")).map_err(|error| error.to_string())?;
        fs::write(root.join("target/generated.rs"), b"pub fn generated() {}\n")
            .map_err(|error| error.to_string())?;

        let cold = scan_project(
            root.to_str().ok_or("non-UTF-8 Git path")?,
            project,
            &BTreeMap::new(),
        )?;
        assert_eq!(cold.files.len(), 1);
        assert_eq!(cold.source_bytes_read, source_bytes.len());
        let warm = scan_project(
            root.to_str().ok_or("non-UTF-8 Git path")?,
            project,
            &reusable_rows(&cold),
        )?;
        assert_eq!(warm.source_bytes_read, 0);
        assert_eq!(warm.files.len(), 1);
        assert_eq!(cold.source_version, warm.source_version);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn changed_source_quota_invalidates_warm_frontier_before_file_admission() -> Result<(), String>
    {
        let Some(repository) = git_test_repo(
            "frontier-source-quota-identity",
            &[("lib.rs", b"pub fn stable() -> u8 { 7 }\n")],
        )?
        else {
            return Ok(());
        };
        let root = &repository.0;
        let project = git_project_key(root);
        let default = SourceAdmissionPolicy::new(SourceAdmissionLimits::default())
            .map_err(|error| error.to_string())?;
        let cold = scan_with_source_policy(root, project, &BTreeMap::new(), default)?;
        let reusable = reusable_rows(&cold);
        assert_eq!(
            cold.source_bytes_read,
            b"pub fn stable() -> u8 { 7 }\n".len()
        );

        let mut changed_limits = SourceAdmissionLimits::default();
        changed_limits.max_file_source_bytes = 16;
        changed_limits.max_in_flight_source_bytes = 32;
        let changed =
            SourceAdmissionPolicy::new(changed_limits).map_err(|error| error.to_string())?;
        assert_ne!(default.identity(), changed.identity());
        let warm = scan_with_source_policy(root, project, &reusable, changed)?;
        let row = warm.files[0].1.file_fields().ok_or("expected source row")?;
        assert_eq!(warm.source_bytes_read, 0);
        assert!(warm.reused_compiler_files.is_empty());
        assert!(warm.compiler_sources.is_empty());
        assert_eq!(
            row.retention,
            backend_engine::DeclarationRetention::Unavailable(SourceUnavailableReason::TooLarge),
            "the tightened quota must refuse the cached file as too large"
        );
        Ok(())
    }

    #[test]
    fn complete_fact_pages_exceeding_the_project_quota_refuse_before_retention()
    -> Result<(), String> {
        let root = scratch_dir("source-fact-page-budget")?;
        let mut source = String::new();
        for index in 0..1_000 {
            source.push_str(&format!(
                "export function declaration_{index:04}(): number {{ return {index}; }}\n"
            ));
        }
        fs::write(root.join("a_many.ts"), source).map_err(|error| error.to_string())?;
        fs::write(root.join("z_small.ts"), "export const small = 1;\n")
            .map_err(|error| error.to_string())?;
        let default = SourceAdmissionPolicy::new(SourceAdmissionLimits::default())
            .map_err(|error| error.to_string())?;
        let admitted = scan_with_source_policy(&root, [29; 32], &BTreeMap::new(), default)?;
        assert_eq!(admitted.files.len(), 2);
        let fact_bytes = admitted
            .source_facts
            .iter()
            .map(ProductSourceFileFactsUpdate::encoded_bytes)
            .sum::<usize>();
        assert!(
            fact_bytes > ProductSourceRecord::ROW_VALUE_CAPACITY,
            "the real parser must produce multiple complete fact pages"
        );
        let mut compact_bytes = 0;
        for (_, row) in &admitted.files {
            let mut encoded = Vec::new();
            ProductSourceRelation::encode_value(row, &mut encoded);
            assert!(encoded.len() <= ProductSourceRecord::ROW_VALUE_CAPACITY);
            compact_bytes += encoded.len();
        }
        let mut limits = default.limits();
        limits.max_project_record_bytes = compact_bytes.max(limits.max_encoded_record_bytes);
        let tight = SourceAdmissionPolicy::new(limits).map_err(|error| error.to_string())?;
        let error = scan_with_source_policy(&root, [29; 32], &BTreeMap::new(), tight)
            .err()
            .ok_or("the complete fact pages escaped the project quota")?;
        assert!(error.contains("project record bytes"), "{error}");
        // The refusing scan drains its workers. A subsequent scan can admit
        // the same exact sources without a poisoned cancellation or channel.
        let retried = scan_with_source_policy(&root, [29; 32], &BTreeMap::new(), default)?;
        assert_eq!(retried.source_version, admitted.source_version);
        assert_eq!(retried.files, admitted.files);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn warm_frontier_charges_unchanged_fact_pages_after_another_file_grows() -> Result<(), String> {
        let repository = GitScratch(scratch_dir("warm-fact-page-budget")?);
        let root = &repository.0;
        git_test_command(root, &["init", "-q"])?;
        git_test_command(
            root,
            &["config", "user.email", "source-budget@example.invalid"],
        )?;
        git_test_command(root, &["config", "user.name", "Source Budget Test"])?;
        let declarations = |count: usize| {
            (0..count)
                .map(|index| {
                    format!(
                        "export function declaration_{index:04}(): number {{ return {index}; }}\n"
                    )
                })
                .collect::<String>()
        };
        fs::write(root.join("a_many.ts"), declarations(1_000))
            .map_err(|error| error.to_string())?;
        fs::write(root.join("b_changed.ts"), declarations(1)).map_err(|error| error.to_string())?;
        git_test_command(root, &["add", "-A"])?;
        git_test_command(root, &["commit", "-m", "fact page budget fixture"])?;
        let project = git_project_key(root);
        let default = SourceAdmissionPolicy::new(SourceAdmissionLimits::default())
            .map_err(|error| error.to_string())?;
        let cold = scan_with_source_policy(root, project, &BTreeMap::new(), default)?;
        let encoded_rows = |snapshot: &IndexSnapshot| {
            snapshot
                .files
                .iter()
                .map(|(_, row)| {
                    let mut bytes = Vec::new();
                    ProductSourceRelation::encode_value(row, &mut bytes);
                    bytes.len()
                })
                .sum::<usize>()
                + snapshot
                    .encoded_fact_bytes
                    .iter()
                    .map(|(_, bytes)| bytes)
                    .sum::<usize>()
        };
        let mut limits = default.limits();
        limits.max_project_record_bytes = encoded_rows(&cold) + 512;
        let tight = SourceAdmissionPolicy::new(limits).map_err(|error| error.to_string())?;
        let seeded = scan_with_source_policy(root, project, &reusable_rows(&cold), tight)?;
        let reusable = reusable_rows(&seeded);
        for _ in 0..2 {
            let warm = scan_with_source_policy(root, project, &reusable, tight)?;
            assert_eq!(
                warm.source_bytes_read, 0,
                "the unchanged frontier must still avoid source reads"
            );
            assert!(
                warm.source_facts.is_empty(),
                "the page charge must not force reconstruction"
            );
            assert_eq!(
                warm.encoded_fact_bytes, seeded.encoded_fact_bytes,
                "a warm frontier must carry full page charges into the next frontier"
            );
        }
        fs::write(root.join("b_changed.ts"), declarations(500))
            .map_err(|error| error.to_string())?;
        // A distinct cache coordinate keeps the tight-policy frontier intact.
        // The following refusal must exercise cached unchanged pages, rather
        // than a full fallback caused by switching this coordinate's policy.
        let grown = scan_with_source_policy(root, [0x73; 32], &BTreeMap::new(), default)?;
        assert!(encoded_rows(&grown) > tight.limits().max_project_record_bytes);
        // The changed file alone fits; only accounting the unchanged file's
        // complete pages makes the aggregate refusal correct.
        let changed_charge = grown
            .encoded_fact_bytes
            .iter()
            .find(|(key, _)| *key == product_source_file_key([0x73; 32], "b_changed.ts"))
            .ok_or("changed file charge")?
            .1;
        assert!(changed_charge < tight.limits().max_project_record_bytes);
        let error = scan_with_source_policy(root, project, &reusable, tight)
            .err()
            .ok_or("warm reuse erased the unchanged file's page charge")?;
        assert!(error.contains("project record bytes"), "{error}");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn many_small_files_85_mib_cold_and_warm_scans_obey_admission_and_memory_caps()
    -> Result<(), String> {
        const FILE_COUNT: usize = 10_880;
        const FILE_BYTES: usize = 8 * 1024;
        let repository = GitScratch(scratch_dir("source-budget-85-mib")?);
        let root = &repository.0;
        git_test_command(root, &["init", "-q"])?;
        git_test_command(
            root,
            &["config", "user.email", "source-budget@example.invalid"],
        )?;
        git_test_command(root, &["config", "user.name", "Source Budget Test"])?;
        for index in 0..FILE_COUNT {
            let prefix = format!("pub fn generated_{index}() -> usize {{ {index} }}\n");
            if prefix.len() >= FILE_BYTES {
                return Err("generated source prefix exceeded the fixture size".to_owned());
            }
            let mut bytes = vec![b' '; FILE_BYTES];
            bytes[..prefix.len()].copy_from_slice(prefix.as_bytes());
            fs::write(root.join(format!("generated_{index:05}.rs")), &bytes)
                .map_err(|error| error.to_string())?;
        }
        git_test_command(root, &["add", "-A"])?;
        git_test_command(root, &["commit", "-m", "large source budget fixture"])?;

        let project = git_project_key(root);
        let policy = SourceAdmissionPolicy::new(SourceAdmissionLimits::default())
            .map_err(|error| error.to_string())?;
        let limits = policy.limits();
        let expected_bytes = FILE_COUNT * FILE_BYTES;
        assert!(expected_bytes > 84 * 1024 * 1024);
        assert!(FILE_COUNT > 10_000);
        assert!(expected_bytes < limits.max_project_source_bytes);
        assert!(expected_bytes < limits.max_retained_compiler_source_bytes);
        assert!(limits.max_in_flight_source_bytes < expected_bytes);

        let cold = scan_with_source_policy(root, project, &BTreeMap::new(), policy)?;
        let cold_handle_bytes = retained_compiler_handle_bytes(&cold);
        assert_eq!(cold.files.len(), FILE_COUNT);
        assert_eq!(cold.source_bytes_read, expected_bytes);
        assert!(cold_handle_bytes < expected_bytes / 20);
        assert!(
            policy.worker_count(usize::MAX, FILE_COUNT) * 2 * limits.max_file_source_bytes
                <= limits.max_in_flight_source_bytes,
            "active parse buffers plus queued results must fit the in-flight budget"
        );

        let compiler_sources = admit_compiler_sources_with_policy(
            root,
            cold.compiler_sources.clone(),
            cold.reused_compiler_files.clone(),
            policy,
        )?;
        let materialized_bytes = compiler_sources
            .iter()
            .map(|source| source.source.len())
            .sum::<usize>();
        assert_eq!(materialized_bytes, expected_bytes);
        assert!(materialized_bytes <= limits.max_retained_compiler_source_bytes);
        drop(compiler_sources);

        let warm = scan_with_source_policy(root, project, &reusable_rows(&cold), policy)?;
        assert_eq!(warm.source_version, cold.source_version);
        assert_eq!(warm.source_bytes_read, 0);
        assert!(warm.compiler_sources.is_empty());
        assert_eq!(warm.reused_compiler_files.len(), FILE_COUNT);
        Ok(())
    }

    #[test]
    fn one_valid_generated_tsx_over_512_kib_is_scanned_and_reopened() -> Result<(), String> {
        const GENERATED_BYTES: usize = 768 * 1024;
        let root = scratch_dir("large-generated-tsx")?;
        let generated = "x".repeat(GENERATED_BYTES);
        let source =
            format!("const Generated = <div>{generated}</div>;\nexport default Generated;\n");
        fs::write(root.join("Generated.tsx"), &source).map_err(|error| error.to_string())?;
        let policy = SourceAdmissionPolicy::new(SourceAdmissionLimits::default())
            .map_err(|error| error.to_string())?;
        assert!(source.len() > 512 * 1024);
        assert!(source.len() < policy.limits().max_file_source_bytes);

        let scan = scan_with_source_policy(&root, [21; 32], &BTreeMap::new(), policy)?;
        assert_eq!(scan.source_bytes_read, source.len());
        assert_eq!(scan.compiler_sources.len(), 1);
        assert_eq!(
            scan.compiler_sources[0].profile,
            LanguageProfile::TypeScript(TypeScriptSource::Tsx)
        );
        assert_eq!(
            scan.compiler_sources[0].content,
            typed_of::<InputContentSchema>(source.as_bytes()).to_bytes()
        );
        let admitted = admit_compiler_sources_with_policy(
            &root,
            scan.compiler_sources.clone(),
            Vec::new(),
            scan.source_admission_policy,
        )?;
        assert_eq!(admitted.len(), 1);
        assert_eq!(admitted[0].content, scan.compiler_sources[0].content);
        assert_eq!(
            admitted[0].source_fact_identity,
            SourceFactIdentity::from_canonical_bytes(source.as_bytes())
        );
        assert_eq!(admitted[0].source, source);
        fs::remove_dir_all(&root).map_err(|error| error.to_string())?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn compiler_handoff_rejects_retargeted_root_alias_with_identical_source() -> Result<(), String>
    {
        use std::os::unix::fs::symlink;

        let scratch = scratch_dir("root-alias-retarget")?;
        let admitted_root = scratch.join("admitted");
        let foreign_root = scratch.join("foreign");
        let alias = scratch.join("project");
        fs::create_dir_all(&admitted_root).map_err(|error| error.to_string())?;
        fs::create_dir_all(&foreign_root).map_err(|error| error.to_string())?;
        let source = b"pub fn same_bytes() {}\n";
        fs::write(admitted_root.join("lib.rs"), source).map_err(|error| error.to_string())?;
        fs::write(foreign_root.join("lib.rs"), source).map_err(|error| error.to_string())?;
        symlink(&admitted_root, &alias).map_err(|error| error.to_string())?;

        let policy = SourceAdmissionPolicy::new(SourceAdmissionLimits::default())
            .map_err(|error| error.to_string())?;
        let scan = scan_with_source_policy(&alias, [31; 32], &BTreeMap::new(), policy)?;
        fs::remove_file(&alias).map_err(|error| error.to_string())?;
        symlink(&foreign_root, &alias).map_err(|error| error.to_string())?;

        let error = match admit_compiler_sources_for_scan(
            &alias,
            &scan.revision_fence,
            scan.compiler_sources,
            scan.reused_compiler_files,
            scan.source_admission_policy,
            None,
        ) {
            Err(error) => error,
            Ok(_) => {
                return Err("same-content foreign root crossed the admitted-root fence".to_owned());
            }
        };
        assert!(
            error.contains("alias now resolves to a different directory"),
            "{error}"
        );
        let _ = fs::remove_dir_all(&scratch);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn compiler_workspace_rejects_retargeted_ancestor_alias_with_identical_files()
    -> Result<(), String> {
        use std::os::unix::fs::symlink;

        let scratch = scratch_dir("workspace-root-alias-retarget")?;
        let admitted_parent = scratch.join("admitted-parent");
        let foreign_parent = scratch.join("foreign-parent");
        let alias = scratch.join("workspace-alias");
        let requested_root = alias.join("project");
        fs::create_dir_all(requested_root.as_path()).map_err(|error| error.to_string())?;
        fs::create_dir_all(&admitted_parent).map_err(|error| error.to_string())?;
        fs::create_dir_all(foreign_parent.join("project")).map_err(|error| error.to_string())?;
        fs::write(requested_root.join("lib.rs"), b"pub fn same() {}\n")
            .map_err(|error| error.to_string())?;
        fs::write(foreign_parent.join("project/lib.rs"), b"pub fn same() {}\n")
            .map_err(|error| error.to_string())?;
        fs::rename(alias.join("project"), admitted_parent.join("project"))
            .map_err(|error| error.to_string())?;
        fs::remove_dir(&alias).map_err(|error| error.to_string())?;
        symlink(&admitted_parent, &alias).map_err(|error| error.to_string())?;

        let snapshot = CompilerWorkspaceSnapshot::open(&requested_root)?;
        fs::remove_file(&alias).map_err(|error| error.to_string())?;
        symlink(&foreign_parent, &alias).map_err(|error| error.to_string())?;
        assert!(!snapshot.revalidate()?);
        let _ = fs::remove_dir_all(&scratch);
        Ok(())
    }

    #[test]
    fn compiler_handoff_rejects_root_replacement_with_identical_source() -> Result<(), String> {
        let scratch = scratch_dir("root-replacement")?;
        let root = scratch.join("project");
        let moved_root = scratch.join("moved-project");
        fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let source = b"pub fn same_bytes() {}\n";
        fs::write(root.join("lib.rs"), source).map_err(|error| error.to_string())?;
        let policy = SourceAdmissionPolicy::new(SourceAdmissionLimits::default())
            .map_err(|error| error.to_string())?;
        let scan = scan_with_source_policy(&root, [32; 32], &BTreeMap::new(), policy)?;

        fs::rename(&root, &moved_root).map_err(|error| error.to_string())?;
        fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        fs::write(root.join("lib.rs"), source).map_err(|error| error.to_string())?;
        let error = match admit_compiler_sources_for_scan(
            &root,
            &scan.revision_fence,
            scan.compiler_sources,
            scan.reused_compiler_files,
            scan.source_admission_policy,
            None,
        ) {
            Err(error) => error,
            Ok(_) => return Err("replacement root crossed the admitted-root fence".to_owned()),
        };
        assert!(error.contains("project root directory changed"), "{error}");
        let _ = fs::remove_dir_all(&scratch);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unchanged_tmp_alias_resolves_to_its_admitted_private_tmp_root() -> Result<(), String> {
        let root = PathBuf::from("/tmp").join(format!(
            "backend-tmp-alias-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        ));
        fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let source = b"pub fn temporary_alias() {}\n";
        fs::write(root.join("lib.rs"), source).map_err(|error| error.to_string())?;
        let canonical = root.canonicalize().map_err(|error| error.to_string())?;
        if canonical == root || !canonical.starts_with("/private/tmp") {
            let _ = fs::remove_dir_all(&root);
            return Err(format!(
                "expected /tmp to resolve through /private/tmp: {canonical:?}"
            ));
        }
        let policy = SourceAdmissionPolicy::new(SourceAdmissionLimits::default())
            .map_err(|error| error.to_string())?;
        let scan = scan_with_source_policy(&root, [33; 32], &BTreeMap::new(), policy)?;
        let admitted = admit_compiler_sources_for_scan(
            &root,
            &scan.revision_fence,
            scan.compiler_sources,
            scan.reused_compiler_files,
            scan.source_admission_policy,
            None,
        )?;
        assert_eq!(admitted.len(), 1);
        assert_eq!(admitted[0].source.as_bytes(), source);
        let snapshot = CompilerWorkspaceSnapshot::open(&root)?;
        assert!(snapshot.revalidate()?);
        fs::remove_dir_all(&root).map_err(|error| error.to_string())?;
        Ok(())
    }

    #[test]
    fn compiler_materialization_cancellation_stops_before_the_next_source_read()
    -> Result<(), String> {
        let root = scratch_dir("handoff-cancel")?;
        let first = b"pub fn first() {}\n";
        let second = b"pub fn second() {}\n";
        fs::write(root.join("a.rs"), first).map_err(|error| error.to_string())?;
        fs::write(root.join("b.rs"), second).map_err(|error| error.to_string())?;
        let policy = SourceAdmissionPolicy::new(SourceAdmissionLimits::default())
            .map_err(|error| error.to_string())?;
        let scan = scan_with_source_policy(&root, [34; 32], &BTreeMap::new(), policy)?;
        fs::write(root.join("b.rs"), b"changed after scan\n").map_err(|error| error.to_string())?;
        let capability = scan.revision_fence.open_bound_root(&root)?;
        let cancellation = AtomicBool::new(false);
        let mut completed = 0;
        let error = match materialize_compiler_sources(
            &capability,
            scan.compiler_sources,
            scan.reused_compiler_files,
            scan.source_admission_policy,
            Some(&cancellation),
            |count| {
                completed = count;
                if count == 1 {
                    cancellation.store(true, Ordering::Release);
                }
            },
        ) {
            Err(error) => error,
            Ok(_) => {
                return Err(
                    "cancellation after the first handle admitted the remaining source".to_owned(),
                );
            }
        };
        assert_eq!(completed, 1);
        assert_eq!(error, INDEX_SCAN_CANCELLED);
        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn git_frontier_reads_only_an_edited_file_and_handles_rename_delete() -> Result<(), String> {
        let Some(repository) = git_test_repo(
            "frontier-edit-rename",
            &[
                ("lib.rs", b"pub fn kept() {}\n"),
                ("old.rs", b"pub fn moved() {}\n"),
            ],
        )?
        else {
            return Ok(());
        };
        let root = &repository.0;
        let root_text = root.to_str().ok_or("non-UTF-8 Git path")?;
        let project = git_project_key(root);
        let cold = scan_project(root_text, project, &BTreeMap::new())?;
        let reusable = reusable_rows(&cold);
        let edited = b"pub fn kept() { let _ = 1; }\n";
        fs::write(root.join("lib.rs"), edited).map_err(|error| error.to_string())?;
        let delta = scan_project(root_text, project, &reusable)?;
        assert_eq!(delta.source_bytes_read, edited.len());
        assert_eq!(delta.reused_compiler_files.len(), 1);

        git_test_command(root, &["mv", "old.rs", "new.rs"])?;
        let renamed = scan_project(root_text, project, &reusable)?;
        assert_eq!(
            renamed.source_bytes_read,
            edited.len() + b"pub fn moved() {}\n".len()
        );
        assert!(renamed.files.iter().all(|(_, record)| {
            record
                .file_fields()
                .is_some_and(|fields| fields.path != "old.rs")
        }));
        assert!(renamed.files.iter().any(|(_, record)| {
            record
                .file_fields()
                .is_some_and(|fields| fields.path == "new.rs")
        }));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn git_frontier_falls_back_for_new_untracked_source_and_ignore_policy_change()
    -> Result<(), String> {
        let Some(repository) = git_test_repo(
            "frontier-untracked-policy",
            &[
                (".gitignore", b"/hidden_sources/\n"),
                ("lib.rs", b"pub fn stable() {}\n"),
            ],
        )?
        else {
            return Ok(());
        };
        let root = &repository.0;
        let root_text = root.to_str().ok_or("non-UTF-8 Git path")?;
        let project = git_project_key(root);
        let cold = scan_project(root_text, project, &BTreeMap::new())?;
        let reusable = reusable_rows(&cold);

        let extra = b"pub fn untracked() {}\n";
        fs::write(root.join("extra.rs"), extra).map_err(|error| error.to_string())?;
        let with_untracked = scan_project(root_text, project, &reusable)?;
        assert_eq!(
            with_untracked.source_bytes_read,
            b"pub fn stable() {}\n".len() + extra.len()
        );
        assert!(with_untracked.files.iter().any(|(_, record)| {
            record
                .file_fields()
                .is_some_and(|fields| fields.path == "extra.rs")
        }));

        fs::remove_file(root.join("extra.rs")).map_err(|error| error.to_string())?;
        fs::create_dir_all(root.join("hidden_sources")).map_err(|error| error.to_string())?;
        let newly_selected = b"pub fn now_visible() {}\n";
        fs::write(root.join("hidden_sources/generated.rs"), newly_selected)
            .map_err(|error| error.to_string())?;
        let policy_change = b"# generated sources are now visible\n";
        fs::write(root.join(".gitignore"), policy_change).map_err(|error| error.to_string())?;
        let changed_policy = scan_project(root_text, project, &reusable)?;
        assert_eq!(
            changed_policy.source_bytes_read,
            b"pub fn stable() {}\n".len() + newly_selected.len()
        );
        assert!(changed_policy.files.iter().any(|(_, record)| {
            record
                .file_fields()
                .is_some_and(|fields| fields.path == "hidden_sources/generated.rs")
        }));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn git_frontier_witnesses_a_local_ignore_file_hidden_by_gitignore() -> Result<(), String> {
        let Some(repository) = git_test_repo(
            "frontier-hidden-ignore-policy",
            &[
                (".gitignore", b"/.ignore\n"),
                ("lib.rs", b"pub fn stable() {}\n"),
            ],
        )?
        else {
            return Ok(());
        };
        let root = &repository.0;
        let root_text = root.to_str().ok_or("non-UTF-8 Git path")?;
        let project = git_project_key(root);
        fs::create_dir_all(root.join("hidden_sources")).map_err(|error| error.to_string())?;
        let hidden = b"pub fn became_visible() {}\n";
        fs::write(root.join("hidden_sources/generated.rs"), hidden)
            .map_err(|error| error.to_string())?;
        fs::write(root.join(".ignore"), b"hidden_sources/\n").map_err(|error| error.to_string())?;

        let cold = scan_project(root_text, project, &BTreeMap::new())?;
        assert_eq!(cold.files.len(), 1);
        let reusable = reusable_rows(&cold);
        fs::write(root.join(".ignore"), b"# source selection changed\n")
            .map_err(|error| error.to_string())?;

        let changed_policy = scan_project(root_text, project, &reusable)?;
        assert_eq!(
            changed_policy.source_bytes_read,
            b"pub fn stable() {}\n".len() + hidden.len()
        );
        assert!(changed_policy.files.iter().any(|(_, record)| {
            record
                .file_fields()
                .is_some_and(|fields| fields.path == "hidden_sources/generated.rs")
        }));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn git_frontier_falls_back_when_a_selected_path_is_a_symlink() -> Result<(), String> {
        use std::os::unix::fs::symlink;

        let Some(repository) =
            git_test_repo("frontier-symlink", &[("lib.rs", b"pub fn stable() {}\n")])?
        else {
            return Ok(());
        };
        let root = &repository.0;
        let root_text = root.to_str().ok_or("non-UTF-8 Git path")?;
        let project = git_project_key(root);
        let cold = scan_project(root_text, project, &BTreeMap::new())?;
        let reusable = reusable_rows(&cold);
        let outside = root
            .parent()
            .ok_or("missing scratch parent")?
            .join("outside.rs");
        fs::write(&outside, b"pub fn outside() {}\n").map_err(|error| error.to_string())?;
        symlink(&outside, root.join("linked.rs")).map_err(|error| error.to_string())?;

        let fallback = scan_project(root_text, project, &reusable)?;
        assert_eq!(fallback.source_bytes_read, b"pub fn stable() {}\n".len());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn git_frontier_detects_mutation_after_a_zero_read_reuse_attempt() -> Result<(), String> {
        let Some(repository) = git_test_repo(
            "frontier-race",
            &[
                ("lib.rs", b"pub fn old_value() {}\n"),
                ("kept.rs", b"pub fn kept() {}\n"),
            ],
        )?
        else {
            return Ok(());
        };
        let root = &repository.0;
        let root_text = root.to_str().ok_or("non-UTF-8 Git path")?;
        let project = git_project_key(root);
        let cold = scan_project(root_text, project, &BTreeMap::new())?;
        let reusable = reusable_rows(&cold);
        let changed = b"pub fn new_value() {}\n";
        source_frontier::install_source_frontier_race(
            root.clone(),
            root.join("lib.rs"),
            changed.to_vec(),
        );

        let raced = scan_project(root_text, project, &reusable)?;
        assert_eq!(
            raced.source_bytes_read,
            changed.len() + b"pub fn kept() {}\n".len()
        );
        assert!(raced.compiler_sources.iter().any(|source| {
            source.relative_path == "lib.rs"
                && source.content == typed_of::<InputContentSchema>(changed).to_bytes()
        }));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn cold_frontier_reopen_uses_the_full_source_read() -> Result<(), String> {
        let Some(repository) = git_test_repo(
            "frontier-cold-reopen",
            &[("lib.rs", b"pub fn stable() {}\n")],
        )?
        else {
            return Ok(());
        };
        let root = &repository.0;
        let root_text = root.to_str().ok_or("non-UTF-8 Git path")?;
        let project = git_project_key(root);
        let cold = scan_project(root_text, project, &BTreeMap::new())?;
        let reusable = reusable_rows(&cold);
        source_frontier::clear_source_frontier_for(root, project);

        let reopened = scan_project(root_text, project, &reusable)?;
        assert_eq!(reopened.source_bytes_read, b"pub fn stable() {}\n".len());
        assert_eq!(reopened.source_version, cold.source_version);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn raw_crlf_lf_change_with_the_same_git_blob_is_reread() -> Result<(), String> {
        let Some(repository) = git_test_repo(
            "frontier-line-endings",
            &[
                (".gitattributes", b"*.rs text\n"),
                ("lib.rs", b"pub fn first() {}\npub fn second() {}\r\n"),
            ],
        )?
        else {
            return Ok(());
        };
        let root = &repository.0;
        git_test_command(root, &["config", "core.autocrlf", "true"])?;
        git_test_command(root, &["add", "--renormalize", "."])?;
        git_test_command(root, &["commit", "--amend", "--no-edit"])?;
        let root_text = root.to_str().ok_or("non-UTF-8 Git path")?;
        let project = git_project_key(root);
        let original = fs::read(root.join("lib.rs")).map_err(|error| error.to_string())?;
        let blob_before = git_test_command(root, &["ls-files", "--stage", "--", "lib.rs"])?;
        let cold = scan_project(root_text, project, &BTreeMap::new())?;
        let reusable = reusable_rows(&cold);

        let swapped = b"pub fn first() {}\r\npub fn second() {}\n";
        assert_eq!(original.len(), swapped.len());
        fs::write(root.join("lib.rs"), swapped).map_err(|error| error.to_string())?;
        git_test_command(root, &["add", "--", "lib.rs"])?;
        let blob_after = git_test_command(root, &["ls-files", "--stage", "--", "lib.rs"])?;
        assert_eq!(blob_before, blob_after);
        assert!(git_test_command(root, &["status", "--porcelain"])?.is_empty());

        let changed = scan_project(root_text, project, &reusable)?;
        assert_eq!(changed.source_bytes_read, swapped.len());
        let old = cold.files[0]
            .1
            .file_fields()
            .ok_or("expected original source record")?
            .content_version;
        let new = changed.files[0]
            .1
            .file_fields()
            .ok_or("expected changed source record")?
            .content_version;
        assert_ne!(
            old, new,
            "raw line endings are part of the admitted source bytes"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "manual large-fixture measurement; reports source bytes and wall time"]
    fn benchmark_git_frontier_reads_large_fixture_once_then_reuses_all_rows() -> Result<(), String>
    {
        use std::time::Instant;

        let Some(repository) = git_test_repo(
            "frontier-large-benchmark",
            &[("src/lib.rs", b"pub fn root() {}\n")],
        )?
        else {
            return Ok(());
        };
        let root = &repository.0;
        fs::create_dir_all(root.join("src/generated")).map_err(|error| error.to_string())?;
        for index in 0..1024 {
            let text = format!(
                "pub fn item_{index}() -> usize {{ {index} }}\n// {}\n",
                "x".repeat(4_000)
            );
            fs::write(root.join(format!("src/generated/item_{index}.rs")), text)
                .map_err(|error| error.to_string())?;
        }
        git_test_commit(root, "large source fixture")?;
        let root_text = root.to_str().ok_or("non-UTF-8 Git path")?;
        let project = git_project_key(root);
        let cold_start = Instant::now();
        let cold = scan_project(root_text, project, &BTreeMap::new())?;
        let cold_elapsed = cold_start.elapsed();
        let reusable = reusable_rows(&cold);
        let warm_start = Instant::now();
        let warm = scan_project(root_text, project, &reusable)?;
        let warm_elapsed = warm_start.elapsed();
        assert_eq!(cold.files.len(), 1025);
        assert_eq!(warm.source_bytes_read, 0);
        eprintln!(
            "source_frontier_benchmark files={} cold_bytes={} warm_bytes={} cold_ms={} warm_ms={}",
            cold.files.len(),
            cold.source_bytes_read,
            warm.source_bytes_read,
            cold_elapsed.as_millis(),
            warm_elapsed.as_millis()
        );
        Ok(())
    }

    #[test]
    fn a_changed_language_does_not_reread_an_unchanged_one() -> Result<(), String> {
        let scratch = scratch_dir("language-delta")?;
        let root = scratch.to_str().ok_or("non-UTF-8 scratch path")?;
        let rust_v1 = "pub fn ferris() {}";
        let rust_v2 = "pub fn ferris() { let _ = 1; }";
        let python = "def monty():\n    pass\n";
        fs::write(scratch.join("lib.rs"), rust_v1).map_err(|error| error.to_string())?;
        fs::write(scratch.join("app.py"), python).map_err(|error| error.to_string())?;
        let project = [14; 32];
        let cold = scan_project(root, project, &BTreeMap::new())?;
        let reusable = cold.files.iter().cloned().collect::<BTreeMap<_, _>>();
        fs::write(scratch.join("lib.rs"), rust_v2).map_err(|error| error.to_string())?;
        let delta = scan_project(root, project, &reusable)?;
        fs::write(scratch.join("app.py"), b"def monty():\n    return 9\n")
            .map_err(|error| error.to_string())?;
        let present = present_compiler_paths(&delta.compiler_sources, &delta.reused_compiler_files);
        let lost = lost_compiler_profiles(scratch.as_path(), &reusable, &present)?;
        assert!(lost.is_empty(), "{lost:?}");
        let skipped = delta.reused_compiler_files.len();
        let (fresh, reused) = select_compiler_inputs(
            scratch.as_path(),
            delta.compiler_sources,
            delta.reused_compiler_files,
            &lost,
        );
        assert_eq!(reused.len(), 0);
        let admitted = admit_compiler_sources_with_policy(
            scratch.as_path(),
            fresh,
            reused,
            delta.source_admission_policy,
        )?;
        assert_eq!(
            admitted
                .iter()
                .map(|source| (source.relative_path.as_str(), source.source.as_str()))
                .collect::<Vec<_>>(),
            [("lib.rs", rust_v2)]
        );
        eprintln!(
            "profile_delta languages=2 changed=rust selected_files=1 skipped_reused={skipped}"
        );
        let _ = fs::remove_dir_all(&scratch);
        Ok(())
    }

    #[test]
    fn a_deleted_language_does_not_reread_the_languages_that_remain() -> Result<(), String> {
        let scratch = scratch_dir("language-delete")?;
        let root = scratch.to_str().ok_or("non-UTF-8 scratch path")?;
        fs::write(scratch.join("lib.rs"), b"pub fn ferris() {}")
            .map_err(|error| error.to_string())?;
        fs::write(scratch.join("app.py"), b"def monty():\n    pass\n")
            .map_err(|error| error.to_string())?;
        let project = [15; 32];
        let cold = scan_project(root, project, &BTreeMap::new())?;
        let reusable = cold.files.iter().cloned().collect::<BTreeMap<_, _>>();
        fs::remove_file(scratch.join("app.py")).map_err(|error| error.to_string())?;
        let delta = scan_project(root, project, &reusable)?;
        fs::write(scratch.join("lib.rs"), b"pub fn torn() {}")
            .map_err(|error| error.to_string())?;
        let present = present_compiler_paths(&delta.compiler_sources, &delta.reused_compiler_files);
        let lost = lost_compiler_profiles(scratch.as_path(), &reusable, &present)?;
        assert!(
            lost.iter()
                .any(|profile| matches!(profile, LanguageProfile::Python(_)))
        );
        assert!(
            !lost
                .iter()
                .any(|profile| matches!(profile, LanguageProfile::Rust(_)))
        );
        let (fresh, reused) = select_compiler_inputs(
            scratch.as_path(),
            delta.compiler_sources,
            delta.reused_compiler_files,
            &lost,
        );
        assert!(fresh.is_empty());
        assert!(reused.is_empty());
        let admitted = admit_compiler_sources_with_policy(
            scratch.as_path(),
            fresh,
            reused,
            delta.source_admission_policy,
        )?;
        assert!(admitted.is_empty());
        let _ = fs::remove_dir_all(&scratch);
        Ok(())
    }

    #[test]
    fn one_rust_edition_does_not_reread_another_crate() -> Result<(), String> {
        let scratch = scratch_dir("edition-delta")?;
        let root = scratch.to_str().ok_or("non-UTF-8 scratch path")?;
        fs::create_dir_all(scratch.join("old/src")).map_err(|error| error.to_string())?;
        fs::create_dir_all(scratch.join("new/src")).map_err(|error| error.to_string())?;
        fs::write(
            scratch.join("old/Cargo.toml"),
            "[package]\nname = \"old\"\nedition = \"2018\"\n",
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            scratch.join("new/Cargo.toml"),
            "[package]\nname = \"new\"\nedition = \"2021\"\n",
        )
        .map_err(|error| error.to_string())?;
        let old_v1 = "pub fn old_crate() {}";
        let old_v2 = "pub fn old_crate() { let _ = 1; }";
        let new_source = "pub fn new_crate() {}";
        fs::write(scratch.join("old/src/lib.rs"), old_v1).map_err(|error| error.to_string())?;
        fs::write(scratch.join("new/src/lib.rs"), new_source).map_err(|error| error.to_string())?;
        let project = [16; 32];
        let cold = scan_project(root, project, &BTreeMap::new())?;
        let reusable = cold.files.iter().cloned().collect::<BTreeMap<_, _>>();
        fs::write(scratch.join("old/src/lib.rs"), old_v2).map_err(|error| error.to_string())?;
        let delta = scan_project(root, project, &reusable)?;
        fs::write(scratch.join("new/src/lib.rs"), b"pub fn torn() {}")
            .map_err(|error| error.to_string())?;
        let present = present_compiler_paths(&delta.compiler_sources, &delta.reused_compiler_files);
        let lost = lost_compiler_profiles(scratch.as_path(), &reusable, &present)?;
        assert!(lost.is_empty(), "{lost:?}");
        let (fresh, reused) = select_compiler_inputs(
            scratch.as_path(),
            delta.compiler_sources,
            delta.reused_compiler_files,
            &lost,
        );
        assert!(reused.is_empty());
        let admitted = admit_compiler_sources_with_policy(
            scratch.as_path(),
            fresh,
            reused,
            delta.source_admission_policy,
        )?;
        assert_eq!(admitted.len(), 1);
        assert_eq!(admitted[0].relative_path, "old/src/lib.rs");
        assert_eq!(admitted[0].source, old_v2);
        assert_eq!(
            compilation_profile(
                scratch.as_path(),
                &admitted[0].relative_path,
                admitted[0].profile
            ),
            LanguageProfile::Rust(RustEdition::Rust2018)
        );
        assert_eq!(
            compilation_profile(
                scratch.as_path(),
                "new/src/lib.rs",
                LanguageProfile::Rust(RustEdition::Rust2024)
            ),
            LanguageProfile::Rust(RustEdition::Rust2021)
        );
        eprintln!("edition_delta crates=2 selected_files=1 skipped_crate=2021");
        let _ = fs::remove_dir_all(&scratch);
        Ok(())
    }

    #[test]
    fn a_deleted_crate_retires_its_edition_after_the_manifest_is_gone() -> Result<(), String> {
        let scratch = scratch_dir("retire-edition")?;
        let root = scratch.to_str().ok_or("non-UTF-8 scratch path")?;
        fs::create_dir_all(scratch.join("old/src")).map_err(|error| error.to_string())?;
        fs::create_dir_all(scratch.join("new/src")).map_err(|error| error.to_string())?;
        fs::write(
            scratch.join("old/Cargo.toml"),
            "[package]\nname = \"old\"\nedition = \"2018\"\n",
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            scratch.join("new/Cargo.toml"),
            "[package]\nname = \"new\"\nedition = \"2021\"\n",
        )
        .map_err(|error| error.to_string())?;
        fs::write(scratch.join("old/src/lib.rs"), b"pub fn old_crate() {}")
            .map_err(|error| error.to_string())?;
        fs::write(scratch.join("new/src/lib.rs"), b"pub fn new_crate() {}")
            .map_err(|error| error.to_string())?;
        fs::write(scratch.join("app.py"), b"def monty():\n    pass\n")
            .map_err(|error| error.to_string())?;
        let project = [17; 32];
        let cold = scan_project(root, project, &BTreeMap::new())?;
        let cold_live = live_compiler_profiles(
            scratch.as_path(),
            &cold.compiler_sources,
            &cold.reused_compiler_files,
        );
        let rust_2018 = LanguageProfile::Rust(RustEdition::Rust2018);
        let rust_2021 = LanguageProfile::Rust(RustEdition::Rust2021);
        assert!(cold_live.contains(&rust_2018));
        assert!(cold_live.contains(&rust_2021));
        let python = *cold_live
            .iter()
            .find(|profile| matches!(profile, LanguageProfile::Python(_)))
            .ok_or("python profile missing from the cold scan")?;
        let reusable = cold.files.iter().cloned().collect::<BTreeMap<_, _>>();
        fs::remove_dir_all(scratch.join("old")).map_err(|error| error.to_string())?;
        let delta = scan_project(root, project, &reusable)?;
        let live = live_compiler_profiles(
            scratch.as_path(),
            &delta.compiler_sources,
            &delta.reused_compiler_files,
        );
        assert!(!live.contains(&rust_2018));
        assert!(live.contains(&rust_2021));
        assert!(live.contains(&python));
        let selected = [rust_2018, rust_2021, python];
        let retired = retired_selected_profiles(&selected, &live);
        assert_eq!(retired.into_iter().collect::<Vec<_>>(), vec![rust_2018]);
        let present = present_compiler_paths(&delta.compiler_sources, &delta.reused_compiler_files);
        let lost = lost_compiler_profiles(scratch.as_path(), &reusable, &present)?;
        let (fresh, reused) = select_compiler_inputs(
            scratch.as_path(),
            delta.compiler_sources,
            delta.reused_compiler_files,
            &lost,
        );
        let compiled = live_compiler_profiles(scratch.as_path(), &fresh, &reused);
        assert!(compiled.is_subset(&live));
        assert!(!compiled.contains(&rust_2018));
        eprintln!(
            "retire_edition live={} retired=1 compiled={}",
            live.len(),
            compiled.len()
        );
        let _ = fs::remove_dir_all(&scratch);
        Ok(())
    }
}

/// Regression laws for the canonical single-row capacity of source records.
///
/// A real source file routinely extracts more declaration detail than one
/// canonical relation row can hold. Before `file_within_row_capacity` existed,
/// ingest admitted such a record and the whole project failed later, when the
/// relation delta was prepared, with an opaque
/// `workspace relation node was rejected`. `memchr 2.8.3` reproduces it with a
/// single file: `src/arch/x86_64/avx2/memchr.rs` extracts 125 declarations
/// carrying 49 452 bytes of excerpt, for a 65 686 byte row against a 65 464
/// byte capacity.
///
/// These tests assert the rendered content of the resulting record, not row
/// counts: a scan that silently dropped every declaration would keep the same
/// file count while erasing everything a surface can show.
#[cfg(test)]
mod row_capacity_tests {
    use super::*;
    use backend_engine::{DeclarationRetention, ProductSourceRecord};
    use std::fmt::Write as _;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(label: &str) -> Result<Scratch, String> {
        let unique = format!(
            "backend-row-capacity-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let directory = std::env::temp_dir().join(unique);
        fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        Ok(Scratch(directory))
    }

    /// Writes a Rust file whose extracted declarations exceed one row.
    ///
    /// Each function carries a documentation block and a body wide enough to
    /// produce a substantial excerpt, so the full record passes the canonical
    /// capacity while every individual declaration stays small.
    fn dense_source(functions: usize) -> String {
        let mut source = String::new();
        for index in 0..functions {
            let _ = writeln!(
                source,
                "/// Declaration {index} exists to widen the extracted record \
                 beyond one canonical relation row.\n\
                 pub fn declaration_{index}(argument: u64) -> u64 {{"
            );
            for step in 0..24 {
                let _ = writeln!(
                    source,
                    "    let intermediate_value_{step} = \
                     argument.wrapping_mul({step}).wrapping_add({index});"
                );
            }
            source.push_str("    argument\n}\n\n");
        }
        source
    }

    fn encoded_bytes(record: &ProductSourceRecord) -> usize {
        let mut bytes = Vec::new();
        ProductSourceRelation::encode_value(record, &mut bytes);
        bytes.len()
    }

    #[test]
    fn a_file_denser_than_one_row_is_indexed_rather_than_rejected() -> Result<(), String> {
        let scratch = scratch("dense")?;
        let source = dense_source(140);
        fs::write(scratch.0.join("dense.rs"), source.as_bytes())
            .map_err(|error| error.to_string())?;
        let root = scratch.0.to_str().ok_or("non-UTF-8 scratch path")?;

        let scan = scan_project(root, [3; 32], &BTreeMap::new())?;

        let (_, record) = scan.files.first().ok_or("dense file was not scanned")?;
        let fields = record.file_fields().ok_or("expected a file record")?;

        let encoded = encoded_bytes(record);
        assert!(
            encoded <= ProductSourceRecord::ROW_VALUE_CAPACITY,
            "a scanned record must fit one canonical row: {encoded} bytes against a \
             {} byte capacity",
            ProductSourceRecord::ROW_VALUE_CAPACITY
        );
        assert_ne!(
            fields.retention,
            DeclarationRetention::Complete,
            "a record that had to shed detail must not claim complete retention"
        );
        assert_eq!(
            fields.retention,
            DeclarationRetention::ExcerptsElided,
            "shedding excerpts alone must be enough for a dense but ordinary file"
        );

        // Content, not counts: every extracted name must still be renderable.
        let names = fields
            .declarations
            .iter()
            .map(|declaration| declaration.name().to_owned())
            .collect::<Vec<_>>();
        assert!(
            names.iter().any(|name| name == "declaration_0"),
            "the first declaration name must survive shedding, got {names:?}"
        );
        assert!(
            names.iter().any(|name| name == "declaration_139"),
            "the last declaration name must survive shedding, got {names:?}"
        );
        assert!(
            fields
                .declarations
                .iter()
                .all(|declaration| !declaration.signature().is_empty()),
            "shedding excerpts must not also erase signatures"
        );
        Ok(())
    }

    #[test]
    fn a_file_within_one_row_keeps_complete_retention_and_its_excerpts() -> Result<(), String> {
        let scratch = scratch("small")?;
        fs::write(
            scratch.0.join("small.rs"),
            b"/// One small declaration.\npub fn ferris(value: u64) -> u64 { value }\n",
        )
        .map_err(|error| error.to_string())?;
        let root = scratch.0.to_str().ok_or("non-UTF-8 scratch path")?;

        let scan = scan_project(root, [4; 32], &BTreeMap::new())?;
        let (_, record) = scan.files.first().ok_or("small file was not scanned")?;
        let fields = record.file_fields().ok_or("expected a file record")?;

        assert_eq!(
            fields.retention,
            DeclarationRetention::Complete,
            "a file that fits must not be reported as reduced"
        );
        let declaration = fields
            .declarations
            .iter()
            .find(|declaration| declaration.name() == "ferris")
            .ok_or_else(|| {
                format!(
                    "expected a `ferris` declaration, got {:?}",
                    fields
                        .declarations
                        .iter()
                        .map(backend_compile::SourceDeclaration::name)
                        .collect::<Vec<_>>()
                )
            })?;
        assert!(
            matches!(
                declaration.source_excerpt(),
                backend_compile::SourceExcerpt::Captured { .. }
            ),
            "a file that fits must keep its captured excerpt"
        );
        Ok(())
    }

    #[test]
    fn shedding_is_a_pure_function_of_the_source() -> Result<(), String> {
        let scratch = scratch("stable")?;
        fs::write(scratch.0.join("dense.rs"), dense_source(140).as_bytes())
            .map_err(|error| error.to_string())?;
        let root = scratch.0.to_str().ok_or("non-UTF-8 scratch path")?;

        let first = scan_project(root, [5; 32], &BTreeMap::new())?;
        let second = scan_project(root, [5; 32], &BTreeMap::new())?;

        assert_eq!(
            first.source_version, second.source_version,
            "an unchanged project must produce an unchanged source version"
        );
        assert_eq!(
            first.files, second.files,
            "an unchanged project must produce byte-identical rows, \
             otherwise re-indexing churns the content-addressed relation"
        );
        Ok(())
    }
}

/// Robustness laws: every failure a real project can cause is typed and
/// survivable.
///
/// A checkout in the wild contains files that cannot be read, files that are
/// not text, files that are too large, symlink cycles, and paths outside
/// ASCII. Before these laws, each of those aborted the entire scan with a
/// single string, so one bad file made a whole project permanently
/// unindexable. Each case now yields a typed per-file reason while every
/// other file still indexes.
///
/// The assertions are on rendered content - the surviving file's declaration
/// names and the failing file's typed reason - because a scan that dropped
/// every declaration would keep the same file count.
#[cfg(all(test, unix))]
mod robustness_tests {
    use super::*;
    use backend_engine::{DeclarationRetention, SourceUnavailableReason};

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::set_permissions(
                self.0.join("broken.rs"),
                <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o644),
            );
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(label: &str) -> Result<Scratch, String> {
        let unique = format!(
            "backend-robust-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let directory = std::env::temp_dir().join(unique);
        fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        Ok(Scratch(directory))
    }

    fn good_source() -> &'static [u8] {
        b"/// A readable declaration.\npub fn ferris(value: u64) -> u64 { value }\n"
    }

    /// Returns the retention claimed for one project-relative path.
    fn retention_of(scan: &IndexSnapshot, path: &str) -> Option<DeclarationRetention> {
        scan.files.iter().find_map(|(_, record)| {
            let fields = record.file_fields()?;
            (fields.path == path).then_some(fields.retention)
        })
    }

    fn names_of(scan: &IndexSnapshot, path: &str) -> Vec<String> {
        scan.files
            .iter()
            .find_map(|(_, record)| {
                let fields = record.file_fields()?;
                (fields.path == path).then(|| {
                    fields
                        .declarations
                        .iter()
                        .map(|declaration| declaration.name().to_owned())
                        .collect::<Vec<_>>()
                })
            })
            .unwrap_or_default()
    }

    fn scan(scratch: &Scratch) -> Result<IndexSnapshot, String> {
        let root = scratch.0.to_str().ok_or("non-UTF-8 scratch path")?;
        scan_project(root, [8; 32], &BTreeMap::new())
    }

    #[test]
    fn an_unreadable_file_is_reported_without_failing_the_project() -> Result<(), String> {
        use std::os::unix::fs::PermissionsExt as _;

        let scratch = scratch("unreadable")?;
        fs::write(scratch.0.join("good.rs"), good_source()).map_err(|e| e.to_string())?;
        let broken = scratch.0.join("broken.rs");
        fs::write(&broken, good_source()).map_err(|e| e.to_string())?;
        fs::set_permissions(&broken, fs::Permissions::from_mode(0o000))
            .map_err(|e| e.to_string())?;
        if fs::read(&broken).is_ok() {
            // Running as a user that bypasses the mode bits; the law is not
            // observable here and a pass would be fabricated.
            return Ok(());
        }

        let scan = scan(&scratch)?;

        assert_eq!(
            retention_of(&scan, "broken.rs"),
            Some(DeclarationRetention::Unavailable(
                SourceUnavailableReason::Unreadable
            )),
            "an unreadable file must keep its place with a typed reason"
        );
        assert_eq!(
            retention_of(&scan, "good.rs"),
            Some(DeclarationRetention::Complete),
            "an unreadable neighbour must not degrade a readable file"
        );
        assert!(
            names_of(&scan, "good.rs").iter().any(|n| n == "ferris"),
            "the readable file must still contribute its declarations"
        );
        Ok(())
    }

    #[test]
    fn a_binary_file_is_reported_as_not_text_without_failing_the_project() -> Result<(), String> {
        let scratch = scratch("binary")?;
        fs::write(scratch.0.join("good.rs"), good_source()).map_err(|e| e.to_string())?;
        let invalid = [0xffu8, 0xfe, 0x00, 0x80, 0x81];
        fs::write(scratch.0.join("blob.rs"), invalid).map_err(|e| e.to_string())?;

        let scan = scan(&scratch)?;

        assert_eq!(
            scan.source_bytes_read,
            good_source().len() + invalid.len(),
            "invalid UTF-8 remains charged by its exact original byte length"
        );
        assert_eq!(
            retention_of(&scan, "blob.rs"),
            Some(DeclarationRetention::Unavailable(
                SourceUnavailableReason::NotText
            )),
            "non-UTF-8 bytes are a fact about one file, not about the project"
        );
        assert!(names_of(&scan, "good.rs").iter().any(|n| n == "ferris"));
        Ok(())
    }

    #[test]
    fn valid_non_ascii_source_keeps_exact_bytes_and_content_identity() -> Result<(), String> {
        let scratch = scratch("non-ascii")?;
        let source = "pub fn greeting() -> &'static str { \"café\" }\n".as_bytes();
        fs::write(scratch.0.join("greeting.rs"), source).map_err(|error| error.to_string())?;

        let scan = scan(&scratch)?;

        assert_eq!(scan.source_bytes_read, source.len());
        assert_eq!(scan.compiler_sources.len(), 1);
        assert_eq!(
            scan.compiler_sources[0].content,
            typed_of::<InputContentSchema>(source).to_bytes()
        );
        let fields = scan.files[0]
            .1
            .file_fields()
            .ok_or("expected a source file row")?;
        assert_eq!(
            fields.content_version,
            typed_of::<InputContentSchema>(source).to_bytes(),
            "source identity remains based on the exact UTF-8 bytes"
        );
        assert!(
            names_of(&scan, "greeting.rs")
                .iter()
                .any(|name| name == "greeting")
        );
        Ok(())
    }

    #[test]
    fn an_oversized_file_is_reported_as_too_large_without_failing_the_project() -> Result<(), String>
    {
        let scratch = scratch("oversized")?;
        fs::write(scratch.0.join("good.rs"), good_source()).map_err(|e| e.to_string())?;
        let huge = vec![
            b'\n';
            SourceAdmissionLimits::default()
                .max_file_source_bytes
                .saturating_add(1)
        ];
        fs::write(scratch.0.join("huge.rs"), &huge).map_err(|e| e.to_string())?;

        let scan = scan(&scratch)?;

        assert_eq!(
            retention_of(&scan, "huge.rs"),
            Some(DeclarationRetention::Unavailable(
                SourceUnavailableReason::TooLarge
            )),
            "a file past the per-file limit must be named, not silently dropped"
        );
        assert!(names_of(&scan, "good.rs").iter().any(|n| n == "ferris"));
        Ok(())
    }

    #[test]
    fn gitignored_sources_are_excluded_while_negated_sources_are_admitted() -> Result<(), String> {
        let scratch = scratch("gitignore")?;
        fs::write(
            scratch.0.join(".gitignore"),
            b"ignored.rs\nsrc/*\n!src/keep.rs\n",
        )
        .map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("good.rs"), good_source()).map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("ignored.rs"), good_source())
            .map_err(|error| error.to_string())?;
        fs::create_dir_all(scratch.0.join("src")).map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("src/drop.rs"), good_source())
            .map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("src/keep.rs"), good_source())
            .map_err(|error| error.to_string())?;

        let scan = scan(&scratch)?;

        assert_eq!(retention_of(&scan, "ignored.rs"), None);
        assert_eq!(retention_of(&scan, "src/drop.rs"), None);
        assert_eq!(
            retention_of(&scan, "src/keep.rs"),
            Some(DeclarationRetention::Complete)
        );
        assert_eq!(
            retention_of(&scan, "good.rs"),
            Some(DeclarationRetention::Complete)
        );
        Ok(())
    }

    #[test]
    fn a_symlink_cycle_is_skipped_and_the_project_still_indexes() -> Result<(), String> {
        let scratch = scratch("symlink")?;
        fs::write(scratch.0.join("good.rs"), good_source()).map_err(|e| e.to_string())?;
        let loop_directory = scratch.0.join("loop");
        std::os::unix::fs::symlink(&scratch.0, &loop_directory).map_err(|e| e.to_string())?;
        std::os::unix::fs::symlink(scratch.0.join("good.rs"), scratch.0.join("alias.rs"))
            .map_err(|e| e.to_string())?;

        let scan = scan(&scratch)?;

        assert!(
            scan.files.iter().all(|(_, record)| record
                .file_fields()
                .is_none_or(|fields| !fields.path.contains("loop"))),
            "a symlinked directory must never be descended into"
        );
        assert_eq!(
            retention_of(&scan, "alias.rs"),
            None,
            "a symlinked file must not be admitted as a second copy of its target"
        );
        assert!(names_of(&scan, "good.rs").iter().any(|n| n == "ferris"));
        Ok(())
    }

    #[test]
    fn a_unicode_path_is_indexed_under_its_exact_coordinate() -> Result<(), String> {
        let scratch = scratch("unicode")?;
        let directory = scratch.0.join("café-日本語");
        fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        fs::write(directory.join("ünïcode.rs"), good_source()).map_err(|e| e.to_string())?;

        let scan = scan(&scratch)?;

        assert_eq!(
            retention_of(&scan, "café-日本語/ünïcode.rs"),
            Some(DeclarationRetention::Complete),
            "a non-ASCII path must round-trip exactly, not be lossily replaced"
        );
        assert!(
            names_of(&scan, "café-日本語/ünïcode.rs")
                .iter()
                .any(|n| n == "ferris")
        );
        Ok(())
    }

    #[test]
    fn a_file_removed_during_the_scan_leaves_the_project_indexable() -> Result<(), String> {
        let scratch = scratch("vanish")?;
        fs::write(scratch.0.join("good.rs"), good_source()).map_err(|e| e.to_string())?;
        let vanishing = scratch.0.join("gone.rs");
        fs::write(&vanishing, good_source()).map_err(|e| e.to_string())?;

        // Discovery lists the file; the read must then see it removed.
        let root = scratch.0.to_str().ok_or("non-UTF-8 scratch path")?;
        let frontends = frontends()?;
        let paths = supported_paths(Path::new(root), frontends)?;
        assert!(
            paths.iter().any(|path| path.ends_with("gone.rs")),
            "the fixture must be discovered before it is removed"
        );
        fs::remove_file(&vanishing).map_err(|e| e.to_string())?;

        let capability = ProjectRoot::open(Path::new(root))?;
        let source_policy = SourceAdmissionPolicy::new(SourceAdmissionLimits::default())
            .map_err(|error| error.to_string())?;
        let scanned = scan_file(
            Path::new(root),
            &capability,
            &vanishing,
            [8; 32],
            &BTreeMap::new(),
            frontends,
            source_policy,
        )?;
        assert!(
            scanned.is_none(),
            "a file removed between discovery and read is no longer part of the project"
        );

        let scan = scan(&scratch)?;
        assert!(names_of(&scan, "good.rs").iter().any(|n| n == "ferris"));
        Ok(())
    }

    #[test]
    fn an_unavailable_file_becomes_available_again_when_it_can_be_read() -> Result<(), String> {
        // The unavailable row carries a zero content version, so a later
        // successful scan must produce a different row rather than reusing the
        // reported failure forever.
        let scratch = scratch("recover")?;
        fs::write(scratch.0.join("blob.rs"), [0xffu8, 0xfe]).map_err(|e| e.to_string())?;
        let broken = scan(&scratch)?;
        assert_eq!(
            retention_of(&broken, "blob.rs"),
            Some(DeclarationRetention::Unavailable(
                SourceUnavailableReason::NotText
            ))
        );

        fs::write(scratch.0.join("blob.rs"), good_source()).map_err(|e| e.to_string())?;
        let repaired = scan(&scratch)?;
        assert_eq!(
            retention_of(&repaired, "blob.rs"),
            Some(DeclarationRetention::Complete),
            "a repaired file must stop being reported as unavailable"
        );
        assert_ne!(
            broken.source_version, repaired.source_version,
            "repairing a file must change the project source version"
        );
        assert!(names_of(&repaired, "blob.rs").iter().any(|n| n == "ferris"));
        Ok(())
    }
}

/// Discovery and semantic-profile agreement laws.
///
/// Discovery admits a file because a frontend claims its extension, and the
/// file then needs a semantic profile.  When those two tables were written
/// separately they disagreed: the TypeScript frontend claimed `js`, `jsx`,
/// `mjs` and `cjs`, the profile table knew none of them, and the mismatch was
/// classified as fatal — so any checkout containing a single `postcss.config.js`
/// could not be indexed at all.
///
/// The assertions are on rendered content — the profile each extension
/// resolves to, the declaration names a scanned config file contributes, and
/// the typed reason an unprofiled file records — because a scan that admitted
/// the files and dropped every declaration inside them would keep the same
/// file count.
#[cfg(test)]
mod profile_tests {
    use super::*;
    use backend_engine::DeclarationRetention;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(label: &str) -> Result<Scratch, String> {
        let unique = format!(
            "backend-profile-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let directory = std::env::temp_dir().join(unique);
        fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        Ok(Scratch(directory))
    }

    fn scan(scratch: &Scratch) -> Result<IndexSnapshot, String> {
        let root = scratch.0.to_str().ok_or("non-UTF-8 scratch path")?;
        scan_project(root, [11; 32], &BTreeMap::new())
    }

    fn retention_of(scan: &IndexSnapshot, path: &str) -> Option<DeclarationRetention> {
        scan.files.iter().find_map(|(_, record)| {
            let fields = record.file_fields()?;
            (fields.path == path).then_some(fields.retention)
        })
    }

    fn names_of(scan: &IndexSnapshot, path: &str) -> Vec<String> {
        scan.files
            .iter()
            .find_map(|(_, record)| {
                let fields = record.file_fields()?;
                (fields.path == path).then(|| {
                    fields
                        .declarations
                        .iter()
                        .map(|declaration| declaration.name().to_owned())
                        .collect::<Vec<_>>()
                })
            })
            .unwrap_or_default()
    }

    fn paths_of(scan: &IndexSnapshot) -> Vec<String> {
        scan.files
            .iter()
            .filter_map(|(_, record)| record.file_fields().map(|fields| fields.path.to_owned()))
            .collect()
    }

    /// Every extension any frontend claims resolves to a profile of that
    /// frontend's own language.
    ///
    /// This is the law the two tables broke.  It fails the moment a frontend
    /// gains an extension whose dialect nobody chose, which is exactly how
    /// `js` arrived without a profile.
    #[test]
    fn every_claimed_extension_resolves_to_its_own_language_profile() -> Result<(), String> {
        let frontends = FrontendSet::build()?;
        let mut checked = 0_usize;
        for frontend in &frontends.values {
            let language = frontend.language();
            for extension in frontend.baseline.extensions() {
                let path = PathBuf::from(format!("claimed.{extension}"));
                let profile = profile_of(&frontends, &path).ok_or_else(|| {
                    format!(
                        "{language:?} claims .{extension} but no semantic profile \
                         covers it, so every project containing one fails"
                    )
                })?;
                if profile.language() != language {
                    return Err(format!(
                        ".{extension} is claimed by {language:?} but resolves to \
                         {:?}",
                        profile.language()
                    ));
                }
                checked = checked.checked_add(1).ok_or("extension count overflow")?;
            }
        }
        if checked < 20 {
            return Err(format!(
                "only {checked} claimed extensions were inspected; the frontend \
                 inventory cannot have shrunk this far"
            ));
        }
        Ok(())
    }

    /// The extensions the QA report named resolve to the dialect they are.
    #[test]
    fn javascript_and_python_stub_extensions_carry_their_dialect() -> Result<(), String> {
        let frontends = FrontendSet::build()?;
        let expected = [
            (
                "app.js",
                LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            ),
            (
                "app.mjs",
                LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            ),
            (
                "app.cjs",
                LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            ),
            (
                "app.mts",
                LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            ),
            (
                "app.cts",
                LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            ),
            (
                "app.ts",
                LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            ),
            (
                "app.jsx",
                LanguageProfile::TypeScript(TypeScriptSource::Tsx),
            ),
            (
                "app.tsx",
                LanguageProfile::TypeScript(TypeScriptSource::Tsx),
            ),
            (
                "stub.pyi",
                LanguageProfile::Python(PythonVersion::Python314),
            ),
            ("app.pyw", LanguageProfile::Python(PythonVersion::Python314)),
            ("lib.m", LanguageProfile::C(CStandard::C23)),
            ("lib.mm", LanguageProfile::Cxx(CxxStandard::Cxx23)),
        ];
        for (name, want) in expected {
            let got = profile_of(&frontends, Path::new(name))
                .ok_or_else(|| format!("{name} has no semantic profile"))?;
            if got != want {
                return Err(format!("{name} resolved to {got:?}, expected {want:?}"));
            }
        }
        Ok(())
    }

    /// An extension with no profile is one unavailable file, never a fatal
    /// scan.
    ///
    /// The distinction is the whole defect: `Fatal` propagates out of
    /// `scan_file` and fails `scan_project`, so the project cannot be indexed,
    /// while `Unavailable` becomes a row and the rest of the project survives.
    #[test]
    fn an_unprofiled_extension_is_unavailable_and_not_fatal() -> Result<(), String> {
        let frontends = FrontendSet::build()?;
        let fault = profile_fault(&frontends, Path::new("weird.qq")).err();
        if fault != Some(SourceFault::Unavailable(SourceUnavailableReason::Unparsed)) {
            return Err(format!(
                "an unprofiled extension produced {fault:?}; anything fatal here \
                 sinks the whole project"
            ));
        }
        profile_fault(&frontends, Path::new("postcss.config.js"))
            .map_err(|fault| format!("a claimed .js file still faults: {fault:?}"))?;
        Ok(())
    }

    /// A front-end checkout with a root config script and a framework cache
    /// indexes: the cache is never walked, the config file is scanned.
    #[test]
    fn a_javascript_project_indexes_its_config_and_skips_framework_caches() -> Result<(), String> {
        let scratch = scratch("javascript")?;
        fs::write(
            scratch.0.join("postcss.config.js"),
            b"function tailwindPlugin(options) { return options; }\n",
        )
        .map_err(|error| error.to_string())?;
        fs::create_dir_all(scratch.0.join("src")).map_err(|error| error.to_string())?;
        fs::write(
            scratch.0.join("src/app.ts"),
            b"export function renderShell(depth: number): number { return depth; }\n",
        )
        .map_err(|error| error.to_string())?;
        fs::create_dir_all(scratch.0.join(".angular/cache")).map_err(|e| e.to_string())?;
        fs::write(
            scratch.0.join(".angular/cache/x.js"),
            vec![
                b'x';
                SourceAdmissionLimits::default()
                    .max_file_source_bytes
                    .saturating_add(1)
            ],
        )
        .map_err(|error| error.to_string())?;
        fs::create_dir_all(scratch.0.join(".next/static")).map_err(|error| error.to_string())?;
        fs::write(
            scratch.0.join(".next/static/chunk.js"),
            vec![
                b'x';
                SourceAdmissionLimits::default()
                    .max_file_source_bytes
                    .saturating_add(1)
            ],
        )
        .map_err(|error| error.to_string())?;

        let scan = scan(&scratch)?;

        assert_eq!(
            retention_of(&scan, "postcss.config.js"),
            Some(DeclarationRetention::Complete),
            "a root config script must be scanned, not refused"
        );
        assert!(
            names_of(&scan, "postcss.config.js")
                .iter()
                .any(|name| name == "tailwindPlugin"),
            "the config file must contribute its declarations, got {:?}",
            names_of(&scan, "postcss.config.js")
        );
        assert!(
            names_of(&scan, "src/app.ts")
                .iter()
                .any(|name| name == "renderShell"),
            "the TypeScript source must still index alongside it, got {:?}",
            names_of(&scan, "src/app.ts")
        );
        let paths = paths_of(&scan);
        assert!(
            !paths.iter().any(|path| path.starts_with(".angular")),
            "a framework cache must never be walked, found {paths:?}"
        );
        assert!(
            !paths.iter().any(|path| path.starts_with(".next")),
            "a framework cache must never be walked, found {paths:?}"
        );
        Ok(())
    }

    /// Every build-output directory the C# oracle refuses to load is also
    /// refused by discovery, so no `.cs` file waits for semantics that the
    /// oracle will never produce.
    ///
    /// Mirrors `frontends/csharp/helper/SourceLoader.cs:73`.
    #[test]
    fn discovery_skips_every_directory_the_csharp_oracle_skips() {
        for name in ["bin", "obj", ".git", ".vs", "node_modules"] {
            assert!(
                backend_library::is_hard_ignored_directory(std::ffi::OsStr::new(name)),
                "{name} is excluded by the C# source loader but still walked here"
            );
        }
    }

    /// The tool and cache directories a real checkout carries are not walked.
    #[test]
    fn discovery_skips_framework_and_tool_caches() {
        for name in [
            ".angular",
            ".next",
            ".nuxt",
            ".svelte-kit",
            ".turbo",
            ".cache",
            ".vite",
            ".parcel-cache",
            ".webpack",
            ".rollup.cache",
            ".nx",
            "out",
            "coverage",
            "storybook-static",
            ".nyc_output",
            "Library",
            ".gradle",
            ".mypy_cache",
            ".pytest_cache",
            "__pycache__",
            ".tox",
            ".ruff_cache",
            "jspm_packages",
            "vendor",
        ] {
            assert!(
                backend_library::is_hard_ignored_directory(std::ffi::OsStr::new(name)),
                "{name} is still walked"
            );
        }
        assert!(
            !backend_library::is_hard_ignored_directory(std::ffi::OsStr::new("src")),
            "discovery must not start guessing which directories are the project"
        );
    }
}

#[cfg(test)]
mod compiler_workspace_snapshot_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch() -> Result<Scratch, String> {
        // Keep the full path below macOS's SUN_LEN limit for socket fixtures.
        let serial = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("bcw-{}-{serial}", std::process::id()));
        fs::create_dir_all(&path).map_err(|error| error.to_string())?;
        Ok(Scratch(path))
    }

    #[test]
    fn snapshot_inventory_includes_root_config_zero_byte_files_and_fences_new_paths()
    -> Result<(), String> {
        let scratch = scratch()?;
        fs::create_dir_all(scratch.0.join("src")).map_err(|error| error.to_string())?;
        fs::create_dir_all(scratch.0.join("target")).map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("Cargo.lock"), b"lock").map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("src/main.rs"), b"fn main() {}")
            .map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("empty.bin"), b"").map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("target/generated.rs"), b"ignored")
            .map_err(|error| error.to_string())?;

        let snapshot = CompilerWorkspaceSnapshot::open(&scratch.0)?;
        let original_fence = snapshot.fence_digest();
        assert_eq!(
            snapshot.entries().first().map(|entry| entry.path.as_str()),
            Some("")
        );
        assert_eq!(
            snapshot.policy_identity(),
            COMPILER_WORKSPACE_POLICY_IDENTITY
        );
        assert!(snapshot.entries().iter().any(|entry| {
            entry.path == "Cargo.lock"
                && entry.kind == CompilerWorkspaceEntryKind::File
                && entry.byte_length == Some(4)
        }));
        assert!(snapshot.entries().iter().any(|entry| {
            entry.path == "empty.bin"
                && entry.kind == CompilerWorkspaceEntryKind::File
                && entry.byte_length == Some(0)
        }));
        assert!(
            !snapshot
                .entries()
                .iter()
                .any(|entry| entry.path.starts_with("target/"))
        );
        assert_eq!(snapshot.read_file("empty.bin", 0)?, b"");
        assert_eq!(snapshot.read_file("Cargo.lock", 4)?, b"lock");
        let mut streamed = Vec::new();
        assert_eq!(
            snapshot.stream_file("Cargo.lock", 4, 2, |chunk| {
                streamed.extend_from_slice(chunk);
                Ok(())
            })?,
            4
        );
        assert_eq!(streamed, b"lock");
        assert!(snapshot.revalidate()?);

        fs::write(scratch.0.join("new.config"), b"new").map_err(|error| error.to_string())?;
        assert!(!snapshot.revalidate()?);
        assert_ne!(
            CompilerWorkspaceSnapshot::open(&scratch.0)?.fence_digest(),
            original_fence
        );
        Ok(())
    }

    #[test]
    fn snapshot_fence_detects_content_and_metadata_changes() -> Result<(), String> {
        let scratch = scratch()?;
        fs::write(scratch.0.join("lockfile"), b"before").map_err(|error| error.to_string())?;
        let snapshot = CompilerWorkspaceSnapshot::open(&scratch.0)?;
        fs::write(scratch.0.join("lockfile"), b"after!").map_err(|error| error.to_string())?;
        assert!(!snapshot.revalidate()?);
        assert!(snapshot.read_file("lockfile", 16).is_err());
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn snapshot_fence_detects_same_length_edit_after_last_write_is_restored() -> Result<(), String>
    {
        use std::fs::{FileTimes, OpenOptions};
        use std::time::Duration;

        let scratch = scratch()?;
        let path = scratch.0.join("Cargo.lock");
        fs::write(&path, b"before!!").map_err(|error| error.to_string())?;
        let original_modified = fs::metadata(&path)
            .map_err(|error| error.to_string())?
            .modified()
            .map_err(|error| error.to_string())?;
        let snapshot = CompilerWorkspaceSnapshot::open(&scratch.0)?;
        let captured = snapshot
            .entries()
            .iter()
            .find(|entry| entry.path == "Cargo.lock")
            .ok_or_else(|| "captured file is missing".to_owned())?
            .revision
            .windows;

        // Give even filesystems with coarse change-time granularity a new
        // timestamp bucket before restoring the user-settable last-write time.
        std::thread::sleep(Duration::from_millis(2_100));
        fs::write(&path, b"after!!!").map_err(|error| error.to_string())?;
        OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(|error| error.to_string())?
            .set_times(FileTimes::new().set_modified(original_modified))
            .map_err(|error| error.to_string())?;
        let restored_modified = fs::metadata(&path)
            .map_err(|error| error.to_string())?
            .modified()
            .map_err(|error| error.to_string())?;
        assert_eq!(restored_modified, original_modified);

        let current = snapshot
            .root_capability
            .revision_relative(Path::new("Cargo.lock"))?
            .windows;
        assert_eq!(current.length, captured.length);
        assert_eq!(current.file_id, captured.file_id);
        assert_eq!(current.last_write_time, captured.last_write_time);
        assert_ne!(current.change_time, captured.change_time);
        assert!(!snapshot.revalidate()?);
        assert!(snapshot.read_file("Cargo.lock", 16).is_err());
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn snapshot_rejects_junctions_and_hardlink_aliases() -> Result<(), String> {
        use std::process::Command;

        let scratch = scratch()?;
        let outside = scratch.0.join("outside-target");
        fs::create_dir(&outside).map_err(|error| error.to_string())?;
        fs::write(outside.join("outside.rs"), b"fn outside() {}\n")
            .map_err(|error| error.to_string())?;
        let junction = scratch.0.join("junction");
        let output = Command::new("cmd.exe")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside)
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(format!("mklink /J failed: {output:?}"));
        }
        assert!(CompilerWorkspaceSnapshot::open(&scratch.0).is_err());
        fs::remove_dir(&junction).map_err(|error| error.to_string())?;

        let outside_file = outside.join("linked.rs");
        fs::write(&outside_file, b"fn shared() {}\n").map_err(|error| error.to_string())?;
        fs::hard_link(&outside_file, scratch.0.join("linked.rs"))
            .map_err(|error| error.to_string())?;
        assert!(CompilerWorkspaceSnapshot::open(&scratch.0).is_err());
        Ok(())
    }

    #[test]
    fn workspace_paths_must_be_utf8_nfc_and_portable() {
        assert!(normalized_workspace_path(Path::new("src/main.rs")).is_ok());
        // A backslash inside a component is a non-portable name on Unix. On
        // Windows it is the separator, so the same text is the path
        // `src/main.rs`, and must normalize to exactly that.
        #[cfg(unix)]
        assert!(normalized_workspace_path(Path::new("src\\main.rs")).is_err());
        #[cfg(windows)]
        assert_eq!(
            normalized_workspace_path(Path::new("src\\main.rs")).as_deref(),
            Ok("src/main.rs")
        );
        assert!(normalized_workspace_path(Path::new("src/e\u{301}.txt")).is_err());
    }

    #[test]
    fn case_colliding_names_fail_portable_workspace_admission() -> Result<(), String> {
        let scratch = scratch()?;
        fs::write(scratch.0.join("Foo"), b"upper").map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("foo"), b"lower").map_err(|error| error.to_string())?;
        if fs::read(scratch.0.join("Foo")).map_err(|error| error.to_string())? != b"upper" {
            // The host filesystem aliases case variants, so it cannot
            // represent the collision this test exercises.
            return Ok(());
        }
        assert!(CompilerWorkspaceSnapshot::open(&scratch.0).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_rejects_symlinks_instead_of_silently_omitting_them() -> Result<(), String> {
        let scratch = scratch()?;
        let outside = scratch.0.with_extension("outside");
        fs::write(&outside, b"external").map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&outside, scratch.0.join("alias"))
            .map_err(|error| error.to_string())?;
        let result = CompilerWorkspaceSnapshot::open(&scratch.0);
        let _ = fs::remove_file(outside);
        assert!(result.is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_rejects_special_files_instead_of_admitting_them() -> Result<(), String> {
        use std::os::unix::net::UnixListener;

        let scratch = scratch()?;
        let _socket = UnixListener::bind(scratch.0.join("control.sock"))
            .map_err(|error| error.to_string())?;
        assert!(CompilerWorkspaceSnapshot::open(&scratch.0).is_err());
        Ok(())
    }
}
