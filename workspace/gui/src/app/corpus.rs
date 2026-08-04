//! Which corpus lindsey opens on (GUI-LOCAL-PLAN §L10).
//!
//! # Why this is a parsed value and not four `env::var` calls in `main`
//!
//! The choice of corpus is the first branch the process takes, and it is the
//! one most likely to be made wrong: a mistyped variable name, a path that no
//! longer exists, a `NUDOX_CORPUS=packages` where `package` was meant. Every
//! one of those failures, handled casually, degrades into *the same symptom* —
//! an app that opens with an empty index and looks like search is broken.
//! That symptom is expensive because it points at the wrong subsystem.
//!
//! So selection is a total function from the environment to a [`CorpusChoice`]
//! or a [`CorpusError`], and every ambiguity is resolved loudly. A typo is an
//! error, not a silent fall back to fixtures.
//!
//! # The pure core
//!
//! [`select`] takes its inputs — variable lookup and a directory probe — as
//! closures, so the whole decision table is testable without touching the
//! process environment or the filesystem. [`from_env`] is the thin shell that
//! supplies the real ones. This split is what lets the tests below enumerate
//! the cases rather than assert one happy path.

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Variable names
// ---------------------------------------------------------------------------

/// Selects the corpus kind: `fixtures` (default) or `package`.
pub const ENV_CORPUS: &str = "NUDOX_CORPUS";
/// Filesystem root of a real package to run the producer over.
pub const ENV_PACKAGE_ROOT: &str = "NUDOX_PACKAGE_ROOT";
/// Optional display name hint for that package.
pub const ENV_PACKAGE_NAME: &str = "NUDOX_PACKAGE_NAME";
/// Optional version hint for that package.
pub const ENV_PACKAGE_VERSION: &str = "NUDOX_PACKAGE_VERSION";

// ---------------------------------------------------------------------------
// Choice
// ---------------------------------------------------------------------------

/// A real package to lower with the live producer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageChoice {
    /// Directory containing the package's `Cargo.toml`.
    pub root: PathBuf,
    /// Display-name *hint*.
    ///
    /// The authoritative name comes from the oracle — the producer reads it
    /// from cargo metadata, which is why an empty name there broke every
    /// registry-produced package until it was fixed. This is only what the UI
    /// shows before the first `Ready` event arrives, so a hyphen/underscore
    /// mismatch here is cosmetic and transient rather than a correctness bug.
    pub name: String,
    /// Version hint, same caveat as [`PackageChoice::name`].
    pub version: String,
}

/// What lindsey should load at startup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CorpusChoice {
    /// The built-in fixture corpus — the default, and what the tests use.
    Fixtures,
    /// One real package, lowered by the live producer.
    Package(PackageChoice),
}

/// Why corpus selection could not be resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CorpusError {
    /// `NUDOX_CORPUS` held something other than `fixtures` or `package`.
    UnknownKind { value: String },
    /// `NUDOX_CORPUS=package` without a `NUDOX_PACKAGE_ROOT` to point at.
    MissingRoot,
    /// The root was given but is not a directory we can read.
    RootNotADirectory { root: PathBuf },
}

impl std::fmt::Display for CorpusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CorpusError::UnknownKind { value } => write!(
                f,
                "{ENV_CORPUS}={value:?} is not a corpus kind; expected \"fixtures\" or \"package\"",
            ),
            CorpusError::MissingRoot => write!(
                f,
                "{ENV_CORPUS}=package requires {ENV_PACKAGE_ROOT} to point at a package directory",
            ),
            CorpusError::RootNotADirectory { root } => write!(
                f,
                "{ENV_PACKAGE_ROOT}={} is not a readable directory",
                root.display(),
            ),
        }
    }
}

impl std::error::Error for CorpusError {}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

/// Resolve the corpus choice from the process environment.
///
/// Existence of the package root is checked here rather than deferred to the
/// producer: a wrong path caught at startup names itself, whereas the same
/// path reaching the producer surfaces as a load failure in the status bar,
/// several seconds later, next to a window that already looks empty.
pub fn from_env() -> Result<CorpusChoice, CorpusError> {
    select(
        |key| std::env::var(key).ok().filter(|v| !v.trim().is_empty()),
        |path| path.is_dir(),
    )
}

