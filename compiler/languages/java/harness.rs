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
}

impl JdkToolchain<'static> {
    /// Reads and validates the pinned JDK named by `NUDOX_JDK`.
    pub fn from_env() -> Result<Self, HarnessError> {
        let root = env::var_os("NUDOX_JDK").ok_or(HarnessError::MissingEnvironment {
            variable: "NUDOX_JDK",
        })?;
        let root = PathBuf::from(root);
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
    pub release: crate::JavaRelease,
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

    /// Writes borrowed sources, runs the extractor, and replaces `output` with its image.
    pub fn image(
        &self,
        toolchain: &JdkToolchain<'_>,
        request: HarnessRequest<'_>,
        output: &mut Vec<u8>,
    ) -> Result<(), HarnessError> {
        let first = request.sources.first().ok_or(HarnessError::NoSources)?;
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
        let mut command = Command::new(toolchain.executable("java"));
        command
            .args(["--add-modules", "jdk.compiler,jdk.javadoc", "-cp"])
            .arg(classpath)
            .arg("nudox.oracle.CompilerExtractor")
            .args(["--release", release_text(request.release), "--outfile"])
            .arg(&image)
            .arg("--source-binding")
            .arg(run.join(first.name));
        for source in request.sources {
            command.arg(run.join(source.name));
        }
        let stderr = run.join("extract.stderr");
        run_command(command, "java", &stderr)?;
        output.clear();
        File::open(&image)
            .map_err(|source| HarnessError::Io {
                path: image.clone(),
                source,
            })?
            .read_to_end(output)
            .map_err(|source| HarnessError::Io {
                path: image,
                source,
            })?;
        fs::remove_dir_all(&run).map_err(|source| HarnessError::Io { path: run, source })
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.root) {
            self.cleanup_error = Some(error);
        }
    }
}

fn source_digest() -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(AUTHORITY_SOURCE.as_bytes());
    digest.update(EXTRACTOR_SOURCE.as_bytes());
    digest.finalize().into()
}

fn release_text(release: crate::JavaRelease) -> &'static str {
    match release {
        crate::JavaRelease::Java8 => "8",
        crate::JavaRelease::Java11 => "11",
        crate::JavaRelease::Java17 => "17",
        crate::JavaRelease::Java21 => "21",
        crate::JavaRelease::Java25 => "25",
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
    let file = File::create(stderr_path).map_err(|source| HarnessError::Io {
        path: stderr_path.to_owned(),
        source,
    })?;
    let status = command
        .stdout(Stdio::null())
        .stderr(Stdio::from(file))
        .status()
        .map_err(|source| HarnessError::Spawn {
            command: command_text.clone(),
            source,
        })?;
    if status.success() {
        return Ok(());
    }
    let mut excerpt = Vec::new();
    File::open(stderr_path)
        .map_err(|source| HarnessError::Io {
            path: stderr_path.to_owned(),
            source,
        })?
        .take(STDERR_LIMIT)
        .read_to_end(&mut excerpt)
        .map_err(|source| HarnessError::Io {
            path: stderr_path.to_owned(),
            source,
        })?;
    Err(HarnessError::Command {
        command: command_text,
        status,
        stderr: String::from_utf8_lossy(&excerpt).into_owned(),
    })
}

/// Failures retain the command and bounded compiler diagnostics where applicable.
#[derive(Debug, Error)]
pub enum HarnessError {
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
