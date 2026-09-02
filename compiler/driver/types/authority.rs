//! Defines exact semantic-authority failures retained by `compiler-driver`.
//! Keeps every language frontend's closed error type intact through IR admission.
//! Projects only bounded diagnostics at application and transport boundaries.

use compiler_vocabulary::LanguageProfile;

pub use compiler_vocabulary::AuthorityPhase;

/// Bounded primary diagnostic retained beside an exact frontend error.
///
/// The primary bytes are copied only by the authority that owns the diagnostic
/// transport.  `observed` remains the complete source diagnostic size, making
/// truncation explicit to application surfaces without replacing the source
/// error with rendered text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthorityDiagnostic<'diagnostic> {
    /// Exact retained primary bytes from the source authority.
    pub primary: &'diagnostic [u8],
    /// Total source diagnostic bytes observed before bounded retention.
    pub observed: usize,
    /// Whether `primary` omits later source diagnostic bytes.
    pub truncated: bool,
}

impl<'diagnostic> AuthorityDiagnostic<'diagnostic> {
    /// Creates a bounded diagnostic after proving its count and truncation facts agree.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorityDiagnosticFault`] if the retained prefix cannot be
    /// a prefix of the observed authority diagnostic.
    pub const fn new(
        primary: &'diagnostic [u8],
        observed: usize,
        truncated: bool,
    ) -> Result<Self, AuthorityDiagnosticFault> {
        if primary.len() > observed {
            return Err(AuthorityDiagnosticFault::PrefixExceedsObserved {
                retained: primary.len(),
                observed,
            });
        }
        if truncated != (primary.len() != observed) {
            return Err(AuthorityDiagnosticFault::TruncationMismatch {
                retained: primary.len(),
                observed,
                truncated,
            });
        }
        Ok(Self {
            primary,
            observed,
            truncated,
        })
    }

    /// Creates the exact empty diagnostic used when an authority has no byte diagnostic channel.
    #[must_use]
    pub const fn absent() -> Self {
        Self {
            primary: &[],
            observed: 0,
            truncated: false,
        }
    }
}

/// Closed validation failure for an [`AuthorityDiagnostic`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorityDiagnosticFault {
    /// The retained prefix is longer than the authority's observed diagnostic stream.
    PrefixExceedsObserved {
        /// Bytes retained in the bounded prefix.
        retained: usize,
        /// Complete bytes reported by the authority.
        observed: usize,
    },
    /// The boolean truncation fact disagrees with exact retained and observed lengths.
    TruncationMismatch {
        /// Bytes retained in the bounded prefix.
        retained: usize,
        /// Complete bytes reported by the authority.
        observed: usize,
        /// Authority-provided truncation bit.
        truncated: bool,
    },
}

/// Concrete error emitted by one exact language authority.
///
/// This enum is deliberately not a common prose/error-code DTO: each variant
/// retains the original closed error type from the frontend that produced it.
#[derive(Debug, thiserror::Error)]
pub enum FrontendAuthorityError {
    /// Direct libclang collection did not yield complete C or C++ facts.
    #[error(transparent)]
    Clang(#[from] compiler_languages_clang::CollectError),
    /// Rust-analyzer did not yield complete HIR/type facts for the selected Cargo graph.
    #[error(transparent)]
    Rust(#[from] compiler_languages_rust::RustAuthorityError),
    /// OXC syntax/binding authority did not yield TypeScript facts.
    #[error(transparent)]
    TypeScript(#[from] compiler_languages_typescript::AuthorityError),
    /// Ruff/Python semantic authority did not yield source facts.
    #[error(transparent)]
    Python(#[from] compiler_languages_python::ExtractionError),
    /// The real Go package/type authority boundary did not yield complete facts.
    #[error(transparent)]
    Go(#[from] compiler_languages_go::OracleError),
    /// The Roslyn authority boundary did not yield complete facts.
    #[error(transparent)]
    CSharp(#[from] compiler_languages_csharp::DecodeError),
    /// The validated javac authority image did not yield complete facts.
    #[error(transparent)]
    Java(#[from] compiler_languages_java::ImageError),
}

impl FrontendAuthorityError {
    /// Returns the unique compiler profile family that owns this source error.
    #[must_use]
    pub const fn language(self: &Self) -> compiler_vocabulary::Language {
        match self {
            Self::Clang(_) => compiler_vocabulary::Language::Clang,
            Self::Rust(_) => compiler_vocabulary::Language::Rust,
            Self::TypeScript(_) => compiler_vocabulary::Language::TypeScript,
            Self::Python(_) => compiler_vocabulary::Language::Python,
            Self::Go(_) => compiler_vocabulary::Language::Go,
            Self::CSharp(_) => compiler_vocabulary::Language::CSharp,
            Self::Java(_) => compiler_vocabulary::Language::Java,
        }
    }

    /// Checks that the authority's language family agrees with the exact requested profile.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorityProfileMismatch`] without losing the source error
    /// when a caller attempts to attach it to a different language profile.
    pub fn bind_profile(self, profile: LanguageProfile) -> Result<Self, AuthorityProfileMismatch> {
        let actual = self.language();
        let expected = profile.language();
        if actual == expected {
            Ok(self)
        } else {
            Err(AuthorityProfileMismatch {
                profile,
                actual,
                source: self,
            })
        }
    }
}

/// A concrete frontend error was offered under a profile owned by another language.
#[derive(Debug, thiserror::Error)]
#[error("{actual:?} authority error cannot satisfy requested {profile:?} profile")]
pub struct AuthorityProfileMismatch {
    /// Exact profile selected by the compiler request.
    pub profile: LanguageProfile,
    /// Language family that produced `source`.
    pub actual: compiler_vocabulary::Language,
    /// Original frontend error that was not projected or replaced.
    #[source]
    pub source: FrontendAuthorityError,
}
