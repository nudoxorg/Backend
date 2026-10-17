//! Exact owner-ticket cancellation is separate from interrupting a UI wait.
//! Only a subsequent durable-key observation can settle the shelf outcome.
use crate::model::IndexOperationClaim;
use backend_client::ClientError;
use backend_library::{
    IndexCancelReceipt, IndexCancelStatus, IndexJobTicket, IndexOperationKey,
    IndexOperationObservation, IndexOperationState, SurfaceCommand, SurfaceReply,
};

pub(super) trait CancellationClient {
    fn status(&mut self, key: IndexOperationKey) -> Result<IndexOperationObservation, ClientError>;
    fn cancel(&mut self, ticket: IndexJobTicket) -> Result<IndexCancelReceipt, ClientError>;
}

fn invalid(message: &str) -> ClientError {
    ClientError::Protocol(message.to_owned())
}

/// Recheck the captured active ticket, send at most one cancellation, then
/// reconcile the same durable key. Never starts work or manufactures a terminal.
pub(super) fn cancel_and_reconcile(
    client: &mut impl CancellationClient,
    operation: &IndexOperationClaim,
) -> Result<IndexOperationObservation, ClientError> {
    let ticket = operation
        .cancellation_ticket()
        .ok_or_else(|| invalid("no admitted active index ticket to cancel"))?;
    let before = client.status(operation.key)?;
    if !operation.admits_observation(&before) {
        return Err(invalid(
            "index cancellation status changed operation identity",
        ));
    }
    match &before {
        IndexOperationObservation::Known(status) => match &status.state {
            IndexOperationState::Active {
                ticket: current, ..
            } if current == &ticket => {}
            IndexOperationState::Active { .. } | IndexOperationState::Accepted => {
                return Err(invalid(
                    "index cancellation ticket changed before submission",
                ));
            }
            // Publication, failure or uncertainty won the race. No mutation.
            _ => return Ok(before),
        },
        _ => return Ok(before),
    }
    let receipt = client.cancel(ticket.clone())?;
    let command = SurfaceCommand::IndexCancel {
        ticket: ticket.clone(),
    };
    if receipt.ticket != ticket
        || SurfaceReply::IndexCancellation(receipt.clone())
            .admit(command.id())
            .is_err()
    {
        return Err(invalid("index cancellation reply changed the exact ticket"));
    }
    if matches!(&receipt.status, IndexCancelStatus::Terminal(terminal) if terminal.ticket != ticket)
    {
        return Err(invalid(
            "index cancellation terminal changed the exact ticket",
        ));
    }
    if matches!(receipt.status, IndexCancelStatus::Unknown) {
        return Err(invalid(
            "the owner no longer recognizes the selected index ticket",
        ));
    }
    let after = client.status(operation.key)?;
    if !operation.admits_observation(&after) {
        return Err(invalid(
            "index cancellation reconciliation changed operation identity",
        ));
    }
    if let IndexOperationObservation::Known(status) = &after {
        match &status.state {
            IndexOperationState::Active {
                ticket: current, ..
            } if current == &ticket && matches!(receipt.status, IndexCancelStatus::Requested) => {}
            IndexOperationState::Active { .. } | IndexOperationState::Accepted => {
                return Err(invalid(
                    "index cancellation reconciliation regressed its ticket or terminal",
                ));
            }
            _ => {}
        }
    }
    Ok(after)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::LocalProjectId;
    use crate::model::index_operation::tests::{claim, observation, published};
    use backend_library::{IndexJobStage, IndexOperationFailureReason, ProductText};
    use std::collections::VecDeque;
    use std::num::NonZeroU64;

    struct Client {
        reads: VecDeque<IndexOperationObservation>,
        receipt: Option<IndexCancelReceipt>,
        read_keys: Vec<IndexOperationKey>,
        cancelled: Vec<IndexJobTicket>,
    }
    impl CancellationClient for Client {
        fn status(
            &mut self,
            key: IndexOperationKey,
        ) -> Result<IndexOperationObservation, ClientError> {
            self.read_keys.push(key);
            self.reads
                .pop_front()
                .ok_or_else(|| invalid("unexpected extra status read"))
        }
        fn cancel(&mut self, ticket: IndexJobTicket) -> Result<IndexCancelReceipt, ClientError> {
            self.cancelled.push(ticket);
            self.receipt
                .take()
                .ok_or_else(|| invalid("unexpected extra cancellation"))
        }
    }
    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn active() -> TestResult<IndexOperationClaim> {
        let project = LocalProjectId::new("/fixture/exact-cancel")?;
        let mut operation = claim(&project, 17);
        let ticket = IndexJobTicket::new(
            NonZeroU64::new(3).ok_or("nonzero")?,
            [12; 16],
            operation.package.clone(),
        );
        operation.observation = Some(observation(
            &operation,
            IndexOperationState::Active {
                ticket,
                stage: IndexJobStage::Compiling,
            },
        ));
        Ok(operation)
    }
    fn client(
        operation: &IndexOperationClaim,
        after: IndexOperationObservation,
    ) -> TestResult<Client> {
        Ok(Client {
            reads: VecDeque::from([operation.observation.clone().ok_or("active")?, after]),
            receipt: Some(IndexCancelReceipt {
                ticket: operation.cancellation_ticket().ok_or("ticket")?,
                status: IndexCancelStatus::Requested,
            }),
            read_keys: Vec::new(),
            cancelled: Vec::new(),
        })
    }

    #[test]
    fn requested_cancel_keeps_the_actual_active_receipt_until_durable_terminal() -> TestResult {
        let operation = active()?;
        let active = operation.observation.clone().ok_or("active")?;
        let mut pending = client(&operation, active.clone())?;
        assert_eq!(cancel_and_reconcile(&mut pending, &operation)?, active);
        assert_eq!(
            pending.cancelled,
            [operation.cancellation_ticket().ok_or("ticket")?]
        );
        assert_eq!(pending.read_keys, [operation.key, operation.key]);
        let cancelled = observation(
            &operation,
            IndexOperationState::Failed {
                reason: IndexOperationFailureReason::Cancelled,
                detail: ProductText::from_static("cancelled before commit"),
                compiler_failure: None,
            },
        );
        let mut terminal = client(&operation, cancelled.clone())?;
        assert_eq!(cancel_and_reconcile(&mut terminal, &operation)?, cancelled);
        Ok(())
    }

    #[test]
    fn publication_race_and_unknown_key_never_send_a_new_mutation() -> TestResult {
        let operation = active()?;
        for before in [
            published(&operation),
            IndexOperationObservation::Unknown {
                operation_key: operation.key,
            },
        ] {
            let mut c = client(&operation, before.clone())?;
            c.reads = VecDeque::from([before.clone()]);
            assert_eq!(cancel_and_reconcile(&mut c, &operation)?, before);
            assert!(c.cancelled.is_empty());
            assert_eq!(c.read_keys, [operation.key]);
        }
        let publication = published(&operation);
        let mut raced = client(&operation, publication.clone())?;
        assert_eq!(cancel_and_reconcile(&mut raced, &operation)?, publication);
        assert_eq!(raced.cancelled.len(), 1);
        Ok(())
    }

    #[test]
    fn changed_operation_ticket_and_cancel_reply_are_refused_without_fabricated_terminal()
    -> TestResult {
        let operation = active()?;
        let active = operation.observation.clone().ok_or("active")?;
        let mut changed = operation.clone();
        if let Some(IndexOperationObservation::Known(status)) = changed.observation.as_mut() {
            if let IndexOperationState::Active { ticket, .. } = &mut status.state {
                *ticket = IndexJobTicket::new(
                    NonZeroU64::new(3).ok_or("id")?,
                    [13; 16],
                    operation.package.clone(),
                );
            }
        }
        let mut before = client(&operation, active.clone())?;
        before.reads[0] = changed.observation.clone().ok_or("other owner")?;
        assert!(cancel_and_reconcile(&mut before, &operation).is_err());
        assert!(
            before.cancelled.is_empty(),
            "never follow a changed owner ticket"
        );
        let mut reply = client(&operation, active.clone())?;
        reply.receipt.as_mut().ok_or("receipt")?.ticket =
            changed.cancellation_ticket().ok_or("other owner")?;
        assert!(cancel_and_reconcile(&mut reply, &operation).is_err());
        assert_eq!(
            reply.read_keys.len(),
            1,
            "invalid acknowledgement does not earn reconciliation"
        );
        let mut after = client(&operation, active)?;
        after.reads[1] = changed.observation.ok_or("other owner")?;
        assert!(cancel_and_reconcile(&mut after, &operation).is_err());
        assert_eq!(after.cancelled.len(), 1, "no automatic retry of a mutation");
        Ok(())
    }

    #[test]
    fn cancellation_refuses_changed_key_payload_and_unconfirmed_acknowledgements() -> TestResult {
        let operation = active()?;
        let active = operation.observation.clone().ok_or("active")?;
        for field in 0..4 {
            let mut wrong = active.clone();
            if let IndexOperationObservation::Known(status) = &mut wrong {
                match field {
                    0 => {
                        status.operation_key =
                            claim(&LocalProjectId::new("/fixture/other")?, 18).key
                    }
                    1 => {
                        status.package =
                            backend_library::PackageReference::parse("/fixture/other".to_owned())
                                .map_err(|error| std::io::Error::other(error.to_string()))?
                    }
                    2 => status.request_digest = [19; 32],
                    _ => {
                        status.execution_intent =
                            backend_library::CompileExecutionIntent::Background
                    }
                }
            }
            let mut c = client(&operation, active.clone())?;
            c.reads[0] = wrong;
            assert!(cancel_and_reconcile(&mut c, &operation).is_err());
            assert!(
                c.cancelled.is_empty(),
                "payload mismatch never authorizes a mutation"
            );
        }
        let mut unknown = client(&operation, active.clone())?;
        unknown.receipt.as_mut().ok_or("receipt")?.status = IndexCancelStatus::Unknown;
        assert!(cancel_and_reconcile(&mut unknown, &operation).is_err());
        assert_eq!(unknown.cancelled.len(), 1);
        assert_eq!(
            unknown.read_keys,
            [operation.key],
            "unknown ack cannot fabricate a terminal"
        );
        let mut lost = client(&operation, active.clone())?;
        lost.receipt = None;
        assert!(cancel_and_reconcile(&mut lost, &operation).is_err());
        assert_eq!(lost.cancelled.len(), 1, "a failed mutation is never resent");
        let mut nested = client(&operation, active)?;
        let ticket = IndexJobTicket::new(
            NonZeroU64::new(3).ok_or("id")?,
            [14; 16],
            operation.package.clone(),
        );
        nested.receipt.as_mut().ok_or("receipt")?.status =
            IndexCancelStatus::Terminal(backend_library::IndexJobTerminal {
                ticket,
                outcome: backend_library::IndexJobOutcome::Cancelled,
            });
        assert!(cancel_and_reconcile(&mut nested, &operation).is_err());
        assert_eq!(nested.read_keys, [operation.key]);
        Ok(())
    }
}
