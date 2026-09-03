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

/// Maximum number of NUL-terminated arguments retained from one compilation-database command.
pub const MAX_DATABASE_ARGUMENTS: usize = 64;

/// Borrowed arguments read verbatim from one compilation-database command.
#[derive(Clone, Copy, Debug)]
pub struct DatabaseArguments<'arguments> {
    values: &'arguments [&'arguments CStr],
}

impl<'arguments> DatabaseArguments<'arguments> {
    /// Validates the caller-owned argument array without truncating it.
    pub fn new(values: &'arguments [&'arguments CStr]) -> Result<Self, DatabaseArgumentError> {
        if values.len() > MAX_DATABASE_ARGUMENTS {
            return Err(DatabaseArgumentError {
                required: values.len(),
                capacity: MAX_DATABASE_ARGUMENTS,
            });
        }
        Ok(Self { values })
    }

    pub(crate) const fn values(self) -> &'arguments [&'arguments CStr] {
        self.values
    }
}

/// Exact capacity rejection for a compilation-database command.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("compilation-database arguments require {required}, capacity is {capacity}")]
pub struct DatabaseArgumentError {
    /// Exact number of arguments read from the command.
    pub required: usize,
    /// Fixed lane capacity.
    pub capacity: usize,
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
    /// A source and the exact arguments supplied by a compilation database.
    Database {
        /// File path used by libclang for source identity and diagnostics.
        file_name: &'source CStr,
        /// Exact source bytes for this translation unit.
        source: &'source [u8],
        /// Arguments copied from the database without adding or truncating flags.
        arguments: DatabaseArguments<'source>,
        /// The command's explicit working directory, supplied by libclang's database authority.
        working_directory: &'source CStr,
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
            Self::C { file_name, .. }
            | Self::Cxx { file_name, .. }
            | Self::Database { file_name, .. } => file_name,
        }
    }

    pub(crate) const fn source(self) -> &'source [u8] {
        match self {
            Self::C { source, .. } | Self::Cxx { source, .. } | Self::Database { source, .. } => {
                source
            }
        }
    }

    pub(crate) const fn dialect_argument(self) -> &'static CStr {
        match self {
            Self::C { .. } => c"c",
            Self::Cxx { .. } => c"c++",
            Self::Database { .. } => c"c",
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
            Self::Database { .. } => c"",
        }
    }

    pub(crate) const fn database_arguments(self) -> Option<&'source [&'source CStr]> {
        match self {
            Self::Database { arguments, .. } => Some(arguments.values()),
            _ => None,
        }
    }

    /// Creates a database-derived input after checking its exact argument count.
    pub fn from_database(
        file_name: &'source CStr,
        source: &'source [u8],
        arguments: &'source [&'source CStr],
        working_directory: &'source CStr,
    ) -> Result<Self, DatabaseArgumentError> {
        Ok(Self::Database {
            file_name,
            source,
            arguments: DatabaseArguments::new(arguments)?,
            working_directory,
        })
    }

    pub(crate) const fn database_working_directory(self) -> Option<&'source CStr> {
        match self {
            Self::Database {
                working_directory, ..
            } => Some(working_directory),
            _ => None,
        }
    }
}
