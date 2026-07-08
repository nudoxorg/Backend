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
            return Err(NameError::NameEmpty);
        }
        let max = length_limit(ecosystem);
        if original.len() > max {
            return Err(NameError::NameTooLong { ecosystem, len: original.len(), max });
        }
        let canonical = match ecosystem {
            Language::Rust => canonicalize_crate(&original),
            Language::Typescript => canonicalize_npm(&original),
            Language::Python => canonicalize_pep503(&original),
            Language::Go => canonicalize_go_module(&original),
            Language::Java => canonicalize_maven_artifact(&original),
        }
        .ok_or_else(|| {
            // Compute the concrete invalid chars for richer error (no raw stored in error).
            let bad: Vec<char> = original
                .chars()
                .filter(|&c| !is_valid_char_for(ecosystem, c))
                .collect();
            if bad.is_empty() {
                // Fallback: e.g. leading . or / or other grammar rule broken; report a sentinel
                NameError::NameHasInvalidChars { ecosystem, invalid_chars: vec!['?'] }
            } else {
                NameError::NameHasInvalidChars { ecosystem, invalid_chars: bad }
            }
        })?;
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
        Language::Go | Language::Java => 256, // generous for module/artifact
    }
}

/// Returns whether the char is in the base allowed set for the ecosystem
/// (grammar rules like start/end/leading-dot are still enforced by canonicalize).
fn is_valid_char_for(ecosystem: Language, c: char) -> bool {
    match ecosystem {
        Language::Rust => c.is_ascii_alphanumeric() || matches!(c, '-' | '_'),
        Language::Typescript | Language::Python => {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')
        }
        Language::Go | Language::Java => {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/')
        }
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

/// Go modules: keep as-is if they look like valid module paths (no / leading etc), lower for canonical? Go modules are case-sensitive but for id we can keep original for now.
fn canonicalize_go_module(raw: &str) -> Option<String> {
    if raw.is_empty() || raw.starts_with('/') || raw.contains("..") {
        return None;
    }
    // simple: accept most, use as lower for safety? but Go prefers original, use as provided if valid chars.
    let valid = raw.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '~' | '+' | ':'));
    valid.then(|| raw.to_string())
}

/// Java/Maven: artifactId similar to npm, group etc.
fn canonicalize_maven_artifact(raw: &str) -> Option<String> {
    let valid = raw.starts_with(|c: char| c.is_ascii_alphanumeric())
        && raw.ends_with(|c: char| c.is_ascii_alphanumeric())
        && raw.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    valid.then(|| raw.to_ascii_lowercase())
}
