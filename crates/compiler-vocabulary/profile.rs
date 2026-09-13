//! Defines source-language profiles that change parsing or semantic meaning.
//! The closed two-byte encoding belongs to canonical compilation recipes.
//! Profile variants prevent incompatible language and dialect combinations.

use crate::Language;
use thiserror::Error;

/// Rust edition selected for parsing, name resolution, and macro semantics.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RustEdition {
    /// Rust 2015 edition.
    Rust2015 = 0,
    /// Rust 2018 edition.
    Rust2018 = 1,
    /// Rust 2021 edition.
    Rust2021 = 2,
    /// Rust 2024 edition.
    Rust2024 = 3,
}

/// TypeScript source grammar selected before binding or type checking.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum TypeScriptSource {
    /// TypeScript without JSX syntax.
    TypeScript = 0,
    /// TypeScript with JSX syntax.
    Tsx = 1,
}

/// Python language version whose grammar and typing semantics are authoritative.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PythonVersion {
    /// Python 3.10.
    Python310 = 0,
    /// Python 3.11.
    Python311 = 1,
    /// Python 3.12.
    Python312 = 2,
    /// Python 3.13.
    Python313 = 3,
    /// Python 3.14.
    Python314 = 4,
}

/// Go language version used for syntax and type-system feature gates.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum GoVersion {
    /// Go 1.22 language semantics.
    Go122 = 0,
    /// Go 1.23 language semantics.
    Go123 = 1,
    /// Go 1.24 language semantics.
    Go124 = 2,
    /// Go 1.25 language semantics.
    Go125 = 3,
}

/// Java source release used by javac and the semantic tree authority.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum JavaRelease {
    /// Java 8 source semantics.
    Java8 = 0,
    /// Java 11 source semantics.
    Java11 = 1,
    /// Java 17 source semantics.
    Java17 = 2,
    /// Java 21 source semantics.
    Java21 = 3,
    /// Java 25 source semantics.
    Java25 = 4,
}

/// C# language version selected by the Roslyn compilation authority.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CSharpVersion {
    /// C# 10.
    CSharp10 = 0,
    /// C# 11.
    CSharp11 = 1,
    /// C# 12.
    CSharp12 = 2,
    /// C# 13.
    CSharp13 = 3,
    /// C# 14.
    CSharp14 = 4,
}

/// ISO C language standard selected by libclang.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CStandard {
    /// ISO C11.
    C11 = 0,
    /// ISO C17.
    C17 = 1,
    /// ISO C23.
    C23 = 2,
}

/// ISO C++ language standard selected by libclang.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CxxStandard {
    /// ISO C++17.
    Cxx17 = 0,
    /// ISO C++20.
    Cxx20 = 1,
    /// ISO C++23.
    Cxx23 = 2,
    /// ISO C++26.
    Cxx26 = 3,
}

/// Closed source profile whose variant proves compatibility with its language.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LanguageProfile {
    /// Rust source under one explicit edition.
    Rust(RustEdition),
    /// TypeScript source under the TS or TSX grammar.
    TypeScript(TypeScriptSource),
    /// Python source under one explicit language version.
    Python(PythonVersion),
    /// Go source under one explicit language version.
    Go(GoVersion),
    /// Java source under one explicit release.
    Java(JavaRelease),
    /// C# source under one explicit Roslyn language version.
    CSharp(CSharpVersion),
    /// C source under one explicit ISO standard.
    C(CStandard),
    /// C++ source under one explicit ISO standard.
    Cxx(CxxStandard),
}

impl LanguageProfile {
    /// Canonical product profiles advertised by the built-in semantic plane.
    ///
    /// C and C++ remain separate rows even though both select the Clang native
    /// tool family. TypeScript and TSX likewise retain distinct source modes.
    pub const PRODUCT_PROFILES: [Self; 9] = [
        Self::Rust(RustEdition::Rust2024),
        Self::Python(PythonVersion::Python314),
        Self::TypeScript(TypeScriptSource::TypeScript),
        Self::TypeScript(TypeScriptSource::Tsx),
        Self::Go(GoVersion::Go125),
        Self::Java(JavaRelease::Java25),
        Self::CSharp(CSharpVersion::CSharp14),
        Self::C(CStandard::C23),
        Self::Cxx(CxxStandard::Cxx23),
    ];

