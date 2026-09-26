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
//! again after decode.  The child and all descendants are reaped on a stream
//! bound, deadline, or cancellation terminal.

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use backend_semantic::vocabulary::{CSharpVersion, NativeTool, NativeWorker, NativeWorkerPanic};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::legacy::{CSharpImage, ImageError};

mod run;

/// The default maximum bytes accepted from either child stream.
pub const DEFAULT_OUTPUT_LIMIT: usize = 32 * 1024 * 1024;
/// The default maximum authority-image bytes retained by one producer call.
pub const DEFAULT_IMAGE_LIMIT: usize = 32 * 1024 * 1024;
/// The default maximum bytes accepted for the selected C# source file.
pub const DEFAULT_SOURCE_LIMIT: usize = 16 * 1024 * 1024;
/// Maximum stderr transcript retained in an exit diagnostic.
const STDERR_TAIL_LIMIT: usize = 4096;
/// Poll interval for the bounded child wait loop.
const CHILD_POLL_INTERVAL: Duration = Duration::from_millis(2);

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
    /// Additional preprocessor symbols, in caller order.
    pub define_symbols: &'config [&'config str],
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
        define_symbols: &[],
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
    /// Exact source bytes that the caller will lower after authority entry.
    pub source: &'request [u8],
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CSharpAuthorityImage {
    bytes: Box<[u8]>,
}

impl CSharpAuthorityImage {
    fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            bytes: bytes.into_boxed_slice(),
        }
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

struct BoundedStream {
    bytes: Vec<u8>,
    exceeded: Option<(&'static str, usize)>,
    error: Option<std::io::Error>,
}

fn read_bounded(
    mut reader: impl Read,
    limit: usize,
    stream: &'static str,
    sender: mpsc::Sender<(&'static str, usize)>,
) -> BoundedStream {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => {
                return BoundedStream {
                    bytes,
                    exceeded: None,
                    error: None,
                };
            }
            Ok(count) if bytes.len().saturating_add(count) > limit => {
                let observed = bytes.len().saturating_add(count);
                let _ = sender.send((stream, observed));
                return BoundedStream {
                    bytes,
                    exceeded: Some((stream, observed)),
                    error: None,
                };
            }
            Ok(count) => bytes.extend_from_slice(&chunk[..count]),
            Err(source) if source.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return BoundedStream {
                    bytes,
                    exceeded: None,
                    error: Some(source),
                };
            }
        }
    }
}

fn join_stream(
    thread: thread::JoinHandle<BoundedStream>,
    worker: NativeWorker,
) -> Result<BoundedStream, CSharpAuthorityError> {
    thread
        .join()
        .map_err(|payload| CSharpAuthorityError::WorkerPanic {
            cause: NativeWorkerPanic::capture(worker, payload.as_ref()),
        })
}

fn wait_for_child(
    child: &mut Child,
    control: CSharpAuthorityControl<'_>,
    limit_receiver: &mpsc::Receiver<(&'static str, usize)>,
    output_limit: usize,
) -> Result<std::process::ExitStatus, CSharpAuthorityError> {
    loop {
        if control.cancelled.load(Ordering::Acquire) {
            terminate_child(child);
            return Err(CSharpAuthorityError::Cancelled {
                phase: CSharpAuthorityPhase::Process,
            });
        }
        if Instant::now() >= control.deadline {
            terminate_child(child);
            return Err(CSharpAuthorityError::Deadline {
                phase: CSharpAuthorityPhase::Process,
            });
        }
        if let Ok((stream, observed)) = limit_receiver.try_recv() {
            terminate_child(child);
            return Err(CSharpAuthorityError::OutputLimit {
                stream,
                observed,
                limit: output_limit,
            });
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|source| CSharpAuthorityError::Pipe {
                stream: "process",
                source,
            })?
        {
            return Ok(status);
        }
        thread::sleep(CHILD_POLL_INTERVAL);
    }
}

fn terminate_child(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id().to_string();
        let result = Command::new("kill")
            .args(["-KILL", &format!("-{pid}")])
            .status();
        if !result.map(|status| status.success()).unwrap_or(false) {
            let _ = child.kill();
        }
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
    let _ = child.wait();
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
            source: b"class C {}",
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
            source: b"class Different {}",
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

    #[test]
    fn stream_join_retains_exact_worker_and_owned_panic_payload() {
        let thread = thread::spawn(|| -> BoundedStream {
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
