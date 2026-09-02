//! Shared fixtures for the direct Clang semantic frontend tests.
//! Fixtures keep the source authority in caller bytes and register only a source-relative
//! display name; include roots are caller-owned temporary directories.

use std::{
    error::Error,
    fmt, fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

use super::{AnalysisInput, AnalysisScratch, ClangFact, ClangReport, ClangSourceLanguage};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

/// Exact test failure retained as typed text; never a panic.
#[derive(Debug)]
pub(crate) struct TestError(pub String);

impl fmt::Display for TestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for TestError {}

impl From<std::io::Error> for TestError {
    fn from(error: std::io::Error) -> Self {
        Self(error.to_string())
    }
}

/// Caller-owned temporary include root with a source-relative display name.
pub(crate) struct Fixture {
    root: PathBuf,
    source_name: PathBuf,
}

impl Fixture {
    /// Creates one isolated include root and its source-relative display name.
    pub(crate) fn new(extension: &str) -> Result<Self, TestError> {
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("compiler-driver-clang-{}-{id}", std::process::id()));
        fs::create_dir(&root)?;
        let source_name = PathBuf::from(format!("fixture.{extension}"));
        Ok(Self { root, source_name })
    }

    /// The caller-owned include root passed as the single `-I` argument.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// The source-relative display name registered for the unsaved buffer.
    pub(crate) fn source_name(&self) -> &Path {
        &self.source_name
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _removed = fs::remove_dir_all(&self.root);
    }
}

/// Caller-probed clang version bytes carrying the executable identity.
pub(crate) struct ToolFact {
    pub(crate) version: Vec<u8>,
}

/// Result of one admitted analysis with its committed facts.
pub(crate) struct Analysis<'source> {
    pub(crate) facts: Vec<ClangFact<'source>>,
    pub(crate) report: ClangReport,
}

/// Refuses to exercise semantic behavior when the link-time libclang authority is absent.
///
/// The typed failure keeps unavailable hosts honest: these tests fail with the exact
/// unavailable cause instead of passing silently.
pub(crate) fn require_linked_library() -> Result<(), TestError> {
    #[cfg(not(clang_native))]
    let verdict = Err(TestError(
        "typed unavailable: the linked libclang authority is absent from this build \
         (ClangError::LibclangUnavailable)"
            .to_owned(),
    ));
    #[cfg(clang_native)]
    let verdict = Ok(());
    verdict
}

/// Probes the host clang executable declared by `NUDOX_CLANG` or the `PATH`.
pub(crate) fn real_clang() -> Result<ToolFact, TestError> {
    let executable = match std::env::var_os("NUDOX_CLANG") {
        Some(configured) => {
            let configured = PathBuf::from(configured);
            if !configured.is_absolute() {
                return Err(TestError("NUDOX_CLANG must be absolute".to_owned()));
            }
            configured
        }
        None => path_lookup("clang")?,
    };
    let version = bounded_version(&executable)?;
    Ok(ToolFact { version })
}

/// Runs one analysis and collects its committed facts, failing on any typed error.
pub(crate) fn run_analysis<'source>(
    fixture: &Fixture,
    source: &'source [u8],
    language: ClangSourceLanguage,
    expected_version: Option<&[u8]>,
    cancellation: Option<&AtomicBool>,
    identity_scratch_bytes: usize,
    fact_scratch_bytes: usize,
) -> Result<Analysis<'source>, TestError> {
    let mut identity = vec![0_u8; identity_scratch_bytes];
    let mut facts = vec![0_u8; fact_scratch_bytes];
    let mut emitted = Vec::new();
    let report = super::analyze(
        AnalysisInput {
            include_root: fixture.root(),
            source_name: fixture.source_name(),
            source_language: language,
            source,
        },
        expected_version,
        cancellation,
        None,
        AnalysisScratch {
            identity: &mut identity,
            facts: &mut facts,
        },
        |fact| emitted.push(fact),
    )
    .map_err(|error| TestError(format!("native analysis failed: {error:?}")))?;
    Ok(Analysis {
        facts: emitted,
        report,
    })
}

/// Resolves one executable name through the caller's `PATH` without spawning anything.
fn path_lookup(name: &str) -> Result<PathBuf, TestError> {
    let paths = std::env::var_os("PATH")
        .ok_or_else(|| TestError(format!("host does not expose {name:?} on PATH")))?;
    for directory in std::env::split_paths(&paths) {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return candidate
                .canonicalize()
                .map_err(|cause| TestError(format!("could not canonicalize {name:?}: {cause}")));
        }
    }
    Err(TestError(format!("host does not expose {name:?} on PATH")))
}

/// Bounded `--version` probe of the host clang executable.
fn bounded_version(executable: &Path) -> Result<Vec<u8>, TestError> {
    const LIMIT: usize = 16 * 1024;
    let mut child = Command::new(executable)
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| TestError("version stdout pipe missing".to_owned()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| TestError("version stderr pipe missing".to_owned()))?;
    let (stdout, stderr) = std::thread::scope(|scope| {
        let stdout = scope.spawn(|| read_limited(stdout, LIMIT));
        let stderr = scope.spawn(|| read_limited(stderr, LIMIT));
        (stdout.join(), stderr.join())
    });
    let status = child.wait()?;
    let stdout = stdout.map_err(|_| TestError("version stdout reader panicked".to_owned()))??;
    let stderr = stderr.map_err(|_| TestError("version stderr reader panicked".to_owned()))??;
    if !status.success() {
        return Err(TestError(format!("version failed: {status:?}")));
    }
    if !stderr.is_empty() {
        return Err(TestError("version wrote stderr".to_owned()));
    }
    if stdout.is_empty() {
        return Err(TestError("version was empty".to_owned()));
    }
    Ok(stdout)
}

fn read_limited(mut reader: impl Read, limit: usize) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            return Ok(bytes);
        }
        let next = bytes.len().checked_add(read).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "version size overflow")
        })?;
        if next > limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "version output exceeded test limit",
            ));
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
}
