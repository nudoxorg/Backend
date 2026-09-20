//! Cursor, subscription, and proof-bearing projection responses.

use crate::protocol::{EngineStatus, ProtocolError};
use backend_engine::{
    CursorEvent, EventDto, SnapshotPageDto, SubscriptionDto, SubscriptionReply, ViewPageCursor,
};

pub(super) fn subscription_status<M, V, A>(
    daemon: &crate::Locald<M, V, A>,
    reply: SubscriptionReply,
    requested_cursor: Option<&[u8]>,
) -> Result<EngineStatus, ProtocolError>
where
    M: backend_engine::WorkspaceModel,
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
{
    match reply {
        SubscriptionReply::Accepted { .. } => {
            let cursor = owner_cursor(daemon)?;
            if let Some(requested) = requested_cursor
                && !requested.is_empty()
                && requested != cursor.encode_control().as_ref()
            {
                return Err(ProtocolError::InvalidControl(
                    "accepted subscription cursor is not the current owner cursor",
                ));
            }
            let subscription = SubscriptionDto::try_events(cursor, cursor, Vec::<EventDto>::new())
                .map_err(|_| ProtocolError::InvalidControl("subscription encoding"))?;
            let payload = backend_engine::encode_subscription_dto(&subscription)
                .map_err(|_| ProtocolError::InvalidControl("subscription encoding"))?;
            Ok(EngineStatus::AcceptedPayload(payload.into_boxed_slice()))
        }
        SubscriptionReply::Events { cursor, events, .. } => {
            encode_subscription_events(daemon, requested_cursor, &cursor, &events)
        }
        SubscriptionReply::ResetWithRoot {
            cursor,
            root,
            reason,
            credit,
        } => {
            let owner = owner_cursor(daemon)?;
            if cursor.as_ref() != owner.encode_control().as_ref() {
                return Err(ProtocolError::InvalidControl(
                    "subscription reset cursor is not the current owner cursor",
                ));
            }
            encode_subscription_reset(&cursor, &root, reason, credit)
        }
        SubscriptionReply::Reset { .. } => Err(ProtocolError::InvalidControl(
            "subscription reset omitted its replacement root",
        )),
    }
}

/// Builds one bounded producer-certified reset page without materializing the
/// replacement root's compatibility row slice.
pub(super) fn snapshot_page(
    root: &backend_engine::ViewRoot,
    cursor: backend_engine::Cursor,
    reason: backend_engine::CursorResetReason,
    page_cursor: ViewPageCursor,
    credit: usize,
) -> Result<SnapshotPageDto, ProtocolError> {
    let descriptor = root.descriptor();
    let page = root
        .page(page_cursor, credit)
        .map_err(|_| ProtocolError::InvalidControl("subscription reset page unavailable"))?;
    let certificate =
        crate::builtin::certificate_for_snapshot_page(root, cursor, page.rows(), None).map_err(
            |_| ProtocolError::InvalidControl("subscription reset page certificate unavailable"),
        )?;
    SnapshotPageDto::from_owner(cursor, descriptor, page, reason)
        .map(|page| page.with_certificate(certificate))
        .map_err(|_| ProtocolError::InvalidControl("subscription reset page encoding"))
}

