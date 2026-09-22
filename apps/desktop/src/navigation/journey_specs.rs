//! Semantic onboarding journeys used by product QA and screenshot capture.
//!
//! These specifications describe service events and visible states only. A
//! runner supplies the admitted `IndexTarget`, lossless native path wire, and
//! producer version root through the shared transport contract. This module
//! deliberately does not define another source identity, path, receipt, or
//! operation owner.

/// A stable name for one onboarding journey.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum JourneyId {
    /// A fresh process with no persisted source membership.
    ColdEmpty,
    /// The native picker was opened and cancelled.
    PickerCancelled,
    /// A source was admitted and is being indexed.
    Indexing,
    /// Two admitted sources are ready and one is active.
    ReadyMultiProject,
    /// An index failed and was retried to readiness.
    FailureRetry,
    /// Persisted source membership was restored after a restart.
    PersistedRestart,
    /// Claude/MCP setup and connection guidance.
    McpSetup,
}

impl JourneyId {
    /// Every required onboarding journey in capture order.
    pub const ALL: [Self; 7] = [
        Self::ColdEmpty,
        Self::PickerCancelled,
        Self::Indexing,
        Self::ReadyMultiProject,
        Self::FailureRetry,
        Self::PersistedRestart,
        Self::McpSetup,
    ];

    /// Stable artifact and command-line spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ColdEmpty => "cold-empty",
            Self::PickerCancelled => "picker-cancelled",
            Self::Indexing => "indexing",
            Self::ReadyMultiProject => "ready-multi-project",
            Self::FailureRetry => "failure-retry",
            Self::PersistedRestart => "persisted-restart",
            Self::McpSetup => "mcp-setup",
        }
    }

    /// Parses one stable journey spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "cold-empty" => Self::ColdEmpty,
            "picker-cancelled" => Self::PickerCancelled,
            "indexing" => Self::Indexing,
            "ready-multi-project" => Self::ReadyMultiProject,
            "failure-retry" => Self::FailureRetry,
            "persisted-restart" => Self::PersistedRestart,
            "mcp-setup" => Self::McpSetup,
            _ => return None,
        })
    }
}

/// One service or user transition in a journey.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum JourneyEvent {
    /// Start from a cold process with no shelf membership.
    ColdStart,
    /// Open the native folder picker.
    OpenFolderPicker,
    /// Dismiss the picker without a selection.
    CancelFolderPicker,
    /// Return one or more selected folders to the source admission boundary.
    SubmitFolderSelection,
    /// Admit a shared `IndexTarget` carrying its lossless native path wire.
    AdmitIndexTarget,
    /// Accept the service operation receipt and attach it to the projection.
    AcceptIndexReceipt,
    /// Receive an owner-reported in-progress update.
    ReceiveIndexProgress,
    /// Receive an owner-reported terminal ready result.
    ReceiveIndexReady,
    /// Receive an owner-reported terminal failure.
    ReceiveIndexFailure,
    /// Request a retry through the service-owned operation boundary.
    RetryIndex,
    /// Request cancellation while retaining the shelf row.
    CancelIndex,
    /// Activate an admitted source after it is on the shelf.
    ActivateSource,
    /// Remove a source membership from the shelf.
    RemoveSource,
    /// Publish the durable source-membership projection.
    PersistSourceMembership,
    /// Reattach persisted membership and owner receipts after restart.
    ReattachPersistedSources,
    /// Open Claude/MCP setup guidance.
    OpenMcpSetup,
    /// Copy the displayed setup command or configuration.
    CopyMcpSetup,
    /// Ask the owner to test the configured connection.
    TestMcpConnection,
    /// Receive the owner connection status.
    ReceiveMcpStatus,
}

