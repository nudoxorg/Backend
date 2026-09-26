//! Maps one language collector failure onto the portable compile terminal.
//!
//! Each language keeps its own authority, checker, and projection cause.
//! The shared lowering rejection is still proved by the parent compile owner.

use backend_semantic::ir::PrepareError;

use super::{
    AuthorityDiagnostic, AuthorityDiagnosticFault, AuthorityFailure, CompileFailure,
    CompileRecipeFact, SourceIdentity,
};
use crate::driver::lower::{self, AdmissionFault, typescript::TypeScriptCollectError};

pub(super) fn python_terminal<'diagnostic>(
    source_identity: SourceIdentity,
    recipe: CompileRecipeFact,
    cause: lower::python::PythonCollectError,
) -> CompileFailure<'diagnostic> {
    match cause {
        lower::python::PythonCollectError::Authority(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::Python {
                diagnostic: AuthorityDiagnostic::absent(),
                cause,
            },
        },
        lower::python::PythonCollectError::Checker(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::PythonChecker {
                diagnostic: AuthorityDiagnostic::absent(),
                cause,
            },
        },
        lower::python::PythonCollectError::Rejected(rejected) => {
            CompileFailure::LoweringUnsupported {
                source_identity,
                recipe,
                cause: super::rejected_lowering(rejected),
            }
        }
        lower::python::PythonCollectError::Projection(fault) => {
            CompileFailure::LoweringUnsupported {
                source_identity,
                recipe,
                cause: backend_semantic::vocabulary::LoweringUnsupported::PythonProjection {
                    fault,
                },
            }
        }
        lower::python::PythonCollectError::Lowering(cause) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause,
        },
        lower::python::PythonCollectError::Span { start, end } => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::PythonSpan {
                diagnostic: AuthorityDiagnostic::absent(),
                start,
                end,
            },
        },
    }
}

pub(super) fn rust_terminal<'diagnostic>(
    source_identity: SourceIdentity,
    recipe: CompileRecipeFact,
    cause: lower::rust::RustCollectError,
) -> CompileFailure<'diagnostic> {
    match cause {
        lower::rust::RustCollectError::Authority(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::Rust {
                diagnostic: AuthorityDiagnostic::absent(),
                cause,
            },
        },
        lower::rust::RustCollectError::Lowering(cause) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause,
        },
    }
}

pub(super) fn go_terminal<'diagnostic>(
    source_identity: SourceIdentity,
    recipe: CompileRecipeFact,
    cause: lower::go::GoCollectError,
) -> CompileFailure<'diagnostic> {
    match cause {
        lower::go::GoCollectError::Image(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::GoImage {
                diagnostic: AuthorityDiagnostic::absent(),
                cause,
            },
        },
        lower::go::GoCollectError::SourceBinding { expected, observed } => {
            CompileFailure::Authority {
                source_identity,
                recipe,
                failure: AuthorityFailure::GoSourceBinding {
                    diagnostic: AuthorityDiagnostic::absent(),
                    expected,
                    observed,
                },
            }
        }
        lower::go::GoCollectError::Rejected(rejected) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause: super::rejected_lowering(rejected),
        },
        lower::go::GoCollectError::Lowering(cause) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause,
        },
    }
}

pub(super) fn csharp_terminal<'diagnostic>(
    source_identity: SourceIdentity,
    recipe: CompileRecipeFact,
    cause: lower::csharp::CSharpCollectError,
) -> CompileFailure<'diagnostic> {
    match cause {
        lower::csharp::CSharpCollectError::Image(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::CSharpImage {
                diagnostic: AuthorityDiagnostic::absent(),
                cause,
            },
        },
        lower::csharp::CSharpCollectError::SourceBinding { expected, observed } => {
            CompileFailure::Authority {
                source_identity,
                recipe,
                failure: AuthorityFailure::CSharpSourceBinding {
                    diagnostic: AuthorityDiagnostic::absent(),
                    expected,
                    observed,
                },
            }
        }
        lower::csharp::CSharpCollectError::Span { start, end } => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::CSharpSpan {
                diagnostic: AuthorityDiagnostic::absent(),
                start,
                end,
            },
        },
        lower::csharp::CSharpCollectError::Rejected(rejected) => {
            CompileFailure::LoweringUnsupported {
                source_identity,
                recipe,
                cause: super::rejected_lowering(rejected),
            }
        }
        lower::csharp::CSharpCollectError::Lowering(cause) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause,
        },
    }
}

pub(super) fn java_terminal<'diagnostic>(
    source_identity: SourceIdentity,
    recipe: CompileRecipeFact,
    cause: lower::java::JavaCollectError,
) -> CompileFailure<'diagnostic> {
    match cause {
        lower::java::JavaCollectError::Image(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::JavaBoundImage {
                diagnostic: AuthorityDiagnostic::absent(),
                cause,
            },
        },
        lower::java::JavaCollectError::Release {
            requested,
            observed,
        } => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::JavaRelease {
                diagnostic: AuthorityDiagnostic::absent(),
                requested,
                observed,
            },
        },
        lower::java::JavaCollectError::SourceBinding { expected, observed } => {
            CompileFailure::Authority {
                source_identity,
                recipe,
                failure: AuthorityFailure::JavaSourceBinding {
                    diagnostic: AuthorityDiagnostic::absent(),
                    expected,
                    observed,
                },
            }
        }
        lower::java::JavaCollectError::Rejected(rejected) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause: super::rejected_lowering(rejected),
        },
        lower::java::JavaCollectError::Lowering(cause) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause,
        },
    }
}

