//! One presentation of the exact local request and producer evidence.
//!
//! The durable phase also drives scheduling and recovery. It deliberately
//! includes queued folders in Indexing, so it cannot by itself say that a
//! producer accepted work. Browsable rows are independent of this lifecycle.

use super::{ProjectPhase, WorkspaceProject};
use backend_library::{
    IndexJobStage, IndexOperationFailureReason, IndexOperationObservation, IndexOperationState,
    PackageCompilerFailurePhase,
};

/// Evidence for the latest project operation, shared by every product view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectLifecycle {
    /// The folder is on the shelf; no request or saved operation exists.
    Queued,
    /// A local preflight request exists, but no mutation claim exists yet.
    CheckingFolder,
    /// An exact caller claim exists without an admitted owner observation.
    AwaitingReceipt,
    /// The producer admitted this exact operation.
    Accepted,
    /// The producer reported its current operation stage.
    Active(IndexJobStage),
    /// An admitted terminal operation receipt proves a publication.
    Published,
    /// Useful profiles published while other exact captured profiles were refused.
    PartiallyPublished,
    /// A retained legacy ready row carries no exact operation receipt.
    Available,
    /// Cancellation is requested; no terminal cancellation is asserted.
    Cancelling,
    /// The operation has been paused, including a proven cancellation.
    Paused,
    /// The latest attempt failed. The optional reason is producer evidence.
    Failed(Option<IndexOperationFailureReason>),
    /// A validated operation carries an exact typed compiler refusal.
    CompilerRefused(PackageCompilerFailurePhase),
    /// Retained active evidence needs reconciliation after interruption.
    CheckingOutcome,
    /// The owner has no retained receipt for the saved caller key.
    UnknownOutcome,
    /// The consumed key is outside the owner's terminal evidence window.
    OutsideReceiptWindow,
    /// The producer explicitly reports an unresolved publication outcome.
    Unresolved,
    /// The admitted folder no longer exists at its saved path.
    Missing,
}

impl ProjectLifecycle {
    /// A short label. Acceptance and publication require owner evidence.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::CheckingFolder => "Checking folder",
            Self::AwaitingReceipt => "Awaiting receipt",
            Self::Accepted => "Accepted",
            Self::Active(_) => "Indexing",
            Self::Published => "Published",
            Self::PartiallyPublished => "Partially published",
            Self::Available => "Available",
            Self::Cancelling => "Cancelling…",
            Self::Paused => "Paused",
            Self::Failed(Some(
                IndexOperationFailureReason::Refused | IndexOperationFailureReason::LedgerFull,
            )) => "Index refused",
            Self::Failed(_) => "Index failed",
            Self::CompilerRefused(_) => "Compilation refused",
            Self::CheckingOutcome
            | Self::UnknownOutcome
            | Self::OutsideReceiptWindow
            | Self::Unresolved => "Outcome unconfirmed",
            Self::Missing => "Folder missing",
        }
    }

    /// Describes the evidence, without parsing raw producer detail or claiming
    /// that a retained library disappeared when the latest attempt failed.
    #[must_use]
    pub const fn detail(self) -> &'static str {
        match self {
            Self::Queued => {
                "Waiting to start. No index request has been submitted."
            }
            Self::CheckingFolder => "Checking the folder before preparing an index operation.",
            Self::AwaitingReceipt => {
                "Waiting for the owner's answer to this saved index operation."
            }
            Self::Accepted => "Accepted by the index owner",
            Self::Active(stage) => match stage {
                IndexJobStage::Acquiring => "Acquiring the package",
                IndexJobStage::Staging => "Preparing the package sources",
                IndexJobStage::Scanning => "Reading the package sources",
                IndexJobStage::Compiling => "Compiling the package",
                IndexJobStage::Publishing => "Publishing the index",
            },
            Self::Published => "Index published",
            Self::PartiallyPublished => "Some profiles published. Others did not publish.",
            Self::Available => {
                "The retained project is available. No current operation receipt is claimed."
            }
            Self::Cancelling => "Waiting for the owner to confirm cancellation.",
            Self::Paused => "The latest index attempt is paused.",
            Self::Failed(Some(IndexOperationFailureReason::Refused)) => {
                "The owner refused the latest index attempt before publication."
            }
            Self::Failed(Some(IndexOperationFailureReason::LedgerFull)) => {
                "The owner refused the latest index attempt because its operation ledger was full."
            }
            Self::Failed(Some(IndexOperationFailureReason::WorkerFailed)) => {
                "The owner could not finish the latest index attempt before publication."
            }
            Self::Failed(Some(IndexOperationFailureReason::Cancelled)) => {
                "The owner cancelled the latest index attempt before publication."
            }
            Self::Failed(None) => {
                "The latest index attempt could not finish. No publication receipt is claimed."
            }
            Self::CompilerRefused(_) => {
                "The compiler refused the latest attempt before publication."
            }
            Self::CheckingOutcome => "Checking the saved index operation",
            Self::UnknownOutcome => "No operation receipt is available",
            Self::OutsideReceiptWindow => "Index receipt is outside the evidence window",
            Self::Unresolved => "Index outcome is unresolved",
            Self::Missing => "The saved folder could not be found.",
        }
    }

    /// Whether onboarding has a pending attempt to describe.
    #[must_use]
    pub const fn pending(self) -> bool {
        matches!(
            self,
            Self::Queued
                | Self::CheckingFolder
                | Self::AwaitingReceipt
                | Self::Accepted
                | Self::Active(_)
        )
    }

    /// A work age is meaningful only after local work actually began.
    #[must_use]
    pub const fn started(self) -> bool {
        matches!(
            self,
            Self::CheckingFolder
                | Self::AwaitingReceipt
                | Self::Accepted
                | Self::Active(_)
                | Self::Cancelling
        )
    }
}