impl JourneyEvent {
    /// Stable capture-manifest spelling for one transition.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ColdStart => "cold-start",
            Self::OpenFolderPicker => "open-folder-picker",
            Self::CancelFolderPicker => "cancel-folder-picker",
            Self::SubmitFolderSelection => "submit-folder-selection",
            Self::AdmitIndexTarget => "admit-index-target",
            Self::AcceptIndexReceipt => "accept-index-receipt",
            Self::ReceiveIndexProgress => "receive-index-progress",
            Self::ReceiveIndexReady => "receive-index-ready",
            Self::ReceiveIndexFailure => "receive-index-failure",
            Self::RetryIndex => "retry-index",
            Self::CancelIndex => "cancel-index",
            Self::ActivateSource => "activate-source",
            Self::RemoveSource => "remove-source",
            Self::PersistSourceMembership => "persist-source-membership",
            Self::ReattachPersistedSources => "reattach-persisted-sources",
            Self::OpenMcpSetup => "open-mcp-setup",
            Self::CopyMcpSetup => "copy-mcp-setup",
            Self::TestMcpConnection => "test-mcp-connection",
            Self::ReceiveMcpStatus => "receive-mcp-status",
        }
    }
}

/// The visual state expected after a journey event.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ScreenshotState {
    /// Empty first launch with a visible choose-folder affordance.
    ColdEmpty,
    /// Picker is open and has not yet returned a selection.
    PickerOpen,
    /// Picker cancellation returned to the empty shelf with no error.
    PickerCancelled,
    /// A source admission is accepted and indexing is indeterminate or active.
    Indexing,
    /// Cancellation was accepted and the owner has not published termination.
    IndexCancelling,
    /// A source operation failed and exposes retry guidance.
    IndexFailure,
    /// One source reached an owner-reported ready terminal state.
    ReadyProject,
    /// Multiple sources are ready with one explicit active selection.
    ReadyMultiProject,
    /// Restart is reconnecting to persisted source membership.
    RestartReattaching,
    /// Persisted membership and active source are restored.
    RestartReady,
    /// Claude/MCP instructions and copy affordances are visible.
    McpSetup,
    /// The owner reported a successful Claude/MCP connection.
    McpConnected,
}

impl ScreenshotState {
    /// Stable artifact suffix and semantic capture spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ColdEmpty => "cold-empty",
            Self::PickerOpen => "picker-open",
            Self::PickerCancelled => "picker-cancelled",
            Self::Indexing => "indexing",
            Self::IndexCancelling => "index-cancelling",
            Self::IndexFailure => "index-failure",
            Self::ReadyProject => "ready-project",
            Self::ReadyMultiProject => "ready-multi-project",
            Self::RestartReattaching => "restart-reattaching",
            Self::RestartReady => "restart-ready",
            Self::McpSetup => "mcp-setup",
            Self::McpConnected => "mcp-connected",
        }
    }
}

/// One expected capture point in a journey.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct JourneyStep {
    /// Event supplied by the user or service owner.
    pub event: JourneyEvent,
    /// Visual state to capture after the event has settled.
    pub screenshot: ScreenshotState,
}

/// One complete semantic journey.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct JourneySpec {
    /// Stable journey identity.
    pub id: JourneyId,
    /// Ordered event and screenshot expectations.
    pub steps: &'static [JourneyStep],
}

impl JourneySpec {
    /// Returns the screenshot state that closes this journey.
    #[must_use]
    pub fn terminal_state(self) -> Option<ScreenshotState> {
        self.steps.last().map(|step| step.screenshot)
    }
}

/// One deterministic viewport/text-scale capture profile.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CaptureProfile {
    /// Logical viewport width in pixels.
    pub width: u16,
    /// Logical viewport height in pixels.
    pub height: u16,
    /// Interface text scale percentage.
    pub text_scale: u16,
}

/// Required screenshot profiles for responsive and large-text review.
pub const CAPTURE_PROFILES: &[CaptureProfile] = &[
    CaptureProfile {
        width: 320,
        height: 480,
        text_scale: 100,
    },
    CaptureProfile {
        width: 640,
        height: 480,
        text_scale: 100,
    },
    CaptureProfile {
        width: 1000,
        height: 640,
        text_scale: 100,
    },
    CaptureProfile {
        width: 1440,
        height: 900,
        text_scale: 100,
    },
    CaptureProfile {
        width: 640,
        height: 480,
        text_scale: 200,
    },
];

const COLD_EMPTY_STEPS: &[JourneyStep] = &[JourneyStep {
    event: JourneyEvent::ColdStart,
    screenshot: ScreenshotState::ColdEmpty,
}];

