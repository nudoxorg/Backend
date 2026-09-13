//! The seven supported source languages, as a two-letter tag and a hue.
//!
//! A project's language mix is one of the few facts about it that is true at a
//! glance, so the shelf shows it as a hue-coded bar rather than as a sentence.
//! The tag is what the tooltip and the narrow rail fall back to.

use super::ramp::Hue;

/// One supported source language.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Language {
    /// Rust.
    Rust,
    /// Python.
    Python,
    /// TypeScript and JavaScript.
    TypeScript,
    /// Go.
    Go,
    /// Java.
    Java,
    /// C#.
    CSharp,
    /// C and C++.
    Clang,
    /// A source file this build does not compile.
    Other,
}

impl Language {
    /// Every language in display order.
    pub(crate) const ALL: [Self; 8] = [
        Self::Rust,
        Self::Python,
        Self::TypeScript,
        Self::Go,
        Self::Java,
        Self::CSharp,
        Self::Clang,
        Self::Other,
    ];

    /// Returns the two-character tag drawn in the glyph tile.
    pub(crate) const fn tag(self) -> &'static str {
        match self {
            Self::Rust => "rs",
            Self::Python => "py",
            Self::TypeScript => "ts",
            Self::Go => "go",
            Self::Java => "jv",
            Self::CSharp => "c#",
            Self::Clang => "c+",
            Self::Other => "··",
        }
    }

    /// Returns the spelled language name used in tooltips.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::Python => "Python",
            Self::TypeScript => "TypeScript",
            Self::Go => "Go",
            Self::Java => "Java",
            Self::CSharp => "C#",
            Self::Clang => "C and C++",
            Self::Other => "Other",
        }
    }

    /// Returns the hue this language occupies on the chromatic plane.
    pub(crate) fn hue(self) -> Hue {
        Hue::degrees(match self {
            Self::Rust => 24.0,
            Self::Python => 52.0,
            Self::TypeScript => 216.0,
            Self::Go => 186.0,
            Self::Java => 2.0,
            Self::CSharp => 288.0,
            Self::Clang => 132.0,
            Self::Other => 232.0,
        })
    }

    /// Classifies a source path by its extension.
    pub(crate) fn of_path(path: &str) -> Self {
        let extension = path.rsplit('.').next().unwrap_or_default();
        match extension {
            "rs" => Self::Rust,
            "py" | "pyi" => Self::Python,
            "ts" | "tsx" | "js" | "jsx" | "mts" | "cts" | "mjs" | "cjs" => Self::TypeScript,
            "go" => Self::Go,
            "java" => Self::Java,
            "cs" => Self::CSharp,
            "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hxx" | "m" | "mm" => Self::Clang,
            _ => Self::Other,
        }
    }
}
