//! Bounded producer for source-bound Roslyn authority images.
//!
//! The checked-in `helper/` project is the semantic producer.  This module is
//! its typed process boundary: callers provide the exact dotnet executable,
//! helper assembly, package root, selected source bytes, and C# profile.  The
//! process is never discovered from `PATH`, and the returned bytes are opened
//! through [`crate::legacy::CSharpImage`] before they become visible to a caller.
//!
//! A source path is read once before the child starts and must equal the
//! caller's source bytes.  That check closes the otherwise subtle race where
//! the helper could digest a different on-disk version than the bytes later
//! lowered by the compiler.  The image's own SHA-256 source binding is checked
//! again after decode. The platform process supervisor bounds stream capture
//! and retires its owned process group or Windows Job Object.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(test)]
use std::{thread, time::Duration};

#[cfg(test)]
use backend_semantic::vocabulary::NativeWorker;
use backend_semantic::vocabulary::{CSharpVersion, NativeTool, NativeWorkerPanic};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::legacy::{CSharpImage, ImageError};

/// Immutable custody for the executable files that participate in one Roslyn
/// invocation.  The bytes are captured before spawning the helper and checked
/// again immediately before use; a changed host/runtime fails closed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CSharpExecutionClosure {
    files: Box<[(PathBuf, [u8; 32], u64)]>,
    digest: [u8; 32],
}

struct CSharpExecutionSnapshot {
    _directory: tempfile::TempDir,
    toolchain: PathBuf,
    oracle: PathBuf,
    digest: [u8; 32],
}

/// Owner-held custody for the exact private runtime/helper snapshot used by a
/// producer attempt. The constructor is private; callers can only obtain this
/// lease from a completed [`CSharpAuthorityImage`].
#[derive(Clone, Debug)]
pub struct CSharpExecutionLease(Arc<CSharpExecutionSnapshot>);

impl CSharpExecutionLease {
    #[must_use]
    pub const fn digest(&self) -> [u8; 32] {
        self.0.digest
    }
}

type SnapshotManifest = Vec<(PathBuf, [u8; 32], usize)>;

#[cfg(target_os = "linux")]
fn require_relocatable_elf_host(bytes: &[u8]) -> Result<(), CSharpAuthorityError> {
    // The private snapshot cannot make an ELF interpreter at an absolute
    // `/nix/store` (or system) path immutable. Refuse such hosts until their
    // interpreter and DT_NEEDED native closure are copied and verified too.
    if bytes.get(..4) != Some(b"\x7fELF") || bytes.get(4) != Some(&2) {
        return Err(CSharpAuthorityError::Configuration);
    }
    if bytes.get(5) != Some(&1) {
        return Err(CSharpAuthorityError::Configuration);
    }
    let read_u16 = |at: usize| {
        bytes
            .get(at..at + 2)
            .map(|part| u16::from_le_bytes([part[0], part[1]]))
    };
    let read_u64 = |at: usize| {
        bytes
            .get(at..at + 8)
            .map(|part| u64::from_le_bytes(part.try_into().expect("slice length")))
    };
    let phoff = read_u64(32).ok_or(CSharpAuthorityError::Configuration)? as usize;
    let phentsize = usize::from(read_u16(54).ok_or(CSharpAuthorityError::Configuration)?);
    let phnum = usize::from(read_u16(56).ok_or(CSharpAuthorityError::Configuration)?);
    if phentsize < 56 || phnum > 128 {
        return Err(CSharpAuthorityError::Configuration);
    }
    for ordinal in 0..phnum {
        let base = phoff
            .checked_add(
                ordinal
                    .checked_mul(phentsize)
                    .ok_or(CSharpAuthorityError::Configuration)?,
            )
            .ok_or(CSharpAuthorityError::Configuration)?;
        let kind = read_u32(bytes, base).ok_or(CSharpAuthorityError::Configuration)?;
        if kind != 3 {
            continue;
        }
        let offset = read_u64(base + 8).ok_or(CSharpAuthorityError::Configuration)? as usize;
        let length = read_u64(base + 32).ok_or(CSharpAuthorityError::Configuration)? as usize;
        let end = offset
            .checked_add(length)
            .ok_or(CSharpAuthorityError::Configuration)?;
        let interpreter = bytes
            .get(offset..end)
            .ok_or(CSharpAuthorityError::Configuration)?;
        if interpreter.first() == Some(&b'/')
            || interpreter.iter().position(|byte| *byte == 0).is_none()
        {
            return Err(CSharpAuthorityError::Configuration);
        }
        return Ok(());
    }
    Err(CSharpAuthorityError::Configuration)
}

#[cfg(target_os = "linux")]
fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at + 4)
        .map(|part| u32::from_le_bytes(part.try_into().expect("slice length")))
}

fn copy_tree_bounded(
    source: &Path,
    target: &Path,
    files: &mut usize,
    bytes: &mut usize,
    manifest: &mut SnapshotManifest,
    control: CSharpAuthorityControl<'_>,
) -> Result<(), CSharpAuthorityError> {
    checkpoint(control, CSharpAuthorityPhase::Spawn)?;
    let metadata = fs::symlink_metadata(source).map_err(|_| CSharpAuthorityError::Configuration)?;
    if metadata.file_type().is_symlink() {
        return Err(CSharpAuthorityError::Configuration);
    }
    if metadata.is_dir() {
        fs::create_dir_all(target).map_err(|_| CSharpAuthorityError::Configuration)?;
        for entry in fs::read_dir(source).map_err(|_| CSharpAuthorityError::Configuration)? {
            let entry = entry.map_err(|_| CSharpAuthorityError::Configuration)?;
            copy_tree_bounded(
                &entry.path(),
                &target.join(entry.file_name()),
                files,
                bytes,
                manifest,
                control,
            )?;
        }
    } else if metadata.is_file() {
        *files = files
            .checked_add(1)
            .filter(|count| *count <= 8192)
            .ok_or(CSharpAuthorityError::Configuration)?;
        let content =
            backend_platform::durable::read_regular_bounded_stable(source, 256 * 1024 * 1024)
                .map_err(|_| CSharpAuthorityError::Configuration)?;
        let digest: [u8; 32] = Sha256::digest(&content).into();
        manifest.push((source.to_path_buf(), digest, content.len()));
        *bytes = bytes
            .checked_add(content.len())
            .filter(|size| *size <= 512 * 1024 * 1024)
            .ok_or(CSharpAuthorityError::Configuration)?;
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|_| CSharpAuthorityError::Configuration)?;
        }
        fs::write(target, content).map_err(|_| CSharpAuthorityError::Configuration)?;
    } else {
        return Err(CSharpAuthorityError::Configuration);
    }
    Ok(())
}

