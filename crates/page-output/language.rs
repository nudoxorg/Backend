//! Source language selected from a path extension by the frontends' rules.
//!
//! Seven frontends claim these extensions (`clang` claims both `C` and `C++`, and
//! the `TypeScript` frontend claims the `JavaScript` dialects it can parse). The
//! mapping is lexical and total: an unclaimed extension is
//! [`Language::Unknown`], never a guess at a neighbouring language.

use crate::identity::PackagePath;
use core::fmt;

/// One source language a surface can label and colour.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Language {
    /// Rust, claimed by the `rust` frontend.
    Rust,
    /// `TypeScript` and its `JavaScript` dialects, claimed by the `typescript` frontend.
    TypeScript,
    /// Python, claimed by the `python` frontend.
    Python,
    /// Go, claimed by the `go` frontend.
    Go,
    /// Java, claimed by the `java` frontend.
    Java,
    /// C#, claimed by the `csharp` frontend.
    CSharp,
    /// C, claimed by the `clang` frontend.
    C,
    /// C++, claimed by the `clang` frontend.
    Cxx,
    /// No frontend claims this path.
    #[default]
    Unknown,
}

impl Language {
    /// Every language a surface can render, in stable display order.
    pub const ALL: [Self; 8] = [
        Self::Rust,
        Self::TypeScript,
        Self::Python,
        Self::Go,
        Self::Java,
        Self::CSharp,
        Self::C,
        Self::Cxx,
    ];

    /// Selects a language from one package-relative path.
    #[must_use]
    pub fn from_path(path: &PackagePath) -> Self {
        path.extension()
            .map_or(Self::Unknown, |extension| Self::from_extension(&extension))
    }

    /// Selects a language from one lowercase file extension.
    #[must_use]
    pub fn from_extension(extension: &str) -> Self {
        match extension {
            "rs" => Self::Rust,
            "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => Self::TypeScript,
            "py" | "pyi" | "pyw" => Self::Python,
            "go" => Self::Go,
            "java" => Self::Java,
            "cs" => Self::CSharp,
            "c" | "h" => Self::C,
            "cc" | "cpp" | "cxx" | "c++" | "hh" | "hpp" | "hxx" | "h++" | "ipp" => Self::Cxx,
            _ => Self::Unknown,
        }
    }

    /// Returns the stable lowercase name shared by every surface.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
            Self::Python => "python",
            Self::Go => "go",
            Self::Java => "java",
            Self::CSharp => "csharp",
            Self::C => "c",
            Self::Cxx => "cpp",
            Self::Unknown => "unknown",
        }
    }

    /// Returns the info-string a Markdown fence uses for this language.
    #[must_use]
    pub const fn fence(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::TypeScript => "typescript",
            Self::Python => "python",
            Self::Go => "go",
            Self::Java => "java",
            Self::CSharp => "csharp",
            Self::C => "c",
            Self::Cxx => "cpp",
            Self::Unknown => "text",
        }
    }

    /// Returns whether this language spells declaration paths with `::`.
    #[must_use]
    pub const fn uses_scope_resolution(self) -> bool {
        matches!(self, Self::Rust | Self::C | Self::Cxx)
    }
}

impl fmt::Display for Language {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.name())
    }
}