impl WorkspaceProject {
    /// Typed unavailable profile details from this row's admitted partial receipt.
    /// No diagnostic text can invent a profile state or a publication.
    #[must_use]
    pub fn partial_refusal_words(&self) -> Option<String> {
        use backend_library::{
            IndexOperationObservation, IndexOperationSemanticUnavailableReason as Reason,
        };
        if self.lifecycle() != ProjectLifecycle::PartiallyPublished {
            return None;
        }
        let IndexOperationObservation::Known(status) =
            self.operation.as_ref()?.observation.as_ref()?
        else {
            return None;
        };
        let IndexOperationState::PartiallyPublished {
            refused_profiles, ..
        } = &status.state
        else {
            return None;
        };
        Some(
            refused_profiles
                .iter()
                .map(|refusal| {
                    let name = refusal.profile.name().unwrap_or("Profile");
                    let reason = match refusal.reason {
                        Reason::Toolchain => "the compiler is unavailable",
                        Reason::ProjectAuthority => "required project information is unavailable",
                        Reason::Cancelled => "the profile refresh was cancelled",
                        Reason::Rejected => "the profile refresh was refused",
                    };
                    let mut words = format!("{name}: {reason}.");
                    if status.source_capture.as_ref().is_some_and(|capture| {
                        capture.profiles().iter().any(|profile| profile.profile == refusal.profile
                            && matches!(profile.state, backend_library::IndexOperationSemanticProfileState::Failed { .. }))
                    }) {
                        words.push_str(" A prior generation was retained as stale evidence.");
                    }
                    if let Some(failure) = &refusal.compiler_failure {
                        words.push_str(&format!(
                            " Compiler phase: {:?}; cause: {:?}.",
                            failure.phase(),
                            failure.facts()
                        ));
                    }
                    words
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    /// Derives presentation from this row's exact request/claim/observation.
    /// Counters, elapsed time and raw failure strings supply no admission.
    #[must_use]
    pub fn lifecycle(&self) -> ProjectLifecycle {
        match self.phase {
            ProjectPhase::Missing => return ProjectLifecycle::Missing,
            ProjectPhase::Cancelling => return ProjectLifecycle::Cancelling,
            ProjectPhase::Cancelled => return ProjectLifecycle::Paused,
            _ => {}
        }
        if let Some(operation) = &self.operation {
            if !operation.belongs_to(&self.id) {
                return ProjectLifecycle::CheckingOutcome;
            }
            if let Some(observation) = &operation.observation {
                if !operation.admits_observation(observation) {
                    return ProjectLifecycle::CheckingOutcome;
                }
                return match observation {
                    IndexOperationObservation::Unknown { .. } => ProjectLifecycle::UnknownOutcome,
                    IndexOperationObservation::OutsideReceiptWindow { .. } => {
                        ProjectLifecycle::OutsideReceiptWindow
                    }
                    IndexOperationObservation::Known(status) => match &status.state {
                        IndexOperationState::Accepted | IndexOperationState::Active { .. }
                            if self.phase == ProjectPhase::Unconfirmed =>
                        {
                            ProjectLifecycle::CheckingOutcome
                        }
                        IndexOperationState::Accepted => ProjectLifecycle::Accepted,
                        IndexOperationState::Active { stage, .. } => {
                            ProjectLifecycle::Active(*stage)
                        }
                        IndexOperationState::Published(_) => ProjectLifecycle::Published,
                        IndexOperationState::PartiallyPublished { .. } => ProjectLifecycle::PartiallyPublished,
                        IndexOperationState::Failed {
                            reason: IndexOperationFailureReason::Cancelled,
                            ..
                        } => ProjectLifecycle::Paused,
                        IndexOperationState::Failed {
                            compiler_failure: Some(failure),
                            ..
                        } => ProjectLifecycle::CompilerRefused(failure.phase()),
                        IndexOperationState::Failed { reason, .. } => {
                            ProjectLifecycle::Failed(Some(*reason))
                        }
                        IndexOperationState::Unresolved { .. } => ProjectLifecycle::Unresolved,
                    },
                };
            }
            return if self.phase == ProjectPhase::Unconfirmed {
                ProjectLifecycle::CheckingOutcome
            } else {
                ProjectLifecycle::AwaitingReceipt
            };
        }
        match self.phase {
            ProjectPhase::Indexing if self.request.is_some() => ProjectLifecycle::CheckingFolder,
            ProjectPhase::Indexing => ProjectLifecycle::Queued,
            ProjectPhase::Ready => ProjectLifecycle::Available,
            ProjectPhase::Failed => ProjectLifecycle::Failed(None),
            ProjectPhase::Unconfirmed => ProjectLifecycle::CheckingOutcome,
            ProjectPhase::Cancelling | ProjectPhase::Cancelled | ProjectPhase::Missing => {
                unreachable!("handled above")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::index_operation::tests::{claim, observation, published};
    use crate::navigation::RequestId;

    #[test]
    fn local_queue_preflight_and_saved_claim_cannot_invent_owner_acceptance() {
        let mut row = WorkspaceProject::indexing("/tmp/nx-queued-presentation").expect("project");
        assert_eq!(row.lifecycle(), ProjectLifecycle::Queued);
        row.request = Some(RequestId::new(1));
        assert_eq!(row.lifecycle(), ProjectLifecycle::CheckingFolder);
        row.operation = Some(claim(&row.id, 0x71));
        assert_eq!(row.lifecycle(), ProjectLifecycle::AwaitingReceipt);
        let saved = row.operation.as_mut().expect("claim");
        saved.observation = Some(observation(saved, IndexOperationState::Accepted));
        assert_eq!(row.lifecycle(), ProjectLifecycle::Accepted);
        row.phase = ProjectPhase::Unconfirmed;
        assert_eq!(
            row.lifecycle(),
            ProjectLifecycle::CheckingOutcome,
            "retained acceptance is not current work after interruption"
        );
    }

    #[test]
    fn only_the_exact_producer_receipt_can_present_a_publication() {
        let mut row =
            WorkspaceProject::indexing("/tmp/nx-published-presentation").expect("project");
        row.phase = ProjectPhase::Ready;
        assert_eq!(
            row.lifecycle(),
            ProjectLifecycle::Available,
            "a legacy ready phase is not a publication receipt"
        );
        let mut saved = claim(&row.id, 0x72);
        saved.observation = Some(published(&saved));
        row.operation = Some(saved);
        assert_eq!(row.lifecycle(), ProjectLifecycle::Published);
        let other = WorkspaceProject::indexing("/tmp/nx-other-presentation").expect("other");
        let foreign = claim(&other.id, 0x73);
        row.operation.as_mut().expect("claim").observation = Some(published(&foreign));
        assert_eq!(
            row.lifecycle(),
            ProjectLifecycle::CheckingOutcome,
            "a valid foreign receipt cannot promote this row"
        );
    }

    #[test]
    fn partial_receipt_discloses_only_exact_typed_unavailable_profiles() -> Result<(), String> {
        let mut row = WorkspaceProject::indexing("/fixture/mixed-presentation")
            .map_err(|error| format!("project: {error}"))?;
        let mut saved = claim(&row.id, 0x83);
        saved.observation = Some(crate::model::index_operation::tests::partially_published(
            &saved,
        )?);
        row.operation = Some(saved);
        row.phase = ProjectPhase::Ready;
        row.error = Some("all compilers failed; complete publication".into());
        assert_eq!(row.lifecycle(), ProjectLifecycle::PartiallyPublished);
        assert_eq!(
            row.partial_refusal_words().as_deref(),
            Some("typescript: the compiler is unavailable.")
        );
        let before = row.clone();
        assert!(!row.lifecycle().pending());
        assert_eq!(row, before);
        if let Some(operation) = row.operation.as_mut() {
            if let Some(IndexOperationObservation::Known(status)) = operation.observation.as_mut() {
                status.source_capture = None;
            }
        }
        assert_eq!(row.lifecycle(), ProjectLifecycle::CheckingOutcome);
        assert_eq!(row.partial_refusal_words(), None);
        Ok(())
    }

    #[test]
    fn partial_failed_profile_names_its_retained_generation_as_stale() -> Result<(), String> {
        let mut row = WorkspaceProject::indexing("/fixture/mixed-stale").map_err(|error| format!("project: {error}"))?;
        let mut saved = claim(&row.id, 0x87);
        let mut receipt = crate::model::index_operation::tests::partially_published(&saved)?;
        if let IndexOperationObservation::Known(status) = &mut receipt {
            if let Some(capture) = status.source_capture.as_mut() {
                for profile in &mut capture.profiles {
                    if let backend_library::IndexOperationSemanticProfileState::Unavailable { reason } = profile.state {
                        profile.state = backend_library::IndexOperationSemanticProfileState::Failed {
                            prior: backend_library::IndexOperationPriorSemantic { generation: [17; 32],
                                coverage: backend_library::IndexOperationSemanticCoverage::Complete }, reason,
                        };
                    }
                }
            }
        }
        assert!(saved.admits_observation(&receipt));
        saved.observation = Some(receipt);
        row.operation = Some(saved);
        row.phase = ProjectPhase::Ready;
        assert_eq!(row.lifecycle(), ProjectLifecycle::PartiallyPublished);
        assert_eq!(row.partial_refusal_words().as_deref(),
            Some("typescript: the compiler is unavailable. A prior generation was retained as stale evidence."));
        Ok(())
    }

    #[test]
    fn failure_presentation_does_not_parse_detail_or_erase_retained_rows() {
        let mut row = WorkspaceProject::indexing("/tmp/nx-retained-presentation").expect("project");
        let mut saved = claim(&row.id, 0x74);
        saved.observation = Some(observation(
            &saved,
            IndexOperationState::Failed {
                reason: IndexOperationFailureReason::Refused,
                detail: backend_library::ProductText::from_static(
                    "connection semantic compilation failed; prior selected semantic generation was preserved",
                ),
                compiler_failure: None,
            },
        ));
        row.phase = ProjectPhase::Failed;
        row.files_indexed = Some(478);
        row.operation = Some(saved);
        let before = row.clone();
        assert_eq!(
            row.lifecycle(),
            ProjectLifecycle::Failed(Some(IndexOperationFailureReason::Refused))
        );
        assert_eq!(
            row.lifecycle().detail(),
            "The owner refused the latest index attempt before publication."
        );
        assert_eq!(
            row, before,
            "retained availability is orthogonal to the latest operation outcome"
        );
    }
}
