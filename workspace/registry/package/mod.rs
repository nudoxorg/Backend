pub mod coordinates;
pub mod resolution;
pub mod version;

pub use coordinates::{CoordinateError, Coordinates};
pub use resolution::State;
pub use version::PackageVersion;

use heart::{NameError, ecosystem::Language};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

/// A validated, ecosystem-normalized package name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PackageName {
    /// The ecosystem this name belongs to (part of its identity).
    pub ecosystem: Language,
    /// Our sanitized name.
    canonical: SmolStr,
    /// Their display name.
    original: SmolStr,
}

impl PackageName {
    pub fn new(ecosystem: Language, raw: impl Into<String>) -> Result<Self, NameError> {
        let original: String = raw.into();
        if original.is_empty() {
            return Err(NameError::Empty);
        }
        if original.len() > length_limit(ecosystem) {
            return Err(NameError::TooLong { ecosystem, raw: original });
        }
        let canonical = match ecosystem {
            Language::Rust => canonicalize_crate(&original),
            Language::Typescript => canonicalize_npm(&original),
            Language::Python => canonicalize_pep503(&original),
        }
        .ok_or_else(|| NameError::Invalid { ecosystem, raw: original.clone() })?;
        Ok(Self { ecosystem, canonical: canonical.into(), original: original.into() })
    }

    pub fn canonical(&self) -> &str { &self.canonical }
    pub fn original(&self) -> &str { &self.original }
    pub const fn ecosystem(&self) -> Language { self.ecosystem }
}

/// The per-ecosystem raw-name length ceiling (crates.io caps at 64; npm and
/// PyPI both live comfortably under npm's documented 214).
const fn length_limit(ecosystem: Language) -> usize {
    match ecosystem {
        Language::Rust => 64,
        Language::Typescript | Language::Python => 214,
    }
}

/// crates.io: ASCII alphanumerics plus `-`/`_`, starting alphanumeric. The
/// canonical (identity) form folds case and the `-`/`_` equivalence crates.io
/// itself enforces, so `serde_json` and `serde-json` are the same package.
fn canonicalize_crate(raw: &str) -> Option<String> {
    let valid = raw.starts_with(|c: char| c.is_ascii_alphanumeric())
        && raw.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    valid.then(|| raw.to_ascii_lowercase().replace('_', "-"))
}

/// npm: an optional single `@scope/` prefix, then a name; each segment is
/// ASCII alphanumerics plus `-`/`_`/`.`, not starting with `.`. The canonical
/// form is lowercase (npm treats names case-insensitively for identity).
fn canonicalize_npm(raw: &str) -> Option<String> {
    let segment_ok = |segment: &str| {
        !segment.is_empty()
            && !segment.starts_with('.')
            && segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    let valid = match raw.strip_prefix('@') {
        Some(scoped) => match scoped.split_once('/') {
            Some((scope, name)) => segment_ok(scope) && segment_ok(name) && !name.contains('/'),
            None => false,
        },
        None => segment_ok(raw) && !raw.contains('/'),
    };
    valid.then(|| raw.to_ascii_lowercase())
}

/// PyPI: PEP 508 name grammar (starts and ends alphanumeric, interior may add
/// `-`/`_`/`.`), canonicalized per PEP 503 — lowercase with every run of
/// separators collapsed to a single `-`.
fn canonicalize_pep503(raw: &str) -> Option<String> {
    let valid = raw.starts_with(|c: char| c.is_ascii_alphanumeric())
        && raw.ends_with(|c: char| c.is_ascii_alphanumeric())
        && raw.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    valid.then(|| {
        raw.to_ascii_lowercase()
            .split(['-', '_', '.'])
            .filter(|run| !run.is_empty())
            .collect::<Vec<_>>()
            .join("-")
    })
}