fn snapshot_execution(
    toolchain: &Path,
    oracle: &Path,
    control: CSharpAuthorityControl<'_>,
) -> Result<CSharpExecutionSnapshot, CSharpAuthorityError> {
    let root = toolchain
        .parent()
        .ok_or(CSharpAuthorityError::Configuration)?;
    let directory = tempfile::tempdir().map_err(|_| CSharpAuthorityError::Configuration)?;
    let copied_root = directory.path().join("dotnet");
    let mut files = 0;
    let mut bytes = 0;
    let mut manifest = SnapshotManifest::new();
    // `dotnet exec` only needs the hostfxr and shared runtime trees. SDK packs,
    // templates, and workload caches are excluded from this execution closure.
    let tool_name = toolchain
        .file_name()
        .ok_or(CSharpAuthorityError::Configuration)?;
    let copied_tool = copied_root.join(tool_name);
    let tool_bytes =
        backend_platform::durable::read_regular_bounded_stable(toolchain, 64 * 1024 * 1024)
            .map_err(|_| CSharpAuthorityError::Configuration)?;
    #[cfg(target_os = "linux")]
    require_relocatable_elf_host(&tool_bytes)?;
    manifest.push((
        toolchain.to_path_buf(),
        Sha256::digest(&tool_bytes).into(),
        tool_bytes.len(),
    ));
    bytes = bytes
        .checked_add(tool_bytes.len())
        .filter(|size| *size <= 512 * 1024 * 1024)
        .ok_or(CSharpAuthorityError::Configuration)?;
    fs::create_dir_all(&copied_root).map_err(|_| CSharpAuthorityError::Configuration)?;
    fs::write(&copied_tool, tool_bytes).map_err(|_| CSharpAuthorityError::Configuration)?;
    // `fs::write` creates a non-executable file regardless of the source
    // mode. Require an executable source, then use a private owner-only mode
    // for the copied host; no source write/special bits enter the snapshot.
    #[cfg(unix)]
    {
        let mode = fs::metadata(toolchain)
            .map_err(|_| CSharpAuthorityError::Configuration)?
            .permissions()
            .mode();
        if mode & 0o111 == 0 {
            return Err(CSharpAuthorityError::Configuration);
        }
        fs::set_permissions(&copied_tool, fs::Permissions::from_mode(0o500))
            .map_err(|_| CSharpAuthorityError::Configuration)?;
    }
    for directory_name in ["host", "shared"] {
        copy_tree_bounded(
            &root.join(directory_name),
            &copied_root.join(directory_name),
            &mut files,
            &mut bytes,
            &mut manifest,
            control,
        )?;
    }
    checkpoint(control, CSharpAuthorityPhase::Spawn)?;
    let copied_oracle = directory.path().join("oracle.dll");
    let oracle_bytes =
        backend_platform::durable::read_regular_bounded_stable(oracle, 64 * 1024 * 1024)
            .map_err(|_| CSharpAuthorityError::Configuration)?;
    manifest.push((
        oracle.to_path_buf(),
        Sha256::digest(&oracle_bytes).into(),
        oracle_bytes.len(),
    ));
    checkpoint(control, CSharpAuthorityPhase::Spawn)?;
    bytes = bytes
        .checked_add(oracle_bytes.len())
        .filter(|size| *size <= 512 * 1024 * 1024)
        .ok_or(CSharpAuthorityError::Configuration)?;
    fs::write(&copied_oracle, oracle_bytes).map_err(|_| CSharpAuthorityError::Configuration)?;
    let oracle_stem = oracle
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or(CSharpAuthorityError::Configuration)?;
    let oracle_parent = oracle.parent().ok_or(CSharpAuthorityError::Configuration)?;
    for suffix in [".deps.json", ".runtimeconfig.json"] {
        let companion = oracle_parent.join(format!("{oracle_stem}{suffix}"));
        let companion_bytes =
            backend_platform::durable::read_regular_bounded_stable(&companion, 16 * 1024 * 1024)
                .map_err(|_| CSharpAuthorityError::Configuration)?;
        manifest.push((
            companion.clone(),
            Sha256::digest(&companion_bytes).into(),
            companion_bytes.len(),
        ));
        bytes = bytes
            .checked_add(companion_bytes.len())
            .filter(|size| *size <= 512 * 1024 * 1024)
            .ok_or(CSharpAuthorityError::Configuration)?;
        fs::write(
            directory.path().join(format!("{oracle_stem}{suffix}")),
            companion_bytes,
        )
        .map_err(|_| CSharpAuthorityError::Configuration)?;
    }
    // Copy exactly the helper runtime files declared by deps.json. Sibling
    // DLLs outside this declared closure are not silently admitted.
    let deps_path = oracle_parent.join(format!("{oracle_stem}.deps.json"));
    let deps_bytes =
        backend_platform::durable::read_regular_bounded_stable(&deps_path, 16 * 1024 * 1024)
            .map_err(|_| CSharpAuthorityError::Configuration)?;
    manifest.push((
        deps_path.clone(),
        Sha256::digest(&deps_bytes).into(),
        deps_bytes.len(),
    ));
    let deps: serde_json::Value =
        serde_json::from_slice(&deps_bytes).map_err(|_| CSharpAuthorityError::Configuration)?;
    let targets = deps
        .get("targets")
        .and_then(serde_json::Value::as_object)
        .ok_or(CSharpAuthorityError::Configuration)?;
    // A helper publish must have one selected target. Choosing the first JSON
    // object would make resolver authority depend on map order when a stale
    // or multi-framework deps file is supplied.
    if targets.len() != 1 {
        return Err(CSharpAuthorityError::Configuration);
    }
    let target = targets
        .values()
        .next()
        .and_then(serde_json::Value::as_object)
        .ok_or(CSharpAuthorityError::Configuration)?;
    let mut declared = std::collections::BTreeSet::new();
    for library in target.values() {
        // `runtimeTargets` carries RID-specific managed/native assets which
        // are omitted from `runtime`; both are part of the loader closure.
        for field in ["runtime", "runtimeTargets"] {
            let Some(runtime) = library.get(field).and_then(serde_json::Value::as_object) else {
                continue;
            };
            for relative in runtime.keys() {
                let relative_path = Path::new(relative);
                if relative_path.is_absolute()
                    || relative_path
                        .components()
                        .any(|component| !matches!(component, std::path::Component::Normal(_)))
                {
                    return Err(CSharpAuthorityError::Configuration);
                }
                declared.insert(relative_path.to_owned());
            }
        }
    }
    declared.insert(PathBuf::from(format!("{oracle_stem}.dll")));
    let mut flattened_names = std::collections::BTreeMap::<std::ffi::OsString, PathBuf>::new();
    for relative in declared {
        // A published helper's deps file records the package-relative asset
        // (`lib/net8.0/Foo.dll`), while `dotnet publish` may flatten that
        // asset beside the apphost.  Prefer the recorded path; only accept a
        // flattened basename when it is an unambiguous regular file in the
        // same confined publish directory.  Never search outside that root or
        // choose among multiple candidates.
        let name = relative
            .file_name()
            .ok_or(CSharpAuthorityError::Configuration)?;
        if let Some(previous) = flattened_names.insert(name.to_owned(), relative.clone()) {
            if previous != relative {
                // Distinct package-relative assets with one basename cannot
                // safely share a flattened publish location.
                return Err(CSharpAuthorityError::Configuration);
            }
        }
        let source = {
            let recorded = oracle_parent.join(&relative);
            if recorded.is_file() {
                recorded
            } else {
                let flattened = oracle_parent.join(name);
                if !flattened.is_file() {
                    return Err(CSharpAuthorityError::Configuration);
                }
                flattened
            }
        };
        let content =
            backend_platform::durable::read_regular_bounded_stable(&source, 64 * 1024 * 1024)
                .map_err(|_| CSharpAuthorityError::Configuration)?;
        manifest.push((
            source.clone(),
            Sha256::digest(&content).into(),
            content.len(),
        ));
        bytes = bytes
            .checked_add(content.len())
            .filter(|size| *size <= 512 * 1024 * 1024)
            .ok_or(CSharpAuthorityError::Configuration)?;
        let destination = directory.path().join(&relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|_| CSharpAuthorityError::Configuration)?;
        }
        fs::write(destination, content).map_err(|_| CSharpAuthorityError::Configuration)?;
    }
    // A stable read protects each file, but not a coherent multi-file
    // runtime/helper closure. Re-read every source member after the complete
    // copy and refuse if any member changed during capture.
    checkpoint(control, CSharpAuthorityPhase::Spawn)?;
    for (source, expected, length) in &manifest {
        let current =
            backend_platform::durable::read_regular_bounded_stable(source, 256 * 1024 * 1024)
                .map_err(|_| CSharpAuthorityError::Configuration)?;
        if current.len() != *length || Sha256::digest(&current).as_slice() != expected {
            return Err(CSharpAuthorityError::Configuration);
        }
        checkpoint(control, CSharpAuthorityPhase::Spawn)?;
    }
    manifest.sort_by(|left, right| left.0.cmp(&right.0));
    let mut digest = Sha256::new();
    digest.update(b"nudox.csharp.execution-snapshot.v1\0");
    for (source, expected, length) in &manifest {
        let path = source.as_os_str().as_encoded_bytes();
        digest.update((path.len() as u64).to_le_bytes());
        digest.update(path);
        digest.update((*length as u64).to_le_bytes());
        digest.update(expected);
    }
    if !copied_tool.is_file() {
        return Err(CSharpAuthorityError::Configuration);
    }
    Ok(CSharpExecutionSnapshot {
        _directory: directory,
        toolchain: copied_tool,
        oracle: copied_oracle,
        digest: digest.finalize().into(),
    })
}

impl CSharpExecutionClosure {
    /// Captures a bounded set of regular executable/helper files.
    pub fn capture(paths: &[&Path]) -> Result<Self, CSharpAuthorityError> {
        if paths.is_empty() || paths.len() > 16 {
            return Err(CSharpAuthorityError::Configuration);
        }
        let mut files = Vec::with_capacity(paths.len());
        let mut total = 0usize;
        let mut hash = Sha256::new();
        hash.update(b"nudox.csharp.execution-closure.v1\0");
        for path in paths {
            let canonical =
                fs::canonicalize(path).map_err(|_| CSharpAuthorityError::Configuration)?;
            let bytes = backend_platform::durable::read_regular_bounded_stable(
                &canonical,
                256 * 1024 * 1024,
            )
            .map_err(|_| CSharpAuthorityError::Configuration)?;
            if bytes.is_empty() {
                return Err(CSharpAuthorityError::Configuration);
            }
            total = total
                .checked_add(bytes.len())
                .filter(|size| *size <= 256 * 1024 * 1024)
                .ok_or(CSharpAuthorityError::Configuration)?;
            let digest: [u8; 32] = Sha256::digest(&bytes).into();
            let name = canonical.as_os_str().as_encoded_bytes();
            hash.update((name.len() as u64).to_le_bytes());
            hash.update(name);
            hash.update((bytes.len() as u64).to_le_bytes());
            hash.update(digest);
            files.push((canonical, digest, bytes.len() as u64));
        }
        Ok(Self {
            files: files.into_boxed_slice(),
            digest: hash.finalize().into(),
        })
    }

