pub mod coordinates;

pub use coordinates::{CoordinateError, Coordinates};

use crate::{NameError, ecosystem::Language};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

/// A validated, ecosystem-normalized package name.
///
/// The name *grammar* (parse/canonicalize) lives in the `ecosystem` spec crate,
/// which sits *above* `heart`. To keep `heart` the light shared vocabulary (and
/// off the `ecosystem`→`ir`→`registry` link path), this type owns only the
/// already-canonicalized data; construction that needs the per-ecosystem
/// grammar goes through `ecosystem::PackageNameExt::new` (which calls
/// [`PackageName::from_canonical`] after running the spec).
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
    /// Assemble a `PackageName` from an already-computed canonical form.
    ///
    /// The `ecosystem`-crate constructor (`PackageNameExt::new`) runs the
    /// per-ecosystem grammar (`parse_name` / `render_canonical`) and then calls
    /// this. `heart` itself performs only the grammar-free length check so it
    /// need not depend on the spec crate.
    pub fn from_canonical(
        ecosystem: Language,
        original: impl Into<String>,
        canonical: impl Into<String>,
    ) -> Self {
        Self {
            ecosystem,
            canonical: canonical.into().into(),
            original: original.into().into(),
        }
    }

    /// The grammar-free length check shared by every ecosystem's constructor.
    /// Returns the raw `original` on success so the caller can proceed to the
    /// (spec-driven) grammar validation.
    pub fn check_length(ecosystem: Language, original: &str) -> Result<(), NameError> {
        if original.is_empty() {
            return Err(NameError::NameEmpty);
        }
        let max = length_limit(ecosystem);
        if original.len() > max {
            return Err(NameError::NameTooLong {
                ecosystem,
                len: original.len(),
                max,
            });
        }
        Ok(())
    }

    pub fn canonical(&self) -> &str {
        &self.canonical
    }
    pub fn original(&self) -> &str {
        &self.original
    }
    pub const fn ecosystem(&self) -> Language {
        self.ecosystem
    }
}

/// The per-ecosystem raw-name length ceiling (crates.io caps at 64; npm and
/// PyPI both live comfortably under npm's documented 214).
pub const fn length_limit(ecosystem: Language) -> usize {
    match ecosystem {
        Language::Rust => 64,
        Language::Typescript | Language::Python => 214,
        Language::Go | Language::Java => 256,
        Language::CSharp => 256,
        Language::Nix => 256,
        // `cpp` names are repository slugs (`host/org/repo`) or scoped forms —
        // roomier than a bare package name; match Go/Java's 256 ceiling.
        Language::Cpp => 256,
    }
}

/// Returns whether the char is in the base allowed set for the ecosystem.
/// Grammar rules (start/end/leading-dot etc.) are enforced by `parse_name`.
pub fn is_valid_char_for(ecosystem: Language, c: char) -> bool {
    match ecosystem {
        Language::Rust => c.is_ascii_alphanumeric() || matches!(c, '-' | '_'),
        Language::Typescript | Language::Python => {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')
        }
        Language::Go | Language::Java => {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':')
        }
        Language::CSharp => c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'),
        Language::Nix => c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'),
        // `cpp` names may be full repository URLs / SCP forms, so the base set
        // admits URL punctuation (`:` `/` `@` `+` `~`); `parse_name` enforces
        // the real slug grammar via `normalize_repo_url`.
        Language::Cpp => {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '@' | '+' | '~')
        }
    }
}
