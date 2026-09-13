//! Deterministic, parallel, content-versioned filesystem ingestion.

use backend_compile::{InputContentSchema, SourceLanguage, SyntaxFrontend, typed_of};
use backend_engine::{
    ProductSourceRecord, ProductSourceRelation, Relation, product_source_file_key,
};
use compiler_vocabulary::{
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
const MAX_ENCODED_RECORD_BYTES: usize = 1024 * 1024;
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

    fn read(&self, relative: &Path) -> Result<Vec<u8>, String> {
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err("source path is not a confined relative path".to_owned());
        }
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags, openat};
            let components = relative.components().collect::<Vec<_>>();
            let mut directory = self
                .directory
                .try_clone()
                .map_err(|error| error.to_string())?;
            for component in &components[..components.len() - 1] {
                let std::path::Component::Normal(name) = component else {
                    return Err("source path escaped its project root".to_owned());
                };
                directory = openat(
                    &directory,
                    *name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map(fs::File::from)
                .map_err(|_| "source directory changed during ingestion".to_owned())?;
            }
            let std::path::Component::Normal(name) = components[components.len() - 1] else {
                return Err("source path escaped its project root".to_owned());
            };
            let mut file = openat(
                &directory,
                name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map(fs::File::from)
            .map_err(|_| "source file changed during ingestion".to_owned())?;
            let metadata = file.metadata().map_err(|error| error.to_string())?;
            if !metadata.is_file() || metadata.len() > MAX_SOURCE_BYTES as u64 {
                return Err("source is not a bounded regular file".to_owned());
            }
            let capacity = usize::try_from(metadata.len())
                .map_err(|_| "source length exceeds this target's address space".to_owned())?;
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(capacity)
                .map_err(|_| "source allocation exceeds available memory".to_owned())?;
            file.by_ref()
                .take((MAX_SOURCE_BYTES as u64).saturating_add(1))
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            if bytes.len() > MAX_SOURCE_BYTES {
                return Err("source grew beyond the bounded file limit".to_owned());
            }
            Ok(bytes)
        }
        #[cfg(not(unix))]
        {
            let path = self.canonical.join(relative);
            let canonical = path
                .canonicalize()
                .map_err(|error| format!("resolve source {}: {error}", path.display()))?;
            if !canonical.starts_with(&self.canonical) {
                return Err("source path escaped its project root".to_owned());
            }
            let mut file = std::fs::File::open(canonical).map_err(|error| error.to_string())?;
            let mut bytes = Vec::new();
            file.by_ref()
                .take((MAX_SOURCE_BYTES as u64).saturating_add(1))
                .read_to_end(&mut bytes)
                .map_err(|error| error.to_string())?;
            if bytes.len() > MAX_SOURCE_BYTES {
                return Err("source grew beyond the bounded file limit".to_owned());
            }
            Ok(bytes)
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
    compiler: &compiler_application::LocalCompilerClient,
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
                    compiler_application::LocalCompilerCapabilityState::Probing => {
                        backend_engine::CapabilityStatus::probing(status.id(), status.family())
                    }
                    compiler_application::LocalCompilerCapabilityState::ProbeFailed => {
                        backend_engine::CapabilityStatus::unavailable(
                            status.id(),
                            status.family(),
                            backend_engine::CapabilityUnavailable::ProbeFailed,
                        )
                    }
                    compiler_application::LocalCompilerCapabilityState::Unavailable => *status,
                    compiler_application::LocalCompilerCapabilityState::Ready => {
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
                Ok(file) if failure.is_none() => {
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

fn preflight_source_bytes(paths: &[PathBuf]) -> Result<(), String> {
    let mut total = 0_usize;
    for path in paths {
        let metadata = fs::metadata(path)
            .map_err(|error| format!("inspect source {}: {error}", path.display()))?;
        let length = usize::try_from(metadata.len())
            .map_err(|_| format!("source {} is too large", path.display()))?;
        if length > MAX_SOURCE_BYTES {
            return Err(format!(
                "source {} exceeds the {} byte file limit",
                path.display(),
                MAX_SOURCE_BYTES
            ));
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

fn scan_file(
    root: &Path,
    root_capability: &ProjectRoot,
    path: &Path,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    frontends: &FrontendSet,
) -> Result<ScannedFile, String> {
    let relative_path = path
        .strip_prefix(root)
        .map_err(|_| "source path escaped its project root".to_owned())?;
    let bytes = root_capability
        .read(relative_path)
        .map_err(|error| format!("read source {}: {error}", path.display()))?;
    let relative = relative_path
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    let source = std::str::from_utf8(&bytes)
        .map_err(|_| format!("source {} is not UTF-8", path.display()))?
        .to_owned();
    let frontend = frontends
        .for_path(path)
        .ok_or_else(|| "unsupported source language".to_owned())?;
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
        return scanned_file(relative, key, record.clone(), source, bytes.len(), path);
    }

    // Structural parsing is an explicit baseline projection for local browsing.
    // Package semantics are compiled and published by compiler-application.
    let analyzed = frontend
        .baseline
        .analyze(Path::new(&relative), &bytes)
        .map_err(|error| format!("analyze source {}: {error}", path.display()))?;
    debug_assert_eq!(analyzed.language(), frontend.language());
    debug_assert_eq!(analyzed.content().to_bytes(), content);
    let record = ProductSourceRecord::file(
        project,
        relative.clone(),
        analyzed.language(),
        analyzed.content().to_bytes(),
        analysis,
        analyzed.declarations().clone(),
    )?;
    scanned_file(relative, key, record, source, bytes.len(), path)
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
        assert_eq!(root.read(Path::new("real/lib.rs"))?, b"pub fn safe() {}");
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
