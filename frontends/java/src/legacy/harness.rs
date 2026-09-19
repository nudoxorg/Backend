//! A bounded, source-owning adapter around the vendored javac authority doclet.

use std::{
    borrow::Cow,
    env, fs,
    fs::File,
    io::{self, Read},
    path::{Component, Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::atomic::{AtomicUsize, Ordering},
};

use sha2::{Digest, Sha256};
use thiserror::Error;

const STDERR_LIMIT: u64 = 64 * 1024;
const MARKER: &str = ".doclet-digest";
const AUTHORITY_SOURCE: &str = include_str!("doclet/AuthorityImage.java");
const EXTRACTOR_SOURCE: &str = include_str!("doclet/CompilerExtractor.java");
static DIRECTORY_SERIAL: AtomicUsize = AtomicUsize::new(0);

/// A JDK home whose two runtime tools are available.
#[derive(Debug)]
pub struct JdkToolchain<'jdk> {
    root: Cow<'jdk, Path>,
}

impl<'jdk> JdkToolchain<'jdk> {
    /// Validates a JDK home without taking ownership of its path.
    pub fn new(root: &'jdk Path) -> Result<Self, HarnessError> {
        for executable in ["javac", "java"] {
            let path = root.join("bin").join(executable);
            if !path.is_file() {
                return Err(HarnessError::MissingExecutable { path });
            }
        }
        Ok(Self {
            root: Cow::Borrowed(root),
        })
    }

    fn executable(&self, name: &str) -> PathBuf {
        self.root.join("bin").join(name)
    }

    /// Returns the exact admitted JDK root used to select javac and java.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

impl JdkToolchain<'static> {
    /// Admits and owns one caller-selected absolute JDK root without reading
    /// an environment variable when the authority is entered later.
    pub fn from_owned_root(root: PathBuf) -> Result<Self, HarnessError> {
        if !root.is_absolute() {
            return Err(HarnessError::RelativeJdkRoot { root });
        }
        for executable in ["javac", "java"] {
            let path = root.join("bin").join(executable);
            if !path.is_file() {
                return Err(HarnessError::MissingExecutable { path });
            }
        }
        Ok(Self {
            root: Cow::Owned(root),
        })
    }

    /// Reads and validates the pinned JDK named by `NUDOX_JDK`.
    pub fn from_env() -> Result<Self, HarnessError> {
        let root = env::var_os("NUDOX_JDK").ok_or(HarnessError::MissingEnvironment {
            variable: "NUDOX_JDK",
        })?;
        Self::from_owned_root(PathBuf::from(root))
    }
}

/// One named Java source, borrowed for the duration of an extraction.
#[derive(Clone, Copy, Debug)]
pub struct JavaSource<'source> {
    /// Relative path used as the source's Java file name.
    pub name: &'source Path,
    /// Exact source bytes used for compilation and source binding.
    pub bytes: &'source [u8],
}

/// Inputs to one authority-image extraction.
#[derive(Clone, Copy, Debug)]
pub struct HarnessRequest<'request> {
    /// Sources. The first source is the source-binding input, matching the doclet CLI.
    pub sources: &'request [JavaSource<'request>],
    /// Jars made visible to javac through the child JVM class path.
    pub classpath: &'request [&'request Path],
    /// Language/platform profile passed to the doclet.
    pub release: crate::legacy::JavaRelease,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnavailableCause {
    Compilation,
    Compiler,
    Toolchain,
}

#[derive(Debug)]
pub struct ReleaseFailure {
    pub release: crate::legacy::JavaRelease,
    pub cause: UnavailableCause,
    pub error: HarnessError,
}

#[derive(Debug)]
pub enum HarnessOutcome {
    Available {
        release: crate::legacy::JavaRelease,
        prior_failures: Vec<ReleaseFailure>,
    },
    Unavailable {
        attempts: Vec<ReleaseFailure>,
    },
}

/// A prepared doclet and its owned scratch lifetime.
#[derive(Debug)]
pub struct Harness {
    root: PathBuf,
    doclet_classes: PathBuf,
    cleanup_error: Option<io::Error>,
}

