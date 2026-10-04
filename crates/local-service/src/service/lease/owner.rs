//! The production [`LeaseSource`]: the one locald daemon behind the owner loop.

use super::super::subscription;
use super::super::{map_queue_error, wait_for_daemon_reply};
use super::host::{EventBatch, LeaseSource};
use crate::protocol::{EngineStatus, ProtocolError};
use backend_engine::{Cursor, CursorEvent, DaemonReply, SubscriptionReply};

/// Borrows the daemon for the duration of one lease request.
pub(crate) struct OwnerSource<'a, M, V, A>
where
    M: backend_engine::WorkspaceModel,
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
{
    daemon: &'a mut crate::Locald<M, V, A>,
}

impl<'a, M, V, A> OwnerSource<'a, M, V, A>
where
    M: backend_engine::WorkspaceModel,
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
{
    pub(crate) fn new(daemon: &'a mut crate::Locald<M, V, A>) -> Self {
        Self { daemon }
    }
}

impl<M, V, A> LeaseSource for OwnerSource<'_, M, V, A>
where
    M: backend_engine::WorkspaceModel,
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
{
    fn subscribe(
        &mut self,
        request_id: u64,
        cursor: &[u8],
        credit: usize,
    ) -> Result<SubscriptionReply, ProtocolError> {
        let receiver = self
            .daemon
            .client()
            .request(
                request_id,
                crate::Request::Subscribe {
                    cursor: cursor.to_vec().into_boxed_slice(),
                    credit,
                },
            )
            .map_err(|error| map_queue_error(&error))?;
        if !self.daemon.serve_one() {
            return Err(ProtocolError::Closed);
        }
        match wait_for_daemon_reply(self.daemon, &receiver)? {
            DaemonReply::Subscribed(Ok(reply)) => Ok(reply),
            DaemonReply::Subscribed(Err(error)) => Err(ProtocolError::InvalidControl(
                subscription_error_text(&error),
            )),
            DaemonReply::Commit(_)
            | DaemonReply::Query(_)
            | DaemonReply::Replicated(_)
            | DaemonReply::Completed(_) => Err(ProtocolError::InvalidControl(
                "subscription reply used the wrong engine lane",
            )),
        }
    }

    fn owner_cursor(&self) -> Result<Cursor, ProtocolError> {
        subscription::owner_cursor(self.daemon)
    }

    fn event_batch(
        &self,
        requested: &[u8],
        credit: usize,
        target: &[u8],
        events: &[CursorEvent],
    ) -> Result<EventBatch, ProtocolError> {
        let reply = SubscriptionReply::Events {
            credit,
            cursor: target.to_vec().into_boxed_slice(),
            events: events.to_vec().into_boxed_slice(),
        };
        let payload = match subscription::subscription_status(self.daemon, reply, Some(requested))?
        {
            EngineStatus::AcceptedPayload(payload) => payload,
            EngineStatus::Accepted
            | EngineStatus::Queued { .. }
            | EngineStatus::Rejected(_)
            | EngineStatus::Subscription(_)
            | EngineStatus::SemanticRangeChunk(_)
            | EngineStatus::SemanticMetadataChunk(_)
            | EngineStatus::SemanticStaleSelection => {
                return Err(ProtocolError::InvalidControl(
                    "subscription reply omitted its typed payload",
                ));
            }
        };
        let root = self.daemon.engine().daemon().library().view();
        let target_cursor = Cursor::decode_control_for_root(target, root)
            .map_err(|_| ProtocolError::InvalidControl("subscription target cursor"))?;
        let previous = event_previous_cursor(requested, target_cursor, events)
            .unwrap_or_else(|| requested.to_vec().into_boxed_slice());
        Ok(EventBatch { previous, payload })
    }
}

/// The cursor an event batch starts from: the one the holder asked for, or,
/// for an empty request, the cursor reached by rewinding the batch.
fn event_previous_cursor(
    requested: &[u8],
    mut target: Cursor,
    events: &[CursorEvent],
) -> Option<Box<[u8]>> {
    if !requested.is_empty() {
        return Some(requested.to_vec().into_boxed_slice());
    }
    for event in events.iter().rev() {
        target = target.rewind_event(event).ok()?;
    }
    Some(target.encode_control())
}

fn subscription_error_text(error: &backend_engine::DaemonError) -> &'static str {
    match error {
        backend_engine::DaemonError::SubscriptionCredit => "subscription credit rejected",
        backend_engine::DaemonError::CursorInvalid => "subscription cursor rejected",
        _ => "subscription request rejected",
    }
}
