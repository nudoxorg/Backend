//! Where each shared language sits on the chromatic plane, and its short tag.
//!
//! The language *set* is [`backend_present::Language`] — the same nine values
//! the CLI prints and the MCP tools report — so a window can never think a
//! file is TypeScript that `backend outline` calls `JavaScript`. What lives here
//! is the part a terminal has no use for: one hue per language and one
//! two-character tag, so a project's language mix reads as a proportion at a
//! glance rather than as a sentence.

use super::ramp::Hue;
use backend_present::Language;

/// Returns the spelled language name used in tooltips and legends.
pub(crate) const fn label(language: Language) -> &'static str {
    match language {
        Language::Rust => "Rust",
        Language::Python => "Python",
        Language::TypeScript => "TypeScript",
        Language::Go => "Go",
        Language::Java => "Java",
        Language::CSharp => "C#",
        Language::C => "C",
        Language::Cxx => "C++",
        Language::Unknown => "Other",
    }
}

/// Returns the hue this language occupies on the single chromatic plane.
///
/// C and C++ are deliberately close but not equal: one frontend claims both,
/// and a reader scanning a mixed tree should see that kinship without losing
/// the distinction.
pub(crate) fn hue(language: Language) -> Hue {
    Hue::degrees(match language {
        Language::Rust => 24.0,
        Language::Python => 52.0,
        Language::TypeScript => 216.0,
        Language::Go => 186.0,
        Language::Java => 2.0,
        Language::CSharp => 288.0,
        Language::C => 132.0,
        Language::Cxx => 118.0,
        Language::Unknown => 232.0,
    })
}

/// Returns the language a package-relative path implies.
pub(crate) fn of_path(path: &str) -> Language {
    path.rsplit_once('.')
        .map_or(Language::Unknown, |(_, extension)| {
            Language::from_extension(&extension.to_ascii_lowercase())
        })
}