    /// Returns the language family proved by this closed profile.
    #[must_use]
    pub const fn language(self) -> Language {
        match self {
            Self::Rust(_) => Language::Rust,
            Self::TypeScript(_) => Language::TypeScript,
            Self::Python(_) => Language::Python,
            Self::Go(_) => Language::Go,
            Self::Java(_) => Language::Java,
            Self::CSharp(_) => Language::CSharp,
            Self::C(_) | Self::Cxx(_) => Language::Clang,
        }
    }
}

/// Unknown or incompatible canonical language-profile bytes.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("unknown language profile code {code:?}")]
pub struct UnknownLanguageProfile {
    /// Exact rejected language and profile discriminants.
    pub code: [u8; 2],
}

impl From<LanguageProfile> for Language {
    /// Projects the language family proven by a closed profile variant.
    fn from(profile: LanguageProfile) -> Self {
        profile.language()
    }
}

impl From<LanguageProfile> for [u8; 2] {
    /// Encodes a language profile as its stable recipe discriminants.
    fn from(profile: LanguageProfile) -> Self {
        let language = u8::from(profile.language());
        let profile = match profile {
            LanguageProfile::Rust(value) => value as u8,
            LanguageProfile::TypeScript(value) => value as u8,
            LanguageProfile::Python(value) => value as u8,
            LanguageProfile::Go(value) => value as u8,
            LanguageProfile::Java(value) => value as u8,
            LanguageProfile::CSharp(value) => value as u8,
            LanguageProfile::C(value) => value as u8,
            LanguageProfile::Cxx(value) => CXX_PROFILE_FLAG | value as u8,
        };
        [language, profile]
    }
}

impl TryFrom<[u8; 2]> for LanguageProfile {
    type Error = UnknownLanguageProfile;

    /// Decodes only canonical language-profile pairs.
    fn try_from(code: [u8; 2]) -> Result<Self, Self::Error> {
        let profile = match Language::try_from(code[0]) {
            Ok(Language::Rust) => decode_rust(code[1]).map(Self::Rust),
            Ok(Language::TypeScript) => decode_typescript(code[1]).map(Self::TypeScript),
            Ok(Language::Python) => decode_python(code[1]).map(Self::Python),
            Ok(Language::Go) => decode_go(code[1]).map(Self::Go),
            Ok(Language::Java) => decode_java(code[1]).map(Self::Java),
            Ok(Language::CSharp) => decode_csharp(code[1]).map(Self::CSharp),
            Ok(Language::Clang) if code[1] & CXX_PROFILE_FLAG == 0 => {
                decode_c(code[1]).map(Self::C)
            }
            Ok(Language::Clang) => decode_cxx(code[1] & !CXX_PROFILE_FLAG).map(Self::Cxx),
            Err(_) => None,
        };
        profile.ok_or(UnknownLanguageProfile { code })
    }
}

impl<'profile> TryFrom<&'profile str> for LanguageProfile {
    type Error = &'profile str;

