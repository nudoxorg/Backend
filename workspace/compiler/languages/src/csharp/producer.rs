//! [`CSharpProducer`] — the Roslyn oracle wired to the shared [`Producer`]
//! contract.
//!
//! # Why the oracle is a subprocess
//!
//! Roslyn is a .NET library; there is no in-process Rust binding for it. The
//! oracle (`oracle/` beside this crate) is a small C# program that parses and
//! binds a package's sources and writes the JSON document [`schema`] mirrors.
//! This module is the bridge: locate the oracle, run it, hand the deserialized
//! document to [`lower`].
//!
//! [`schema`]: crate::csharp::schema
//! [`lower`]: crate::csharp::lower

use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
};

use crate::{PackageSource, Producer, ProducerError, ProducerId, oracle};
use nudox_ir::{body::Language, lower::Lowering};

use crate::csharp::schema::Extraction;

/// Overrides the path to the published oracle (`oracle.dll` or its directory).
pub const ORACLE_PATH_ENV: &str = "NUDOX_CSHARP_ORACLE";

/// Overrides the `dotnet` executable used to host the oracle.
pub const DOTNET_ENV: &str = "NUDOX_DOTNET";

/// The C# package producer: `dotnet oracle.dll` → [`Extraction`] → IR.
///
/// The two paths are immutable configuration rather than per-run state, which
/// is what the [`Producer`] contract asks for: one registered instance serves
/// every C# package, and the package identity arrives with each
/// [`PackageSource`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CSharpProducer {
    dotnet: PathBuf,
    oracle_dll: PathBuf,
}

impl CSharpProducer {
    /// Build a producer for an explicitly located oracle.
    ///
    /// `oracle` may be the published `oracle.dll` itself or the directory
    /// containing it; both spellings occur in practice (a build system hands
    /// over a publish directory, a developer points at the file).
    pub fn new(oracle: impl Into<PathBuf>) -> Self {
        Self {
            dotnet: PathBuf::from("dotnet"),
            oracle_dll: normalise_oracle_path(oracle.into()),
        }
    }

    /// Locate the oracle from the environment, falling back to the in-tree
    /// publish directory.
    ///
    /// This is infallible on purpose. A registry builds every producer up front,
    /// long before any package is loaded, so a missing toolchain must not stop
    /// the registry from being constructed — it has to surface later, on the
    /// package that actually needed it, where the error can name it. See
    /// [`Self::is_available`] for the pre-flight check.
    pub fn from_env() -> Self {
        let oracle =
            std::env::var_os(ORACLE_PATH_ENV).map_or_else(default_oracle_path, PathBuf::from);

        let dotnet =
            std::env::var_os(DOTNET_ENV).map_or_else(|| PathBuf::from("dotnet"), PathBuf::from);

        Self {
            dotnet,
            oracle_dll: normalise_oracle_path(oracle),
        }
    }

    /// Use a specific `dotnet` executable to host the oracle.
    #[must_use]
    pub fn with_dotnet(mut self, dotnet: impl Into<PathBuf>) -> Self {
        self.dotnet = dotnet.into();
        self
    }

    /// The published `oracle.dll` this producer will run.
    pub fn oracle_dll(&self) -> &Path {
        &self.oracle_dll
    }

    /// Whether the oracle has been built.
    ///
    /// Only answers for the managed assembly; whether a `dotnet` host exists on
    /// `PATH` is left to the spawn, which reports it with the command attached.
    pub fn is_available(&self) -> bool {
        self.oracle_dll.is_file()
    }
}

impl Default for CSharpProducer {
    fn default() -> Self {
        Self::from_env()
    }
}

impl Producer for CSharpProducer {
    /// The Roslyn `DocumentationCommentId` (`T:Ns.Type`, `M:Ns.Type.M(…)`).
    ///
    /// Kept as `String` rather than a newtype to match [`crate::csharp::lower`], which
    /// is the pre-existing authority on this crate's shape and is written
    /// against `Lowering<String>` throughout. The value is not arbitrary text:
    /// it is the id Roslyn itself mints, and it is the same string that appears
    /// in a `<see cref="…"/>`, which is what makes doc links resolve by
    /// construction.
    type Id = String;

    type Oracle = Extraction;

    const ID: ProducerId = ProducerId("csharp-roslyn/1");

    const LANGUAGE: Language = Language::CSharp;

