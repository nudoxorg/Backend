//! Oracle subprocess helper.
//!
//! Three of the seven language producers (Go, C#, Java) are structured as
//! external toolchain processes that write a JSON document to stdout.  This
//! module provides the single shared helper they all use: run a command,
//! capture stdout/stderr, treat a non-zero exit as a typed error, and
//! deserialize stdout as JSON into `T`.
//!
//! # Usage pattern
//!
//! ```no_run
//! use nudox_languages::oracle;
//! use serde::Deserialize;
//!
//! #[derive(Deserialize)]
//! struct GoOutput { /* … */ }
//!
//! let output: GoOutput = oracle::run_json("go-oracle/1", "go", &["run", "./oracle", "/pkg/root"])?;
//! # Ok::<(), nudox_languages::ProducerError>(())
//! ```
//!
//! # Memory
//!
//! `run_json` captures the full stdout into a `Vec<u8>` (one allocation),
//! then hands the slice to `serde_json::from_slice` so the deserialized
//! types can borrow `&str` fields from the heap buffer if needed.  The
//! `Vec<u8>` is dropped after deserialization; the returned `T` is the only
//! survivor.

use std::{
    ffi::OsStr,
    process::{Command, Stdio},
};

use nudox_ir::body::Language;
use serde::de::DeserializeOwned;

use crate::ProducerError;

/// How a language's oracle is reached at runtime.
///
/// Rust/TypeScript/Python run in-process (no `NUDOX_*_ORACLE_*` binary).
/// Go/Java/C# spawn a helper whose override variable is the one a packaged
/// `lindsey.app` must set — cargo-bundle does not, which is why
/// `nix/lindsey-bundle.nix` wraps it. C/C++ `dlopen`s libclang.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleKind {
    /// rust-analyzer / pyrefly / OXC: no extra binary to ship.
    InProcess,
    /// Spawned helper. `override_env` is the variable the producer actually
    /// reads — a wrapper that sets a different name is a silent miss.
    Subprocess {
        /// Absolute-path override for the helper (`NUDOX_GO_ORACLE_BIN`, …).
        override_env: &'static str,
    },
    /// libclang via `dlopen`. `override_env` selects the library directory.
    NativeLibrary {
        /// Directory containing `libclang` (`LIBCLANG_PATH`).
        override_env: &'static str,
    },
}

/// Runtime oracle for `language`.
///
/// Exhaustive on [`Language`] so a new language cannot ship without saying
/// whether the bundle has to wrap a helper for it.
pub fn oracle_kind(language: Language) -> OracleKind {
    match language {
        Language::Go => OracleKind::Subprocess {
            override_env: crate::go::producer::ORACLE_BIN_ENV,
        },
        Language::Java => OracleKind::Subprocess {
            override_env: crate::java::invoke::ORACLE_CLASSES_ENV,
        },
        Language::CSharp => OracleKind::Subprocess {
            override_env: crate::csharp::producer::ORACLE_PATH_ENV,
        },
        Language::C | Language::Cpp => OracleKind::NativeLibrary {
            override_env: "LIBCLANG_PATH",
        },
        Language::Rust
        | Language::TypeScript
        | Language::Python
        | Language::Nix
        | Language::Other => OracleKind::InProcess,
    }
}

/// Run an oracle command, capture stdout/stderr, and deserialize stdout as
/// JSON.
///
/// * `producer_label` — the [`ProducerId`] string; embedded in error messages
///   so a failure trace names the producer.
/// * `program` — the binary to run.
/// * `args` — the arguments; each element is passed as a separate `arg`.
///
/// On success returns `T` deserialized from the command's stdout.
///
/// # Errors
///
/// * [`ProducerError::OracleSpawn`] — the process could not be started.
/// * [`ProducerError::OracleExit`] — the process exited with a non-zero status.
///   The captured stderr is included so the caller can log it.
/// * [`ProducerError::Decode`] — the process succeeded but stdout could not be
///   deserialized as `T`.
///
/// [`ProducerId`]: crate::ProducerId
pub fn run_json<T, P, A, I>(producer_label: &str, program: P, args: I) -> Result<T, ProducerError>
where
    T: DeserializeOwned,
    P: AsRef<OsStr>,
    A: AsRef<OsStr>,
    I: IntoIterator<Item = A>,
{
    run_json_with_override(producer_label, program, args, None)
}

