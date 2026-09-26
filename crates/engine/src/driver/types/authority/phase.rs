//! Per-language phase and class for one exact frontend error.
//!
//! Failure types stay in the parent. Projection calls these classifiers.

use super::{AuthorityDiagnosticClass, AuthorityPhase};

/// Returns the typed phase for one Clang collect error.
pub(super) fn clang_phase(cause: &backend_frontend_clang::legacy::CollectError) -> AuthorityPhase {
    match cause {
        backend_frontend_clang::legacy::CollectError::Parse { .. }
        | backend_frontend_clang::legacy::CollectError::SourceContainsNul
        | backend_frontend_clang::legacy::CollectError::SourceTooLarge { .. }
        | backend_frontend_clang::legacy::CollectError::SourceLengthTooLarge { .. } => {
            AuthorityPhase::Parse
        }
        backend_frontend_clang::legacy::CollectError::ScratchCapacity { .. }
        | backend_frontend_clang::legacy::CollectError::CoordinateTooLarge { .. }
        | backend_frontend_clang::legacy::CollectError::SlotOrdinalTooLarge { .. }
        | backend_frontend_clang::legacy::CollectError::SlotCountOverflow { .. } => {
            AuthorityPhase::Project
        }
        backend_frontend_clang::legacy::CollectError::Cancelled
        | backend_frontend_clang::legacy::CollectError::Library(_)
        | backend_frontend_clang::legacy::CollectError::MissingApi { .. }
        | backend_frontend_clang::legacy::CollectError::IndexUnavailable
        | backend_frontend_clang::legacy::CollectError::MainFileUnavailable => AuthorityPhase::Open,
    }
}

/// Returns the diagnostic class for one Clang collect error.
pub(super) fn clang_class(
    cause: &backend_frontend_clang::legacy::CollectError,
) -> AuthorityDiagnosticClass {
    match clang_phase(cause) {
        AuthorityPhase::Parse => AuthorityDiagnosticClass::Syntax,
        AuthorityPhase::Project => AuthorityDiagnosticClass::Projection,
        AuthorityPhase::Open | AuthorityPhase::Resolve | AuthorityPhase::TypeCheck => {
            AuthorityDiagnosticClass::Authority
        }
    }
}

/// Returns the typed phase for one Rust authority error.
pub(super) fn rust_phase(
    cause: &backend_frontend_rust::legacy::RustAuthorityError,
) -> AuthorityPhase {
    match cause {
        backend_frontend_rust::legacy::RustAuthorityError::Workspace { .. }
        | backend_frontend_rust::legacy::RustAuthorityError::SourceNotLoaded { .. }
        | backend_frontend_rust::legacy::RustAuthorityError::EditionMismatch { .. }
        | backend_frontend_rust::legacy::RustAuthorityError::MissingSemanticFact { .. } => {
            AuthorityPhase::Resolve
        }
        backend_frontend_rust::legacy::RustAuthorityError::UnresolvedInferredType => {
            AuthorityPhase::TypeCheck
        }
        backend_frontend_rust::legacy::RustAuthorityError::InvalidSpan { .. }
        | backend_frontend_rust::legacy::RustAuthorityError::Coordinate { .. }
        | backend_frontend_rust::legacy::RustAuthorityError::Admission { .. } => {
            AuthorityPhase::Project
        }
        backend_frontend_rust::legacy::RustAuthorityError::SourceBinding { .. } => {
            AuthorityPhase::Parse
        }
        backend_frontend_rust::legacy::RustAuthorityError::Cancelled
        | backend_frontend_rust::legacy::RustAuthorityError::DeadlineExceeded
        | backend_frontend_rust::legacy::RustAuthorityError::Toolchain(_)
        | backend_frontend_rust::legacy::RustAuthorityError::ProjectRoot { .. }
        | backend_frontend_rust::legacy::RustAuthorityError::ProjectSource { .. }
        | backend_frontend_rust::legacy::RustAuthorityError::MissingManifest { .. }
        | backend_frontend_rust::legacy::RustAuthorityError::SourceNotFile { .. }
        | backend_frontend_rust::legacy::RustAuthorityError::SourceBudget { .. }
        | backend_frontend_rust::legacy::RustAuthorityError::SourceRead { .. } => {
            AuthorityPhase::Open
        }
    }
}

/// Returns the diagnostic class for one Rust authority error.
pub(super) fn rust_class(
    cause: &backend_frontend_rust::legacy::RustAuthorityError,
) -> AuthorityDiagnosticClass {
    match rust_phase(cause) {
        AuthorityPhase::Resolve => AuthorityDiagnosticClass::Binding,
        AuthorityPhase::TypeCheck => AuthorityDiagnosticClass::Type,
        AuthorityPhase::Project => AuthorityDiagnosticClass::Projection,
        AuthorityPhase::Open | AuthorityPhase::Parse => AuthorityDiagnosticClass::Authority,
    }
}

