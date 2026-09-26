//! Defines exact semantic-authority failures retained by the `backend-engine` driver.
//! Keeps each frontend's closed error type intact through canonical admission.
//! Projects bounded diagnostics only after deriving their typed cause class.

use backend_semantic::vocabulary::{AuthorityDiagnosticClass, AuthorityPhase, LanguageProfile};

mod phase;

use phase::{
    clang_class, clang_phase, csharp_class, csharp_image_class, csharp_image_phase, csharp_phase,
    go_class, go_image_class, go_image_phase, go_phase, java_bound_class, java_bound_phase,
    java_class, java_phase, python_class, python_phase, rust_class, rust_phase, typescript_class,
    typescript_phase,
};

/// Bounded primary diagnostic retained beside one exact frontend error.
///
/// The primary bytes are copied only by the authority that owns the diagnostic
/// transport. `observed` remains the complete source diagnostic size, making
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

/// Complete source error from one language authority.
///
/// A variant selects its language and retains its concrete closed source
/// error, so callers cannot attach a mismatched phase or diagnostic class.
#[derive(Debug, thiserror::Error)]
pub enum AuthorityFailure<'diagnostic> {
    /// Direct libclang collection did not yield complete C or C++ facts.
    #[error("direct libclang authority failed")]
    Clang {
        /// Bounded source diagnostic retained by the Clang authority.
        diagnostic: AuthorityDiagnostic<'diagnostic>,
        /// Exact libclang source failure.
        #[source]
        cause: backend_frontend_clang::legacy::CollectError,
    },
    /// Rust-analyzer did not yield complete HIR/type facts for the selected Cargo graph.
    #[error("Rust semantic authority failed")]
    Rust {
        /// Bounded source diagnostic retained by the Rust authority.
        diagnostic: AuthorityDiagnostic<'diagnostic>,
        /// Exact rust-analyzer source failure.
        #[source]
        cause: backend_frontend_rust::legacy::RustAuthorityError,
    },
    /// OXC syntax/binding authority did not yield TypeScript facts.
    #[error("TypeScript semantic authority failed")]
    TypeScript {
        /// Bounded source diagnostic retained by the TypeScript authority.
        diagnostic: AuthorityDiagnostic<'diagnostic>,
        /// Exact OXC source failure.
        #[source]
        cause: backend_frontend_typescript::legacy::AuthorityError,
    },
    /// TypeScript source bytes could not be lent to OXC as valid UTF-8 text.
    #[error("TypeScript source is not valid UTF-8")]
    TypeScriptUtf8 {
        /// Bounded source diagnostic retained by the TypeScript authority.
        diagnostic: AuthorityDiagnostic<'diagnostic>,
        /// Exact UTF-8 decoding failure from the caller's source bytes.
        #[source]
        cause: std::str::Utf8Error,
    },
    /// OXC returned a byte span outside the exact TypeScript source authority.
    #[error("TypeScript authority returned an invalid source span")]
    TypeScriptSpan {
        /// Bounded source diagnostic retained by the TypeScript authority.
        diagnostic: AuthorityDiagnostic<'diagnostic>,
        /// Inclusive OXC source-byte start retained without a lossy message.
        start: u32,
        /// Exclusive OXC source-byte end retained without a lossy message.
        end: u32,
    },
    /// Ruff/Python semantic authority did not yield source facts.
    #[error("Python semantic authority failed")]
    Python {
        /// Bounded source diagnostic retained by the Python authority.
        diagnostic: AuthorityDiagnostic<'diagnostic>,
        /// Exact Python source failure.
        #[source]
        cause: backend_frontend_python::legacy::ExtractionError,
    },
    /// Pyrefly started for this source but did not complete its type-authority
    /// transaction. This is not checker unavailability: the exact failure is
    /// retained through the driver boundary.
    #[error("Python checker authority failed")]
    PythonChecker {
        /// Bounded source diagnostic retained by the Python authority.
        diagnostic: AuthorityDiagnostic<'diagnostic>,
        /// Exact pyrefly transaction failure.
        #[source]
        cause: backend_frontend_python::legacy::CheckerError,
    },
    /// Ruff returned a declaration span outside the exact Python source authority.
    #[error("Python authority returned an invalid source span")]
    PythonSpan {
        /// Bounded source diagnostic retained by the Python authority.
        diagnostic: AuthorityDiagnostic<'diagnostic>,
        /// Inclusive Ruff source-byte start retained without a lossy message.
        start: u32,
        /// Exclusive Ruff source-byte end retained without a lossy message.
        end: u32,
    },
    /// The real Go package/type authority boundary did not yield complete facts.
    #[error("Go semantic authority failed")]
    Go {
        /// Bounded source diagnostic retained by the Go authority.
        diagnostic: AuthorityDiagnostic<'diagnostic>,
        /// Exact Go source failure.
        #[source]
        cause: backend_frontend_go::legacy::OracleError,
    },
    /// The fixed Go authority image failed before lending semantic facts.
    #[error("Go authority image failed")]
    GoImage {
        /// Bounded source diagnostic retained by the Go authority.
        diagnostic: AuthorityDiagnostic<'diagnostic>,
        /// Exact binary-image validation failure.
        #[source]
        cause: backend_frontend_go::legacy::ImageError,
    },
    /// A Go image was produced for a source other than the compile request.
    #[error("Go authority image source binding differs from the compile request")]
    GoSourceBinding {
        /// Bounded source diagnostic retained by the Go authority.
        diagnostic: AuthorityDiagnostic<'diagnostic>,
        /// SHA-256 digest of the exact compile request source bytes.
        expected: [u8; 32],
        /// SHA-256 digest retained by the Go authority image.
        observed: [u8; 32],
    },
    /// The Roslyn authority boundary did not yield complete facts.
    #[error("C# semantic authority failed")]
    CSharp {
        /// Bounded source diagnostic retained by the C# authority.
        diagnostic: AuthorityDiagnostic<'diagnostic>,
        /// Exact Roslyn source failure.
        #[source]
        cause: backend_frontend_csharp::legacy::DecodeError,
    },
    /// The fixed Roslyn authority image failed before lending declarations.
    #[error("C# authority image failed")]
    CSharpImage {
        /// Bounded source diagnostic retained by the Roslyn authority.
        diagnostic: AuthorityDiagnostic<'diagnostic>,
        /// Exact binary authority-image validation cause.
        #[source]
        cause: backend_frontend_csharp::legacy::ImageError,
    },
    /// A Roslyn image was generated for a source other than the compile request.
    #[error("C# authority image source binding differs from the compile request")]
    CSharpSourceBinding {
        /// Bounded source diagnostic retained by the Roslyn authority.
        diagnostic: AuthorityDiagnostic<'diagnostic>,
        /// SHA-256 digest of exact compile request bytes.
        expected: [u8; 32],
        /// SHA-256 digest retained by the binary Roslyn image.
        observed: [u8; 32],
    },
    /// A Roslyn declaration identifier span cannot name the bound source bytes.
    #[error("C# authority declaration span differs from the compile source")]
    CSharpSpan {
        /// Bounded source diagnostic retained by the Roslyn authority.
        diagnostic: AuthorityDiagnostic<'diagnostic>,
        /// Inclusive offending source-byte start coordinate.
        start: u32,
        /// Exclusive offending source-byte end coordinate.
        end: u32,
    },
    /// The validated javac authority image did not yield complete facts.
    #[error("Java semantic authority failed")]
    Java {
        /// Bounded source diagnostic retained by the Java authority.
        diagnostic: AuthorityDiagnostic<'diagnostic>,
        /// Exact javac image failure.
        #[source]
        cause: backend_frontend_java::legacy::ImageError,
    },
    /// The source-bound javac authority envelope failed before yielding facts.
    #[error("Java source-bound authority image failed")]
    JavaBoundImage {
        /// Bounded source diagnostic retained by the javac authority.
        diagnostic: AuthorityDiagnostic<'diagnostic>,
        /// Exact outer-envelope or embedded image failure.
        #[source]
        cause: backend_frontend_java::legacy::BoundImageError,
    },
    /// A source-bound javac image was generated for a different Java release.
    #[error("Java authority image release differs from the requested profile")]
    JavaRelease {
        /// Bounded source diagnostic retained by the javac authority.
        diagnostic: AuthorityDiagnostic<'diagnostic>,
        /// Java release selected by the compiler request.
        requested: backend_semantic::vocabulary::JavaRelease,
        /// Java release retained by the attributed javac image.
        observed: backend_frontend_java::legacy::JavaRelease,
    },
    /// A source-bound javac image was generated for source bytes other than this request.
    #[error("Java authority image source binding differs from the compile request")]
    JavaSourceBinding {
        /// Bounded source diagnostic retained by the javac authority.
        diagnostic: AuthorityDiagnostic<'diagnostic>,
        /// SHA-256 digest of exact compile request bytes.
        expected: [u8; 32],
        /// SHA-256 digest retained by the source-bound javac image.
        observed: [u8; 32],
    },
}