    fn invoke(&self, src: &PackageSource) -> Result<Self::Oracle, ProducerError> {
        let command = self.command_label();

        if !self.is_available() {
            // Reported before the spawn so the message names the artefact that
            // is missing and how to produce it, rather than surfacing as a bare
            // "No such file or directory" from the host.
            return Err(ProducerError::OracleSpawn {
                command,
                reason: io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "C# oracle not built at {}; publish it with \
                         `dotnet publish -c Release --no-self-contained -o publish` \
                         in workspace/compiler/languages/oracle/csharp, or set {ORACLE_PATH_ENV}",
                        self.oracle_dll.display(),
                    ),
                ),
            });
        }

        let args: Vec<OsString> = vec![
            self.oracle_dll.clone().into_os_string(),
            OsString::from("--mode"),
            OsString::from("source"),
            OsString::from("--root"),
            src.root().as_os_str().to_os_string(),
            OsString::from("--assembly-name"),
            OsString::from(src.name.as_str()),
        ];

        let extraction: Extraction = oracle::run_json(Self::ID.0, &self.dotnet, args)?;

        // A binary older than this build under-reports silently: every field
        // is `#[serde(default)]`, so its missing `references` arrives as an
        // empty `Vec` and `refs` answers "nothing here" for the whole
        // language. Failing loudly is the only way that reads as a stale
        // oracle rather than an empty package — see
        // `schema::Extraction::staleness`, and `crate::go::producer::GoProducer`'s
        // identical wiring for the incident that made this worth doing before
        // it happens again in C#.
        if let Some(stale) = extraction.staleness() {
            return Err(ProducerError::OracleExit {
                command,
                code: "stale".to_owned(),
                stderr: stale.to_string(),
            });
        }

        if extraction.types.is_empty() {
            // An empty document is not a package with no API; it means the
            // oracle bound nothing, and lowering it would produce a package
            // whose emptiness is indistinguishable from a real one.
            return Err(unacceptable(
                src,
                format!(
                    "oracle extracted no types from {}; check that the root contains \
                     the package's .cs sources",
                    src.root().display(),
                ),
            ));
        }

        Ok(extraction)
    }

    fn lower(
        &self,
        oracle: &Self::Oracle,
        out: &mut Lowering<Self::Id>,
    ) -> Result<(), ProducerError> {
        crate::csharp::lower::lower_extraction(oracle, out);
        Ok(())
    }
}

impl CSharpProducer {
    /// The command label carried on spawn/exit errors.
    fn command_label(&self) -> String {
        format!("{} {}", self.dotnet.display(), self.oracle_dll.display())
    }
}

/// Reject a document that parsed but says something this producer cannot honour.
///
/// [`ProducerError`] has no variant for "structurally valid, semantically
/// unacceptable", so this reuses [`ProducerError::Decode`] — whose stated
/// meaning, the oracle's output could not be turned into the expected type, is
/// exactly the situation. The `serde_json::Error` is built through
/// [`serde::de::Error::custom`], which is that type's supported constructor, so
/// the message survives the `#[source]` chain intact.
fn unacceptable(src: &PackageSource, message: String) -> ProducerError {
    use serde::de::Error as _;

    ProducerError::Decode {
        package: src.name.as_str().to_owned(),
        reason: serde_json::Error::custom(message),
    }
}

/// Accept either the published `oracle.dll` or the directory holding it.
fn normalise_oracle_path(path: PathBuf) -> PathBuf {
    if path.is_dir() {
        path.join("oracle.dll")
    } else {
        path
    }
}

/// The publish directory produced by the oracle's own build.
///
/// `CARGO_MANIFEST_DIR` is this crate's root, and the oracle lives beside
/// `src/`, so the two stay together no matter where the workspace is checked
/// out.
fn default_oracle_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("oracle")
        .join("csharp")
        .join("publish")
        .join("oracle.dll")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory and the dll inside it must name the same oracle, because
    /// both spellings are handed to `--oracle`/`NUDOX_CSHARP_ORACLE` in practice.
    ///
    /// Uses a tempdir rather than the real publish directory so the test is
    /// hermetic (the publish output is gitignored and may not exist on disk).
    #[test]
    fn directory_and_dll_paths_resolve_to_the_same_oracle() {
        let dir = tempfile::tempdir().expect("create temp publish dir");
        let dir = dir.path().to_path_buf();

        let from_dir = CSharpProducer::new(&dir);
        let from_dll = CSharpProducer::new(dir.join("oracle.dll"));

        assert_eq!(
            from_dir.oracle_dll(),
            from_dll.oracle_dll(),
            "a publish directory must normalise to the oracle.dll inside it"
        );
    }

    /// A missing oracle must be reported as a spawn failure that names the
    /// path and the command to build it — not as a decode error or a panic.
    #[test]
    fn missing_oracle_reports_spawn_failure_naming_the_path() {
        let producer = CSharpProducer::new("/nonexistent/nudox/oracle.dll");
        let src = PackageSource::new("/nonexistent/pkg", "Some.Package", "1.0.0");

        let err = producer
            .invoke(&src)
            .expect_err("a missing oracle cannot produce an extraction");

        match err {
            ProducerError::OracleSpawn { command, reason } => {
                assert!(
                    command.contains("/nonexistent/nudox/oracle.dll"),
                    "the command label must name the oracle that is missing, got {command:?}"
                );
                assert_eq!(reason.kind(), io::ErrorKind::NotFound);
                assert!(
                    reason.to_string().contains("dotnet publish"),
                    "the error must say how to build the oracle, got {reason}"
                );
            }
            other => panic!("expected OracleSpawn, got {other:?}"),
        }
    }

    /// The producer identity is part of the cache key; an accidental rename
    /// would silently invalidate or, worse, alias stored IR.
    #[test]
    fn producer_identity_names_the_roslyn_tier() {
        assert_eq!(CSharpProducer::ID.0, "csharp-roslyn/1");
        assert_eq!(CSharpProducer::LANGUAGE, Language::CSharp);
    }
}
