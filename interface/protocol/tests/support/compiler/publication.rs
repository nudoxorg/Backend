//! Exercises the `interface-protocol` tests support compiler publication contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use interface_core::{PublicationCause, PublicationPhase};
use serde::Deserialize;

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum GoldenPublicationCause {
    CancelledBeforeStorage,
    CancelledBeforeAdmission,
    CancelledAfterAdmission,
    AdmissionFull,
    AdmissionClosed,
    Rejected { phase: GoldenPublicationPhase },
    StableGenerationMismatch,
}

#[derive(Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GoldenPublicationPhase {
    Canonical,
    Manifest,
    Fragment,
    Generation,
    Binding,
    Durable,
}

impl From<PublicationCause> for GoldenPublicationCause {
    fn from(cause: PublicationCause) -> Self {
        match cause {
            PublicationCause::CancelledBeforeStorage => Self::CancelledBeforeStorage,
            PublicationCause::CancelledBeforeAdmission => Self::CancelledBeforeAdmission,
            PublicationCause::CancelledAfterAdmission => Self::CancelledAfterAdmission,
            PublicationCause::AdmissionFull => Self::AdmissionFull,
            PublicationCause::AdmissionClosed => Self::AdmissionClosed,
            PublicationCause::Rejected(phase) => Self::Rejected {
                phase: phase.into(),
            },
            PublicationCause::StableGenerationMismatch => Self::StableGenerationMismatch,
        }
    }
}

impl From<PublicationPhase> for GoldenPublicationPhase {
    fn from(phase: PublicationPhase) -> Self {
        match phase {
            PublicationPhase::Canonical => Self::Canonical,
            PublicationPhase::Manifest => Self::Manifest,
            PublicationPhase::Fragment => Self::Fragment,
            PublicationPhase::Generation => Self::Generation,
            PublicationPhase::Binding => Self::Binding,
            PublicationPhase::Durable => Self::Durable,
        }
    }
}
