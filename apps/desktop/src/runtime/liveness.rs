//! Worker-only confirmation of an attached owner's transport loss.
//!
//! One failed command is not proof that the service died. A fresh revision
//! round trip must fail independently, and only the generation that issued
//! the command may change the owner gate. The probe never runs on GPUI.

use super::owner::{Epoch, OwnerGate};
use backend_client::{ClientError, Session};
use std::path::Path;
use std::sync::Arc;

pub(crate) fn transport_break(error: &ClientError) -> bool {
    matches!(error, ClientError::Disconnected(_) | ClientError::Io(_) | ClientError::Transport(_) | ClientError::RemoteDeadlineExceeded)
}

/// Worker-only probe after an attached owner's terminal transport error.
pub(crate) fn report_if_dead(
    gate: Option<&OwnerGate>,
    epoch: Option<Epoch>,
    endpoint: &Path,
    terminal: &ClientError,
) -> bool {
    let (Some(gate), Some(epoch)) = (gate, epoch) else { return false };
    if !transport_break(terminal) { return false }
    let liveness = Session::connect(endpoint).and_then(|mut session| session.revision()).map(|_| ());
    confirm_attached_loss(gate, epoch, terminal, liveness)
}

fn confirm_attached_loss(
    gate: &OwnerGate,
    epoch: Epoch,
    terminal: &ClientError,
    liveness: Result<(), ClientError>,
) -> bool {
    if !transport_break(terminal) || !liveness.is_err_and(|error| transport_break(&error)) {
        return false;
    }
    gate.attached_lost_at(epoch, Arc::from(terminal.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::VersionedRoot;
    use crate::model::ServiceMode;
    use crate::runtime::owner::{OwnerFault, OwnerState};

    #[test]
    fn a_query_fault_needs_an_independent_loss_before_failing_an_attachment() {
        let gate = OwnerGate::ready(VersionedRoot::unserved(), ServiceMode::Attached);
        let epoch = gate.attached_ready_epoch().expect("attached generation");
        let query = ClientError::Io("query failed".to_owned());
        assert!(!confirm_attached_loss(&gate, epoch, &query, Ok(())));
        assert!(matches!(gate.state(), OwnerState::Ready { .. }), "a healthy independent probe keeps the owner ready");

        assert!(confirm_attached_loss(
            &gate,
            epoch,
            &query,
            Err(ClientError::Io("independent probe failed".to_owned())),
        ));
        assert!(matches!(gate.state(), OwnerState::Failed(OwnerFault::Lost(_))));
        assert!(!confirm_attached_loss(
            &gate,
            epoch,
            &query,
            Err(ClientError::Io("late probe failed".to_owned())),
        ), "a stale result cannot fail a later owner generation");
    }
}
