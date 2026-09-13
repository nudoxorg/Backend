//! Durable dispatch recovery and publication rebind.
//!
//! Recovery actions are replayed through the owner state machine after exact
//! request identity checks. A persisted accepted proof is acknowledged before
//! any new execution is considered.

use super::{
    BuiltinAuthorityVerifier, BuiltinModel, BuiltinReplication, BuiltinValidator,
    DispatchRecoveryAction, TransportMessage,
};

impl BuiltinReplication {
    pub(super) fn poll_recovered(
        &mut self,
        daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
    ) -> bool {
        let Some(action) = self.recovered.pop_front() else {
            return false;
        };
        let (key, select_fallback, request_bytes) = match &action {
            DispatchRecoveryAction::Resend { attempt }
            | DispatchRecoveryAction::PublishAccepted { attempt, .. } => {
                (attempt.key(), true, attempt.intent.request.clone())
            }
            DispatchRecoveryAction::Fallback { attempt, .. } => {
                (attempt.key(), false, attempt.intent.request.clone())
            }
        };
        let Ok(message) = TransportMessage::decode(&request_bytes, self.limits) else {
            self.recovered.push_front(action);
            return false;
        };
        let TransportMessage::WireRecipeRequest(request) = message else {
            self.recovered.push_front(action);
            return false;
        };
        if request.work_key.as_bytes() != key.work_key || request.attempt.get() != key.attempt {
            self.recovered.push_front(action);
            return false;
        }
        // An accepted proof may have crossed the process boundary after the
        // catalog selected its immutable objects but before the publication
        // acknowledgement was synced.  Recheck those object identities
        // against the restored owner head and complete only the journal tail;
        // executing the recipe again would create a second result and could
        // move the visible head away from the accepted proof.
        if let DispatchRecoveryAction::PublishAccepted { proof, .. } = &action {
            match daemon
                .engine_mut()
                .daemon_mut()
                .acknowledge_recovered_publication(key, proof)
            {
                Ok(true) => return true,
                Ok(false) => {}
                Err(_) => {
                    self.recovered.push_front(action);
                    return false;
                }
            }
        }
        if select_fallback
            && daemon
                .engine_mut()
                .daemon_mut()
                .select_recovered_fallback(key)
                .is_err()
        {
            self.recovered.push_front(action);
            return false;
        }

        // The scheduler's in-memory affine lease cannot be reconstructed from
        // a journal record after a process boundary. Fencing first and
        // routing through the checked local plan is therefore the only safe
        // recovery route; a stale worker can never publish against this
        // attempt. Once the engine exposes a durable lease rebind capability,
        // the Resend arm can opt into it without changing this boundary.
        self.recovery_fallback = Some(key);
        let result = self.dispatch(daemon, &request);
        self.recovery_fallback = None;
        if result.is_ok() {
            true
        } else {
            self.recovered.push_front(action);
            false
        }
    }

    pub(super) fn complete_recovered_output(
        &mut self,
        daemon: &mut crate::Locald<BuiltinModel, BuiltinValidator, BuiltinAuthorityVerifier>,
        completion: &backend_engine::DispatchCompletion,
    ) -> Result<(), crate::protocol::ProtocolError> {
        let Some(key) = self.recovery_fallback else {
            return Ok(());
        };
        let root = match completion {
            backend_engine::DispatchCompletion::Accepted(receipt) => receipt.output().to_bytes(),
            backend_engine::DispatchCompletion::Reused(output) => output.output().to_bytes(),
            backend_engine::DispatchCompletion::Waiting(_) => {
                return Err(crate::protocol::ProtocolError::InvalidControl(
                    "recovered fallback remained a follower",
                ));
            }
        };
        daemon
            .engine_mut()
            .daemon_mut()
            .complete_recovered_fallback(key, root)
            .map_err(|error| crate::protocol::ProtocolError::InvalidCommand(error.to_string()))?;
        self.recovery_fallback = None;
        Ok(())
    }
}
