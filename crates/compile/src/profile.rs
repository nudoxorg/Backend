//! Closed source profiles that alter parsing or semantic meaning.

use backend_semantic::vocabulary::JavaRelease;
use crate::SourceLanguage;

/// Rust edition selected before name resolution and macro expansion.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RustEdition {
    /// Rust 2015.
    Rust2015,
    /// Rust 2018.
    Rust2018,
    /// Rust 2021.
    Rust2021,
    /// Rust 2024.
    Rust2024,
}

/// TypeScript grammar selected before binding and type checking.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TypeScriptSource {
    /// TypeScript without JSX syntax.
    TypeScript,
    /// TypeScript with JSX syntax.
    Tsx,
}

impl TypeScriptSource {
    /// Returns the canonical helper-protocol spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::TypeScript => "ts",
            Self::Tsx => "tsx",
        }
    }
}

impl<'source> TryFrom<&'source str> for TypeScriptSource {
    type Error = UnsupportedProfile<'source>;

    fn try_from(value: &'source str) -> Result<Self, Self::Error> {
        match value {
            "ts" | "typescript" => Ok(Self::TypeScript),
            "tsx" => Ok(Self::Tsx),
            value => Err(UnsupportedProfile::new(SourceLanguage::TypeScript, value)),
        }
    }
}

/// Python language version selected before parsing and type analysis.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PythonVersion {
    /// Python 3.10.
    Python310,
    /// Python 3.11.
    Python311,
    /// Python 3.12.
    Python312,
    /// Python 3.13.
    Python313,
    /// Python 3.14.
    Python314,
}

impl PythonVersion {
    /// Returns the canonical helper-protocol spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Python310 => "3.10",
            Self::Python311 => "3.11",
            Self::Python312 => "3.12",
            Self::Python313 => "3.13",
            Self::Python314 => "3.14",
        }
    }
}

impl<'source> TryFrom<&'source str> for PythonVersion {
    type Error = UnsupportedProfile<'source>;

    fn try_from(value: &'source str) -> Result<Self, Self::Error> {
        match value {
            "3.10" | "python-3.10" => Ok(Self::Python310),
            "3.11" | "python-3.11" => Ok(Self::Python311),
            "3.12" | "python-3.12" => Ok(Self::Python312),
            "3.13" | "python-3.13" => Ok(Self::Python313),
            "3.14" | "python-3.14" => Ok(Self::Python314),
            value => Err(UnsupportedProfile::new(SourceLanguage::Python, value)),
        }
    }
}

/// Go language version used by syntax and type-system feature gates.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum GoVersion {
    /// Go 1.22.
    Go122,
    /// Go 1.23.
    Go123,
    /// Go 1.24.
    Go124,
    /// Go 1.25.
    Go125,
}

/// C# language version selected by Roslyn.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CSharpVersion {
    /// C# 10.
    CSharp10,
    /// C# 11.
    CSharp11,
    /// C# 12.
    CSharp12,
    /// C# 13.
    CSharp13,
    /// C# 14.
    CSharp14,
}

/// ISO C language standard selected by Clang.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CStandard {
    /// ISO C11.
    C11,
    /// ISO C17.
    C17,
    /// ISO C23.
    C23,
}

/// ISO C++ language standard selected by Clang.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CxxStandard {
    /// ISO C++17.
    Cxx17,
    /// ISO C++20.
    Cxx20,
    /// ISO C++23.
    Cxx23,
    /// ISO C++26.
    Cxx26,
}

/// One profile whose variant proves that it belongs to its language.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LanguageProfile {
    /// Rust source under an exact edition.
    Rust(RustEdition),
    /// TypeScript source under an exact grammar.
    TypeScript(TypeScriptSource),
    /// Python source under an exact language version.
    Python(PythonVersion),
    /// Go source under an exact language version.
    Go(GoVersion),
    /// Java source under an exact release.
    Java(JavaRelease),
    /// C# source under an exact language version.
    CSharp(CSharpVersion),
    /// C source under an exact ISO standard.
    C(CStandard),
    /// C++ source under an exact ISO standard.
    Cxx(CxxStandard),
}

impl LanguageProfile {
    /// Returns the language proved by this profile variant.
    #[must_use]
    pub const fn language(self) -> SourceLanguage {
        match self {
            Self::Rust(_) => SourceLanguage::Rust,
            Self::TypeScript(_) => SourceLanguage::TypeScript,
            Self::Python(_) => SourceLanguage::Python,
            Self::Go(_) => SourceLanguage::Go,
            Self::Java(_) => SourceLanguage::Java,
            Self::CSharp(_) => SourceLanguage::CSharp,
            Self::C(_) | Self::Cxx(_) => SourceLanguage::Clang,
        }
    }
}

/// A borrowed, allocation-free profile parse failure retaining the input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnsupportedProfile<'source> {
    language: SourceLanguage,
    value: &'source str,
}

impl<'source> UnsupportedProfile<'source> {
    const fn new(language: SourceLanguage, value: &'source str) -> Self {
        Self { language, value }
    }

    /// Returns the language whose profile was being parsed.
    #[must_use]
    pub const fn language(self) -> SourceLanguage {
        self.language
    }

    /// Returns the exact rejected input.
    #[must_use]
    pub const fn value(self) -> &'source str {
        self.value
    }
}

impl std::fmt::Display for UnsupportedProfile<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "unsupported {} profile {:?}",
            self.language.name(),
            self.value
        )
    }
}

impl std::error::Error for UnsupportedProfile<'_> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_variants_cannot_cross_language_boundaries() {
        assert_eq!(
            LanguageProfile::TypeScript(TypeScriptSource::Tsx).language(),
            SourceLanguage::TypeScript
        );
        assert_eq!(
            LanguageProfile::Cxx(CxxStandard::Cxx23).language(),
            SourceLanguage::Clang
        );
    }

    #[test]
    fn parsers_normalize_aliases_and_retain_rejected_input() -> Result<(), &'static str> {
        assert_eq!(
            TypeScriptSource::try_from("typescript"),
            Ok(TypeScriptSource::TypeScript)
        );
        assert_eq!(
            PythonVersion::try_from("python-3.13"),
            Ok(PythonVersion::Python313)
        );
        let Err(error) = PythonVersion::try_from("python-next") else {
            return Err("open version was accepted");
        };
        assert_eq!(error.language(), SourceLanguage::Python);
        assert_eq!(error.value(), "python-next");
        assert_eq!(JavaRelease::try_from("latest"), Err("latest"));
        Ok(())
    }
}