    /// Parses the canonical transport spelling of one exact source profile.
    fn try_from(value: &'profile str) -> Result<Self, Self::Error> {
        match value {
            "rust-2015" => Ok(Self::Rust(RustEdition::Rust2015)),
            "rust-2018" => Ok(Self::Rust(RustEdition::Rust2018)),
            "rust-2021" => Ok(Self::Rust(RustEdition::Rust2021)),
            "rust-2024" => Ok(Self::Rust(RustEdition::Rust2024)),
            "typescript" => Ok(Self::TypeScript(TypeScriptSource::TypeScript)),
            "tsx" => Ok(Self::TypeScript(TypeScriptSource::Tsx)),
            "python-3.10" => Ok(Self::Python(PythonVersion::Python310)),
            "python-3.11" => Ok(Self::Python(PythonVersion::Python311)),
            "python-3.12" => Ok(Self::Python(PythonVersion::Python312)),
            "python-3.13" => Ok(Self::Python(PythonVersion::Python313)),
            "python-3.14" => Ok(Self::Python(PythonVersion::Python314)),
            "go-1.22" => Ok(Self::Go(GoVersion::Go122)),
            "go-1.23" => Ok(Self::Go(GoVersion::Go123)),
            "go-1.24" => Ok(Self::Go(GoVersion::Go124)),
            "go-1.25" => Ok(Self::Go(GoVersion::Go125)),
            "java-8" => Ok(Self::Java(JavaRelease::Java8)),
            "java-11" => Ok(Self::Java(JavaRelease::Java11)),
            "java-17" => Ok(Self::Java(JavaRelease::Java17)),
            "java-21" => Ok(Self::Java(JavaRelease::Java21)),
            "java-25" => Ok(Self::Java(JavaRelease::Java25)),
            "csharp-10" => Ok(Self::CSharp(CSharpVersion::CSharp10)),
            "csharp-11" => Ok(Self::CSharp(CSharpVersion::CSharp11)),
            "csharp-12" => Ok(Self::CSharp(CSharpVersion::CSharp12)),
            "csharp-13" => Ok(Self::CSharp(CSharpVersion::CSharp13)),
            "csharp-14" => Ok(Self::CSharp(CSharpVersion::CSharp14)),
            "c-11" => Ok(Self::C(CStandard::C11)),
            "c-17" => Ok(Self::C(CStandard::C17)),
            "c-23" => Ok(Self::C(CStandard::C23)),
            "cxx-17" => Ok(Self::Cxx(CxxStandard::Cxx17)),
            "cxx-20" => Ok(Self::Cxx(CxxStandard::Cxx20)),
            "cxx-23" => Ok(Self::Cxx(CxxStandard::Cxx23)),
            "cxx-26" => Ok(Self::Cxx(CxxStandard::Cxx26)),
            _ => Err(value),
        }
    }
}

const CXX_PROFILE_FLAG: u8 = 1 << 7;

const fn decode_rust(value: u8) -> Option<RustEdition> {
    match value {
        0 => Some(RustEdition::Rust2015),
        1 => Some(RustEdition::Rust2018),
        2 => Some(RustEdition::Rust2021),
        3 => Some(RustEdition::Rust2024),
        _ => None,
    }
}

const fn decode_typescript(value: u8) -> Option<TypeScriptSource> {
    match value {
        0 => Some(TypeScriptSource::TypeScript),
        1 => Some(TypeScriptSource::Tsx),
        _ => None,
    }
}

const fn decode_python(value: u8) -> Option<PythonVersion> {
    match value {
        0 => Some(PythonVersion::Python310),
        1 => Some(PythonVersion::Python311),
        2 => Some(PythonVersion::Python312),
        3 => Some(PythonVersion::Python313),
        4 => Some(PythonVersion::Python314),
        _ => None,
    }
}

const fn decode_go(value: u8) -> Option<GoVersion> {
    match value {
        0 => Some(GoVersion::Go122),
        1 => Some(GoVersion::Go123),
        2 => Some(GoVersion::Go124),
        3 => Some(GoVersion::Go125),
        _ => None,
    }
}

const fn decode_java(value: u8) -> Option<JavaRelease> {
    match value {
        0 => Some(JavaRelease::Java8),
        1 => Some(JavaRelease::Java11),
        2 => Some(JavaRelease::Java17),
        3 => Some(JavaRelease::Java21),
        4 => Some(JavaRelease::Java25),
        _ => None,
    }
}

const fn decode_csharp(value: u8) -> Option<CSharpVersion> {
    match value {
        0 => Some(CSharpVersion::CSharp10),
        1 => Some(CSharpVersion::CSharp11),
        2 => Some(CSharpVersion::CSharp12),
        3 => Some(CSharpVersion::CSharp13),
        4 => Some(CSharpVersion::CSharp14),
        _ => None,
    }
}