const PICKER_CANCELLED_STEPS: &[JourneyStep] = &[
    JourneyStep {
        event: JourneyEvent::ColdStart,
        screenshot: ScreenshotState::ColdEmpty,
    },
    JourneyStep {
        event: JourneyEvent::OpenFolderPicker,
        screenshot: ScreenshotState::PickerOpen,
    },
    JourneyStep {
        event: JourneyEvent::CancelFolderPicker,
        screenshot: ScreenshotState::PickerCancelled,
    },
];

const INDEXING_STEPS: &[JourneyStep] = &[
    JourneyStep {
        event: JourneyEvent::ColdStart,
        screenshot: ScreenshotState::ColdEmpty,
    },
    JourneyStep {
        event: JourneyEvent::OpenFolderPicker,
        screenshot: ScreenshotState::PickerOpen,
    },
    JourneyStep {
        event: JourneyEvent::SubmitFolderSelection,
        screenshot: ScreenshotState::PickerOpen,
    },
    JourneyStep {
        event: JourneyEvent::AdmitIndexTarget,
        screenshot: ScreenshotState::Indexing,
    },
    JourneyStep {
        event: JourneyEvent::AcceptIndexReceipt,
        screenshot: ScreenshotState::Indexing,
    },
    JourneyStep {
        event: JourneyEvent::ReceiveIndexProgress,
        screenshot: ScreenshotState::Indexing,
    },
    JourneyStep {
        event: JourneyEvent::CancelIndex,
        screenshot: ScreenshotState::IndexCancelling,
    },
];

const READY_MULTI_PROJECT_STEPS: &[JourneyStep] = &[
    JourneyStep {
        event: JourneyEvent::ColdStart,
        screenshot: ScreenshotState::ColdEmpty,
    },
    JourneyStep {
        event: JourneyEvent::SubmitFolderSelection,
        screenshot: ScreenshotState::Indexing,
    },
    JourneyStep {
        event: JourneyEvent::AdmitIndexTarget,
        screenshot: ScreenshotState::Indexing,
    },
    JourneyStep {
        event: JourneyEvent::ReceiveIndexReady,
        screenshot: ScreenshotState::ReadyProject,
    },
    JourneyStep {
        event: JourneyEvent::SubmitFolderSelection,
        screenshot: ScreenshotState::Indexing,
    },
    JourneyStep {
        event: JourneyEvent::AdmitIndexTarget,
        screenshot: ScreenshotState::Indexing,
    },
    JourneyStep {
        event: JourneyEvent::ReceiveIndexReady,
        screenshot: ScreenshotState::ReadyMultiProject,
    },
    JourneyStep {
        event: JourneyEvent::ActivateSource,
        screenshot: ScreenshotState::ReadyMultiProject,
    },
    JourneyStep {
        event: JourneyEvent::ReceiveIndexReady,
        screenshot: ScreenshotState::ReadyMultiProject,
    },
    JourneyStep {
        event: JourneyEvent::RemoveSource,
        screenshot: ScreenshotState::ReadyProject,
    },
];

const FAILURE_RETRY_STEPS: &[JourneyStep] = &[
    JourneyStep {
        event: JourneyEvent::ColdStart,
        screenshot: ScreenshotState::ColdEmpty,
    },
    JourneyStep {
        event: JourneyEvent::AdmitIndexTarget,
        screenshot: ScreenshotState::Indexing,
    },
    JourneyStep {
        event: JourneyEvent::ReceiveIndexFailure,
        screenshot: ScreenshotState::IndexFailure,
    },
    JourneyStep {
        event: JourneyEvent::RetryIndex,
        screenshot: ScreenshotState::Indexing,
    },
    JourneyStep {
        event: JourneyEvent::ReceiveIndexProgress,
        screenshot: ScreenshotState::Indexing,
    },
    JourneyStep {
        event: JourneyEvent::ReceiveIndexReady,
        screenshot: ScreenshotState::ReadyProject,
    },
];

