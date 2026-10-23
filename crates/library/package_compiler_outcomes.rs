//! Current, source-bound compiler capture discovery and exact diagnostic reads.
use crate::{
    IndexOperationProfileRefusal, IndexOperationSemanticProfileState as State,
    IndexOperationSemanticUnavailableReason, IndexSourceCaptureSummary, PackageReference,
    ProductAdmissionError, SemanticLanguageProfile,
};
use serde::{Deserialize, Serialize};

/// Maximum number of immutable coverage observations returned in one page.
pub const MAX_PACKAGE_COMPILER_COVERAGE_PAGE_ROWS: usize = 25;
/// Maximum serialized size of one immutable coverage page.
pub const MAX_PACKAGE_COMPILER_COVERAGE_PAGE_BYTES: usize = 4096;

/// Public compiler coverage row contract, shared unchanged with authenticated IR storage.
pub use backend_semantic::ir::SourceCoverageRow as PackageCompilerCoverageRow;
/// Cause carried by a typed compiler coverage gap row.
pub use backend_semantic::ir::SourceCoverageGapCause as PackageCompilerCoverageGapCause;
/// Optional source-bound diagnostic carried by a typed compiler coverage gap row.
pub use backend_semantic::ir::SourceCoverageGapDiagnostic as PackageCompilerCoverageGapDiagnostic;

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

/// Exact resident-view publication roots authorizing a package coverage read.
///
/// The owner issues this binding only when the requested view is still
/// resident and its source, semantic-publication, and capture roots are the
/// exact roots recorded when that view was published.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageCoverageBinding {
    /// Exact package row identity in the bound view.
    #[serde(with = "crate::surface::hex_32")]
    pub package_key: [u8; 32],
    /// Exact current product view revision.
    #[serde(with = "crate::surface::hex_32")]
    pub view_root: [u8; 32],
    /// Source relation root used to publish the view.
    #[serde(with = "crate::surface::hex_32")]
    pub source_relation_root: [u8; 32],
    /// Semantic publication relation root used to publish the view.
    #[serde(with = "crate::surface::hex_32")]
    pub semantic_relation_root: [u8; 32],
    /// Selected source-capture relation root used to publish the view.
    #[serde(with = "crate::surface::hex_32")]
    pub capture_relation_root: [u8; 32],
}

impl PackageCoverageBinding {
    /// Returns whether every identity is present and usable.
    #[must_use]
    pub fn has_admissible_shape(&self) -> bool {
        [
            self.package_key,
            self.view_root,
            self.source_relation_root,
            self.semantic_relation_root,
            self.capture_relation_root,
        ]
        .iter()
        .all(|value| value.iter().any(|byte| *byte != 0))
    }
}

/// Owner response to an exact package-coverage request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PackageCoverageCurrentResult {
    /// Profile evidence from the exact published view and relation roots.
    Current {
        /// Owner-issued publication binding.
        binding: PackageCoverageBinding,
        /// Complete bounded selected profile summary.
        outcomes: PackageCompilerOutcomes,
    },
    /// Requested view no longer matches the owner's resident publication.
    Stale {
        /// Exact package requested.
        package: PackageReference,
        /// Exact view root supplied by the caller.
        #[serde(with = "crate::surface::hex_32")]
        view_root: [u8; 32],
    },
    /// Exact published view is current, but no usable profile summary exists.
    Unavailable {
        /// Exact package requested.
        package: PackageReference,
        /// Exact view root supplied by the caller.
        #[serde(with = "crate::surface::hex_32")]
        view_root: [u8; 32],
        /// Typed reason no summary could be returned.
        reason: PackageCompilerOutcomesUnavailable,
    },
}

impl PackageCoverageCurrentResult {
    /// Exact package carried by this bounded result.
    #[must_use]
    pub fn package(&self) -> &PackageReference {
        match self {
            Self::Current { outcomes, .. } => outcomes.package(),
            Self::Stale { package, .. } | Self::Unavailable { package, .. } => package,
        }
    }

    /// Exact requested view root carried by this bounded result.
    #[must_use]
    pub const fn view_root(&self) -> [u8; 32] {
        match self {
            Self::Current { binding, .. } => binding.view_root,
            Self::Stale { view_root, .. } | Self::Unavailable { view_root, .. } => *view_root,
        }
    }

