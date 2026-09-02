//! Exercises exact frontend-authority terminals exposed by `compiler-driver`.
//! Proves profile binding and bounded diagnostic projection never erase a cause.
//! Uses direct closed frontend errors rather than parser or transport fallbacks.

use compiler_driver::{
    AuthorityDiagnostic, AuthorityDiagnosticFault, AuthorityProfileMismatch, FrontendAuthorityError,
};
use compiler_languages_clang::CollectError;
use compiler_vocabulary::{Language, LanguageProfile, PythonVersion};

#[derive(Debug, thiserror::Error)]
enum TestError {
    #[error("frontend authority unexpectedly bound to another language profile")]
    Bound,
    #[error("authority profile mismatch changed the requested profile")]
    Profile,
    #[error("authority profile mismatch changed the source language")]
    Language,
    #[error("authority profile mismatch changed the concrete source error")]
    Source,
}

#[test]
fn bounded_authority_diagnostic_preserves_exact_count_and_truncation() {
    let diagnostic = AuthorityDiagnostic::new(b"first", 11, true);
    assert_eq!(
        diagnostic,
        Ok(AuthorityDiagnostic {
            primary: b"first",
            observed: 11,
            truncated: true,
        })
    );
}

#[test]
fn authority_diagnostic_rejects_inconsistent_transport_claims() {
    assert_eq!(
        AuthorityDiagnostic::new(b"excess", 5, true),
        Err(AuthorityDiagnosticFault::PrefixExceedsObserved {
            retained: 6,
            observed: 5,
        })
    );
    assert_eq!(
        AuthorityDiagnostic::new(b"full", 4, true),
        Err(AuthorityDiagnosticFault::TruncationMismatch {
            retained: 4,
            observed: 4,
            truncated: true,
        })
    );
}

#[test]
fn profile_mismatch_retains_the_concrete_frontend_error() -> Result<(), TestError> {
    let error = FrontendAuthorityError::Clang(CollectError::Cancelled);
    let profile = LanguageProfile::Python(PythonVersion::Python314);
    let Err(AuthorityProfileMismatch {
        profile: observed,
        actual,
        source,
    }) = error.bind_profile(profile)
    else {
        return Err(TestError::Bound);
    };
    if observed != profile {
        return Err(TestError::Profile);
    }
    if actual != Language::Clang {
        return Err(TestError::Language);
    }
    if !matches!(
        source,
        FrontendAuthorityError::Clang(CollectError::Cancelled)
    ) {
        return Err(TestError::Source);
    }
    Ok(())
}