/// Bounded application projection derived from one exact authority failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthorityFailureProjection<'diagnostic> {
    /// Typed authority transaction phase derived from the concrete source error.
    pub phase: AuthorityPhase,
    /// Typed source-diagnostic class derived from the concrete source error.
    pub class: AuthorityDiagnosticClass,
    /// Bounded source diagnostic bytes and exact count facts.
    pub diagnostic: AuthorityDiagnostic<'diagnostic>,
}

impl<'diagnostic> AuthorityFailure<'diagnostic> {
    /// Derives the only public diagnostic projection for this exact source failure.
    #[must_use]
    pub fn projection(&self) -> AuthorityFailureProjection<'diagnostic> {
        match self {
            Self::Clang { diagnostic, cause } => AuthorityFailureProjection {
                phase: clang_phase(cause),
                class: clang_class(cause),
                diagnostic: *diagnostic,
            },
            Self::Rust { diagnostic, cause } => AuthorityFailureProjection {
                phase: rust_phase(cause),
                class: rust_class(cause),
                diagnostic: *diagnostic,
            },
            Self::TypeScript { diagnostic, cause } => AuthorityFailureProjection {
                phase: typescript_phase(cause),
                class: typescript_class(cause),
                diagnostic: *diagnostic,
            },
            Self::TypeScriptUtf8 { diagnostic, .. } | Self::TypeScriptSpan { diagnostic, .. } => {
                AuthorityFailureProjection {
                    phase: AuthorityPhase::Parse,
                    class: AuthorityDiagnosticClass::Syntax,
                    diagnostic: *diagnostic,
                }
            }
            Self::Python { diagnostic, cause } => AuthorityFailureProjection {
                phase: python_phase(cause),
                class: python_class(cause),
                diagnostic: *diagnostic,
            },
            Self::PythonChecker { diagnostic, .. } => AuthorityFailureProjection {
                phase: AuthorityPhase::TypeCheck,
                class: AuthorityDiagnosticClass::Type,
                diagnostic: *diagnostic,
            },
            Self::PythonSpan { diagnostic, .. } => AuthorityFailureProjection {
                phase: AuthorityPhase::Project,
                class: AuthorityDiagnosticClass::Projection,
                diagnostic: *diagnostic,
            },
            Self::Go { diagnostic, cause } => AuthorityFailureProjection {
                phase: go_phase(cause),
                class: go_class(cause),
                diagnostic: *diagnostic,
            },
            Self::GoImage { diagnostic, cause } => AuthorityFailureProjection {
                phase: go_image_phase(cause),
                class: go_image_class(cause),
                diagnostic: *diagnostic,
            },
            Self::GoSourceBinding { diagnostic, .. } => AuthorityFailureProjection {
                phase: AuthorityPhase::Project,
                class: AuthorityDiagnosticClass::Projection,
                diagnostic: *diagnostic,
            },
            Self::CSharp { diagnostic, cause } => AuthorityFailureProjection {
                phase: csharp_phase(cause),
                class: csharp_class(cause),
                diagnostic: *diagnostic,
            },
            Self::CSharpImage { diagnostic, cause } => AuthorityFailureProjection {
                phase: csharp_image_phase(cause),
                class: csharp_image_class(cause),
                diagnostic: *diagnostic,
            },
            Self::CSharpSourceBinding { diagnostic, .. } | Self::CSharpSpan { diagnostic, .. } => {
                AuthorityFailureProjection {
                    phase: AuthorityPhase::Project,
                    class: AuthorityDiagnosticClass::Projection,
                    diagnostic: *diagnostic,
                }
            }
            Self::Java { diagnostic, cause } => AuthorityFailureProjection {
                phase: java_phase(cause),
                class: java_class(cause),
                diagnostic: *diagnostic,
            },
            Self::JavaBoundImage { diagnostic, cause } => AuthorityFailureProjection {
                phase: java_bound_phase(cause),
                class: java_bound_class(cause),
                diagnostic: *diagnostic,
            },
            Self::JavaRelease { diagnostic, .. } | Self::JavaSourceBinding { diagnostic, .. } => {
                AuthorityFailureProjection {
                    phase: AuthorityPhase::Project,
                    class: AuthorityDiagnosticClass::Projection,
                    diagnostic: *diagnostic,
                }
            }
        }
    }

    /// Binds this source error only to a profile owned by its exact authority.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorityProfileMismatch`] with this untouched source error
    /// when the caller selects a profile from another language family.
    pub fn bind_profile(
        self,
        profile: LanguageProfile,
    ) -> Result<Self, AuthorityProfileMismatch<'diagnostic>> {
        let accepted = matches!(
            (&self, profile),
            (
                Self::Clang { .. },
                LanguageProfile::C(_) | LanguageProfile::Cxx(_)
            ) | (Self::Rust { .. }, LanguageProfile::Rust(_))
                | (
                    Self::TypeScript { .. }
                        | Self::TypeScriptUtf8 { .. }
                        | Self::TypeScriptSpan { .. },
                    LanguageProfile::TypeScript(_)
                )
                | (
                    Self::Python { .. } | Self::PythonChecker { .. } | Self::PythonSpan { .. },
                    LanguageProfile::Python(_)
                )
                | (
                    Self::Go { .. } | Self::GoImage { .. } | Self::GoSourceBinding { .. },
                    LanguageProfile::Go(_)
                )
                | (
                    Self::CSharp { .. }
                        | Self::CSharpImage { .. }
                        | Self::CSharpSourceBinding { .. }
                        | Self::CSharpSpan { .. },
                    LanguageProfile::CSharp(_)
                )
                | (
                    Self::Java { .. }
                        | Self::JavaBoundImage { .. }
                        | Self::JavaRelease { .. }
                        | Self::JavaSourceBinding { .. },
                    LanguageProfile::Java(_)
                )
        );
        if accepted {
            Ok(self)
        } else {
            Err(AuthorityProfileMismatch {
                profile,
                failure: self,
            })
        }
    }
}

/// A concrete frontend error was offered under a profile owned by another authority.
#[derive(Debug, thiserror::Error)]
#[error("semantic authority and requested profile differ")]
pub struct AuthorityProfileMismatch<'diagnostic> {
    /// Exact profile selected by the compiler request.
    pub profile: LanguageProfile,
    /// Original frontend error that was not projected or replaced.
    pub failure: AuthorityFailure<'diagnostic>,
}