/// [`run_json`], plus the name of the environment variable that points at this
/// oracle's binary.
///
/// # Why the variable name has to travel with the call
///
/// Every one of these producers looks its binary up by bare name on `PATH`
/// (`nudox-go-oracle`, `dotnet`, `javadoc`) with an environment variable as the
/// override. When the binary is simply absent — which is the normal state of a
/// packaged application that does not ship it — the spawn error is the only
/// thing the user ever sees, and a message that does not name the override
/// leaves them with no next step. This helper cannot derive the variable name;
/// only the caller knows it, so it is passed rather than guessed.
pub fn run_json_with_override<T, P, A, I>(
    producer_label: &str,
    program: P,
    args: I,
    override_env: Option<&str>,
) -> Result<T, ProducerError>
where
    T: DeserializeOwned,
    P: AsRef<OsStr>,
    A: AsRef<OsStr>,
    I: IntoIterator<Item = A>,
{
    let program_ref = program.as_ref();

    // Build the command label for diagnostics before consuming the iterator.
    let label = format!("{} {}", producer_label, program_ref.to_string_lossy());

    let output = Command::new(program_ref)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| ProducerError::OracleSpawn {
            // The spawn label carries the fix as well as the name. A failed
            // spawn is overwhelmingly "this host does not have the binary",
            // and that reader needs the override variable more than they need
            // anything else in the error.
            command: spawn_label(&label, override_env),
            reason: e,
        })?;

    if !output.status.success() {
        let code = output
            .status
            .code()
            .map_or_else(|| "signal".to_owned(), |c| c.to_string());
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(ProducerError::OracleExit {
            command: label,
            code,
            stderr,
        });
    }

    serde_json::from_slice::<T>(&output.stdout).map_err(|e| ProducerError::Decode {
        // `label` is still available here (not moved into OracleExit above
        // because that branch returns early).
        package: label,
        reason: e,
    })
}