pub(super) fn clang_terminal<'diagnostic>(
    source_identity: SourceIdentity,
    recipe: CompileRecipeFact,
    cause: lower::clang::ClangCollectError,
) -> CompileFailure<'diagnostic> {
    match cause {
        lower::clang::ClangCollectError::Authority(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::Clang {
                diagnostic: AuthorityDiagnostic::absent(),
                cause,
            },
        },
        lower::clang::ClangCollectError::Rejected(rejected) => {
            CompileFailure::LoweringUnsupported {
                source_identity,
                recipe,
                cause: super::rejected_lowering(rejected),
            }
        }
        lower::clang::ClangCollectError::Projection(fault) => CompileFailure::ClangProjection {
            source_identity,
            recipe,
            fault,
        },
        lower::clang::ClangCollectError::Lowering(cause) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause,
        },
        lower::clang::ClangCollectError::Admission(cause) => match cause {
            AdmissionFault::Canonical(cause) => CompileFailure::Prepare {
                source_identity,
                recipe,
                cause: PrepareError::SemanticData { cause },
            },
            AdmissionFault::Prepare(cause) => CompileFailure::Prepare {
                source_identity,
                recipe,
                cause,
            },
            AdmissionFault::Write(cause) => CompileFailure::Write {
                source_identity,
                recipe,
                cause,
            },
            AdmissionFault::ExtensionAtom {
                row,
                provisional,
                atom_count,
            } => CompileFailure::ExtensionAtomUnbound {
                source_identity,
                recipe,
                row,
                provisional,
                atom_count,
            },
            AdmissionFault::ExtensionTypeParameters {
                row,
                start,
                length,
                element_count,
            } => CompileFailure::ExtensionTypeParametersUnbound {
                source_identity,
                recipe,
                row,
                start,
                length,
                element_count,
            },
        },
    }
}

pub(super) fn typescript_terminal<'diagnostic>(
    diagnostic_output: Option<&'diagnostic mut [u8]>,
    source: &[u8],
    source_identity: SourceIdentity,
    recipe: CompileRecipeFact,
    cause: TypeScriptCollectError,
) -> CompileFailure<'diagnostic> {
    match cause {
        TypeScriptCollectError::Utf8(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::TypeScriptUtf8 {
                diagnostic: AuthorityDiagnostic::absent(),
                cause,
            },
        },
        TypeScriptCollectError::Authority(cause) => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::TypeScript {
                diagnostic: typescript_diagnostic(diagnostic_output, source, &cause),
                cause,
            },
        },
        TypeScriptCollectError::Rejected(rejected) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause: super::rejected_lowering(rejected),
        },
        TypeScriptCollectError::Projection(fault) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause: backend_semantic::vocabulary::LoweringUnsupported::TypeScriptProjection {
                fault,
            },
        },
        TypeScriptCollectError::Span { start, end } => CompileFailure::Authority {
            source_identity,
            recipe,
            failure: AuthorityFailure::TypeScriptSpan {
                diagnostic: AuthorityDiagnostic::absent(),
                start,
                end,
            },
        },
        TypeScriptCollectError::Lowering(cause) => CompileFailure::LoweringUnsupported {
            source_identity,
            recipe,
            cause,
        },
    }
}

fn typescript_diagnostic<'diagnostic>(
    output: Option<&'diagnostic mut [u8]>,
    source: &[u8],
    cause: &backend_frontend_typescript::legacy::AuthorityError,
) -> AuthorityDiagnostic<'diagnostic> {
    let Some(span) = cause.primary_span() else {
        return AuthorityDiagnostic::absent();
    };
    let Ok(start) = usize::try_from(span.start) else {
        return AuthorityDiagnostic::absent();
    };
    let Ok(end) = usize::try_from(span.end) else {
        return AuthorityDiagnostic::absent();
    };
    let Some(primary) = source.get(start..end) else {
        return AuthorityDiagnostic::absent();
    };
    let Some(output) = output else {
        return AuthorityDiagnostic::absent();
    };
    let retained = primary.len().min(output.len());
    let (Some(source), Some(destination)) = (primary.get(..retained), output.get_mut(..retained))
    else {
        return AuthorityDiagnostic::absent();
    };
    destination.copy_from_slice(source);
    match AuthorityDiagnostic::new(destination, primary.len(), retained != primary.len()) {
        Ok(diagnostic) => diagnostic,
        Err(AuthorityDiagnosticFault::PrefixExceedsObserved { .. })
        | Err(AuthorityDiagnosticFault::TruncationMismatch { .. }) => AuthorityDiagnostic::absent(),
    }
}