    /// Correlates the result with the package and view requested by the caller.
    pub fn admit_request(
        &self,
        package: &PackageReference,
        view_root: [u8; 32],
    ) -> Result<(), ProductAdmissionError> {
        match self {
            Self::Stale {
                package: observed,
                view_root: observed_root,
            } if observed == package
                && *observed_root == view_root
                && matches!(observed, PackageReference::Local(_))
                && view_root.iter().any(|byte| *byte != 0) =>
            {
                Ok(())
            }
            Self::Unavailable {
                package: observed,
                view_root: observed_root,
                ..
            } if observed == package
                && *observed_root == view_root
                && matches!(observed, PackageReference::Local(_))
                && view_root.iter().any(|byte| *byte != 0) =>
            {
                Ok(())
            }
            Self::Current { binding, outcomes }
                if binding.has_admissible_shape()
                    && binding.package_key == crate::package_key(package.as_str()).to_bytes()
                    && binding.view_root == view_root =>
            {
                let read = PackageCompilerOutcomeRead::Current;
                outcomes.admit_request(package, Some(&read))?;
                let PackageCompilerOutcomes::Summary { basis, .. } = outcomes else {
                    return Err(ProductAdmissionError::IndexOperationShape);
                };
                if basis.package_key != binding.package_key
                    || basis.source_relation_root != binding.source_relation_root
                    || basis.capture_relation_root != binding.capture_relation_root
                {
                    return Err(ProductAdmissionError::IndexOperationShape);
                }
                Ok(())
            }
            _ => Err(ProductAdmissionError::IndexOperationShape),
        }
    }
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

/// Copies one immutable coverage row under an exact current capture and catalog.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageCompilerCoverageSelector {
    /// Copied current source/profile basis.
    pub outcome: PackageCompilerOutcomeSelector,
    /// Authenticated immutable coverage catalog identity.
    #[serde(with = "crate::surface::hex_32")]
    pub catalog: [u8; 32],
    /// Exact selected generation returned by discovery.
    #[serde(with = "crate::surface::hex_32")]
    pub generation: [u8; 32],
    /// Exact compiler input returned by discovery.
    #[serde(with = "crate::surface::hex_32")]
    pub input_digest: [u8; 32],
    /// Zero-based row offset. The next selector advances by the actual returned row count.
    pub offset: u32,
}

/// Successful partial publication observations, independent of terminal refusals.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PackageCompilerCoverage {
    /// Publication metadata did not preserve actual per-file cause observations.
    LegacyNotRecorded {
        /// Exact published profile; no reason is inferred from its aggregate count.
        profile: SemanticLanguageProfile,
    },
    /// Authenticated producer basis retained, but the complete explanation exceeded its budget.
    NotRecordedCapacity {
        /// Exact published profile; semantic Partial/Complete is unchanged.
        profile: SemanticLanguageProfile,
        /// Exact independent explanation recording bound.
        reason: backend_semantic::ir::SourceCoverageCapacityReason,
    },
    /// Complete immutable observation catalog is retained, including selected config identities.
    Retained {
        /// Exact bounded follow-up; copy rather than reconstruct it.
        read: PackageCompilerOutcomeRead,
    },
}

/// A bounded page of a complete coverage catalog; catalog absence is never an empty-gap proof.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageCompilerCoveragePage {
    /// Exact selected semantic generation.
    #[serde(with = "crate::surface::hex_32")]
    pub generation: [u8; 32],
    /// Exact compiler input bound to the captured profile.
    #[serde(with = "crate::surface::hex_32")]
    pub input_digest: [u8; 32],
    /// Captured TypeScript source count.
    pub source_count: u32,
    /// Published semantic artifact count; never upgrades Partial to Complete.
    pub completed: u32,
    /// Total configuration and gap rows.
    pub row_count: u32,
    /// Config and gap observations from the compiler producer, at most 25 per page.
    pub rows: Box<[PackageCompilerCoverageRow]>,
    /// Exact next page, absent only after the last row.
    pub next: Option<PackageCompilerOutcomeRead>,
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
    /// Read the next exact compiler coverage observation.
    Coverage(PackageCompilerCoverageSelector),
}

impl PackageCompilerOutcomeRead {
    /// Exact diagnostic selector, absent for current discovery.
    #[must_use]
    pub const fn selector(&self) -> Option<&PackageCompilerOutcomeSelector> {
        match self {
            Self::Current => None,
            Self::Diagnostic(selector) => Some(selector),
            Self::Coverage(selector) => Some(&selector.outcome),
        }
    }

