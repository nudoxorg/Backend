pub mod coordinates;

pub use coordinates::{CoordinateError, Coordinates};

use crate::{NameError, ecosystem::Language};
use ecosystem::LanguageExt as _;
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
        let structured = ecosystem.spec().parse_name(&original).ok_or_else(|| {
            let bad: Vec<char> = original
                .chars()
                .filter(|&c| !is_valid_char_for(ecosystem, c))
                .collect();
            if bad.is_empty() {
                NameError::NameHasInvalidChars { ecosystem, invalid_chars: vec!['?'] }
            } else {
                NameError::NameHasInvalidChars { ecosystem, invalid_chars: bad }
            }
        })?;
        let canonical = ecosystem.spec().render_canonical(&structured);
        Ok(Self { ecosystem, canonical: canonical.into(), original: original.into() })
    }

    pub fn canonical(&self) -> &str { &self.canonical }
    pub fn original(&self) -> &str { &self.original }
    pub const fn ecosystem(&self) -> Language { self.ecosystem }

    /// Decompose this name into its structured form. Re-parses on demand;
    /// valid by construction (the name passed `parse_name` at creation time).
    pub fn structured(&self) -> ecosystem::name::StructuredName {
        self.ecosystem
            .spec()
            .parse_name(&self.original)
            .expect("PackageName::original is always a valid name for its ecosystem")
    }
}

/// The per-ecosystem raw-name length ceiling (crates.io caps at 64; npm and
/// PyPI both live comfortably under npm's documented 214).
const fn length_limit(ecosystem: Language) -> usize {
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
fn is_valid_char_for(ecosystem: Language, c: char) -> bool {
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