    /// Revalidates every executable and the closure digest before invocation.
    pub fn validate(&self) -> bool {
        let mut hash = Sha256::new();
        hash.update(b"nudox.csharp.execution-closure.v1\0");
        for (path, expected, length) in &self.files {
            let Ok(bytes) =
                backend_platform::durable::read_regular_bounded_stable(path, 256 * 1024 * 1024)
            else {
                return false;
            };
            if bytes.len() as u64 != *length || Sha256::digest(&bytes).as_slice() != expected {
                return false;
            }
            let name = path.as_os_str().as_encoded_bytes();
            hash.update((name.len() as u64).to_le_bytes());
            hash.update(name);
            hash.update(length.to_le_bytes());
            hash.update(expected);
        }
        hash.finalize().as_slice() == self.digest
    }

    /// Stable digest for same-attempt receipts.
    #[must_use]
    pub const fn digest(&self) -> [u8; 32] {
        self.digest
    }
}

/// The default maximum bytes accepted from either child stream.
pub const DEFAULT_OUTPUT_LIMIT: usize = 32 * 1024 * 1024;
/// The default maximum authority-image bytes retained by one producer call.
pub const DEFAULT_IMAGE_LIMIT: usize = 32 * 1024 * 1024;
/// The default maximum bytes accepted for the selected C# source file.
pub const DEFAULT_SOURCE_LIMIT: usize = 16 * 1024 * 1024;
/// Versioned identity of the isolated Roslyn host-process environment.
pub const CSHARP_PACKAGE_CHILD_ENVIRONMENT_POLICY_ID_V1: &str =
    "csharp-package-child-environment.v1";
/// Captured compilation-unit input and image-fidelity policy, used to invalidate
/// producer caches made by the earlier live source-directory walk.
pub const CSHARP_CAPTURED_UNIT_POLICY_ID_V1: &str = "csharp-captured-unit.v1";
/// Maximum stderr transcript retained in an exit diagnostic.
const STDERR_TAIL_LIMIT: usize = 4096;
/// Poll interval for the bounded child wait loop.

/// Configuration for the helper's package compilation.
///
/// These options mirror only the helper's explicit command-line facts.  The
/// package root and source path remain on [`CSharpAuthorityRequest`] so they
/// cannot accidentally be reused for a different source transaction.
#[derive(Clone, Copy, Debug)]
pub struct CSharpAuthorityConfiguration<'config> {
    /// Optional assembly name forwarded to Roslyn.
    pub assembly_name: Option<&'config str>,
    /// Optional reference-assembly directory forwarded to Roslyn.
    pub reference_directory: Option<&'config Path>,
    /// Additional explicitly selected reference-assembly directories.
    pub additional_reference_directories: &'config [&'config Path],
    /// Additional preprocessor symbols, in caller order.
    pub define_symbols: &'config [&'config str],
    /// Use only explicitly supplied symbols from a captured SDK observation.
    pub exact_define_symbols: bool,
    /// Captured SDK target framework, when observed for this attempt.
    pub target_framework: Option<&'config str>,
    /// Captured SDK nullable option, with the paired unsafe flag.
    pub exact_nullable: Option<&'config str>,
    /// Captured SDK unsafe option, paired with exact nullable.
    pub exact_unsafe: Option<bool>,
    /// Captured SDK output type when observed.
    pub exact_output_type: Option<&'config str>,
    /// Additional global usings, in caller order.
    pub extra_usings: &'config [&'config str],
    /// Whether SDK-style implicit global usings are reconstructed.
    pub implicit_usings: bool,
    /// Whether non-public declarations are retained by Roslyn.
    pub include_non_public: bool,
    /// Maximum bytes accepted for the selected source file.
    pub maximum_source_bytes: usize,
}

impl CSharpAuthorityConfiguration<'static> {
    /// Configuration matching the helper's default source-mode policy.
    pub const DEFAULT: Self = Self {
        assembly_name: None,
        reference_directory: None,
        additional_reference_directories: &[],
        define_symbols: &[],
        exact_define_symbols: false,
        target_framework: None,
        exact_nullable: None,
        exact_unsafe: None,
        exact_output_type: None,
        extra_usings: &[],
        implicit_usings: true,
        include_non_public: true,
        maximum_source_bytes: DEFAULT_SOURCE_LIMIT,
    };
}

/// Caller-owned cancellation and deadline authority for one Roslyn run.
#[derive(Clone, Copy, Debug)]
pub struct CSharpAuthorityControl<'cancel> {
    /// Monotonic deadline after which the child is killed and reaped.
    pub deadline: Instant,
    /// Caller-owned cancellation flag observed before and during the child.
    pub cancelled: &'cancel AtomicBool,
}

/// One fully explicit Roslyn authority-image request.
///
/// `toolchain` must be the already-resolved absolute dotnet executable that
/// owns the enclosing compilation request.  `oracle` is held by
/// [`CSharpOracle`] and must be an absolute path to the published helper DLL.
#[derive(Clone, Copy, Debug)]
pub struct CSharpAuthorityRequest<'request, 'config, 'cancel> {
    /// Package tree that supplies source and package-local C# files.
    pub package_root: &'request Path,
    /// Exact source file whose bytes and spans the image must bind.
    pub source_path: &'request Path,
    /// Exact `.csproj` selected by a V2 CSharpProject unit, when present.
    pub project_file: Option<&'request Path>,
    /// Exact source bytes that the caller will lower after authority entry.
    pub source: &'request [u8],
    /// Exact captured source members for this compilation unit, when supplied.
    pub captured_sources: Option<&'request [CSharpCapturedSource<'request>]>,
    /// Owner-captured ordered project/config probes, when available.
    pub captured_config: Option<&'request [CSharpCapturedConfig<'request>]>,
    /// Exact bounded project graph. Nodes compile into separate assemblies.
    pub captured_project_graph: Option<&'request CSharpCapturedProjectGraph<'request>>,
    /// Immutable executable/helper custody for this invocation, when the
    /// owner has admitted a native execution closure.
    pub execution_closure: Option<&'request CSharpExecutionClosure>,
    /// Closed C# language version selected for Roslyn parsing.
    pub profile: CSharpVersion,
    /// Native tool family already selected by the enclosing compiler request.
    pub native_tool: NativeTool,
    /// Already-resolved absolute dotnet executable.
    pub toolchain: &'request Path,
    /// Cancellation/deadline authority for this transaction.
    pub control: CSharpAuthorityControl<'cancel>,
    /// Explicit Roslyn source-mode configuration.
    pub configuration: CSharpAuthorityConfiguration<'config>,
}

/// One package-relative source and its exact captured UTF-8 content.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct CSharpCapturedSource<'source> {
    /// Forward-slash-separated package-relative path.
    #[serde(rename = "Path")]
    pub path: &'source str,
    /// Captured source text.
    #[serde(rename = "Text")]
    pub text: &'source str,
}

/// One captured project/config probe. None records absence at capture time.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct CSharpCapturedConfig<'source> {
    /// Package-relative portable path in the helper's required probe order.
    #[serde(rename = "Path")]
    pub path: &'source str,
    /// Captured UTF-8 bytes, or an observed absence.
    #[serde(rename = "Text")]
    pub text: Option<&'source str>,
}

/// A closed captured project-reference graph; no MSBuild execution is permitted.
#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct CSharpCapturedProjectGraph<'a> {
    /// Protocol version, currently one.
    pub format: u8,
    /// Exact package-relative project selected for this invocation.
    pub root_project: &'a str,
    /// All and only projects reachable from the root (at most 64).
    pub projects: &'a [CSharpCapturedProjectNode<'a>],
}

/// Exact input custody and compiler options for one distinct assembly.
#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct CSharpCapturedProjectNode<'a> {
    /// Exact package-relative .csproj path.
    pub project_path: &'a str,
    /// Assembly identity admitted from the captured configuration.
    pub assembly_name: &'a str,
    /// Exact simple target framework moniker.
    pub target_framework: &'a str,
    /// Closed language version, for example "12.0".
    pub language_version: &'a str,
    /// Exact preprocessor symbols.
    pub define_symbols: &'a [&'a str],
    /// Nullable mode (disable, enable, warnings, annotations).
    pub nullable: &'a str,
    /// Exact unsafe compiler setting.
    pub allow_unsafe: bool,
    /// Exe, Library or WinExe.
    pub output_type: &'a str,
    /// Complete admitted global namespace imports.
    pub usings: &'a [&'a str],
    /// Exact source buffers, scoped to this project.
    pub sources: &'a [CSharpCapturedSource<'a>],
    /// Ordered project and ancestor presence/absence probes.
    pub config: &'a [CSharpCapturedConfig<'a>],
    /// Owner-staged immutable directories of exact managed reference snapshots.
    pub reference_directories: &'a [&'a Path],
    /// Direct dependencies by exact package-relative project path.
    pub project_references: &'a [&'a str],
}

