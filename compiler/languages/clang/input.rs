//! Defines the closed C and C++ parsing inputs accepted by the native authority.
//! The profile type proves that each standard belongs to C or C++ before libclang is called.
//! Input source bytes and the synthetic source filename are borrowed, never copied or guessed.

use core::ffi::CStr;

use compiler_vocabulary::{CStandard, CxxStandard, LanguageProfile};
use thiserror::Error;

/// A closed failure preserving the non-Clang canonical profile supplied by a caller.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("{profile:?} is not a C or C++ language profile")]
pub struct UnsupportedLanguageProfile {
    /// The exact rejected canonical profile.
    pub profile: LanguageProfile,
}

/// Borrowed source authority and typed parsing configuration for one translation unit.
#[derive(Clone, Copy, Debug)]
pub enum ClangInput<'source> {
    /// C source with a canonical C standard proven in the variant.
    C {
        /// NUL-terminated synthetic name used by libclang to bind source locations.
        file_name: &'source CStr,
        /// Exact byte authority; every emitted span indexes this slice.
        source: &'source [u8],
        /// Canonical C standard passed directly to libclang.
        standard: CStandard,
    },
    /// C++ source with a canonical C++ standard proven in the variant.
    Cxx {
        /// NUL-terminated synthetic name used by libclang to bind source locations.
        file_name: &'source CStr,
        /// Exact byte authority; every emitted span indexes this slice.
        source: &'source [u8],
        /// Canonical C++ standard passed directly to libclang.
        standard: CxxStandard,
    },
}

impl<'source> ClangInput<'source> {
    /// Binds source authority to a canonical C or C++ profile without filename inference.
    ///
    /// # Errors
    ///
    /// Returns [`UnsupportedLanguageProfile`] unchanged when `profile` is not C or C++.
    pub const fn from_profile(
        file_name: &'source CStr,
        source: &'source [u8],
        profile: LanguageProfile,
    ) -> Result<Self, UnsupportedLanguageProfile> {
        match profile {
            LanguageProfile::C(standard) => Ok(Self::C {
                file_name,
                source,
                standard,
            }),
            LanguageProfile::Cxx(standard) => Ok(Self::Cxx {
                file_name,
                source,
                standard,
            }),
            profile => Err(UnsupportedLanguageProfile { profile }),
        }
    }

    pub(crate) const fn file_name(self) -> &'source CStr {
        match self {
            Self::C { file_name, .. } | Self::Cxx { file_name, .. } => file_name,
        }
    }

    pub(crate) const fn source(self) -> &'source [u8] {
        match self {
            Self::C { source, .. } | Self::Cxx { source, .. } => source,
        }
    }

    pub(crate) const fn dialect_argument(self) -> &'static CStr {
        match self {
            Self::C { .. } => c"c",
            Self::Cxx { .. } => c"c++",
        }
    }

    pub(crate) const fn standard_argument(self) -> &'static CStr {
        match self {
            Self::C {
                standard: CStandard::C11,
                ..
            } => c"-std=c11",
            Self::C {
                standard: CStandard::C17,
                ..
            } => c"-std=c17",
            Self::C {
                standard: CStandard::C23,
                ..
            } => c"-std=c23",
            Self::Cxx {
                standard: CxxStandard::Cxx17,
                ..
            } => c"-std=c++17",
            Self::Cxx {
                standard: CxxStandard::Cxx20,
                ..
            } => c"-std=c++20",
            Self::Cxx {
                standard: CxxStandard::Cxx23,
                ..
            } => c"-std=c++23",
            Self::Cxx {
                standard: CxxStandard::Cxx26,
                ..
            } => c"-std=c++2c",
        }
    }
}
