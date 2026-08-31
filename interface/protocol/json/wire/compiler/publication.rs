//! Defines json wire compiler publication behavior for `interface-protocol`, whose purpose is to decode and project the shared application vocabulary for external transports.
//! This module owns the json wire compiler publication invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use interface_core::{PublicationCause, PublicationPhase};
use serde::{Serialize, Serializer, ser::SerializeStruct};

/// Remote serde definition for the closed non-cancellation publication phase.
#[derive(Serialize)]
#[serde(remote = "interface_core::PublicationPhase", rename_all = "snake_case")]
pub(crate) enum PublicationPhaseWire {
    Canonical,
    Manifest,
    Fragment,
    Generation,
    Binding,
    Durable,
}

pub(crate) struct PublicationPhaseRef<'value>(pub(crate) &'value PublicationPhase);

impl Serialize for PublicationPhaseRef<'_> {
    fn serialize<Output: Serializer>(
        &self,
        serializer: Output,
    ) -> Result<Output::Ok, Output::Error> {
        PublicationPhaseWire::serialize(self.0, serializer)
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)]
pub(crate) fn serialize_publication_cause<Output: Serializer>(
    cause: &PublicationCause,
    serializer: Output,
) -> Result<Output::Ok, Output::Error> {
    let mut state = serializer.serialize_struct("PublicationCause", 2)?;
    match cause {
        PublicationCause::CancelledBeforeStorage => {
            state.serialize_field("kind", "cancelled_before_storage")?;
        }
        PublicationCause::CancelledBeforeAdmission => {
            state.serialize_field("kind", "cancelled_before_admission")?;
        }
        PublicationCause::CancelledAfterAdmission => {
            state.serialize_field("kind", "cancelled_after_admission")?;
        }
        PublicationCause::AdmissionFull => {
            state.serialize_field("kind", "admission_full")?;
        }
        PublicationCause::AdmissionClosed => {
            state.serialize_field("kind", "admission_closed")?;
        }
        PublicationCause::Rejected(phase) => {
            state.serialize_field("kind", "rejected")?;
            state.serialize_field("phase", &PublicationPhaseRef(phase))?;
        }
        PublicationCause::StableGenerationMismatch => {
            state.serialize_field("kind", "stable_generation_mismatch")?;
        }
    }
    state.end()
}

const _: fn(&PublicationCause) = |_| {};
const _: fn(&PublicationPhase) = |_| {};