const fn decode_c(value: u8) -> Option<CStandard> {
    match value {
        0 => Some(CStandard::C11),
        1 => Some(CStandard::C17),
        2 => Some(CStandard::C23),
        _ => None,
    }
}

const fn decode_cxx(value: u8) -> Option<CxxStandard> {
    match value {
        0 => Some(CxxStandard::Cxx17),
        1 => Some(CxxStandard::Cxx20),
        2 => Some(CxxStandard::Cxx23),
        3 => Some(CxxStandard::Cxx26),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CompileRecipeFact, NativeTool, Stage};
    use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};

    const PROFILES: [LanguageProfile; 32] = [
        LanguageProfile::Rust(RustEdition::Rust2015),
        LanguageProfile::Rust(RustEdition::Rust2018),
        LanguageProfile::Rust(RustEdition::Rust2021),
        LanguageProfile::Rust(RustEdition::Rust2024),
        LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
        LanguageProfile::TypeScript(TypeScriptSource::Tsx),
        LanguageProfile::Python(PythonVersion::Python310),
        LanguageProfile::Python(PythonVersion::Python311),
        LanguageProfile::Python(PythonVersion::Python312),
        LanguageProfile::Python(PythonVersion::Python313),
        LanguageProfile::Python(PythonVersion::Python314),
        LanguageProfile::Go(GoVersion::Go122),
        LanguageProfile::Go(GoVersion::Go123),
        LanguageProfile::Go(GoVersion::Go124),
        LanguageProfile::Go(GoVersion::Go125),
        LanguageProfile::Java(JavaRelease::Java8),
        LanguageProfile::Java(JavaRelease::Java11),
        LanguageProfile::Java(JavaRelease::Java17),
        LanguageProfile::Java(JavaRelease::Java21),
        LanguageProfile::Java(JavaRelease::Java25),
        LanguageProfile::CSharp(CSharpVersion::CSharp10),
        LanguageProfile::CSharp(CSharpVersion::CSharp11),
        LanguageProfile::CSharp(CSharpVersion::CSharp12),
        LanguageProfile::CSharp(CSharpVersion::CSharp13),
        LanguageProfile::CSharp(CSharpVersion::CSharp14),
        LanguageProfile::C(CStandard::C11),
        LanguageProfile::C(CStandard::C17),
        LanguageProfile::C(CStandard::C23),
        LanguageProfile::Cxx(CxxStandard::Cxx17),
        LanguageProfile::Cxx(CxxStandard::Cxx20),
        LanguageProfile::Cxx(CxxStandard::Cxx23),
        LanguageProfile::Cxx(CxxStandard::Cxx26),
    ];

    #[test]
    fn every_profile_round_trips_and_proves_its_language() -> Result<(), UnknownLanguageProfile> {
        for profile in PROFILES {
            let code = <[u8; 2]>::from(profile);
            assert_eq!(LanguageProfile::try_from(code)?, profile);
            assert_eq!(Language::try_from(code[0]), Ok(Language::from(profile)));
        }
        Ok(())
    }

    #[test]
    fn incompatible_and_unknown_pairs_retain_both_bytes() {
        for code in [[7, 0], [0, 4], [1, 2], [6, 3], [6, 0x84]] {
            assert_eq!(
                LanguageProfile::try_from(code),
                Err(UnknownLanguageProfile { code })
            );
        }
    }

    #[test]
    fn profile_semantics_are_part_of_recipe_identity() {
        let source = ContentId::<SourceFactDomain>::from_canonical_bytes(b"profile-source");
        let toolchain = ContentId::<ToolchainDomain>::from_canonical_bytes(b"profile-toolchain");
        let rust_2021 = CompileRecipeFact::derive(
            LanguageProfile::Rust(RustEdition::Rust2021),
            Stage::LowerIr,
            NativeTool::Rustc,
            source,
            toolchain,
        );
        let rust_2024 = CompileRecipeFact::derive(
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            NativeTool::Rustc,
            source,
            toolchain,
        );

        assert_ne!(rust_2021.identity, rust_2024.identity);
    }
}
