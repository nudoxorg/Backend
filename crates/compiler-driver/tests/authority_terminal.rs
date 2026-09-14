//! Exercises exact frontend-authority terminals exposed by `compiler-driver`.
//! Proves profile binding and bounded diagnostic projection never erase a cause.
//! Uses direct closed frontend errors rather than parser or transport fallbacks.

use compiler_driver::{
    AuthorityDiagnostic, AuthorityDiagnosticFault, AuthorityFailure, AuthorityProfileMismatch,
};
use backend_frontend_clang::legacy::CollectError;
use backend_frontend_typescript::legacy::{AuthorityError, with_analysis};
use backend_semantic::vocabulary::{
    AuthorityDiagnosticClass, AuthorityPhase, LanguageProfile, PythonVersion, TypeScriptSource,
};

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error("frontend authority unexpectedly bound to another language profile")]
    Bound,
    #[error("authority profile mismatch changed the requested profile")]
    Profile,
    #[error("authority profile mismatch changed the concrete source error")]
    Failure,
    #[error("bounded authority diagnostic differed from its exact input")]
    Diagnostic,
    #[error("bounded authority diagnostic accepted a prefix longer than its source")]
    PrefixAccepted,
    #[error("bounded authority diagnostic accepted a false truncation bit")]
    TruncationAccepted,
    #[error("OXC accepted syntactically malformed TypeScript")]
    OxcAccepted,
    #[error("OXC authority failure was not a syntax diagnostic")]
    OxcFailure,
    #[error("OXC authority projection lost its parse/syntax classification")]
    OxcProjection,
}

#[test]
fn bounded_authority_diagnostic_preserves_exact_count_and_truncation() -> Result<(), TestError> {
    let Ok(diagnostic) = AuthorityDiagnostic::new(b"first", 11, true) else {
        return Err(TestError::Diagnostic);
    };
    let expected = AuthorityDiagnostic {
        primary: b"first",
        observed: 11,
        truncated: true,
    };
    if diagnostic == expected {
        Ok(())
    } else {
        Err(TestError::Diagnostic)
    }
}

#[test]
fn authority_diagnostic_rejects_inconsistent_transport_claims() -> Result<(), TestError> {
    let prefix = AuthorityDiagnostic::new(b"excess", 5, true);
    let Err(AuthorityDiagnosticFault::PrefixExceedsObserved {
        retained: 6,
        observed: 5,
    }) = prefix
    else {
        return Err(TestError::PrefixAccepted);
    };
    let truncation = AuthorityDiagnostic::new(b"full", 4, true);
    let Err(AuthorityDiagnosticFault::TruncationMismatch {
        retained: 4,
        observed: 4,
        truncated: true,
    }) = truncation
    else {
        return Err(TestError::TruncationAccepted);
    };
    Ok(())
}

#[test]
fn profile_mismatch_retains_the_concrete_frontend_error() -> Result<(), TestError> {
    let error = AuthorityFailure::Clang {
        diagnostic: AuthorityDiagnostic::absent(),
        cause: CollectError::Cancelled,
    };
    let profile = LanguageProfile::Python(PythonVersion::Python314);
    let Err(AuthorityProfileMismatch {
        profile: observed,
        failure,
    }) = error.bind_profile(profile)
    else {
        return Err(TestError::Bound);
    };
    if observed != profile {
        return Err(TestError::Profile);
    }
    let AuthorityFailure::Clang {
        diagnostic:
            AuthorityDiagnostic {
                primary: [],
                observed: 0,
                truncated: false,
            },
        cause: CollectError::Cancelled,
    } = failure
    else {
        return Err(TestError::Failure);
    };
    Ok(())
}

#[test]
fn oxc_syntax_error_projects_through_the_typed_authority_boundary() -> Result<(), TestError> {
    let authority = with_analysis(TypeScriptSource::TypeScript, "const =;", |_| ());
    let Err(cause) = authority else {
        return Err(TestError::OxcAccepted);
    };
    let AuthorityError::Syntax { diagnostics } = cause else {
        return Err(TestError::OxcFailure);
    };
    if diagnostics.is_empty() {
        return Err(TestError::OxcFailure);
    }
    let failure = AuthorityFailure::TypeScript {
        diagnostic: AuthorityDiagnostic::absent(),
        cause: AuthorityError::Syntax { diagnostics },
    };
    let projection = failure.projection();
    if projection.phase != AuthorityPhase::Parse
        || projection.class != AuthorityDiagnosticClass::Syntax
        || projection.diagnostic != AuthorityDiagnostic::absent()
    {
        return Err(TestError::OxcProjection);
    }
    Ok(())
}