/// The command label for a *spawn* failure: what was run, plus what to do.
///
/// Only the spawn path gets the extra sentence. A non-zero exit means the
/// binary was found and had its own complaint, which `OracleExit` already
/// carries in `stderr`; telling that reader how to point at a different binary
/// would send them to fix the one thing that is not wrong.
fn spawn_label(label: &str, override_env: Option<&str>) -> String {
    override_env.map_or_else(|| label.to_owned(), |var| {
        format!(
            "{label} (resolved against PATH; set {var} to its absolute path, \
             or add it to PATH)"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flatten an error the way `nudox-engine` does before it reaches a user.
    ///
    /// `store::source::producer` maps `OracleSpawn`/`OracleExit` to
    /// `Error::OracleFailed { detail: chain(&err) }`, and `chain` walks
    /// `Display` plus every `#[source]`. That flattened string is what
    /// `PackageLoadEvent::LoadFailed` carries and what lindsey shows. Anything
    /// not reachable from it is, from a user's point of view, not reported.
    fn rendered_chain(error: &ProducerError) -> String {
        let mut out = error.to_string();
        let mut source = std::error::Error::source(error);
        while let Some(link) = source {
            out.push_str(": ");
            out.push_str(&link.to_string());
            source = std::error::Error::source(link);
        }
        out
    }

    /// A missing oracle binary must name the binary in the message a user sees.
    ///
    /// # The incident this pins
    ///
    /// `lindsey.app`'s bundle does not ship `nudox-go-oracle`, so opening a Go
    /// package spawned a binary that was not on `PATH`. The failure reached the
    /// user as `oracle spawn failed: No such file or directory (os error 2)` —
    /// the `command` field naming the binary was present on the struct but
    /// absent from both the `Display` and the `#[source]` chain, so it was
    /// unreachable from every diagnostic surface. Diagnosing it took finding
    /// the binary in the repo by hand.
    ///
    /// `OracleExit`'s own doc comment already states the rule this violated:
    /// fields with no `#[source]` must be inlined into the message, because
    /// "the terse-`Display`-plus-walk-the-chain rule only works when a chain
    /// exists".
    #[test]
    fn a_missing_oracle_binary_names_the_binary() {
        let error = run_json_with_override::<serde_json::Value, _, _, _>(
            "go-oracle/2",
            "nudox-go-oracle-that-is-not-installed",
            ["/tmp"],
            Some(crate::go::producer::ORACLE_BIN_ENV),
        )
        .expect_err("a binary that does not exist cannot be spawned");

        let rendered = rendered_chain(&error);
        assert!(
            rendered.contains("nudox-go-oracle-that-is-not-installed"),
            "the user-visible message must name the binary that could not be \
             spawned, or the reader cannot tell which helper is missing: \
             {rendered}",
        );
    }

    /// ...and must name the environment variable that overrides the lookup.
    ///
    /// Naming the binary says *what* is missing; this says *what to do*. The
    /// binary is looked up on `PATH` by bare name, so a user whose bundle does
    /// not ship it has no way to guess that an override exists — which is
    /// exactly the step that had to be discovered by reading source.
    #[test]
    fn a_missing_oracle_binary_names_the_override_variable() {
        let error = run_json_with_override::<serde_json::Value, _, _, _>(
            "go-oracle/2",
            "nudox-go-oracle-that-is-not-installed",
            ["/tmp"],
            Some(crate::go::producer::ORACLE_BIN_ENV),
        )
        .expect_err("a binary that does not exist cannot be spawned");

        let rendered = rendered_chain(&error);
        assert!(
            rendered.contains("NUDOX_GO_ORACLE_BIN"),
            "the message must name the variable that points at the binary, or \
             the reader is told what broke and not how to fix it: {rendered}",
        );
    }

    /// A non-zero exit keeps naming its command, code and stderr.
    ///
    /// Guards the half that already worked, because the fix for the spawn path
    /// touches the same enum: `OracleExit` inlines these deliberately and a
    /// regression there is the same defect wearing a different variant.
    #[test]
    fn a_failing_oracle_reports_its_own_stderr() {
        let error = run_json::<serde_json::Value, _, _, _>(
            "go-oracle/2",
            "sh",
            ["-c", "echo the-oracles-own-message >&2; exit 3"],
        )
        .expect_err("a command exiting 3 is a failure");

        let rendered = rendered_chain(&error);
        assert!(
            rendered.contains("the-oracles-own-message"),
            "the oracle's stderr is the only place its actual complaint \
             appears: {rendered}",
        );
        assert!(
            rendered.contains('3'),
            "the exit status must survive: {rendered}",
        );
    }

    /// The subprocess languages are exactly the ones whose helpers a
    /// packaged app has to wrap. In-process languages (Rust/TS/Python) must
    /// not grow a `NUDOX_*_ORACLE_*` requirement without this table moving —
    /// that was the "set NUDOX_GO_ORACLE_BIN or Go indexing is dead" miss.
    #[test]
    fn subprocess_oracles_are_go_java_csharp() {
        use OracleKind::*;

        assert!(matches!(oracle_kind(Language::Go), Subprocess { .. }));
        assert!(matches!(oracle_kind(Language::Java), Subprocess { .. }));
        assert!(matches!(oracle_kind(Language::CSharp), Subprocess { .. }));
        assert!(matches!(oracle_kind(Language::Rust), InProcess));
        assert!(matches!(oracle_kind(Language::TypeScript), InProcess));
        assert!(matches!(oracle_kind(Language::Python), InProcess));
        assert!(matches!(oracle_kind(Language::C), NativeLibrary { .. }));
        assert!(matches!(oracle_kind(Language::Cpp), NativeLibrary { .. }));
    }

    #[test]
    fn subprocess_override_names_match_the_producer_lookups() {
        match oracle_kind(Language::Go) {
            OracleKind::Subprocess { override_env } => {
                assert_eq!(override_env, crate::go::producer::ORACLE_BIN_ENV);
            }
            other => panic!("Go must be a subprocess oracle, not {other:?}"),
        }
        match oracle_kind(Language::Java) {
            OracleKind::Subprocess { override_env } => {
                assert_eq!(override_env, crate::java::invoke::ORACLE_CLASSES_ENV);
            }
            other => panic!("Java must be a subprocess oracle, not {other:?}"),
        }
        match oracle_kind(Language::CSharp) {
            OracleKind::Subprocess { override_env } => {
                assert_eq!(override_env, crate::csharp::producer::ORACLE_PATH_ENV);
            }
            other => panic!("C# must be a subprocess oracle, not {other:?}"),
        }
    }
}