    /// Checks a read's closed profile and bounded basis before owner dispatch.
    #[must_use]
    pub fn has_admissible_shape(&self) -> bool {
        self.selector()
            .is_none_or(PackageCompilerOutcomeSelector::has_admissible_shape)
            && !matches!(self, Self::Coverage(selector) if selector.catalog==[0;32] || selector.generation==[0;32] || selector.input_digest==[0;32] || selector.offset as usize > backend_semantic::ir::MAX_SOURCE_COVERAGE_ROWS || !matches!(selector.outcome.profile.profile(),Ok(backend_semantic::vocabulary::LanguageProfile::TypeScript(_))))
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
    /// This published generation has no persisted coverage catalog.
    CoverageNotRecorded,
    /// Explanation recording exceeded its independent budget; no cause pages exist.
    CoverageNotRecordedCapacity,
    /// This profile has not published a generation for the current capture.
    ProfileNotPublished,
    /// One exact row cannot fit the bounded coverage response; no row was truncated.
    CoverageRowTooLarge,
    /// The exact published view could not be checked against its owner snapshot.
    PublicationUnavailable,
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
        /// Coverage availability for every published profile, distinct from refusals.
        /// None means an older response omitted this field; a present list must be complete.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        coverage: Option<Box<[PackageCompilerCoverage]>>,
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
    /// One bounded authenticated coverage page under the selected capture basis.
    Coverage {
        /// Exact local package.
        package: PackageReference,
        /// Exact immutable selected roots.
        basis: PackageCompilerOutcomeBasis,
        /// Exact requested row and catalog.
        selector: PackageCompilerCoverageSelector,
        /// Complete row with explicit continuation, never a truncated observation.
        page: PackageCompilerCoveragePage,
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
            Self::Coverage { package, .. }
            | Self::Stale { package }
            | Self::Unavailable { package, .. } => package,
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
                } => {
                    !matches!(read,Some(PackageCompilerOutcomeRead::Diagnostic(requested)) if requested==observed)
                }
                Self::Coverage {
                    selector: observed, ..
                } => {
                    !matches!(read,Some(PackageCompilerOutcomeRead::Coverage(requested)) if requested==observed)
                }
                Self::Stale { .. } => selector.is_none(),
                Self::Unavailable { reason, .. } => match reason {
                    PackageCompilerOutcomesUnavailable::ProjectNotSelected
                    | PackageCompilerOutcomesUnavailable::CaptureNotRecorded
                    | PackageCompilerOutcomesUnavailable::CaptureNotCurrent => selector.is_some(),
                    PackageCompilerOutcomesUnavailable::ProfileNotRefused
                    | PackageCompilerOutcomesUnavailable::DiagnosticNotRecorded => {
                        !matches!(read, Some(PackageCompilerOutcomeRead::Diagnostic(_)))
                    }
                    PackageCompilerOutcomesUnavailable::CoverageNotRecorded
                    | PackageCompilerOutcomesUnavailable::CoverageNotRecordedCapacity
                    | PackageCompilerOutcomesUnavailable::ProfileNotPublished
                    | PackageCompilerOutcomesUnavailable::CoverageRowTooLarge => {
                        !matches!(read, Some(PackageCompilerOutcomeRead::Coverage(_)))
                    }
                    PackageCompilerOutcomesUnavailable::PublicationUnavailable => {
                        selector.is_none()
                    }
                    PackageCompilerOutcomesUnavailable::CaptureIncomplete => false,
                },
            }
        {
            return Err(ProductAdmissionError::IndexOperationShape);
        }
        Ok(())
    }

    /// Validates complete source tuples and the exact refusal partition.
    pub fn admit(&self) -> Result<(), ProductAdmissionError> {
        if let Self::Coverage {
            package,
            basis,
            selector,
            page,
        } = self
        {
            let fail = ProductAdmissionError::IndexOperationShape;
            if !matches!(package, PackageReference::Local(_))
                || !basis.has_admissible_shape()
                || basis.package_key != crate::package_key(package.as_str()).to_bytes()
                || selector.outcome.basis != basis.identity()
                || selector.catalog == [0; 32]
                || selector.generation == [0; 32]
                || selector.input_digest == [0; 32]
                || !selector.outcome.has_admissible_shape()
                || !matches!(
                    selector.outcome.profile.profile(),
                    Ok(backend_semantic::vocabulary::LanguageProfile::TypeScript(_))
                )
                || page.generation != selector.generation
                || page.input_digest != selector.input_digest
                || page.generation == [0; 32]
                || page.input_digest == [0; 32]
                || page.source_count == 0
                || page.source_count > 100_000
                || page.completed > page.source_count
                || page.row_count as usize > backend_semantic::ir::MAX_SOURCE_COVERAGE_ROWS
                || page.row_count < page.source_count - page.completed
                || page.row_count > (page.source_count - page.completed).saturating_add(256)
                || selector.offset > page.row_count
                || page.rows.len() > MAX_PACKAGE_COMPILER_COVERAGE_PAGE_ROWS
                || (page.rows.is_empty() != (selector.offset == page.row_count))
                || selector
                    .offset
                    .checked_add(page.rows.len() as u32)
                    .is_none_or(|end| end > page.row_count)
                || page
                    .rows
                    .iter()
                    .any(|row| row.validate_typescript().is_err())
                || serde_json::to_vec(page)
                    .map_err(|_| ProductAdmissionError::IndexOperationShape)?
                    .len()
                    > MAX_PACKAGE_COMPILER_COVERAGE_PAGE_BYTES
            {
                return Err(fail);
            }
            let end = selector.offset + page.rows.len() as u32;
            let expected = (end < page.row_count).then_some(PackageCompilerOutcomeRead::Coverage(
                PackageCompilerCoverageSelector {
                    offset: end,
                    ..*selector
                },
            ));
            if page.next != expected {
                return Err(ProductAdmissionError::IndexOperationShape);
            }
            return Ok(());
        }
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
            Self::Coverage { .. } => return Err(ProductAdmissionError::IndexOperationShape),
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
            Self::Summary {
                refusals, coverage, ..
            } => {
                // An omitted legacy field is accepted for old payloads; new producers enumerate all published profiles.
                let mut seen = Vec::new();
                if let Some(coverage) = coverage {
                    let expected = capture
                        .profiles
                        .iter()
                        .filter(|p| matches!(p.state, State::Published { .. }))
                        .map(|p| p.profile)
                        .collect::<Vec<_>>();
                    let observed = coverage
                        .iter()
                        .map(|entry| match entry {
                            PackageCompilerCoverage::LegacyNotRecorded { profile }
                            | PackageCompilerCoverage::NotRecordedCapacity { profile, .. } => {
                                Some(*profile)
                            }
                            PackageCompilerCoverage::Retained {
                                read: PackageCompilerOutcomeRead::Coverage(read),
                            } => Some(read.outcome.profile),
                            PackageCompilerCoverage::Retained { .. } => None,
                        })
                        .collect::<Option<Vec<_>>>()
                        .ok_or(ProductAdmissionError::IndexOperationShape)?;
                    if observed != expected {
                        return Err(ProductAdmissionError::IndexOperationShape);
                    }
                }
                for observed in coverage.iter().flat_map(|rows| rows.iter()) {
                    if let PackageCompilerCoverage::NotRecordedCapacity { profile, .. } = observed
                        && !matches!(
                            profile.profile(),
                            Ok(backend_semantic::vocabulary::LanguageProfile::TypeScript(_))
                        )
                    {
                        return Err(ProductAdmissionError::IndexOperationShape);
                    }
                    let profile = match observed {
                        PackageCompilerCoverage::LegacyNotRecorded { profile }
                        | PackageCompilerCoverage::NotRecordedCapacity { profile, .. } => *profile,
                        PackageCompilerCoverage::Retained { read } => {
                            let PackageCompilerOutcomeRead::Coverage(read) = read else {
                                return Err(ProductAdmissionError::IndexOperationShape);
                            };
                            if read.outcome.basis != basis.identity()
                                || read.offset != 0
                                || !PackageCompilerOutcomeRead::Coverage(*read)
                                    .has_admissible_shape()
                            {
                                return Err(ProductAdmissionError::IndexOperationShape);
                            }
                            if !capture.profiles.iter().any(|p|p.profile==read.outcome.profile && p.input_digest==read.input_digest && matches!(p.state,State::Published{generation,..} if generation==read.generation)) {return Err(ProductAdmissionError::IndexOperationShape);}
                            read.outcome.profile
                        }
                    };
                    if seen.last().is_some_and(|previous| *previous >= profile)
                        || !capture.profiles.iter().any(|p| {
                            p.profile == profile && matches!(p.state, State::Published { .. })
                        })
                    {
                        return Err(ProductAdmissionError::IndexOperationShape);
                    }
                    seen.push(profile);
                }

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