/// Returns the typed phase for one TypeScript authority error.
pub(super) fn typescript_phase(
    cause: &backend_frontend_typescript::legacy::AuthorityError,
) -> AuthorityPhase {
    match cause {
        backend_frontend_typescript::legacy::AuthorityError::Syntax { .. } => AuthorityPhase::Parse,
        backend_frontend_typescript::legacy::AuthorityError::Binding { .. } => {
            AuthorityPhase::Resolve
        }
        // A checker authority rejection is the type-checking phase by
        // definition: the checker ran and rejected the transaction.
        backend_frontend_typescript::legacy::AuthorityError::Checker { .. } => {
            AuthorityPhase::TypeCheck
        }
    }
}

/// Returns the diagnostic class for one TypeScript authority error.
pub(super) fn typescript_class(
    cause: &backend_frontend_typescript::legacy::AuthorityError,
) -> AuthorityDiagnosticClass {
    match cause {
        backend_frontend_typescript::legacy::AuthorityError::Syntax { .. } => {
            AuthorityDiagnosticClass::Syntax
        }
        backend_frontend_typescript::legacy::AuthorityError::Binding { .. } => {
            AuthorityDiagnosticClass::Binding
        }
        // A checker rejection classifies as a type diagnostic.
        backend_frontend_typescript::legacy::AuthorityError::Checker { .. } => {
            AuthorityDiagnosticClass::Type
        }
    }
}

/// Returns the typed phase for one Python extraction error.
pub(super) fn python_phase(
    cause: &backend_frontend_python::legacy::ExtractionError,
) -> AuthorityPhase {
    match cause {
        backend_frontend_python::legacy::ExtractionError::RejectedSyntax { .. }
        | backend_frontend_python::legacy::ExtractionError::NonModuleParse { .. }
        | backend_frontend_python::legacy::ExtractionError::InvalidUtf8 { .. } => {
            AuthorityPhase::Parse
        }
        backend_frontend_python::legacy::ExtractionError::SourceLength { .. }
        | backend_frontend_python::legacy::ExtractionError::InvalidRange { .. } => {
            AuthorityPhase::Project
        }
    }
}

/// Returns the diagnostic class for one Python extraction error.
pub(super) fn python_class(
    cause: &backend_frontend_python::legacy::ExtractionError,
) -> AuthorityDiagnosticClass {
    match python_phase(cause) {
        AuthorityPhase::Parse => AuthorityDiagnosticClass::Syntax,
        AuthorityPhase::Project => AuthorityDiagnosticClass::Projection,
        AuthorityPhase::Open | AuthorityPhase::Resolve | AuthorityPhase::TypeCheck => {
            AuthorityDiagnosticClass::Authority
        }
    }
}

/// Returns the typed phase for one Go oracle error.
pub(super) fn go_phase(cause: &backend_frontend_go::legacy::OracleError) -> AuthorityPhase {
    match cause {
        backend_frontend_go::legacy::OracleError::Decode { .. } => AuthorityPhase::Parse,
        backend_frontend_go::legacy::OracleError::Staleness { .. } => AuthorityPhase::Project,
        backend_frontend_go::legacy::OracleError::Spawn { .. }
        | backend_frontend_go::legacy::OracleError::ToolingUnavailable { .. }
        | backend_frontend_go::legacy::OracleError::Exit { .. }
        | backend_frontend_go::legacy::OracleError::OutputLimit { .. }
        | backend_frontend_go::legacy::OracleError::Timeout { .. }
        | backend_frontend_go::legacy::OracleError::Pipe { .. }
        | backend_frontend_go::legacy::OracleError::WorkerPanic { .. } => AuthorityPhase::Open,
    }
}

/// Returns the diagnostic class for one Go oracle error.
pub(super) fn go_class(
    cause: &backend_frontend_go::legacy::OracleError,
) -> AuthorityDiagnosticClass {
    match go_phase(cause) {
        AuthorityPhase::Parse => AuthorityDiagnosticClass::Syntax,
        AuthorityPhase::Project => AuthorityDiagnosticClass::Projection,
        AuthorityPhase::Open | AuthorityPhase::Resolve | AuthorityPhase::TypeCheck => {
            AuthorityDiagnosticClass::Authority
        }
    }
}

/// Returns the typed phase for one Go image error.
pub(super) fn go_image_phase(cause: &backend_frontend_go::legacy::ImageError) -> AuthorityPhase {
    match cause {
        backend_frontend_go::legacy::ImageError::Header(_)
        | backend_frontend_go::legacy::ImageError::Digest => AuthorityPhase::Parse,
        _ => AuthorityPhase::Project,
    }
}

/// Returns the diagnostic class for one Go image error.
pub(super) fn go_image_class(
    cause: &backend_frontend_go::legacy::ImageError,
) -> AuthorityDiagnosticClass {
    match go_image_phase(cause) {
        AuthorityPhase::Parse => AuthorityDiagnosticClass::Syntax,
        AuthorityPhase::Project => AuthorityDiagnosticClass::Projection,
        AuthorityPhase::Open | AuthorityPhase::Resolve | AuthorityPhase::TypeCheck => {
            AuthorityDiagnosticClass::Authority
        }
    }
}

