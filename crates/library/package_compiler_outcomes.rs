//! Current, source-bound compiler capture discovery and exact diagnostic reads.
use crate::{
    IndexOperationProfileRefusal, IndexOperationSemanticProfileState as State,
    IndexOperationSemanticUnavailableReason, IndexSourceCaptureSummary, PackageReference,
    ProductAdmissionError, SemanticLanguageProfile,
};
use serde::{Deserialize, Serialize};

/// Immutable selected roots required to resume a compiler outcome read.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageCompilerOutcomeBasis {
    /// Canonical key of the exact local package.
    #[serde(with = "crate::surface::hex_32")]
    pub package_key: [u8; 32],
    /// Selected Project/File relation root.
    #[serde(with = "crate::surface::hex_32")]
    pub source_relation_root: [u8; 32],
    /// Source version recorded by that selected Project.
    #[serde(with = "crate::surface::hex_32")]
    pub source_version: [u8; 32],
    /// Closure-admitted PSC2 capture relation root.
    #[serde(with = "crate::surface::hex_32")]
    pub capture_relation_root: [u8; 32],
}

/// Copies one retained profile diagnostic from the discovered current basis.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageCompilerOutcomeSelector {
    /// Identity of the exact basis returned by package-profile discovery.
    #[serde(with = "crate::surface::hex_32")]
    pub basis: [u8; 32],
    /// Exact closed profile returned by that discovery.
    pub profile: SemanticLanguageProfile,
}

/// Read-only current capture discovery or one copied exact diagnostic read.
/// Explicit reads are independent of registry/packaging metadata resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "selector",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum PackageCompilerOutcomeRead {
    /// Discover every current captured profile and retained diagnostic selector.
    Current,
    /// Copy a diagnostic read returned by current discovery.
    Diagnostic(PackageCompilerOutcomeSelector),
}

impl PackageCompilerOutcomeRead {
    /// Exact diagnostic selector, absent for current discovery.
    #[must_use]
    pub const fn selector(&self) -> Option<&PackageCompilerOutcomeSelector> {
        match self {
            Self::Current => None,
            Self::Diagnostic(selector) => Some(selector),
        }
    }

    /// Checks a read's closed profile and bounded basis before owner dispatch.
    #[must_use]
    pub fn has_admissible_shape(&self) -> bool {
        self.selector()
            .is_none_or(PackageCompilerOutcomeSelector::has_admissible_shape)
    }
}

impl PackageCompilerOutcomeSelector {
    /// Validates bounded identities before an owner or transport call.
    #[must_use]
    pub fn has_admissible_shape(&self) -> bool {
        self.basis.iter().any(|byte| *byte != 0) && self.profile.profile().is_ok()
    }
}

impl PackageCompilerOutcomeBasis {
    /// Commits all fixed-width selected identities, independent of JSON spelling.
    /// Callers copy owner-returned selectors; this is not an authority credential.
    #[must_use]
    pub fn identity(&self) -> [u8; 32] {
        let mut hash = blake3::Hasher::new();
        hash.update(b"nudox.package-compiler-outcome-basis.v1\0");
        for value in [
            self.package_key,
            self.source_relation_root,
            self.source_version,
            self.capture_relation_root,
        ] {
            hash.update(&value);
        }
        *hash.finalize().as_bytes()
    }

    /// Reserved roots and keys cannot select an outcome.
    #[must_use]
    pub fn has_admissible_shape(&self) -> bool {
        [
            self.package_key,
            self.source_relation_root,
            self.source_version,
            self.capture_relation_root,
        ]
        .iter()
        .all(|value| value.iter().any(|byte| *byte != 0))
    }
}

/// Diagnostic availability is separate from the profile's terminal reason.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PackageCompilerDiagnostic {
    /// This capture did not record a typed compiler diagnostic.
    NotRecorded,
    /// A complete diagnostic is retained; copy this selector for an exact read.
    Retained {
        /// Exact bounded follow-up, never an inferred compiler cause.
        read: PackageCompilerOutcomeRead,
    },
}

/// One refused profile in complete current-capture discovery.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageCompilerProfileRefusal {
    /// Closed profile retained by the capture.
    pub profile: SemanticLanguageProfile,
    /// Exact retained semantic refusal category.
    pub reason: IndexOperationSemanticUnavailableReason,
    /// Explicit absence or an exact selector for the complete diagnostic.
    pub diagnostic: PackageCompilerDiagnostic,
}

/// Why current capture discovery or a selected profile read cannot answer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackageCompilerOutcomesUnavailable {
    /// No selected local Project row exists.
    ProjectNotSelected,
    /// The selected closure retains no capture for this package.
    CaptureNotRecorded,
    /// Retained capture rows describe a different source version.
    CaptureNotCurrent,
    /// Selected markers cannot establish one complete current capture.
    CaptureIncomplete,
    /// The selected profile did not end in a compiler refusal.
    ProfileNotRefused,
    /// The refused profile retained no typed compiler diagnostic.
    DiagnosticNotRecorded,
}