const PERSISTED_RESTART_STEPS: &[JourneyStep] = &[
    JourneyStep {
        event: JourneyEvent::ReceiveIndexReady,
        screenshot: ScreenshotState::ReadyProject,
    },
    JourneyStep {
        event: JourneyEvent::PersistSourceMembership,
        screenshot: ScreenshotState::ReadyProject,
    },
    JourneyStep {
        event: JourneyEvent::ColdStart,
        screenshot: ScreenshotState::RestartReattaching,
    },
    JourneyStep {
        event: JourneyEvent::ReattachPersistedSources,
        screenshot: ScreenshotState::RestartReady,
    },
    JourneyStep {
        event: JourneyEvent::ActivateSource,
        screenshot: ScreenshotState::RestartReady,
    },
];

const MCP_SETUP_STEPS: &[JourneyStep] = &[
    JourneyStep {
        event: JourneyEvent::ReceiveIndexReady,
        screenshot: ScreenshotState::ReadyMultiProject,
    },
    JourneyStep {
        event: JourneyEvent::OpenMcpSetup,
        screenshot: ScreenshotState::McpSetup,
    },
    JourneyStep {
        event: JourneyEvent::CopyMcpSetup,
        screenshot: ScreenshotState::McpSetup,
    },
    JourneyStep {
        event: JourneyEvent::TestMcpConnection,
        screenshot: ScreenshotState::McpSetup,
    },
    JourneyStep {
        event: JourneyEvent::ReceiveMcpStatus,
        screenshot: ScreenshotState::McpConnected,
    },
];

/// The complete semantic capture matrix consumed by the desktop harness adapter.
pub const JOURNEYS: &[JourneySpec] = &[
    JourneySpec {
        id: JourneyId::ColdEmpty,
        steps: COLD_EMPTY_STEPS,
    },
    JourneySpec {
        id: JourneyId::PickerCancelled,
        steps: PICKER_CANCELLED_STEPS,
    },
    JourneySpec {
        id: JourneyId::Indexing,
        steps: INDEXING_STEPS,
    },
    JourneySpec {
        id: JourneyId::ReadyMultiProject,
        steps: READY_MULTI_PROJECT_STEPS,
    },
    JourneySpec {
        id: JourneyId::FailureRetry,
        steps: FAILURE_RETRY_STEPS,
    },
    JourneySpec {
        id: JourneyId::PersistedRestart,
        steps: PERSISTED_RESTART_STEPS,
    },
    JourneySpec {
        id: JourneyId::McpSetup,
        steps: MCP_SETUP_STEPS,
    },
];

/// Looks up one semantic journey for a harness or product-QA adapter.
#[must_use]
pub fn spec(id: JourneyId) -> Option<&'static JourneySpec> {
    JOURNEYS.iter().find(|candidate| candidate.id == id)
}

#[cfg(test)]
mod tests {
    use super::{CAPTURE_PROFILES, JourneyId, ScreenshotState, spec};

    #[test]
    fn capture_matrix_covers_required_viewports_and_large_text() {
        assert!(CAPTURE_PROFILES.iter().any(|profile| profile.width == 320));
        assert!(CAPTURE_PROFILES.iter().any(|profile| profile.width == 640));
        assert!(CAPTURE_PROFILES.iter().any(|profile| profile.width == 1000));
        assert!(CAPTURE_PROFILES.iter().any(|profile| profile.width == 1440));
        assert!(
            CAPTURE_PROFILES
                .iter()
                .any(|profile| profile.text_scale == 200)
        );
    }

    #[test]
    fn every_required_journey_has_a_terminal_capture_state() {
        for journey in JourneyId::ALL {
            let journey = spec(journey).expect("required journey");
            assert!(!journey.steps.is_empty());
            let terminal = journey.steps.last().map(|step| step.screenshot);
            let expected = match journey.id {
                JourneyId::ColdEmpty => ScreenshotState::ColdEmpty,
                JourneyId::PickerCancelled => ScreenshotState::PickerCancelled,
                JourneyId::Indexing => ScreenshotState::IndexCancelling,
                JourneyId::ReadyMultiProject => ScreenshotState::ReadyProject,
                JourneyId::FailureRetry => ScreenshotState::ReadyProject,
                JourneyId::PersistedRestart => ScreenshotState::RestartReady,
                JourneyId::McpSetup => ScreenshotState::McpConnected,
            };
            assert_eq!(terminal, Some(expected));
        }
    }
}