impl Harness {
    /// Creates the private scratch directory used by this harness.
    pub fn new() -> Result<Self, HarnessError> {
        for attempt in 0..64 {
            let serial = DIRECTORY_SERIAL.fetch_add(1, Ordering::Relaxed);
            let root = env::temp_dir().join(format!(
                "nudox-java-harness-{}-{serial}-{attempt}",
                std::process::id()
            ));
            match fs::create_dir(&root) {
                Ok(()) => {
                    return Ok(Self {
                        doclet_classes: root.join("classes"),
                        root,
                        cleanup_error: None,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => return Err(HarnessError::Io { path: root, source }),
            }
        }
        Err(HarnessError::UniqueDirectory)
    }

    /// Returns the compiled classes directory, useful for lifecycle observability.
    pub fn classes_dir(&self) -> &Path {
        &self.doclet_classes
    }

    /// Returns the preparation marker path.
    pub fn marker_path(&self) -> PathBuf {
        self.doclet_classes.join(MARKER)
    }

    /// Returns a cleanup failure observed by `Drop`, if any.
    pub fn take_cleanup_error(&mut self) -> Option<io::Error> {
        self.cleanup_error.take()
    }

    /// Compiles the embedded doclet once per source digest.
    pub fn prepare(&mut self, toolchain: &JdkToolchain<'_>) -> Result<(), HarnessError> {
        fs::create_dir(&self.doclet_classes)
            .or_else(|error| {
                if error.kind() == io::ErrorKind::AlreadyExists {
                    Ok(())
                } else {
                    Err(error)
                }
            })
            .map_err(|source| HarnessError::Io {
                path: self.doclet_classes.clone(),
                source,
            })?;
        let digest = source_digest();
        if fs::read(self.marker_path()).ok().as_deref() == Some(digest.as_slice()) {
            return Ok(());
        }
        let source_dir = self.root.join("doclet-source");
        fs::create_dir(&source_dir).map_err(|source| HarnessError::Io {
            path: source_dir.clone(),
            source,
        })?;
        let authority = source_dir.join("AuthorityImage.java");
        let extractor = source_dir.join("CompilerExtractor.java");
        fs::write(&authority, AUTHORITY_SOURCE).map_err(|source| HarnessError::Io {
            path: authority.clone(),
            source,
        })?;
        fs::write(&extractor, EXTRACTOR_SOURCE).map_err(|source| HarnessError::Io {
            path: extractor.clone(),
            source,
        })?;
        let stderr = self.root.join("prepare.stderr");
        let mut command = Command::new(toolchain.executable("javac"));
        command
            .args([
                "--release",
                "21",
                "--add-modules",
                "jdk.compiler,jdk.javadoc",
                "-d",
            ])
            .arg(&self.doclet_classes)
            .args([authority, extractor]);
        run_command(command, "javac", &stderr)?;
        fs::write(self.marker_path(), digest).map_err(|source| HarnessError::Io {
            path: self.marker_path(),
            source,
        })
    }

    pub fn image_with_releases(
        &self,
        toolchain: &JdkToolchain<'_>,
        request: HarnessRequest<'_>,
        sourcepath: &[&Path],
        releases: &[crate::legacy::JavaRelease],
        output: &mut Vec<u8>,
    ) -> Result<HarnessOutcome, HarnessError> {
        output.clear();
        let mut attempts = Vec::new();
        for &release in std::iter::once(&request.release).chain(releases.iter()) {
            if attempts
                .iter()
                .any(|attempt: &ReleaseFailure| attempt.release == release)
            {
                continue;
            }
            match self.image_with_sourcepath(
                toolchain,
                HarnessRequest { release, ..request },
                sourcepath,
                output,
            ) {
                Ok(()) => {
                    return Ok(HarnessOutcome::Available {
                        release,
                        prior_failures: attempts,
                    });
                }
                Err(error) => {
                    let Some(cause) = error.unavailable_cause() else {
                        return Err(error);
                    };
                    attempts.push(ReleaseFailure {
                        release,
                        cause,
                        error,
                    });
                    if cause != UnavailableCause::Compilation {
                        break;
                    }
                }
            }
        }
        Ok(HarnessOutcome::Unavailable { attempts })
    }

    /// Writes borrowed sources, runs the extractor, and replaces `output` with its image.
    pub fn image(
        &self,
        toolchain: &JdkToolchain<'_>,
        request: HarnessRequest<'_>,
        output: &mut Vec<u8>,
    ) -> Result<(), HarnessError> {
        self.image_with_sourcepath(toolchain, request, &[], output)
    }

    /// Writes borrowed sources, runs the extractor with a best-effort
    /// `-sourcepath`, and replaces `output` with its image.
    ///
    /// See [`Self::image_with_corpus`] for the extraction contract; the
    /// fleet corpus here is read from `$NUDOX_JAVA_CORPUS_DIR`.
    pub fn image_with_sourcepath(
        &self,
        toolchain: &JdkToolchain<'_>,
        request: HarnessRequest<'_>,
        sourcepath: &[&Path],
        output: &mut Vec<u8>,
    ) -> Result<(), HarnessError> {
        self.image_with_corpus(toolchain, request, sourcepath, None, output)
    }

    /// Writes borrowed sources, runs the extractor, and replaces `output`
    /// with its image, discovering fleet siblings from `corpus` when given
    /// and from `$NUDOX_JAVA_CORPUS_DIR` otherwise.
    ///
    /// The entries are host source roots the extractor passes to javac so
    /// same-group sibling artifacts resolve without an explicit classpath.
    /// Missing or non-directory entries are ignored. The per-run staging
    /// directory always heads the path so module-mode compilation (a
    /// `module-info.java` among the sources) resolves the staged copies.
    ///
    /// After the caller's roots, every version root discovered under the
    /// corpus root is appended (see [`super::sourcepath`]), so cross-group
    /// fleet siblings — annotation jars like `org.jspecify` or
    /// `org.jetbrains.annotations`, and sibling APIs like `org.slf4j` —
    /// resolve the same way the Go oracle resolves corpus checkouts. Caller
    /// roots keep precedence and the corpus never shadows them. Root
    /// discovery skips `module-info.java`-carrying roots because javac turns
    /// any such source-path entry into a required-module lookup that fails
    /// every unnamed-module compilation. When the corpus is absent the
    /// behavior is exactly the caller-provided roots.
    ///
    /// JPMS module walls are crossed honestly, without rewriting sources:
    /// when the compile closure needs modules — a top-level
    /// `module-info.java` is staged among the sources or under a caller
    /// root — the extraction runs in module mode. The unnamed `-sourcepath`
    /// is replaced by repeated `--module-source-path <module>=<path>`
    /// entries (javac rejects the two options together): the target module
    /// maps to the staged run directory, and every module-walled corpus
    /// root maps to its canonical directory, deduplicated by module name
    /// with the target's own name never shadowed. javac then compiles the
    /// required corpus modules from source. A `requires` whose sources are
    /// absent stays a typed compilation refusal naming that module; when an
    /// unnamed attempt fails with `module not found: <name>` and the corpus
    /// can provision modules, the extraction retries once in module mode
    /// and preserves the retry's precise diagnostic. Release 8 predates the
    /// module system and never enters module mode. Non-modular closures
    /// keep the byte-identical default invocation.
    pub fn image_with_corpus(
        &self,
        toolchain: &JdkToolchain<'_>,
        request: HarnessRequest<'_>,
        sourcepath: &[&Path],
        corpus: Option<&Path>,
        output: &mut Vec<u8>,
    ) -> Result<(), HarnessError> {
        output.clear();
        let first = request.sources.first().ok_or(HarnessError::NoSources)?;
        let discovered =
            super::sourcepath::merged_source_roots(sourcepath, corpus).map_err(|source| {
                HarnessError::Io {
                    path: self.root.clone(),
                    source,
                }
            })?;
        let run = self.root.join(format!(
            "run-{}",
            DIRECTORY_SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&run).map_err(|source| HarnessError::Io {
            path: run.clone(),
            source,
        })?;
        for source in request.sources {
            validate_name(source.name)?;
            let path = run.join(source.name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|source| HarnessError::Io {
                    path: parent.to_owned(),
                    source,
                })?;
            }
            let source_path = path;
            fs::write(&source_path, source.bytes).map_err(|error| HarnessError::Io {
                path: source_path,
                source: error,
            })?;
        }
        let image = run.join("authority.image");
        let mut cp = vec![self.doclet_classes.clone()];
        cp.extend(request.classpath.iter().copied().map(Path::to_owned));
        let classpath = env::join_paths(cp).map_err(|source| HarnessError::Classpath { source })?;
        let plan = module_plan(request.release, request.sources, &discovered, corpus, &run)
            .map_err(|source| HarnessError::Io {
                path: run.clone(),
                source,
            })?;
        let attempt = |entries: Option<&[(String, PathBuf)]>| -> Result<Vec<u8>, HarnessError> {
            let mut command = Command::new(toolchain.executable("java"));
            command
                .args(["--add-modules", "jdk.compiler,jdk.javadoc", "-cp"])
                .arg(&classpath)
                .arg("nudox.oracle.CompilerExtractor")
                .args(["--release", release_text(request.release), "--outfile"])
                .arg(&image)
                .arg("--source-binding")
                .arg(run.join(first.name));
            // Module mode replaces the unnamed source path entirely: javac
            // rejects `--source-path` together with `--module-source-path`,
            // and a named compilation resolves everything required through
            // the module source path entries below. The target module's
            // entry points at the run directory, which holds the explicit
            // sources in their package layout.
            match entries {
                Some(entries) => {
                    for (name, path) in entries {
                        command
                            .arg("--module-source-path")
                            .arg(format!("{name}={}", path.display()));
                    }
                }
                None => {
                    // The run directory always heads the source path: it
                    // holds the explicit sources in their package layout,
                    // which module-mode javac requires (a `module-info.java`
                    // among the sources makes every explicit file resolve
                    // through the source path, and the host roots alone do
                    // not contain the staged copies). For unnamed-module
                    // packages the extra entry is benign: it carries the
                    // same files.
                    let mut roots: Vec<&Path> =
                        Vec::with_capacity(sourcepath.len().saturating_add(1));
                    roots.push(&run);
                    roots.extend(discovered.iter().map(PathBuf::as_path));
                    let joined = env::join_paths(&roots)
                        .map_err(|source| HarnessError::Classpath { source })?;
                    command.arg("--sourcepath").arg(joined);
                }
            }
            for source in request.sources {
                command.arg(run.join(source.name));
            }
            let stderr = run.join("extract.stderr");
            if let Err(mut error) = run_command(command, "java", &stderr) {
                // The child's stderr stream is not dependable on every host,
                // so the doclet also persists the exact diagnostic text beside
                // the image. When present it is the preserved diagnostic; the
                // captured stderr stays as the fallback.
                let sidecar = run.join("authority.diagnostics");
                if let HarnessError::Command { stderr, .. } = &mut error {
                    if let Ok(bytes) = fs::read(&sidecar) {
                        if !bytes.is_empty() {
                            let capped = &bytes[..bytes.len().min(STDERR_LIMIT as usize)];
                            *stderr = String::from_utf8_lossy(capped).into_owned();
                        }
                    }
                }
                return Err(error);
            }
            let mut bytes = Vec::new();
            File::open(&image)
                .map_err(|source| HarnessError::Io {
                    path: image.clone(),
                    source,
                })?
                .read_to_end(&mut bytes)
                .map_err(|source| HarnessError::Io {
                    path: image.clone(),
                    source,
                })?;
            Ok(bytes)
        };
        let mut extraction = attempt(plan.as_deref());
        if extraction
            .as_ref()
            .is_err_and(HarnessError::names_missing_module)
            && plan.is_none()
        {
            // Detection by failure: an unnamed closure whose javac diagnostic
            // names an unresolvable module retries once with the fleet's
            // module roots. The retry's diagnostic replaces the first
            // attempt's, so the preserved refusal names the true missing
            // artifact; when the corpus cannot provision modules the first
            // attempt's error is kept exactly.
            let retry = super::sourcepath::corpus_module_roots(corpus).map(|modules| {
                let mut entries: Vec<(String, PathBuf)> = Vec::new();
                for module in modules {
                    if entries.iter().any(|(name, _)| *name == module.name) {
                        continue;
                    }
                    entries.push((module.name, module.path));
                }
                entries
            });
            if let Ok(entries) = retry {
                if !entries.is_empty() {
                    extraction = attempt(Some(&entries));
                }
            }
        }
        match extraction {
            Ok(bytes) => {
                output.clear();
                *output = bytes;
                fs::remove_dir_all(&run).map_err(|source| HarnessError::Io { path: run, source })
            }
            Err(error) => Err(error),
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.root) {
            self.cleanup_error = Some(error);
        }
    }
}

/// The `--module-source-path` plan for one extraction, when the compile
/// closure needs modules.
///
/// Detection: a top-level `module-info.java` is staged among the explicit
/// sources or sits at a discovered root — the package itself is
/// module-walled. The plan maps the target module to its staged run
/// directory first, so the staged copies win, and then every fleet corpus
/// module root, deduplicated by module name, never shadowing the target's
/// own module name. Release 8 predates the module system and never plans
/// module mode. A non-modular closure yields no plan, keeping the
/// byte-identical default `-sourcepath` invocation.
fn module_plan(
    release: crate::legacy::JavaRelease,
    sources: &[JavaSource<'_>],
    discovered: &[PathBuf],
    corpus: Option<&Path>,
    run: &Path,
) -> io::Result<Option<Vec<(String, PathBuf)>>> {
    if release == crate::legacy::JavaRelease::Java8 {
        return Ok(None);
    }
    let target = sources
        .iter()
        .find(|source| source.name == Path::new("module-info.java"))
        .and_then(|source| super::sourcepath::parse_module_name(source.bytes));
    let modular = target.is_some()
        || discovered
            .iter()
            .any(|root| root.join("module-info.java").is_file());
    if !modular {
        return Ok(None);
    }
    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    if let Some(name) = target.as_ref() {
        entries.push((name.clone(), run.to_path_buf()));
    }
    for module in super::sourcepath::corpus_module_roots(corpus)? {
        if target.as_ref() == Some(&module.name)
            || entries.iter().any(|(name, _)| *name == module.name)
        {
            continue;
        }
        entries.push((module.name, module.path));
    }
    if entries.is_empty() {
        return Ok(None);
    }
    Ok(Some(entries))
}

fn source_digest() -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(AUTHORITY_SOURCE.as_bytes());
    digest.update(EXTRACTOR_SOURCE.as_bytes());
    digest.finalize().into()
}

fn release_text(release: crate::legacy::JavaRelease) -> &'static str {
    match release {
        crate::legacy::JavaRelease::Java8 => "8",
        crate::legacy::JavaRelease::Java11 => "11",
        crate::legacy::JavaRelease::Java17 => "17",
        crate::legacy::JavaRelease::Java21 => "21",
        crate::legacy::JavaRelease::Java25 => "25",
    }
}

fn validate_name(path: &Path) -> Result<(), HarnessError> {
    if path.as_os_str().is_empty()
        || path.components().any(|component| {
            matches!(
                component,
                Component::RootDir | Component::Prefix(_) | Component::ParentDir
            )
        })
    {
        return Err(HarnessError::InvalidSourceName {
            path: path.to_owned(),
        });
    }
    Ok(())
}

fn run_command(
    mut command: Command,
    program: &'static str,
    stderr_path: &Path,
) -> Result<(), HarnessError> {
    let command_text = format!("{program} {command:?}");
    // stderr is captured through a pipe that is drained to EOF before the
    // child is reaped, so the preserved excerpt cannot race the JVM's final
    // writes. The drained bytes are then persisted at `stderr_path` exactly
    // as the file-based capture used to expose them.
    command.stdout(Stdio::null()).stderr(Stdio::piped());
    let output = command.output().map_err(|source| HarnessError::Spawn {
        command: command_text.clone(),
        source,
    })?;
    let excerpt = &output.stderr[..output.stderr.len().min(STDERR_LIMIT as usize)];
    fs::write(stderr_path, excerpt).map_err(|source| HarnessError::Io {
        path: stderr_path.to_owned(),
        source,
    })?;
    let status = output.status;
    if status.success() {
        return Ok(());
    }
    Err(HarnessError::Command {
        command: command_text,
        status,
        stderr: String::from_utf8_lossy(excerpt).into_owned(),
    })
}

/// Failures retain the command and bounded compiler diagnostics where applicable.
#[derive(Debug, Error)]
pub enum HarnessError {
    /// A retained JDK root must be absolute so the host cannot consult an
    /// ambient working directory when the authority session starts.
    #[error("JDK root is not absolute: {root:?}")]
    RelativeJdkRoot {
        /// Rejected caller-supplied root.
        root: PathBuf,
    },
    /// The environment variable was not set.
    #[error("environment variable {variable} is not set")]
    MissingEnvironment {
        /// The required environment variable name.
        variable: &'static str,
    },
    /// A required JDK executable is absent.
    #[error("JDK executable is missing: {path:?}")]
    MissingExecutable {
        /// The executable path that was absent.
        path: PathBuf,
    },
    /// A filesystem operation failed.
    #[error("Java harness filesystem operation at {path:?} failed: {source}")]
    Io {
        /// The path involved in the failed operation.
        path: PathBuf,
        /// The original filesystem error.
        #[source]
        source: io::Error,
    },
    /// No unique scratch directory could be allocated.
    #[error("could not allocate a unique Java harness scratch directory")]
    UniqueDirectory,
    /// The request has no source to bind.
    #[error("Java harness requires at least one source file")]
    NoSources,
    /// A source name could escape the per-run directory.
    #[error("invalid relative Java source name: {path:?}")]
    InvalidSourceName {
        /// The rejected caller-provided source name.
        path: PathBuf,
    },
    /// Classpath paths could not be represented for this platform.
    #[error("could not construct the Java classpath: {source}")]
    Classpath {
        /// The original platform path-list error.
        source: env::JoinPathsError,
    },
    /// The child process could not be started.
    #[error("could not start {command}: {source}")]
    Spawn {
        /// The complete command representation.
        command: String,
        /// The original process-start error.
        #[source]
        source: io::Error,
    },
    /// The child exited unsuccessfully; stderr is capped at 64 KiB.
    #[error("{command} failed with status {status}: {stderr}")]
    Command {
        /// The complete command representation.
        command: String,
        /// The process exit status.
        status: ExitStatus,
        /// At most 64 KiB of stderr.
        stderr: String,
    },
}

impl HarnessError {
    #[must_use]
    pub fn unavailable_cause(&self) -> Option<UnavailableCause> {
        match self {
            Self::Command {
                command, status, ..
            } if command.contains("nudox.oracle.CompilerExtractor") => Some(match status.code() {
                Some(2) => UnavailableCause::Compilation,
                Some(3) => UnavailableCause::Compiler,
                _ => UnavailableCause::Toolchain,
            }),
            Self::MissingExecutable { .. } | Self::Spawn { .. } => {
                Some(UnavailableCause::Toolchain)
            }
            _ => None,
        }
    }

    /// True when an extractor failure's preserved stderr names an
    /// unresolvable module — the signal that a module-mode retry may be able
    /// to provision it from the fleet corpus.
    fn names_missing_module(&self) -> bool {
        matches!(self, Self::Command { stderr, .. } if stderr.contains("module not found: "))
    }
}

#[cfg(test)]
mod toolchain_tests {
    use std::path::PathBuf;

    use super::{HarnessError, JdkToolchain};

    #[test]
    fn owned_jdk_root_rejects_relative_path_before_executable_probe() {
        let error = JdkToolchain::from_owned_root(PathBuf::from("jdk"))
            .expect_err("relative JDK root must not enter host authority");
        assert!(matches!(
            error,
            HarnessError::RelativeJdkRoot { root } if root == PathBuf::from("jdk")
        ));
    }
}

#[cfg(test)]
mod plan_tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicUsize, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{JavaSource, module_plan};
    use crate::legacy::JavaRelease;

    struct TempDir(PathBuf);

    static NEXT_TEMP: AtomicUsize = AtomicUsize::new(0);

    impl TempDir {
        fn new(label: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let ordinal = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "nudox-java-plan-{label}-{}-{ordinal}-{nanos}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create unique temporary directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            if !std::thread::panicking() {
                fs::remove_dir_all(&self.0).expect("remove temporary directory");
            }
        }
    }