/// Bounded discovery or one full diagnostic, under the same selected roots.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PackageCompilerOutcomes {
    /// Every current profile and every retained diagnostic selector. Full
    /// diagnostics are read individually so sixteen 8 KiB facts cannot block discovery.
    Summary {
        /// Exact selected source and capture roots.
        basis: PackageCompilerOutcomeBasis,
        /// Existing complete source/input/observation and state tuples.
        source_capture: IndexSourceCaptureSummary,
        /// Every failed/unavailable profile, in canonical order.
        refusals: Box<[PackageCompilerProfileRefusal]>,
    },
    /// One entire typed diagnostic, with the complete current profile basis.
    Diagnostic {
        /// Exact selected source/capture identities committed by the selector.
        basis: PackageCompilerOutcomeBasis,
        /// Exact copied discovery selector.
        selector: PackageCompilerOutcomeSelector,
        /// The current captured profile set; never a prior generation substitute.
        source_capture: IndexSourceCaptureSummary,
        /// Existing typed profile/refusal/optional compiler diagnostic.
        refusal: IndexOperationProfileRefusal,
    },
    /// The requested selection no longer matches the current source/capture roots.
    Stale {
        /// Exact local package requested.
        package: PackageReference,
    },
    /// No current compiler outcome was established.
    Unavailable {
        /// Exact local package requested.
        package: PackageReference,
        /// Stable reason, without inventing a compiler cause.
        reason: PackageCompilerOutcomesUnavailable,
    },
}

impl PackageCompilerOutcomes {
    /// The exact package whose current capture was read.
    #[must_use]
    pub fn package(&self) -> &PackageReference {
        match self {
            Self::Summary { source_capture, .. } | Self::Diagnostic { source_capture, .. } => {
                &source_capture.producer_package
            }
            Self::Stale { package } | Self::Unavailable { package, .. } => package,
        }
    }

    /// Correlates this closed result with the complete request, not just its shape.
    pub fn admit_request(
        &self,
        package: &PackageReference,
        read: Option<&PackageCompilerOutcomeRead>,
    ) -> Result<(), ProductAdmissionError> {
        self.admit()?;
        let selector = read.and_then(PackageCompilerOutcomeRead::selector);
        if read.is_some_and(|read| !read.has_admissible_shape())
            || self.package() != package
            || match self {
                Self::Summary { .. } => selector.is_some(),
                Self::Diagnostic {
                    selector: observed, ..
                } => selector != Some(observed),
                _ => false,
            }
        {
            return Err(ProductAdmissionError::IndexOperationShape);
        }
        Ok(())
    }

    /// Validates complete source tuples and the exact refusal partition.
    pub fn admit(&self) -> Result<(), ProductAdmissionError> {
        let (basis, capture) = match self {
            Self::Summary {
                basis,
                source_capture,
                ..
            } => (basis, source_capture),
            Self::Diagnostic {
                basis,
                source_capture,
                ..
            } => (basis, source_capture),
            Self::Stale { package } | Self::Unavailable { package, .. } => {
                return if matches!(package, PackageReference::Local(_)) {
                    Ok(())
                } else {
                    Err(ProductAdmissionError::IndexOperationShape)
                };
            }
        };
        capture.admit()?;
        if !basis.has_admissible_shape()
            || !matches!(&capture.producer_package, PackageReference::Local(_))
            || basis.package_key != crate::package_key(capture.producer_package.as_str()).to_bytes()
            || capture
                .profiles
                .iter()
                .any(|profile| profile.source_version != basis.source_version)
        {
            return Err(ProductAdmissionError::IndexOperationShape);
        }
        match self {
            Self::Summary { refusals, .. } => {
                let refused: Vec<_> = capture
                    .profiles
                    .iter()
                    .filter_map(|profile| match profile.state {
                        State::Unavailable { reason } | State::Failed { reason, .. } => {
                            Some((profile.profile, reason))
                        }
                        _ => None,
                    })
                    .collect();
                if refused.len() != refusals.len()
                    || refused
                        .iter()
                        .zip(refusals.iter())
                        .any(|((profile, reason), refusal)| {
                            *profile != refusal.profile
                                || *reason != refusal.reason
                                || match &refusal.diagnostic {
                                    PackageCompilerDiagnostic::NotRecorded => false,
                                    PackageCompilerDiagnostic::Retained {
                                        read: PackageCompilerOutcomeRead::Diagnostic(read),
                                    } => {
                                        *reason != IndexOperationSemanticUnavailableReason::Rejected
                                            || read.basis != basis.identity()
                                            || read.profile != *profile
                                            || !read.has_admissible_shape()
                                    }
                                    PackageCompilerDiagnostic::Retained { .. } => true,
                                }
                        })
                {
                    return Err(ProductAdmissionError::IndexOperationShape);
                }
            }
            Self::Diagnostic {
                selector, refusal, ..
            } => {
                if selector.basis != basis.identity() || refusal.compiler_failure.is_none() {
                    return Err(ProductAdmissionError::IndexOperationShape);
                }
                let profile = capture
                    .profiles
                    .iter()
                    .find(|profile| profile.profile == selector.profile)
                    .ok_or(ProductAdmissionError::IndexOperationShape)?;
                refusal.admit_for(profile)?;
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "package_compiler_outcomes_tests.rs"]
mod tests;
