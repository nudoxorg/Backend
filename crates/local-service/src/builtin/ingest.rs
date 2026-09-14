//! Deterministic, parallel, content-versioned filesystem ingestion.

use backend_compile::{InputContentSchema, SourceLanguage, SyntaxFrontend, typed_of};
use backend_engine::{
    ProductSourceRecord, ProductSourceRelation, Relation, SourceUnavailableReason,
    product_source_file_key,
};
use backend_semantic::vocabulary::{
    CSharpVersion, CStandard, CxxStandard, GoVersion, JavaRelease, LanguageProfile, PythonVersion,
    RustEdition, TypeScriptSource,
};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, mpsc};
use std::thread;

const MAX_SOURCE_BYTES: usize = 512 * 1024;
// One relation row can never exceed the canonical node capacity, so the
// per-record ceiling is that bound rather than an independent number that
// would admit records the tree must later reject.
const MAX_ENCODED_RECORD_BYTES: usize = ProductSourceRecord::ROW_VALUE_CAPACITY;
const MAX_TOTAL_SOURCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_TOTAL_ENCODED_RECORD_BYTES: usize = 64 * 1024 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 100_000;
const MAX_DISCOVERY_ENTRIES: usize = 500_000;
const MAX_WORKERS: usize = 8;
const RESULT_QUEUE_PER_WORKER: usize = 2;

/// One complete, deterministic project scan before comparison with the
/// selected versioned relation.
pub(super) struct IndexSnapshot {
    pub(super) source_version: [u8; 32],
    pub(super) files: Vec<([u8; 32], ProductSourceRecord)>,
    pub(super) compiler_sources: Vec<CompilerSource>,
}

/// UTF-8 source admitted for one exact semantic authority slot.
pub(super) struct CompilerSource {
    pub(super) relative_path: String,
    pub(super) profile: LanguageProfile,
    pub(super) source: String,
}

struct ScannedFile {
    relative: String,
    key: [u8; 32],
    record: ProductSourceRecord,
    source_bytes: usize,
    encoded_record_bytes: usize,
    compiler_source: CompilerSource,
}

/// An opened project directory capability. Source reads resolve every path
/// component relative to this descriptor with symlink following disabled, so
/// a concurrently modified checkout cannot redirect ingestion outside the
/// admitted project after discovery.
struct ProjectRoot {
    #[cfg(unix)]
    directory: fs::File,
    #[cfg(not(unix))]
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
        #[cfg(not(unix))]
        {
            Ok(Self {
                canonical: path.to_path_buf(),
            })
        }
    }

    /// Opens one project-relative path, following no symlink at any step.
    ///
    /// Each component is resolved against the previously opened directory
    /// descriptor, so a checkout mutated during ingestion cannot redirect the
    /// read outside the admitted project.
    #[cfg(unix)]
    fn open_confined(&self, relative: &Path) -> Result<fs::File, SourceFault> {
        use rustix::fs::{Mode, OFlags, openat};
        let escaped =
            || SourceFault::Fatal("source path escaped its project root".to_owned());
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
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(fs::File::from)
        .map_err(|error| SourceFault::from_open(error.kind()))
    }

    fn read(&self, relative: &Path) -> Result<Vec<u8>, SourceFault> {
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
            read_bounded(self.open_confined(relative)?)
        }
        #[cfg(not(unix))]
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
            read_bounded(file)
        }
    }
}

/// Reads one already-opened regular file within the per-file byte bound.
///
/// Every failure here is a fact about the file rather than the project, so it
/// is classified rather than propagated as a scan error.
fn read_bounded(mut file: fs::File) -> Result<Vec<u8>, SourceFault> {
    let metadata = file
        .metadata()
        .map_err(|_| SourceFault::Unavailable(SourceUnavailableReason::Unreadable))?;
    if !metadata.is_file() {
        return Err(SourceFault::Vanished);
    }
    let capacity = usize::try_from(metadata.len())
        .ok()
        .filter(|length| *length <= MAX_SOURCE_BYTES)
        .ok_or(SourceFault::Unavailable(SourceUnavailableReason::TooLarge))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| SourceFault::Fatal("source allocation exceeds memory".to_owned()))?;
    let bound = u64::try_from(MAX_SOURCE_BYTES)
        .map_err(|_| SourceFault::Fatal("source bound exceeds this target".to_owned()))?;
    file.by_ref()
        .take(bound.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| SourceFault::Unavailable(SourceUnavailableReason::Unreadable))?;
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(SourceFault::Unavailable(SourceUnavailableReason::TooLarge));
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
struct IngestBudget {
    source_bytes: usize,
    encoded_record_bytes: usize,
}

impl IngestBudget {
    fn charge(&mut self, file: &ScannedFile) -> Result<(), String> {
        self.charge_lengths(file.source_bytes, file.encoded_record_bytes)
    }