/// Returns the typed phase for one C# decode error.
pub(super) fn csharp_phase(
    _cause: &backend_frontend_csharp::legacy::DecodeError,
) -> AuthorityPhase {
    AuthorityPhase::Parse
}

/// Returns the diagnostic class for one C# decode error.
pub(super) fn csharp_class(
    _cause: &backend_frontend_csharp::legacy::DecodeError,
) -> AuthorityDiagnosticClass {
    AuthorityDiagnosticClass::Syntax
}

/// Returns the typed phase for one C# image error.
pub(super) fn csharp_image_phase(
    cause: &backend_frontend_csharp::legacy::ImageError,
) -> AuthorityPhase {
    match cause {
        backend_frontend_csharp::legacy::ImageError::Header(_)
        | backend_frontend_csharp::legacy::ImageError::Digest => AuthorityPhase::Parse,
        backend_frontend_csharp::legacy::ImageError::DeclarationKind { .. }
        | backend_frontend_csharp::legacy::ImageError::DeclarationReserved { .. }
        | backend_frontend_csharp::legacy::ImageError::NameRange { .. }
        | backend_frontend_csharp::legacy::ImageError::NameUtf8 { .. }
        | backend_frontend_csharp::legacy::ImageError::Span { .. }
        | backend_frontend_csharp::legacy::ImageError::TypeChildCount { .. } => {
            AuthorityPhase::Project
        }
    }
}

/// Returns the diagnostic class for one C# image error.
pub(super) fn csharp_image_class(
    cause: &backend_frontend_csharp::legacy::ImageError,
) -> AuthorityDiagnosticClass {
    match csharp_image_phase(cause) {
        AuthorityPhase::Parse => AuthorityDiagnosticClass::Syntax,
        AuthorityPhase::Project => AuthorityDiagnosticClass::Projection,
        AuthorityPhase::Open | AuthorityPhase::Resolve | AuthorityPhase::TypeCheck => {
            AuthorityDiagnosticClass::Authority
        }
    }
}

/// Returns the typed phase for one Java image error.
pub(super) fn java_phase(cause: &backend_frontend_java::legacy::ImageError) -> AuthorityPhase {
    match cause {
        backend_frontend_java::legacy::ImageError::Header(_)
        | backend_frontend_java::legacy::ImageError::Section { .. }
        | backend_frontend_java::legacy::ImageError::Digest => AuthorityPhase::Parse,
        backend_frontend_java::legacy::ImageError::Atom { .. }
        | backend_frontend_java::legacy::ImageError::AbsentAtom
        | backend_frontend_java::legacy::ImageError::Coordinate { .. }
        | backend_frontend_java::legacy::ImageError::Tag { .. }
        | backend_frontend_java::legacy::ImageError::ChildRange { .. }
        | backend_frontend_java::legacy::ImageError::ReferenceRange { .. }
        | backend_frontend_java::legacy::ImageError::DocumentationPresence
        | backend_frontend_java::legacy::ImageError::ModifierBits { .. }
        | backend_frontend_java::legacy::ImageError::RecordComponentKind { .. }
        | backend_frontend_java::legacy::ImageError::ExtensionReserved => AuthorityPhase::Project,
    }
}

/// Returns the diagnostic class for one Java image error.
pub(super) fn java_class(
    cause: &backend_frontend_java::legacy::ImageError,
) -> AuthorityDiagnosticClass {
    match java_phase(cause) {
        AuthorityPhase::Parse => AuthorityDiagnosticClass::Syntax,
        AuthorityPhase::Project => AuthorityDiagnosticClass::Projection,
        AuthorityPhase::Open | AuthorityPhase::Resolve | AuthorityPhase::TypeCheck => {
            AuthorityDiagnosticClass::Authority
        }
    }
}

/// Returns the typed phase for one bound Java image error.
pub(super) fn java_bound_phase(
    cause: &backend_frontend_java::legacy::BoundImageError,
) -> AuthorityPhase {
    match cause {
        backend_frontend_java::legacy::BoundImageError::Header(_)
        | backend_frontend_java::legacy::BoundImageError::Digest => AuthorityPhase::Parse,
        backend_frontend_java::legacy::BoundImageError::Image(cause) => java_phase(cause),
    }
}

/// Returns the diagnostic class for one bound Java image error.
pub(super) fn java_bound_class(
    cause: &backend_frontend_java::legacy::BoundImageError,
) -> AuthorityDiagnosticClass {
    match java_bound_phase(cause) {
        AuthorityPhase::Parse => AuthorityDiagnosticClass::Syntax,
        AuthorityPhase::Project => AuthorityDiagnosticClass::Projection,
        AuthorityPhase::Open | AuthorityPhase::Resolve | AuthorityPhase::TypeCheck => {
            AuthorityDiagnosticClass::Authority
        }
    }
}
