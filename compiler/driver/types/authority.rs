//! Defines exact semantic-authority failures retained by `compiler-driver`.
//! Keeps each frontend's closed error type intact through canonical admission.
//! Projects bounded diagnostics only after deriving their typed cause class.

use compiler_vocabulary::{AuthorityDiagnosticClass, AuthorityPhase, LanguageProfile};

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
        cause: compiler_languages_clang::CollectError,
    },
    /// Rust-analyzer did not yield complete HIR/type facts for the selected Cargo graph.
    #[error("Rust semantic authority failed")]
    Rust {
        /// Bounded source diagnostic retained by the Rust authority.
        diagnostic: AuthorityDiagnostic<'diagnostic>,
        /// Exact rust-analyzer source failure.
        #[source]
        cause: compiler_languages_rust::RustAuthorityError,
    },
    /// OXC syntax/binding authority did not yield TypeScript facts.
    #[error("TypeScript semantic authority failed")]
    TypeScript {
        /// Bounded source diagnostic retained by the TypeScript authority.
        diagnostic: AuthorityDiagnostic<'diagnostic>,
        /// Exact OXC source failure.
        #[source]
        cause: compiler_languages_typescript::AuthorityError,
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
        cause: compiler_languages_python::ExtractionError,
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
        cause: compiler_languages_go::OracleError,
    },
    /// The fixed Go authority image failed before lending semantic facts.
    #[error("Go authority image failed")]
    GoImage {
        /// Bounded source diagnostic retained by the Go authority.
        diagnostic: AuthorityDiagnostic<'diagnostic>,
        /// Exact binary-image validation failure.
        #[source]
        cause: compiler_languages_go::ImageError,
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
        cause: compiler_languages_csharp::DecodeError,
    },
    /// The fixed Roslyn authority image failed before lending declarations.
    #[error("C# authority image failed")]
    CSharpImage {
        /// Bounded source diagnostic retained by the Roslyn authority.
        diagnostic: AuthorityDiagnostic<'diagnostic>,
        /// Exact binary authority-image validation cause.
        #[source]
        cause: compiler_languages_csharp::ImageError,
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
        cause: compiler_languages_java::ImageError,
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
                    Self::Python { .. } | Self::PythonSpan { .. },
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
                | (Self::Java { .. }, LanguageProfile::Java(_))
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

fn clang_phase(cause: &compiler_languages_clang::CollectError) -> AuthorityPhase {
    match cause {
        compiler_languages_clang::CollectError::Parse { .. }
        | compiler_languages_clang::CollectError::SourceContainsNul
        | compiler_languages_clang::CollectError::SourceTooLarge { .. }
        | compiler_languages_clang::CollectError::SourceLengthTooLarge { .. } => {
            AuthorityPhase::Parse
        }
        compiler_languages_clang::CollectError::ScratchCapacity { .. }
        | compiler_languages_clang::CollectError::CoordinateTooLarge { .. }
        | compiler_languages_clang::CollectError::SlotOrdinalTooLarge { .. }
        | compiler_languages_clang::CollectError::SlotCountOverflow { .. } => {
            AuthorityPhase::Project
        }
        compiler_languages_clang::CollectError::Cancelled
        | compiler_languages_clang::CollectError::Library(_)
        | compiler_languages_clang::CollectError::MissingApi { .. }
        | compiler_languages_clang::CollectError::IndexUnavailable
        | compiler_languages_clang::CollectError::MainFileUnavailable => AuthorityPhase::Open,
    }
}

fn clang_class(cause: &compiler_languages_clang::CollectError) -> AuthorityDiagnosticClass {
    match clang_phase(cause) {
        AuthorityPhase::Parse => AuthorityDiagnosticClass::Syntax,
        AuthorityPhase::Project => AuthorityDiagnosticClass::Projection,
        AuthorityPhase::Open | AuthorityPhase::Resolve | AuthorityPhase::TypeCheck => {
            AuthorityDiagnosticClass::Authority
        }
    }
}

fn rust_phase(cause: &compiler_languages_rust::RustAuthorityError) -> AuthorityPhase {
    match cause {
        compiler_languages_rust::RustAuthorityError::Workspace { .. }
        | compiler_languages_rust::RustAuthorityError::SourceNotLoaded { .. }
        | compiler_languages_rust::RustAuthorityError::EditionMismatch { .. }
        | compiler_languages_rust::RustAuthorityError::MissingSemanticFact { .. } => {
            AuthorityPhase::Resolve
        }
        compiler_languages_rust::RustAuthorityError::UnresolvedInferredType => {
            AuthorityPhase::TypeCheck
        }
        compiler_languages_rust::RustAuthorityError::InvalidSpan { .. }
        | compiler_languages_rust::RustAuthorityError::Coordinate { .. }
        | compiler_languages_rust::RustAuthorityError::Admission { .. } => AuthorityPhase::Project,
        compiler_languages_rust::RustAuthorityError::SourceBinding { .. } => AuthorityPhase::Parse,
        compiler_languages_rust::RustAuthorityError::Cancelled
        | compiler_languages_rust::RustAuthorityError::Toolchain(_)
        | compiler_languages_rust::RustAuthorityError::ProjectRoot { .. }
        | compiler_languages_rust::RustAuthorityError::ProjectSource { .. }
        | compiler_languages_rust::RustAuthorityError::MissingManifest { .. }
        | compiler_languages_rust::RustAuthorityError::SourceNotFile { .. }
        | compiler_languages_rust::RustAuthorityError::SourceBudget { .. }
        | compiler_languages_rust::RustAuthorityError::SourceRead { .. } => AuthorityPhase::Open,
    }
}

fn rust_class(cause: &compiler_languages_rust::RustAuthorityError) -> AuthorityDiagnosticClass {
    match rust_phase(cause) {
        AuthorityPhase::Resolve => AuthorityDiagnosticClass::Binding,
        AuthorityPhase::TypeCheck => AuthorityDiagnosticClass::Type,
        AuthorityPhase::Project => AuthorityDiagnosticClass::Projection,
        AuthorityPhase::Open | AuthorityPhase::Parse => AuthorityDiagnosticClass::Authority,
    }
}

fn typescript_phase(cause: &compiler_languages_typescript::AuthorityError) -> AuthorityPhase {
    match cause {
        compiler_languages_typescript::AuthorityError::Syntax { .. } => AuthorityPhase::Parse,
        compiler_languages_typescript::AuthorityError::Binding { .. } => AuthorityPhase::Resolve,
    }
}

fn typescript_class(
    cause: &compiler_languages_typescript::AuthorityError,
) -> AuthorityDiagnosticClass {
    match cause {
        compiler_languages_typescript::AuthorityError::Syntax { .. } => {
            AuthorityDiagnosticClass::Syntax
        }
        compiler_languages_typescript::AuthorityError::Binding { .. } => {
            AuthorityDiagnosticClass::Binding
        }
    }
}

fn python_phase(cause: &compiler_languages_python::ExtractionError) -> AuthorityPhase {
    match cause {
        compiler_languages_python::ExtractionError::RejectedSyntax { .. }
        | compiler_languages_python::ExtractionError::NonModuleParse { .. }
        | compiler_languages_python::ExtractionError::InvalidUtf8 { .. } => AuthorityPhase::Parse,
        compiler_languages_python::ExtractionError::SourceLength { .. }
        | compiler_languages_python::ExtractionError::InvalidRange { .. }
        | compiler_languages_python::ExtractionError::MissingFunctionDelimiter { .. } => {
            AuthorityPhase::Project
        }
    }
}

fn python_class(cause: &compiler_languages_python::ExtractionError) -> AuthorityDiagnosticClass {
    match python_phase(cause) {
        AuthorityPhase::Parse => AuthorityDiagnosticClass::Syntax,
        AuthorityPhase::Project => AuthorityDiagnosticClass::Projection,
        AuthorityPhase::Open | AuthorityPhase::Resolve | AuthorityPhase::TypeCheck => {
            AuthorityDiagnosticClass::Authority
        }
    }
}

fn go_phase(cause: &compiler_languages_go::OracleError) -> AuthorityPhase {
    match cause {
        compiler_languages_go::OracleError::Decode { .. } => AuthorityPhase::Parse,
        compiler_languages_go::OracleError::Staleness { .. } => AuthorityPhase::Project,
        compiler_languages_go::OracleError::Spawn { .. }
        | compiler_languages_go::OracleError::ToolingUnavailable { .. }
        | compiler_languages_go::OracleError::Exit { .. }
        | compiler_languages_go::OracleError::OutputLimit { .. }
        | compiler_languages_go::OracleError::Timeout { .. }
        | compiler_languages_go::OracleError::Pipe { .. } => AuthorityPhase::Open,
    }
}

fn go_class(cause: &compiler_languages_go::OracleError) -> AuthorityDiagnosticClass {
    match go_phase(cause) {
        AuthorityPhase::Parse => AuthorityDiagnosticClass::Syntax,
        AuthorityPhase::Project => AuthorityDiagnosticClass::Projection,
        AuthorityPhase::Open | AuthorityPhase::Resolve | AuthorityPhase::TypeCheck => {
            AuthorityDiagnosticClass::Authority
        }
    }
}

fn go_image_phase(cause: &compiler_languages_go::ImageError) -> AuthorityPhase {
    match cause {
        compiler_languages_go::ImageError::Header(_)
        | compiler_languages_go::ImageError::Digest => AuthorityPhase::Parse,
        compiler_languages_go::ImageError::DeclarationKind { .. }
        | compiler_languages_go::ImageError::ExportedFlag { .. }
        | compiler_languages_go::ImageError::DeclarationReserved { .. }
        | compiler_languages_go::ImageError::NameRange { .. }
        | compiler_languages_go::ImageError::NameUtf8 { .. } => AuthorityPhase::Project,
    }
}

fn go_image_class(cause: &compiler_languages_go::ImageError) -> AuthorityDiagnosticClass {
    match go_image_phase(cause) {
        AuthorityPhase::Parse => AuthorityDiagnosticClass::Syntax,
        AuthorityPhase::Project => AuthorityDiagnosticClass::Projection,
        AuthorityPhase::Open | AuthorityPhase::Resolve | AuthorityPhase::TypeCheck => {
            AuthorityDiagnosticClass::Authority
        }
    }
}

fn csharp_phase(_cause: &compiler_languages_csharp::DecodeError) -> AuthorityPhase {
    AuthorityPhase::Parse
}

fn csharp_class(_cause: &compiler_languages_csharp::DecodeError) -> AuthorityDiagnosticClass {
    AuthorityDiagnosticClass::Syntax
}

fn csharp_image_phase(cause: &compiler_languages_csharp::ImageError) -> AuthorityPhase {
    match cause {
        compiler_languages_csharp::ImageError::Header(_)
        | compiler_languages_csharp::ImageError::Digest => AuthorityPhase::Parse,
        compiler_languages_csharp::ImageError::DeclarationKind { .. }
        | compiler_languages_csharp::ImageError::DeclarationReserved { .. }
        | compiler_languages_csharp::ImageError::NameRange { .. }
        | compiler_languages_csharp::ImageError::NameUtf8 { .. }
        | compiler_languages_csharp::ImageError::Span { .. } => AuthorityPhase::Project,
    }
}

fn csharp_image_class(cause: &compiler_languages_csharp::ImageError) -> AuthorityDiagnosticClass {
    match csharp_image_phase(cause) {
        AuthorityPhase::Parse => AuthorityDiagnosticClass::Syntax,
        AuthorityPhase::Project => AuthorityDiagnosticClass::Projection,
        AuthorityPhase::Open | AuthorityPhase::Resolve | AuthorityPhase::TypeCheck => {
            AuthorityDiagnosticClass::Authority
        }
    }
}

fn java_phase(cause: &compiler_languages_java::ImageError) -> AuthorityPhase {
    match cause {
        compiler_languages_java::ImageError::Header(_)
        | compiler_languages_java::ImageError::Section { .. }
        | compiler_languages_java::ImageError::Digest => AuthorityPhase::Parse,
        compiler_languages_java::ImageError::Atom { .. }
        | compiler_languages_java::ImageError::AbsentAtom
        | compiler_languages_java::ImageError::Coordinate { .. }
        | compiler_languages_java::ImageError::Tag { .. }
        | compiler_languages_java::ImageError::ChildRange { .. }
        | compiler_languages_java::ImageError::ReferenceRange { .. }
        | compiler_languages_java::ImageError::DocumentationPresence
        | compiler_languages_java::ImageError::ModifierBits { .. } => AuthorityPhase::Project,
    }
}

fn java_class(cause: &compiler_languages_java::ImageError) -> AuthorityDiagnosticClass {
    match java_phase(cause) {
        AuthorityPhase::Parse => AuthorityDiagnosticClass::Syntax,
        AuthorityPhase::Project => AuthorityDiagnosticClass::Projection,
        AuthorityPhase::Open | AuthorityPhase::Resolve | AuthorityPhase::TypeCheck => {
            AuthorityDiagnosticClass::Authority
        }
    }
}