struct ManifestWriter(Vec<u8>);

impl Write for ManifestWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > (64 * 1024 * 1024usize).saturating_sub(self.0.len()) {
            return Err(std::io::Error::other(
                "captured source manifest exceeds limit",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Configured producer for the vendored Roslyn authority helper.
#[derive(Clone, Debug)]
pub struct CSharpOracle {
    oracle: PathBuf,
    /// Maximum bytes accepted from stdout or stderr.
    pub output_limit: usize,
    /// Maximum image bytes retained after the bounded child completes.
    pub image_limit: usize,
}

/// Owned, already-validated Roslyn authority-image bytes.
///
/// The owner is intentionally only a byte container: a borrowed
/// [`CSharpImage`] can be opened from [`Self::as_bytes`] for exactly as long
/// as this value remains alive, without introducing a self-referential Rust
/// object.
#[derive(Clone, Debug)]
pub struct CSharpAuthorityImage {
    bytes: Box<[u8]>,
    evidence: Option<Box<[u8]>>,
    execution_digest: [u8; 32],
    execution: Arc<CSharpExecutionSnapshot>,
}

impl CSharpAuthorityImage {
    /// Helper-emitted, same-invocation evidence after local consistency checks.
    /// This is an untrusted claim, not a selected publication capability.
    #[must_use]
    pub fn captured_evidence(&self) -> Option<&[u8]> {
        self.evidence.as_deref()
    }

    /// Digest of the complete private helper/runtime snapshot used for this
    /// image. Owners must carry this into their selected-attempt receipt.
    #[must_use]
    pub const fn execution_digest(&self) -> [u8; 32] {
        self.execution_digest
    }

    /// Retains the immutable runtime/helper snapshot for selected-attempt
    /// publication and cold replay custody.
    #[must_use]
    pub fn execution_lease(&self) -> CSharpExecutionLease {
        CSharpExecutionLease(Arc::clone(&self.execution))
    }

    /// Borrows the exact validated image bytes for the next compiler boundary.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the owned image bytes, preserving the producer's validation.
    #[must_use]
    pub fn into_bytes(self) -> Box<[u8]> {
        self.bytes
    }

    /// Separates the validated image and untrusted same-invocation evidence.
    #[must_use]
    pub fn into_parts(self) -> (Box<[u8]>, Option<Box<[u8]>>) {
        (self.bytes, self.evidence)
    }

    /// Reopens the retained bytes through the typed image reader.
    ///
    /// Reopening is cheap relative to production and keeps this accessor
    /// honest even when a caller deliberately wants the image reader rather
    /// than a raw byte slice.
    pub fn open(&self) -> Result<CSharpImage<'_>, ImageError> {
        CSharpImage::open(&self.bytes)
    }
}

impl AsRef<[u8]> for CSharpAuthorityImage {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl CSharpOracle {
    /// Creates a producer bound to one published helper assembly.
    ///
    /// The path is intentionally not searched or built by this constructor;
    /// an unavailable helper is reported by the process boundary with its
    /// exact configured path.
    #[must_use]
    pub fn new(oracle: PathBuf) -> Self {
        Self {
            oracle,
            output_limit: DEFAULT_OUTPUT_LIMIT,
            image_limit: DEFAULT_IMAGE_LIMIT,
        }
    }

    /// Returns the exact helper assembly path this producer will execute.
    #[must_use]
    pub fn oracle_path(&self) -> &Path {
        &self.oracle
    }

    /// Changes the per-stream child output bound.
    #[must_use]
    pub const fn with_output_limit(mut self, limit: usize) -> Self {
        self.output_limit = limit;
        self
    }

    /// Changes the retained authority-image bound.
    #[must_use]
    pub const fn with_image_limit(mut self, limit: usize) -> Self {
        self.image_limit = limit;
        self
    }

    /// Runs the real Roslyn helper and returns a validated source-bound image.
    ///
    /// The helper receives the package root and exact binding path, so package
    /// context is available to Roslyn while the returned image remains bound
    /// to only the caller's source bytes.  The image is returned as owned
    /// bytes because the next compiler boundary may retain it while borrowing
    /// a validated [`CSharpImage`].
    ///
    /// # Errors
    ///
    /// Returns typed path, source-binding, cancellation, deadline, process,
    /// output-limit, and image-validation causes.  A child failure retains a
    /// bounded stderr tail rather than collapsing the helper's diagnosis into
    /// a boolean unavailable result.
    pub fn authority_image<'request, 'config, 'cancel>(
        &self,
        request: CSharpAuthorityRequest<'request, 'config, 'cancel>,
    ) -> Result<Vec<u8>, CSharpAuthorityError> {
        self.authority_image_with_evidence(request)
            .map(|(image, _, _, _)| image)
    }

    fn authority_image_with_evidence<'request, 'config, 'cancel>(
        &self,
        request: CSharpAuthorityRequest<'request, 'config, 'cancel>,
    ) -> Result<
        (
            Vec<u8>,
            Option<Box<[u8]>>,
            [u8; 32],
            Arc<CSharpExecutionSnapshot>,
        ),
        CSharpAuthorityError,
    > {
        checkpoint(request.control, CSharpAuthorityPhase::Admission)?;
        if request.configuration.exact_nullable.is_some()
            != request.configuration.exact_unsafe.is_some()
        {
            return Err(CSharpAuthorityError::Configuration);
        }
        if request.native_tool != NativeTool::CSharpCompiler {
            return Err(CSharpAuthorityError::WrongToolchain {
                observed: request.native_tool,
            });
        }
        if let Some(closure) = request.execution_closure {
            if !closure.validate() {
                return Err(CSharpAuthorityError::Configuration);
            }
        }
        validate_absolute_paths(&self.oracle, request)?;
        let maximum_source_bytes = request.configuration.maximum_source_bytes;
        if request.source.len() > maximum_source_bytes {
            return Err(CSharpAuthorityError::SourceLimit {
                observed: u64::try_from(request.source.len()).unwrap_or(u64::MAX),
                maximum: maximum_source_bytes,
            });
        }

        let package_root = fs::canonicalize(request.package_root).map_err(|source| {
            CSharpAuthorityError::Path {
                phase: CSharpAuthorityPhase::Admission,
                path: request.package_root.to_path_buf(),
                source,
            }
        })?;
        let source_path =
            fs::canonicalize(request.source_path).map_err(|source| CSharpAuthorityError::Path {
                phase: CSharpAuthorityPhase::Source,
                path: request.source_path.to_path_buf(),
                source,
            })?;
        if source_path.strip_prefix(&package_root).is_err() {
            return Err(CSharpAuthorityError::SourceOutsidePackage {
                package_root: package_root.into_boxed_path(),
                source_path: source_path.into_boxed_path(),
            });
        }

        let project_file = request
            .project_file
            .map(|path| {
                let canonical =
                    fs::canonicalize(path).map_err(|source| CSharpAuthorityError::Path {
                        phase: CSharpAuthorityPhase::Admission,
                        path: path.to_path_buf(),
                        source,
                    })?;
                if canonical.strip_prefix(&package_root).is_err()
                    || !canonical.is_file()
                    || canonical
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .map_or(true, |extension| !extension.eq_ignore_ascii_case("csproj"))
                {
                    return Err(CSharpAuthorityError::ProjectOutsidePackage {
                        package_root: package_root.clone().into_boxed_path(),
                        project_file: canonical.into_boxed_path(),
                    });
                }
                Ok(canonical)
            })
            .transpose()?;

        checkpoint(request.control, CSharpAuthorityPhase::Source)?;
        let source_length = fs::metadata(&source_path)
            .map_err(|source| CSharpAuthorityError::Path {
                phase: CSharpAuthorityPhase::Source,
                path: source_path.clone(),
                source,
            })?
            .len();
        if source_length > u64::try_from(maximum_source_bytes).unwrap_or(u64::MAX) {
            return Err(CSharpAuthorityError::SourceLimit {
                observed: source_length,
                maximum: maximum_source_bytes,
            });
        }
        let on_disk = fs::read(&source_path).map_err(|source| CSharpAuthorityError::Path {
            phase: CSharpAuthorityPhase::Source,
            path: source_path.clone(),
            source,
        })?;
        if on_disk != request.source {
            return Err(CSharpAuthorityError::SourceBinding {
                source_path: source_path.into_boxed_path(),
                expected: Sha256::digest(request.source).into(),
                observed: Sha256::digest(&on_disk).into(),
            });
        }

        checkpoint(request.control, CSharpAuthorityPhase::Spawn)?;
        let manifest = request
            .captured_sources
            .map(|sources| {
                let bound = request
                    .source_path
                    .strip_prefix(request.package_root)
                    .ok()
                    .and_then(Path::to_str)
                    .map(|path| path.replace('\\', "/"));
                if !sources.iter().any(|source| {
                    Some(source.path) == bound.as_deref()
                        && source.text.as_bytes() == request.source
                }) {
                    return Err(CSharpAuthorityError::Manifest);
                }
                let mut writer = ManifestWriter(Vec::new());
                serde_json::to_writer(&mut writer, sources)
                    .map_err(|_| CSharpAuthorityError::Manifest)?;
                Ok(writer.0)
            })
            .transpose()?;
        let graph_manifest = request
            .captured_project_graph
            .map(|graph| {
                if request.captured_config.is_some()
                    || project_file.is_none()
                    || request.captured_sources.is_none()
                    || graph.format != 1
                    || graph.projects.is_empty()
                    || graph.projects.len() > 64
                    || project_file
                        .as_ref()
                        .and_then(|path| path.strip_prefix(&package_root).ok())
                        .and_then(Path::to_str)
                        .map(|path| path.replace('\\', "/"))
                        .as_deref()
                        != Some(graph.root_project)
                {
                    return Err(CSharpAuthorityError::Manifest);
                }
                let expected = request
                    .captured_sources
                    .ok_or(CSharpAuthorityError::Manifest)?;
                if graph
                    .projects
                    .iter()
                    .map(|node| node.sources.len())
                    .sum::<usize>()
                    != expected.len()
                    || graph
                        .projects
                        .iter()
                        .flat_map(|node| node.sources)
                        .any(|source| {
                            expected
                                .iter()
                                .filter(|entry| {
                                    entry.path == source.path && entry.text == source.text
                                })
                                .count()
                                != 1
                        })
                {
                    return Err(CSharpAuthorityError::Manifest);
                }
                let mut writer = ManifestWriter(Vec::new());
                serde_json::to_writer(&mut writer, graph)
                    .map_err(|_| CSharpAuthorityError::Manifest)?;
                Ok(writer.0)
            })
            .transpose()?;
        let config_manifest = request
            .captured_config
            .map(|config| {
                if request.captured_sources.is_none() || project_file.is_none() {
                    return Err(CSharpAuthorityError::Manifest);
                }
                let mut writer = ManifestWriter(Vec::new());
                serde_json::to_writer(&mut writer, config)
                    .map_err(|_| CSharpAuthorityError::Manifest)?;
                if writer.0.len() > 16 * 1024 * 1024 {
                    return Err(CSharpAuthorityError::Manifest);
                }
                Ok(writer.0)
            })
            .transpose()?;
        checkpoint(request.control, CSharpAuthorityPhase::Spawn)?;
        let execution = Arc::new(snapshot_execution(
            request.toolchain,
            &self.oracle,
            request.control,
        )?);
        let execution_digest = execution.digest;
        let mut command = Command::new(&execution.toolchain);
        isolate_authority_environment(&mut command);
        command
            .arg("exec")
            .arg(&execution.oracle)
            .args(["--mode", "source", "--root"])
            .arg(&package_root)
            .args(["--authority-image", "--source-binding"])
            .arg(&source_path)
            .args(["--lang-version", profile_tag(request.profile)])
            .current_dir(&package_root);
        let mut evidence_request = None;
        let manifest_directory = if let Some(manifest) = manifest {
            let directory = tempfile::tempdir().map_err(|source| CSharpAuthorityError::Pipe {
                stream: "manifest",
                source,
            })?;
            let path = directory.path().join("captured-sources.json");
            fs::write(&path, manifest).map_err(|source| CSharpAuthorityError::Pipe {
                stream: "manifest",
                source,
            })?;
            if graph_manifest.is_none() {
                command.arg("--captured-sources-file").arg(path);
            }
            let capture_config = graph_manifest.as_ref().or(config_manifest.as_ref());
            if let Some(config) = capture_config {
                let config_hash = format!("{:x}", Sha256::digest(config));
                let path = directory.path().join("captured-config.json");
                fs::write(&path, config).map_err(|source| CSharpAuthorityError::Pipe {
                    stream: "config manifest",
                    source,
                })?;
                command
                    .arg(if graph_manifest.is_some() {
                        "--captured-project-graph-file"
                    } else {
                        "--captured-config-file"
                    })
                    .arg(path);
                let nonce_hash = format!(
                    "{:x}",
                    Sha256::digest(directory.path().as_os_str().as_encoded_bytes())
                );
                let nonce = nonce_hash[..32].to_owned();
                let evidence_path = directory.path().join("captured-evidence.json");
                command
                    .arg("--capture-evidence-file")
                    .arg(&evidence_path)
                    .arg("--capture-evidence-nonce")
                    .arg(&nonce);
                evidence_request = Some((evidence_path, nonce, config_hash));
            }
            Some(directory)
        } else {
            None
        };
        checkpoint(request.control, CSharpAuthorityPhase::Spawn)?;

        if let Some(project_file) = &project_file {
            command.arg("--project-file").arg(project_file);
        }

        let configuration = request.configuration;
        if request.captured_project_graph.is_none() {
            if let Some(name) = configuration.assembly_name {
                command.args(["--assembly-name", name]);
            }
            if let Some(directory) = configuration.reference_directory {
                command.arg("--ref-dir").arg(directory);
            }
            for directory in configuration.additional_reference_directories {
                command.arg("--ref-dir").arg(directory);
            }
            for symbol in configuration.define_symbols {
                command.args(["--define", symbol]);
            }
            if configuration.exact_define_symbols {
                command.arg("--exact-defines");
            }
            if let Some(framework) = configuration.target_framework {
                command.args(["--target-framework", framework]);
            }
            if let Some(output_type) = configuration.exact_output_type {
                command.args(["--exact-output-type", output_type]);
            }
            if let (Some(nullable), Some(allow_unsafe)) =
                (configuration.exact_nullable, configuration.exact_unsafe)
            {
                command.args([
                    "--exact-nullable",
                    nullable,
                    "--exact-unsafe",
                    if allow_unsafe { "true" } else { "false" },
                ]);
            }
            for namespace in configuration.extra_usings {
                command.args(["--using", namespace]);
            }
            if !configuration.implicit_usings {
                command.arg("--no-implicit-usings");
            }
        }
        if !configuration.include_non_public {
            command.arg("--public-only");
        }

        let spec = backend_platform::child_output::CaptureCommand {
            program: command.get_program().to_os_string(),
            args: command.get_args().map(|arg| arg.to_os_string()).collect(),
            cwd: command.get_current_dir().map(Path::to_path_buf),
            environment: backend_platform::child_output::CaptureEnvironment::Clear,
            overrides: command
                .get_envs()
                .map(|(key, value)| (key.to_os_string(), value.map(|value| value.to_os_string())))
                .collect(),
        };
        let result = backend_platform::child_output::capture(
            &spec,
            backend_platform::child_output::CaptureLimits {
                deadline: request.control.deadline,
                stdout_bytes: self.output_limit,
                stderr_bytes: self.output_limit,
            },
            request.control.cancelled,
        )
        .map_err(|cause| capture_error(cause, request.toolchain, &self.oracle))?;
        if !result.status.success() {
            return Err(CSharpAuthorityError::Exit {
                status: result.status.to_string(),
                stderr: stderr_tail(&result.stderr),
            });
        }
        let diagnostic = stderr_tail(&result.stderr);
        let stdout = result.stdout;
        checkpoint(request.control, CSharpAuthorityPhase::Decode)?;
        if stdout.len() > self.image_limit {
            return Err(CSharpAuthorityError::ImageLimit {
                observed: stdout.len(),
                limit: self.image_limit,
            });
        }
        let image = CSharpImage::open(&stdout).map_err(CSharpAuthorityError::Image)?;
        let expected: [u8; 32] = Sha256::digest(request.source).into();
        let observed = image.source_digest();
        if observed != expected {
            return Err(CSharpAuthorityError::ImageBinding { expected, observed });
        }
        if image.compilation_errors() != 0 {
            return Err(CSharpAuthorityError::CompilationIncomplete {
                count: image.compilation_errors(),
                diagnostic,
            });
        }
        let evidence = if let Some((path, nonce, config_hash)) = evidence_request {
            let bytes =
                backend_platform::durable::read_regular_bounded_stable(&path, 2 * 1024 * 1024)
                    .map_err(|_| CSharpAuthorityError::Evidence)?;
            let binding = source_path
                .strip_prefix(&package_root)
                .ok()
                .and_then(Path::to_str)
                .map(|path| path.replace('\\', "/"))
                .ok_or(CSharpAuthorityError::Evidence)?;
            validate_captured_evidence(
                &bytes,
                &nonce,
                &config_hash,
                &binding,
                &stdout,
                request
                    .captured_sources
                    .ok_or(CSharpAuthorityError::Evidence)?,
            )?;
            Some(bytes.into_boxed_slice())
        } else {
            None
        };
        drop(manifest_directory);
        checkpoint(request.control, CSharpAuthorityPhase::Return)?;
        Ok((stdout, evidence, execution_digest, execution))
    }

    /// Runs [`Self::authority_image`] and retains its bytes in an owned image.
    ///
    /// The returned owner is convenient for a package authority transaction
    /// whose driver input borrows the image for the duration of lowering.
    pub fn produce<'request, 'config, 'cancel>(
        &self,
        request: CSharpAuthorityRequest<'request, 'config, 'cancel>,
    ) -> Result<CSharpAuthorityImage, CSharpAuthorityError> {
        self.authority_image_with_evidence(request).map(
            |(bytes, evidence, execution_digest, execution)| CSharpAuthorityImage {
                bytes: bytes.into_boxed_slice(),
                evidence,
                execution_digest,
                execution,
            },
        )
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CapturedEvidence {
    format: u8,
    nonce: String,
    source_binding: String,
    image_sha256: String,
    image_length: u64,
    config_sha256: String,
    #[serde(deserialize_with = "bounded_evidence_vec")]
    sources: Vec<CapturedEvidenceSource>,
    #[serde(deserialize_with = "bounded_evidence_vec")]
    references: Vec<CapturedEvidenceReference>,
    #[serde(default, deserialize_with = "bounded_optional_evidence_vec")]
    occurrences: Option<Vec<CapturedForeignOccurrence>>,
    assembly: String,
    framework: String,
    language: String,
    output_kind: String,
    #[serde(deserialize_with = "bounded_evidence_vec")]
    defines: Vec<String>,
    #[serde(deserialize_with = "bounded_evidence_vec")]
    usings: Vec<String>,
    generators: String,
    input_closure: String,
    errors: u32,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CapturedForeignOccurrence {
    owner: u32,
    start: u32,
    end: u32,
    target: String,
}

fn bounded_optional_evidence_vec<'de, D, T>(deserializer: D) -> Result<Option<Vec<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    bounded_evidence_vec(deserializer).map(Some)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CapturedEvidenceSource {
    path: String,
    length: u64,
    sha256: String,
}
#[derive(serde::Deserialize)]
#[serde(rename_all = "PascalCase", deny_unknown_fields)]
struct CapturedEvidenceReference {
    name: String,
    length: u64,
    sha256: String,
}

fn bounded_evidence_vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    struct Visitor<T>(std::marker::PhantomData<T>);
    impl<'de, T: serde::Deserialize<'de>> serde::de::Visitor<'de> for Visitor<T> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("at most 8192 evidence entries")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            if seq.size_hint().is_some_and(|count| count > 8192) {
                return Err(serde::de::Error::custom("evidence entry limit"));
            }
            let mut values = Vec::new();
            while let Some(value) = seq.next_element()? {
                if values.len() == 8192 {
                    return Err(serde::de::Error::custom("evidence entry limit"));
                }
                values.push(value);
            }
            Ok(values)
        }
    }
    deserializer.deserialize_seq(Visitor(std::marker::PhantomData))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn validate_captured_evidence(
    bytes: &[u8],
    nonce: &str,
    config_hash: &str,
    binding: &str,
    image: &[u8],
    sources: &[CSharpCapturedSource<'_>],
) -> Result<(), CSharpAuthorityError> {
    let evidence: CapturedEvidence =
        serde_json::from_slice(bytes).map_err(|_| CSharpAuthorityError::Evidence)?;
    if !matches!(
        (evidence.format, evidence.occurrences.is_some()),
        (1, false) | (2, true)
    ) || evidence.nonce != nonce
        || evidence.config_sha256 != config_hash
        || evidence.source_binding != binding
        || evidence.image_length != image.len() as u64
        || evidence.image_sha256 != format!("{:x}", Sha256::digest(image))
        || evidence.generators != "unknown"
        || evidence.input_closure != "unknown"
        || evidence.errors != 0
        || evidence.sources.len() != sources.len()
        || sources.len() > 8192
        || evidence.references.is_empty()
        || evidence.references.len() > 8192
        || [
            &evidence.assembly,
            &evidence.framework,
            &evidence.language,
            &evidence.output_kind,
        ]
        .iter()
        .any(|s| s.is_empty() || s.len() > 4096)
        || evidence.defines.len() > 8192
        || evidence.usings.len() > 8192
        || evidence
            .defines
            .iter()
            .chain(&evidence.usings)
            .any(|s| s.len() > 4096)
    {
        return Err(CSharpAuthorityError::Evidence);
    }
    if let Some(occurrences) = &evidence.occurrences {
        let image = CSharpImage::open(image).map_err(CSharpAuthorityError::Image)?;
        let mut actual = image.references().filter_map(|reference| match reference {
            Ok(reference) if reference.foreign_source.is_some() => Some(Ok(reference)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        });
        let mut target_bytes = 0usize;
        for occurrence in occurrences {
            target_bytes = target_bytes
                .checked_add(occurrence.target.len())
                .ok_or(CSharpAuthorityError::Evidence)?;
            if target_bytes > 1024 * 1024 {
                return Err(CSharpAuthorityError::Evidence);
            }
            let claim = super::image::parse_foreign_source_claim(&occurrence.target)
                .ok_or(CSharpAuthorityError::Evidence)?;
            let reference = actual
                .next()
                .ok_or(CSharpAuthorityError::Evidence)?
                .map_err(CSharpAuthorityError::Image)?;
            if reference.owner != occurrence.owner
                || reference.start != occurrence.start
                || reference.end != occurrence.end
                || reference.foreign_source != Some(claim)
            {
                return Err(CSharpAuthorityError::Evidence);
            }
        }
        if actual.next().is_some() {
            return Err(CSharpAuthorityError::Evidence);
        }
    }
    let mut expected = std::collections::BTreeMap::new();
    for source in sources {
        if expected
            .insert(source.path, source.text.as_bytes())
            .is_some()
        {
            return Err(CSharpAuthorityError::Evidence);
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for source in &evidence.sources {
        let source_bytes = expected
            .get(source.path.as_str())
            .ok_or(CSharpAuthorityError::Evidence)?;
        if !seen.insert(source.path.as_str())
            || source.length != source_bytes.len() as u64
            || source.sha256 != format!("{:x}", Sha256::digest(source_bytes))
        {
            return Err(CSharpAuthorityError::Evidence);
        }
    }
    let mut reference_names = std::collections::BTreeSet::new();
    let mut reference_bytes = 0u64;
    for reference in &evidence.references {
        reference_bytes = reference_bytes
            .checked_add(reference.length)
            .ok_or(CSharpAuthorityError::Evidence)?;
        if reference.name.is_empty()
            || reference.name.len() > 4096
            || !reference_names.insert(reference.name.to_ascii_lowercase())
            || reference.length == 0
            || reference.length > 32 * 1024 * 1024
            || reference_bytes > 256 * 1024 * 1024
            || !valid_sha256(&reference.sha256)
        {
            return Err(CSharpAuthorityError::Evidence);
        }
    }
    Ok(())
}

/// Alias emphasizing that this adapter is the C# authority-image producer.
pub type CSharpAuthorityProducer = CSharpOracle;

/// One phase at which the bounded producer can stop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CSharpAuthorityPhase {
    /// Request admission before filesystem or child work.
    Admission,
    /// Canonical package/source path and source-byte binding.
    Source,
    /// Child construction and spawn.
    Spawn,
    /// Child wait and bounded stream collection.
    Process,
    /// Image envelope, checksum, and source binding validation.
    Decode,
    /// Final cancellation/deadline gate before returning owned bytes.
    Return,
}

/// Exact terminal returned by the Roslyn authority-image producer.
#[derive(Debug, Error)]
pub enum CSharpAuthorityError {
    /// Exact compiler options are incomplete or inconsistent.
    #[error("C# captured compiler configuration is incomplete")]
    Configuration,
    /// Captured input set is too large, malformed, or omits the binding.
    #[error("C# captured input manifest is invalid or exceeds its bound")]
    Manifest,
    /// Captured invocation evidence was absent, malformed or mismatched.
    #[error("C# captured invocation evidence is invalid")]
    Evidence,
    /// Roslyn could not bind this source-mode compilation fully.
    #[error(
        "C# source-mode compilation reported at least {count} errors; check project references and SDK inputs: {diagnostic}"
    )]
    CompilationIncomplete {
        /// Number of errors, saturated at u16::MAX.
        count: u16,
        /// Bounded compiler diagnostic tail, without manifest contents.
        diagnostic: String,
    },
    /// Bounded platform process capture failed, including cleanup failures.
    #[error("C# authority process capture failed: {detail}")]
    Capture {
        /// Exact platform capture failure.
        detail: String,
    },
    /// A required path was not absolute or could not be canonicalized/read.
    #[error("C# authority {phase:?} path {path:?} failed: {source}")]
    Path {
        /// Producer phase that touched the path.
        phase: CSharpAuthorityPhase,
        /// Exact caller-supplied or canonical path.
        path: PathBuf,
        /// Underlying filesystem cause.
        #[source]
        source: std::io::Error,
    },
    /// A relative path would reintroduce ambient process-directory lookup.
    #[error("C# authority {kind} path is not absolute: {path:?}")]
    RelativePath {
        /// Path role rejected.
        kind: &'static str,
        /// Exact rejected path.
        path: PathBuf,
    },
    /// The enclosing request selected a native tool other than C#'s compiler.
    #[error("C# authority received the wrong native tool family: {observed:?}")]
    WrongToolchain {
        /// Exact tool family carried by the enclosing request.
        observed: NativeTool,
    },
    /// The selected source does not remain beneath the selected package root.
    #[error("C# authority source {source_path:?} is outside package root {package_root:?}")]
    SourceOutsidePackage {
        /// Canonical package root.
        package_root: Box<Path>,
        /// Canonical source path.
        source_path: Box<Path>,
    },
    /// The exact project file is outside the admitted package or is not a `.csproj` file.
    #[error("C# project {project_file:?} is not an admitted project beneath {package_root:?}")]
    ProjectOutsidePackage {
        /// Admitted package root.
        package_root: Box<Path>,
        /// Rejected project path.
        project_file: Box<Path>,
    },
    /// The source file changed or differs from the bytes supplied to the compiler.
    #[error("C# authority source binding differs for {source_path:?}")]
    SourceBinding {
        /// Canonical source path checked before spawning the helper.
        source_path: Box<Path>,
        /// Digest of the caller-owned bytes.
        expected: [u8; 32],
        /// Digest of the bytes read from the selected path.
        observed: [u8; 32],
    },
    /// The selected source exceeds the explicit pre-read byte bound.
    #[error("C# authority source observed {observed} bytes; maximum is {maximum}")]
    SourceLimit {
        /// Exact on-disk byte length observed before reading the source.
        observed: u64,
        /// Explicit source-byte bound.
        maximum: usize,
    },
    /// The dotnet process could not be started.
    #[error("C# authority dotnet {toolchain:?} could not execute helper {oracle:?}: {source}")]
    Spawn {
        /// Exact resolved dotnet executable.
        toolchain: Box<Path>,
        /// Exact helper assembly path.
        oracle: Box<Path>,
        /// Underlying process-spawn failure.
        #[source]
        source: std::io::Error,
    },
    /// One child stream could not be piped or read.
    #[error("C# authority {stream} pipe failed: {source}")]
    Pipe {
        /// Child stream name.
        stream: &'static str,
        /// Underlying pipe failure.
        #[source]
        source: std::io::Error,
    },
    /// A stream reader thread panicked; its exact bounded payload and worker
    /// identity survive the join boundary.
    #[error("C# authority stream worker panicked: {cause}")]
    WorkerPanic {
        /// Bounded original join payload, including the standard-output or
        /// standard-error worker category.
        #[source]
        cause: NativeWorkerPanic,
    },
    /// A child stream exceeded the configured byte bound.
    #[error("C# authority {stream} output observed {observed} bytes; limit is {limit}")]
    OutputLimit {
        /// Child stream that exceeded the bound.
        stream: &'static str,
        /// Bytes observed when the bound was crossed.
        observed: usize,
        /// Configured per-stream bound.
        limit: usize,
    },
    /// The child stopped because its enclosing request was cancelled.
    #[error("C# authority was cancelled during {phase:?}")]
    Cancelled {
        /// Phase that observed cancellation.
        phase: CSharpAuthorityPhase,
    },
    /// The child stopped because its enclosing deadline expired.
    #[error("C# authority reached its deadline during {phase:?}")]
    Deadline {
        /// Phase that observed the deadline.
        phase: CSharpAuthorityPhase,
    },
    /// The helper exited unsuccessfully, retaining a bounded stderr tail.
    #[error("C# authority helper exited with {status}; stderr tail: {stderr}")]
    Exit {
        /// Rendered child exit status.
        status: String,
        /// Bounded diagnostic tail from stderr.
        stderr: String,
    },
    /// The image exceeded the producer's retained image bound.
    #[error("C# authority image observed {observed} bytes; limit is {limit}")]
    ImageLimit {
        /// Image bytes observed.
        observed: usize,
        /// Configured image bound.
        limit: usize,
    },
    /// The helper returned malformed or structurally invalid image bytes.
    #[error(transparent)]
    Image(#[from] ImageError),
    /// The validated image was bound to source bytes other than this request.
    #[error("C# authority image source digest differs from the request")]
    ImageBinding {
        /// Digest of the request's exact source bytes.
        expected: [u8; 32],
        /// Digest retained in the image envelope.
        observed: [u8; 32],
    },
}

fn capture_error(
    error: backend_platform::child_output::CaptureError,
    toolchain: &Path,
    oracle: &Path,
) -> CSharpAuthorityError {
    use backend_platform::child_output::{CaptureError, OutputStream};
    match error {
        CaptureError::Cancelled => CSharpAuthorityError::Cancelled {
            phase: CSharpAuthorityPhase::Process,
        },
        CaptureError::Deadline => CSharpAuthorityError::Deadline {
            phase: CSharpAuthorityPhase::Process,
        },
        CaptureError::Spawn(source) => CSharpAuthorityError::Spawn {
            toolchain: toolchain.to_path_buf().into_boxed_path(),
            oracle: oracle.to_path_buf().into_boxed_path(),
            source,
        },
        CaptureError::OutputLimit {
            stream,
            observed,
            maximum,
        } => CSharpAuthorityError::OutputLimit {
            stream: match stream {
                OutputStream::Stdout => "stdout",
                OutputStream::Stderr => "stderr",
            },
            observed,
            limit: maximum,
        },
        other => CSharpAuthorityError::Capture {
            detail: format!("{other:?}"),
        },
    }
}

fn profile_tag(profile: CSharpVersion) -> &'static str {
    match profile {
        CSharpVersion::CSharp10 => "csharp-10",
        CSharpVersion::CSharp11 => "csharp-11",
        CSharpVersion::CSharp12 => "csharp-12",
        CSharpVersion::CSharp13 => "csharp-13",
        CSharpVersion::CSharp14 => "csharp-14",
    }
}

fn validate_absolute_paths(
    oracle: &Path,
    request: CSharpAuthorityRequest<'_, '_, '_>,
) -> Result<(), CSharpAuthorityError> {
    for (kind, path) in [
        ("oracle", oracle),
        ("package root", request.package_root),
        ("source", request.source_path),
        ("toolchain", request.toolchain),
    ] {
        if !path.is_absolute() {
            return Err(CSharpAuthorityError::RelativePath {
                kind,
                path: path.to_path_buf(),
            });
        }
    }
    for path in request.configuration.reference_directory.into_iter().chain(
        request
            .configuration
            .additional_reference_directories
            .iter()
            .copied(),
    ) {
        if !path.is_absolute() {
            return Err(CSharpAuthorityError::RelativePath {
                kind: "reference directory",
                path: path.to_path_buf(),
            });
        }
    }
    Ok(())
}

fn checkpoint(
    control: CSharpAuthorityControl<'_>,
    phase: CSharpAuthorityPhase,
) -> Result<(), CSharpAuthorityError> {
    if control.cancelled.load(Ordering::Acquire) {
        return Err(CSharpAuthorityError::Cancelled { phase });
    }
    if Instant::now() >= control.deadline {
        return Err(CSharpAuthorityError::Deadline { phase });
    }
    Ok(())
}

#[cfg(test)]
fn join_stream<T>(
    thread: thread::JoinHandle<T>,
    worker: NativeWorker,
) -> Result<T, CSharpAuthorityError> {
    thread
        .join()
        .map_err(|payload| CSharpAuthorityError::WorkerPanic {
            cause: NativeWorkerPanic::capture(worker, payload.as_ref()),
        })
}

/// Runs the explicitly selected dotnet host without inheriting owner
/// credentials, runtime overrides, or unrelated SDK selection variables.
/// Package and reference roots are already carried in the command arguments.
fn isolate_authority_environment(command: &mut Command) {
    command.env_clear();
    #[cfg(windows)]
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", system_root);
    }
}

