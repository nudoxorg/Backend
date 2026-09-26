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

mod run;

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

/// `Some` when every `error:` line is a missing package or a symbol that
/// follows from one. `None` when there is no missing package, or when any
/// other `error:` is present.
fn unresolved_dependency_packages(stderr: &str) -> Option<String> {
    let mut packages = Vec::new();
    for line in stderr.lines() {
        let Some((_, rest)) = line.split_once("error:") else {
            continue;
        };
        let rest = rest.trim();
        if let Some(name) = rest
            .strip_prefix("package ")
            .and_then(|suffix| suffix.strip_suffix(" does not exist"))
        {
            packages.push(name.trim().to_owned());
            continue;
        }
        if rest.starts_with("cannot find symbol") {
            continue;
        }
        return None;
    }
    if packages.is_empty() {
        return None;
    }
    packages.sort();
    packages.dedup();
    Some(packages.join(", "))
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
    /// `javac` exited because named packages are not on the source or class
    /// path. The compiler was not relaxed: the same invocation still fails.
    #[error(
        "javac could not resolve dependency package(s) {packages}; the compiler was \
         not weakened (exit {exit_code}, {command}). Put the missing jars on \
         NUDOX_JAVA_CLASS_PATH. {stderr}"
    )]
    UnresolvedDependencies {
        /// Packages named by `error: package … does not exist`, sorted and joined.
        packages: String,
        /// `javac`'s exit status.
        exit_code: String,
        /// The command label `run_command` reported.
        command: String,
        /// The full diagnostic, so the missing types stay readable.
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
mod dependency_tests {
    use super::unresolved_dependency_packages;

    #[test]
    fn a_missing_package_is_an_unresolved_dependency() {
        let stderr = "\
App.java:1: error: package com.google.common.base does not exist
import com.google.common.base.Preconditions;
App.java:4: error: cannot find symbol
        Preconditions.checkNotNull(name);
  symbol:   variable Preconditions
1 error
";
        assert_eq!(
            unresolved_dependency_packages(stderr).as_deref(),
            Some("com.google.common.base")
        );
    }

    #[test]
    fn a_language_error_stays_a_compiler_failure_even_beside_a_missing_package() {
        let stderr = "\
App.java:3: error: records are not supported in -source 8
App.java:1: error: package com.google.common.base does not exist
";
        assert!(unresolved_dependency_packages(stderr).is_none());
    }

    #[test]
    fn a_bare_cannot_find_symbol_is_not_relabeled_as_a_missing_dependency() {
        let stderr = "App.java:2: error: cannot find symbol\n  symbol: class Typo\n";
        assert!(unresolved_dependency_packages(stderr).is_none());
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