    fn source(name: &'static Path, bytes: &'static [u8]) -> JavaSource<'static> {
        JavaSource { name, bytes }
    }

    #[test]
    fn non_modular_closures_never_plan_module_mode() {
        let corpus = TempDir::new("plain");
        fs::write(
            corpus.path().join("unrelated"),
            b"module org.corpus.mod {\n}\n",
        )
        .expect("seed corpus marker");
        let plan = module_plan(
            JavaRelease::Java21,
            &[source(Path::new("demo/App.java"), b"package demo;\n")],
            &[],
            Some(corpus.path()),
            corpus.path(),
        )
        .expect("plan without modules");
        assert!(plan.is_none(), "unnamed closures keep the default path");
    }

    #[test]
    fn release8_never_plans_module_mode_even_when_modular() {
        let corpus = TempDir::new("release8");
        let module_info = b"module org.target {\n}\n";
        let plan = module_plan(
            JavaRelease::Java8,
            &[
                source(Path::new("demo/App.java"), b"package demo;\n"),
                source(Path::new("module-info.java"), module_info),
            ],
            &[],
            Some(corpus.path()),
            corpus.path(),
        )
        .expect("plan for release 8");
        assert!(plan.is_none(), "release 8 predates the module system");
    }

    #[test]
    fn staged_module_plans_target_first_then_corpus_siblings_deduplicated() {
        let corpus = TempDir::new("siblings");
        let sibling = corpus.path().join("org/testlib/api/1.0.0");
        fs::create_dir_all(&sibling).expect("seed sibling root");
        fs::write(
            sibling.join("module-info.java"),
            b"module org.testlib.api {\n\texports org.testlib.api;\n}\n",
        )
        .expect("seed sibling declaration");
        let other = corpus.path().join("org/testlib/api/2.0.0");
        fs::create_dir_all(&other).expect("seed duplicate root");
        fs::write(
            other.join("module-info.java"),
            b"module org.testlib.api {\n\texports org.testlib.api;\n}\n",
        )
        .expect("seed duplicate declaration");
        let module_info =
            b"/* module fake.decoy */\nmodule org.target {\n\trequires org.testlib.api;\n}\n";
        let plan = module_plan(
            JavaRelease::Java21,
            &[
                source(Path::new("demo/App.java"), b"package demo;\n"),
                source(Path::new("module-info.java"), module_info),
            ],
            &[],
            Some(corpus.path()),
            corpus.path(),
        )
        .expect("plan for a modular closure")
        .expect("modular closures plan module mode");
        assert_eq!(
            plan,
            vec![
                (String::from("org.target"), corpus.path().to_path_buf()),
                (
                    String::from("org.testlib.api"),
                    fs::canonicalize(&other).unwrap()
                ),
            ],
            "target first, corpus siblings after, duplicate module names pruned to the newest version"
        );
    }

    #[test]
    fn corpus_modules_matching_the_target_name_never_shadow_the_staged_copy() {
        let corpus = TempDir::new("shadow");
        let root = corpus.path().join("org/target/target/1.0.0");
        fs::create_dir_all(&root).expect("seed target-named corpus root");
        fs::write(root.join("module-info.java"), b"module org.target {\n}\n")
            .expect("seed target-named declaration");
        let module_info = b"module org.target {\n}\n";
        let plan = module_plan(
            JavaRelease::Java21,
            &[source(Path::new("module-info.java"), module_info)],
            &[],
            Some(corpus.path()),
            corpus.path(),
        )
        .expect("plan")
        .expect("modular closures plan module mode");
        assert_eq!(
            plan,
            vec![(String::from("org.target"), corpus.path().to_path_buf())],
            "the staged run directory is the only org.target entry"
        );
    }

    #[test]
    fn modular_caller_root_engages_module_mode_without_staged_declaration() {
        let caller = TempDir::new("caller");
        let target_root = caller.path().join("org/target/target/1.0.0");
        fs::create_dir_all(&target_root).expect("seed modular caller root");
        fs::write(
            target_root.join("module-info.java"),
            b"module org.target {\n}\n",
        )
        .expect("seed caller declaration");
        let corpus = TempDir::new("caller-corpus");
        let plan = module_plan(
            JavaRelease::Java21,
            &[source(Path::new("demo/App.java"), b"package demo;\n")],
            &[fs::canonicalize(&target_root).unwrap()],
            Some(corpus.path()),
            corpus.path(),
        )
        .expect("plan");
        assert!(
            plan.is_none(),
            "with no nameable target and no corpus modules there is nothing to map, so the default path stays: {plan:?}"
        );
    }
}