/// The pure decision table behind [`from_env`].
///
/// `lookup` reads a variable (returning `None` for unset *or* blank — an
/// exported-but-empty variable is how shell scripts spell "unset", and treating
/// it as a value would produce a package rooted at `""`). `is_dir` probes the
/// filesystem.
pub fn select(
    lookup: impl Fn(&str) -> Option<String>,
    is_dir: impl Fn(&Path) -> bool,
) -> Result<CorpusChoice, CorpusError> {
    let root = lookup(ENV_PACKAGE_ROOT).map(PathBuf::from);

    // A bare `NUDOX_PACKAGE_ROOT` is unambiguous intent, so it implies the
    // kind. Requiring both variables would mean the most common invocation —
    // "point lindsey at this checkout" — fails with a message about a second
    // variable the user had no reason to know about.
    let kind = match lookup(ENV_CORPUS) {
        Some(v) => v,
        None if root.is_some() => "package".to_owned(),
        None => return Ok(CorpusChoice::Fixtures),
    };

    match kind.trim().to_ascii_lowercase().as_str() {
        "fixtures" => Ok(CorpusChoice::Fixtures),
        "package" => {
            let root = root.ok_or(CorpusError::MissingRoot)?;
            if !is_dir(&root) {
                return Err(CorpusError::RootNotADirectory { root });
            }
            let name = lookup(ENV_PACKAGE_NAME).unwrap_or_else(|| default_name(&root));
            let version =
                lookup(ENV_PACKAGE_VERSION).unwrap_or_else(|| "0.0.0".to_owned());
            Ok(CorpusChoice::Package(PackageChoice {
                root,
                name,
                version,
            }))
        }
        _ => Err(CorpusError::UnknownKind { value: kind }),
    }
}

/// Best-effort display name for a package root: its last path component.
///
/// Falls back to `"package"` rather than an empty string, because the empty
/// name is precisely the value that made `documented_package_names` return
/// nothing and silently disabled the entire production path once already.
fn default_name(root: &Path) -> String {
    root.file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .unwrap_or("package")
        .to_owned()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |key| map.get(key).cloned().filter(|v| !v.trim().is_empty())
    }

    fn any_dir(_: &Path) -> bool {
        true
    }

    fn no_dir(_: &Path) -> bool {
        false
    }

    #[test]
    fn empty_environment_is_fixtures() {
        assert_eq!(
            select(env(&[]), any_dir).unwrap(),
            CorpusChoice::Fixtures,
        );
    }

    #[test]
    fn explicit_fixtures_is_fixtures() {
        assert_eq!(
            select(env(&[(ENV_CORPUS, "fixtures")]), any_dir).unwrap(),
            CorpusChoice::Fixtures,
        );
    }

    #[test]
    fn a_bare_root_implies_the_package_kind() {
        let choice = select(env(&[(ENV_PACKAGE_ROOT, "/tmp/axum")]), any_dir).unwrap();
        assert_eq!(
            choice,
            CorpusChoice::Package(PackageChoice {
                root: PathBuf::from("/tmp/axum"),
                name: "axum".to_owned(),
                version: "0.0.0".to_owned(),
            }),
        );
    }

    #[test]
    fn name_and_version_override_the_defaults() {
        let choice = select(
            env(&[
                (ENV_PACKAGE_ROOT, "/tmp/checkout"),
                (ENV_PACKAGE_NAME, "axum"),
                (ENV_PACKAGE_VERSION, "0.8.9"),
            ]),
            any_dir,
        )
        .unwrap();
        let CorpusChoice::Package(p) = choice else {
            panic!("expected a package choice");
        };
        assert_eq!(p.name, "axum");
        assert_eq!(p.version, "0.8.9");
    }

    #[test]
    fn package_without_a_root_is_an_error() {
        assert_eq!(
            select(env(&[(ENV_CORPUS, "package")]), any_dir).unwrap_err(),
            CorpusError::MissingRoot,
        );
    }

    #[test]
    fn a_missing_root_is_caught_at_selection_time() {
        let err = select(env(&[(ENV_PACKAGE_ROOT, "/nope")]), no_dir).unwrap_err();
        assert_eq!(
            err,
            CorpusError::RootNotADirectory {
                root: PathBuf::from("/nope"),
            },
        );
    }

    /// The point of the whole module: a typo must not degrade into fixtures.
    #[test]
    fn an_unknown_kind_is_loud() {
        let err = select(env(&[(ENV_CORPUS, "packages")]), any_dir).unwrap_err();
        assert_eq!(
            err,
            CorpusError::UnknownKind {
                value: "packages".to_owned(),
            },
        );
        assert!(err.to_string().contains("expected"));
    }

    #[test]
    fn kind_parsing_tolerates_case_and_padding() {
        assert_eq!(
            select(env(&[(ENV_CORPUS, "  Fixtures ")]), any_dir).unwrap(),
            CorpusChoice::Fixtures,
        );
    }

    /// An exported-but-blank variable is how shells spell "unset".
    #[test]
    fn blank_variables_are_treated_as_unset() {
        assert_eq!(
            select(env(&[(ENV_CORPUS, ""), (ENV_PACKAGE_ROOT, "")]), any_dir).unwrap(),
            CorpusChoice::Fixtures,
        );
    }

    #[test]
    fn a_root_with_a_trailing_slash_still_yields_a_name() {
        let choice = select(env(&[(ENV_PACKAGE_ROOT, "/tmp/axum/")]), any_dir).unwrap();
        let CorpusChoice::Package(p) = choice else {
            panic!("expected a package choice");
        };
        assert_eq!(p.name, "axum");
    }
}