fn stderr_tail(bytes: &[u8]) -> String {
    let start = bytes.len().saturating_sub(STDERR_TAIL_LIMIT);
    String::from_utf8_lossy(&bytes[start..]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_semantic::vocabulary::{MAX_NATIVE_WORKER_PANIC_BYTES, NativeWorkerPanicClass};
    use std::sync::atomic::AtomicBool;

    #[test]
    fn execution_closure_rejects_changed_helper_bytes() {
        let directory = tempfile::tempdir().expect("tempdir");
        let dotnet = directory.path().join("dotnet");
        let helper = directory.path().join("oracle.dll");
        std::fs::write(&dotnet, b"dotnet-bytes").expect("dotnet");
        std::fs::write(&helper, b"helper-bytes").expect("helper");
        let closure = CSharpExecutionClosure::capture(&[dotnet.as_path(), helper.as_path()])
            .expect("capture execution closure");
        assert!(closure.validate());
        std::fs::write(&helper, b"changed-helper").expect("mutate helper");
        assert!(!closure.validate());
    }

    #[test]
    fn captured_graph_protocol_keeps_project_edges_and_buffers_separate() {
        let sources = [CSharpCapturedSource {
            path: "Api/Api.cs",
            text: "public class Api {}",
        }];
        let config = [CSharpCapturedConfig {
            path: "Api/Api.csproj",
            text: Some("<Project/>"),
        }];
        let nodes = [CSharpCapturedProjectNode {
            project_path: "Api/Api.csproj",
            assembly_name: "Api",
            target_framework: "net8.0",
            language_version: "12.0",
            define_symbols: &["NET8_0"],
            nullable: "enable",
            allow_unsafe: false,
            output_type: "Library",
            usings: &["System"],
            sources: &sources,
            config: &config,
            reference_directories: &[Path::new("/snapshots/api")],
            project_references: &["Core/Core.csproj"],
        }];
        let graph = CSharpCapturedProjectGraph {
            format: 1,
            root_project: "Api/Api.csproj",
            projects: &nodes,
        };
        let json = serde_json::to_value(graph).expect("serialize captured graph");
        assert_eq!(json["RootProject"], "Api/Api.csproj");
        let node = &json["Projects"][0];
        assert_eq!(node["ProjectPath"], "Api/Api.csproj");
        assert_eq!(node["ProjectReferences"][0], "Core/Core.csproj");
        assert_eq!(node["Sources"][0]["Text"], "public class Api {}");
        assert_eq!(node["Config"][0]["Text"], "<Project/>");
        assert_eq!(node["ReferenceDirectories"][0], "/snapshots/api");
        assert_eq!(node["AllowUnsafe"], false);
        assert_eq!(node["DefineSymbols"][0], "NET8_0");
    }

    #[test]
    fn captured_evidence_rejects_changed_attempt_image_or_inputs() {
        let source = CSharpCapturedSource {
            path: "App.cs",
            text: "class C {}",
        };
        let image = b"image";
        let nonce = "0123456789abcdef0123456789abcdef";
        let config = "a".repeat(64);
        let mut value = serde_json::json!({
            "format": 1, "nonce": nonce, "sourceBinding": "App.cs",
            "imageSha256": format!("{:x}", Sha256::digest(image)), "imageLength": image.len(),
            "configSha256": config, "sources": [{"path":source.path,"length":source.text.len(),"sha256":format!("{:x}",Sha256::digest(source.text.as_bytes()))}],
            "references": [{"Name":"System.Runtime","Length":1,"Sha256":"b".repeat(64)}],
            "assembly":"App", "framework":"net10.0", "language":"CSharp14", "outputKind":"DynamicallyLinkedLibrary",
            "defines":[], "usings":[], "generators":"unknown", "inputClosure":"unknown", "errors":0,
        });
        let check = |value: &serde_json::Value| {
            validate_captured_evidence(
                &serde_json::to_vec(value).unwrap(),
                nonce,
                &"a".repeat(64),
                "App.cs",
                image,
                &[source],
            )
        };
        assert!(check(&value).is_ok());
        value["nonce"] = "different".into();
        assert!(check(&value).is_err());
        value["nonce"] = nonce.into();
        value["imageLength"] = 6.into();
        assert!(check(&value).is_err());
        value["imageLength"] = image.len().into();
        value["sources"][0]["sha256"] = "f".repeat(64).into();
        assert!(check(&value).is_err());
    }

    #[test]
    fn profile_tags_are_closed_and_explicit() {
        assert_eq!(profile_tag(CSharpVersion::CSharp10), "csharp-10");
        assert_eq!(profile_tag(CSharpVersion::CSharp14), "csharp-14");
    }

    #[test]
    fn cancellation_wins_before_filesystem_work() {
        let cancelled = AtomicBool::new(true);
        let oracle = CSharpOracle::new(PathBuf::from("/oracle.dll"));
        let result = oracle.authority_image(CSharpAuthorityRequest {
            package_root: Path::new("/missing"),
            source_path: Path::new("/missing/source.cs"),
            project_file: None,
            source: b"class C {}",
            captured_sources: None,
            captured_config: None,
            captured_project_graph: None,
            execution_closure: None,
            profile: CSharpVersion::CSharp14,
            native_tool: NativeTool::CSharpCompiler,
            toolchain: Path::new("/dotnet"),
            control: CSharpAuthorityControl {
                deadline: Instant::now() + Duration::from_secs(1),
                cancelled: &cancelled,
            },
            configuration: CSharpAuthorityConfiguration::DEFAULT,
        });
        assert!(matches!(
            result,
            Err(CSharpAuthorityError::Cancelled {
                phase: CSharpAuthorityPhase::Admission
            })
        ));
    }

    #[test]
    fn source_binding_rejects_bytes_before_child_spawn() {
        let source_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/producer/fidelity.cs");
        let package_root = source_path.parent().expect("fixture parent");
        let cancelled = AtomicBool::new(false);
        let oracle = CSharpOracle::new(PathBuf::from("/oracle.dll"));
        let result = oracle.authority_image(CSharpAuthorityRequest {
            package_root,
            source_path: &source_path,
            project_file: None,
            source: b"class Different {}",
            captured_sources: None,
            captured_config: None,
            captured_project_graph: None,
            execution_closure: None,
            profile: CSharpVersion::CSharp14,
            native_tool: NativeTool::CSharpCompiler,
            toolchain: Path::new("/dotnet"),
            control: CSharpAuthorityControl {
                deadline: Instant::now() + Duration::from_secs(1),
                cancelled: &cancelled,
            },
            configuration: CSharpAuthorityConfiguration::DEFAULT,
        });
        assert!(matches!(
            result,
            Err(CSharpAuthorityError::SourceBinding { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn producer_deadline_includes_descendant_holding_output_pipes() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().expect("private fixture");
        let source_path = directory.path().join("A.cs");
        let tool = directory.path().join("fake-dotnet");
        fs::write(&source_path, b"class A {}").expect("source fixture");
        fs::write(&tool, b"#!/bin/sh\nsleep 10 & exit 0\n").expect("tool fixture");
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o700)).expect("tool permissions");
        let helper = directory.path().join("oracle.dll");
        let oracle = CSharpOracle::new(helper);
        let cancelled = AtomicBool::new(false);
        let start = Instant::now();
        let result = oracle.authority_image(CSharpAuthorityRequest {
            package_root: directory.path(),
            source_path: &source_path,
            project_file: None,
            source: b"class A {}",
            captured_sources: None,
            captured_config: None,
            captured_project_graph: None,
            execution_closure: None,
            profile: CSharpVersion::CSharp14,
            native_tool: NativeTool::CSharpCompiler,
            toolchain: &tool,
            control: CSharpAuthorityControl {
                deadline: start + Duration::from_millis(300),
                cancelled: &cancelled,
            },
            configuration: CSharpAuthorityConfiguration::DEFAULT,
        });
        assert!(
            matches!(result, Err(CSharpAuthorityError::Deadline { .. })),
            "{result:?}"
        );
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn stream_join_retains_exact_worker_and_owned_panic_payload() {
        let thread = thread::spawn(|| {
            std::panic::panic_any(String::from("csharp stderr reader failed"));
        });
        let result = join_stream(thread, NativeWorker::StandardErrorReader);
        let cause = match result {
            Err(CSharpAuthorityError::WorkerPanic { cause }) => cause,
            Err(other) => panic!("unexpected join error: {other:?}"),
            Ok(_) => panic!("panicking stream unexpectedly joined successfully"),
        };
        assert_eq!(cause.worker, NativeWorker::StandardErrorReader);
        assert_eq!(cause.class, NativeWorkerPanicClass::OwnedMessage);
        assert_eq!(cause.message.byte_len, "csharp stderr reader failed".len());
        assert!(!cause.message.truncated);
        assert_eq!(
            &cause.message.bytes[..cause.message.byte_len],
            b"csharp stderr reader failed"
        );
        assert!(cause.message.byte_len <= MAX_NATIVE_WORKER_PANIC_BYTES);
    }
}