fn encode_subscription_events<M, V, A>(
    daemon: &crate::Locald<M, V, A>,
    requested_cursor: Option<&[u8]>,
    cursor_bytes: &[u8],
    events: &[CursorEvent],
) -> Result<EngineStatus, ProtocolError>
where
    M: backend_engine::WorkspaceModel,
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
{
    let source = owner_cursor(daemon)?;
    if cursor_bytes != source.encode_control().as_ref() {
        return Err(ProtocolError::InvalidControl(
            "subscription cursor is not the current owner cursor",
        ));
    }
    if let Some(requested) = requested_cursor
        && !requested.is_empty()
        && requested == source.encode_control().as_ref()
        && !events.is_empty()
    {
        return Err(ProtocolError::InvalidControl(
            "subscription returned events for its current cursor",
        ));
    }
    let count = u64::try_from(events.len())
        .map_err(|_| ProtocolError::InvalidControl("subscription event count"))?;
    let _start_sequence = source
        .sequence()
        .checked_sub(count)
        .ok_or(ProtocolError::InvalidControl("subscription event sequence"))?;

    // Walk backwards from the owner cursor through the shared cursor kernel.
    // Each event exposes its exact checked base/target identities; no cursor
    // identity is reconstructed from a client-supplied digest.
    let mut after = source;
    let mut reversed = Vec::with_capacity(events.len());
    for event in events.iter().rev() {
        let before = after
            .rewind_event(event)
            .map_err(|_| ProtocolError::InvalidControl("subscription event does not chain"))?;
        reversed.push((before, after, event.clone()));
        after = before;
    }
    if !requested_cursor.is_none_or(|requested| {
        requested.is_empty() || requested == encode_cursor_for_service(after).as_slice()
    }) {
        return Err(ProtocolError::InvalidControl(
            "subscription events do not start at the requested cursor",
        ));
    }
    reversed.reverse();
    let initial = after;
    let mut event_dtos = Vec::with_capacity(reversed.len());
    for (_before, after, event) in reversed {
        let certificate = match &event {
            CursorEvent::View { delta } => crate::builtin::certificate_for_compact_event(delta)
                .map_err(|_| {
                    ProtocolError::InvalidControl("subscription event certificate unavailable")
                })?,
            CursorEvent::Intent { .. } => {
                return Err(ProtocolError::InvalidControl(
                    "subscription intent certificate unavailable",
                ));
            }
        };
        event_dtos.push(EventDto::new(after, event).with_certificate(certificate));
    }
    let output = backend_engine::encode_compact_subscription_dto(initial, source, &event_dtos)
        .map_err(|_| ProtocolError::InvalidControl("subscription event encoding"))?;
    Ok(EngineStatus::AcceptedPayload(output.into_boxed_slice()))
}

/// Derives the owner cursor from the daemon's authoritative binary cursor
/// envelope and the current typed view root. A stale cached projection cannot
/// be advertised as process health or subscription state.
pub(super) fn owner_cursor<M, V, A>(
    daemon: &crate::Locald<M, V, A>,
) -> Result<backend_engine::Cursor, ProtocolError>
where
    M: backend_engine::WorkspaceModel,
    M::Intent: backend_engine::QueueSized,
    V: backend_engine::OutputAdmissionValidator + backend_engine::SemanticCoverageValidator,
    A: backend_engine::AttestationVerifier + Send + Sync + 'static,
{
    let library = daemon.engine().daemon().library();
    let cursor_bytes = daemon.engine().daemon().cursor_bytes();
    let cursor = backend_engine::Cursor::decode_control_for_root(&cursor_bytes, library.view())
        .map_err(|_| ProtocolError::InvalidControl("owner cursor is not bound to its view root"))?;
    if cursor != library.cursor() {
        return Err(ProtocolError::InvalidControl(
            "owner cursor disagrees with the library projection",
        ));
    }
    Ok(cursor)
}

fn encode_subscription_reset(
    cursor_bytes: &[u8],
    root: &backend_engine::ViewRoot,
    reason: backend_engine::CursorResetReason,
    credit: usize,
) -> Result<EngineStatus, ProtocolError> {
    let cursor = decode_cursor_for_service(cursor_bytes, root)?;
    let page_credit = credit.min(backend_engine::MAX_SNAPSHOT_PAGE_ROWS);
    if page_credit == 0 {
        return Err(ProtocolError::InvalidControl(
            "subscription reset page credit",
        ));
    }
    let page = snapshot_page(
        root,
        cursor,
        reason,
        ViewPageCursor::first(root),
        page_credit,
    )?;
    let output = backend_engine::encode_snapshot_page_dto(&page)
        .map_err(|_| ProtocolError::InvalidControl("subscription reset page encoding"))?;
    Ok(EngineStatus::AcceptedPayload(output.into_boxed_slice()))
}

fn encode_cursor_for_service(cursor: backend_engine::Cursor) -> Vec<u8> {
    cursor.encode_control().into_vec()
}

pub(super) fn decode_cursor_for_service(
    bytes: &[u8],
    root: &backend_engine::ViewRoot,
) -> Result<backend_engine::Cursor, ProtocolError> {
    backend_engine::Cursor::decode_control_for_root(bytes, root)
        .map_err(|_| ProtocolError::InvalidControl("subscription reset cursor"))
}