    fn charge_lengths(
        &mut self,
        source_bytes: usize,
        encoded_record_bytes: usize,
    ) -> Result<(), String> {
        self.source_bytes = self
            .source_bytes
            .checked_add(source_bytes)
            .ok_or_else(|| "source byte count overflow".to_owned())?;
        self.encoded_record_bytes = self
            .encoded_record_bytes
            .checked_add(encoded_record_bytes)
            .ok_or_else(|| "decoded record byte count overflow".to_owned())?;
        if self.source_bytes > MAX_TOTAL_SOURCE_BYTES {
            return Err("project source exceeds the bounded ingest budget".to_owned());
        }
        if self.encoded_record_bytes > MAX_TOTAL_ENCODED_RECORD_BYTES {
            return Err("project decoded records exceed the bounded ingest budget".to_owned());
        }
        Ok(())
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
                    backend_engine::application::LocalCompilerCapabilityState::Unavailable => *status,
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

/// Reads supported sources and reuses prior analyses behind exact content and
/// producer-version fences.
pub(super) fn scan_project(
    coordinate: &str,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
) -> Result<IndexSnapshot, String> {
    let root = Path::new(coordinate)
        .canonicalize()
        .map_err(|error| format!("open project {coordinate}: {error}"))?;
    if !root.is_dir() {
        return Err(format!("project {} is not a directory", root.display()));
    }
    let root_capability = ProjectRoot::open(&root)?;
    let frontends = frontends()?;
    let mut paths = supported_paths(&root, frontends)?;
    paths.sort();
    preflight_source_bytes(&paths)?;
    let workers = thread::available_parallelism()
        .map_or(1, usize::from)
        .min(MAX_WORKERS)
        .min(paths.len().max(1));
    let chunk = paths.len().div_ceil(workers);
    let scanned = thread::scope(|scope| {
        let queue = workers
            .checked_mul(RESULT_QUEUE_PER_WORKER)
            .ok_or_else(|| "source result queue width overflow".to_owned())?;
        let (sender, receiver) = mpsc::sync_channel(queue);
        let mut handles = Vec::with_capacity(workers);
        for group in paths.chunks(chunk.max(1)) {
            let root = &root;
            let root_capability = &root_capability;
            let sender = sender.clone();
            handles.push(scope.spawn(move || {
                for path in group {
                    let result =
                        scan_file(root, root_capability, path, project, reusable, frontends);
                    let failed = result.is_err();
                    if sender.send(result).is_err() || failed {
                        break;
                    }
                }
            }));
        }
        drop(sender);
        let mut output = Vec::with_capacity(paths.len());
        let mut budget = IngestBudget::default();
        let mut failure = None;
        for result in receiver {
            match result {
                Ok(Some(file)) if failure.is_none() => {
                    if let Err(error) = budget.charge(&file) {
                        failure = Some(error);
                    } else {
                        output.push(file);
                    }
                }
                Err(error) if failure.is_none() => failure = Some(error),
                Ok(_) | Err(_) => {}
            }
        }
        for handle in handles {
            if handle.join().is_err() && failure.is_none() {
                failure = Some("source analysis worker panicked".to_owned());
            }
        }
        failure.map_or(Ok(output), Err)
    })?;
    let mut scanned = scanned;
    scanned.sort_by(|left, right| left.relative.cmp(&right.relative));

    let mut source = blake3::Hasher::new();
    source.update(b"backend.project-snapshot.v2\0");
    let mut files = Vec::with_capacity(scanned.len());
    let mut compiler_sources = Vec::with_capacity(scanned.len());
    for scanned in scanned {
        let ScannedFile {
            key,
            record,
            compiler_source,
            ..
        } = scanned;
        let file = record
            .file_fields()
            .ok_or_else(|| "frontend produced a non-file record".to_owned())?;
        source.update(&key);
        source.update(&file.content_version);
        source.update(&file.analysis_version);
        files.push((key, record));
        compiler_sources.push(compiler_source);
    }
    files.sort_by_key(|(key, _)| *key);
    Ok(IndexSnapshot {
        source_version: *source.finalize().as_bytes(),
        files,
        compiler_sources,
    })
}

fn supported_paths(root: &Path, frontends: &FrontendSet) -> Result<Vec<PathBuf>, String> {
    let mut output = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    let mut budget = DiscoveryBudget::default();
    while let Some(directory) = pending.pop() {
        let reader = fs::read_dir(&directory)
            .map_err(|error| format!("read {}: {error}", directory.display()))?;
        let mut entries = Vec::new();
        for entry in reader {
            let entry = entry.map_err(|error| format!("read {}: {error}", directory.display()))?;
            budget
                .admit_entry(entries.len())
                .map_err(|error| format!("{error}: {}", directory.display()))?;
            entries.push(entry);
        }
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries.into_iter().rev() {
            let file_type = entry
                .file_type()
                .map_err(|error| format!("inspect {}: {error}", entry.path().display()))?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if !ignored_directory(&entry.file_name().to_string_lossy()) {
                    pending.push(entry.path());
                }
            } else if file_type.is_file() && frontends.for_path(&entry.path()).is_some() {
                if output.len() >= ProductSourceRecord::MAX_PROJECT_FILES {
                    return Err("project contains too many supported source files".to_owned());
                }
                output.push(entry.path());
            }
        }
    }
    Ok(output)
}

/// Charges the aggregate source budget before any file is read.
///
/// A file that is oversized or unreadable is not a reason to refuse the
/// project: it is charged as zero bytes here and reported per file by
/// [`scan_file`].  Only the aggregate budget, which protects the process
/// rather than describing a file, can still stop the scan.
fn preflight_source_bytes(paths: &[PathBuf]) -> Result<(), String> {
    let mut total = 0_usize;
    for path in paths {
        let Ok(metadata) = fs::metadata(path) else {
            continue;
        };
        let Ok(length) = usize::try_from(metadata.len()) else {
            continue;
        };
        if length > MAX_SOURCE_BYTES {
            continue;
        }
        total = total
            .checked_add(length)
            .ok_or_else(|| "source byte count overflow".to_owned())?;
        if total > MAX_TOTAL_SOURCE_BYTES {
            return Err("project source exceeds the bounded ingest budget".to_owned());
        }
    }
    Ok(())
}

fn ignored_directory(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".backend"
            | "target"
            | "node_modules"
            | ".venv"
            | "venv"
            | "dist"
            | "build"
            | ".idea"
            | ".vscode"
    )
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
) -> Result<Option<ScannedFile>, String> {
    match scan_one(root, root_capability, path, project, reusable, frontends) {
        Ok(file) => Ok(Some(file)),
        Err(SourceFault::Vanished) => Ok(None),
        Err(SourceFault::Fatal(message)) => {
            Err(format!("read source {}: {message}", path.display()))
        }
        Err(SourceFault::Unavailable(reason)) => {
            unavailable_file(root, path, project, frontends, reason).map(Some)
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
        compiler_source: CompilerSource {
            profile: source_profile(path)?,
            relative_path: relative.clone(),
            source: String::new(),
        },
        relative,
        key,
        record,
        source_bytes: 0,
        encoded_record_bytes: encoded.len(),
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
) -> Result<ScannedFile, SourceFault> {
    let relative_path = path.strip_prefix(root).map_err(|_| {
        SourceFault::Fatal("source path escaped its project root".to_owned())
    })?;
    let bytes = root_capability.read(relative_path)?;
    let relative = relative_path
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    let source = std::str::from_utf8(&bytes)
        .map_err(|_| SourceFault::Unavailable(SourceUnavailableReason::NotText))?
        .to_owned();
    let frontend = frontends
        .for_path(path)
        .ok_or_else(|| SourceFault::Fatal("unsupported source language".to_owned()))?;
    let key = product_source_file_key(project, &relative);
    let content = typed_of::<InputContentSchema>(&bytes).to_bytes();
    let analysis = frontend.analysis_version();

    if let Some(record) = reusable.get(&key)
        && record.file_fields().is_some_and(|fields| {
            fields.project == project
                && fields.path == relative
                && fields.language == frontend.language()
                && fields.content_version == content
                && fields.analysis_version == analysis
        })
    {
        return scanned_file(relative, key, record.clone(), source, bytes.len(), path)
            .map_err(SourceFault::Fatal);
    }

    // Structural parsing is an explicit baseline projection for local browsing.
    // Package semantics are compiled and published by the engine application module.
    let analyzed = frontend
        .baseline
        .analyze(Path::new(&relative), &bytes)
        .map_err(|_| SourceFault::Unavailable(SourceUnavailableReason::Unparsed))?;
    debug_assert_eq!(analyzed.language(), frontend.language());
    debug_assert_eq!(analyzed.content().to_bytes(), content);
    // A real source file routinely extracts more detail than one canonical
    // relation row can carry: `memchr 2.8.3` alone produces a 65 686 byte row
    // for `src/arch/x86_64/avx2/memchr.rs` against a 65 464 byte capacity.
    // Constructing through the capacity-aware path sheds derived detail in a
    // fixed order and records how far it had to go, instead of failing the
    // whole project when the relation delta is later prepared.
    let record = ProductSourceRecord::file_within_row_capacity(
        project,
        relative.clone(),
        analyzed.language(),
        analyzed.content().to_bytes(),
        analysis,
        analyzed.declarations().clone(),
    )
    .map_err(SourceFault::Fatal)?;
    scanned_file(relative, key, record, source, bytes.len(), path).map_err(SourceFault::Fatal)
}

fn scanned_file(
    relative: String,
    key: [u8; 32],
    record: ProductSourceRecord,
    source: String,
    source_bytes: usize,
    path: &Path,
) -> Result<ScannedFile, String> {
    let mut encoded = Vec::new();
    ProductSourceRelation::encode_value(&record, &mut encoded);
    let encoded_record_bytes = encoded.len();
    if encoded_record_bytes > MAX_ENCODED_RECORD_BYTES {
        return Err(format!(
            "source projection {} exceeds the {} byte record limit",
            path.display(),
            MAX_ENCODED_RECORD_BYTES
        ));
    }
    Ok(ScannedFile {
        compiler_source: CompilerSource {
            profile: source_profile(path)?,
            relative_path: relative.clone(),
            source,
        },
        relative,
        key,
        record,
        source_bytes,
        encoded_record_bytes,
    })
}

pub(super) fn source_profile(path: &Path) -> Result<LanguageProfile, String> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    match extension {
        "rs" => Ok(LanguageProfile::Rust(RustEdition::Rust2024)),
        "ts" => Ok(LanguageProfile::TypeScript(TypeScriptSource::TypeScript)),
        "tsx" => Ok(LanguageProfile::TypeScript(TypeScriptSource::Tsx)),
        "py" => Ok(LanguageProfile::Python(PythonVersion::Python314)),
        "go" => Ok(LanguageProfile::Go(GoVersion::Go125)),
        "java" => Ok(LanguageProfile::Java(JavaRelease::Java25)),
        "cs" => Ok(LanguageProfile::CSharp(CSharpVersion::CSharp14)),
        "c" | "h" => Ok(LanguageProfile::C(CStandard::C23)),
        "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => Ok(LanguageProfile::Cxx(CxxStandard::Cxx23)),
        _ => Err(format!("source {} has no semantic profile", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn aggregate_budgets_reject_the_first_byte_beyond_each_limit() {
        let mut source = IngestBudget {
            source_bytes: MAX_TOTAL_SOURCE_BYTES,
            encoded_record_bytes: 0,
        };
        assert!(source.charge_lengths(1, 0).is_err());

        let mut decoded = IngestBudget {
            source_bytes: 0,
            encoded_record_bytes: MAX_TOTAL_ENCODED_RECORD_BYTES,
        };
        assert!(decoded.charge_lengths(0, 1).is_err());
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
        assert!(root.read(Path::new("victim.rs")).is_err());

        fs::create_dir(project.join("real")).map_err(|error| error.to_string())?;
        fs::write(project.join("real/lib.rs"), b"pub fn safe() {}")
            .map_err(|error| error.to_string())?;
        // A confined read still succeeds; a substituted symlink is refused
        // rather than admitted, and the refusal is now a typed per-file fault.
        assert_eq!(
            root.read(Path::new("real/lib.rs"))
                .map_err(|error| format!("{error:?}"))?,
            b"pub fn safe() {}"
        );
        fs::remove_dir_all(project.join("real")).map_err(|error| error.to_string())?;
        symlink(&scratch.0, project.join("real")).map_err(|error| error.to_string())?;
        assert!(root.read(Path::new("real/outside.rs")).is_err());
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
    fn a_file_within_one_row_keeps_complete_retention_and_its_excerpts()
    -> Result<(), String> {
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
        fs::write(
            scratch.0.join("dense.rs"),
            dense_source(140).as_bytes(),
        )
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
#[cfg(test)]
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
    fn a_binary_file_is_reported_as_not_text_without_failing_the_project()
    -> Result<(), String> {
        let scratch = scratch("binary")?;
        fs::write(scratch.0.join("good.rs"), good_source()).map_err(|e| e.to_string())?;
        fs::write(scratch.0.join("blob.rs"), [0xffu8, 0xfe, 0x00, 0x80, 0x81])
            .map_err(|e| e.to_string())?;

        let scan = scan(&scratch)?;

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
    fn an_oversized_file_is_reported_as_too_large_without_failing_the_project()
    -> Result<(), String> {
        let scratch = scratch("oversized")?;
        fs::write(scratch.0.join("good.rs"), good_source()).map_err(|e| e.to_string())?;
        let huge = vec![b'\n'; MAX_SOURCE_BYTES.saturating_add(1)];
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
        let scanned = scan_file(
            Path::new(root),
            &capability,
            &vanishing,
            [8; 32],
            &BTreeMap::new(),
            frontends,
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
    fn an_unavailable_file_becomes_available_again_when_it_can_be_read()
    -> Result<(), String> {
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
