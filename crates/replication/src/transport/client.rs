//! Correlated local control client.

use super::{
    LOCAL_CONTROL_MAX_PENDING, LOCAL_CONTROL_MAX_PENDING_BYTES, LocalControlError,
    LocalControlLimits, LocalControlRequest, LocalControlResponse, decode_response, encode_request,
    read_frame_into, write_frame,
};
use std::collections::{BTreeMap, btree_map::Entry};
use std::io::{Read, Write};

/// A bounded synchronous local control client.
///
/// The client owns only transport buffers and a bounded response mailbox. It
/// does not interpret command or subscription payloads, so CLI/MCP/desktop
/// can share this correlation and fragmentation behavior while their typed
/// adapters remain responsible for certificate admission.
pub struct LocalControlClient<S> {
    stream: S,
    limits: LocalControlLimits,
    receive: Vec<u8>,
    pending: BTreeMap<u64, LocalControlResponse>,
    pending_bytes: usize,
}

impl<S> LocalControlClient<S> {
    /// Wraps a local stream with shared framing limits.
    #[must_use]
    pub fn new(stream: S, limits: LocalControlLimits) -> Self {
        Self {
            stream,
            limits,
            receive: Vec::new(),
            pending: BTreeMap::new(),
            pending_bytes: 0,
        }
    }

    /// Returns the configured limits.
    #[must_use]
    pub const fn limits(&self) -> LocalControlLimits {
        self.limits
    }

    /// Returns the number of responses retained for other request IDs.
    #[must_use]
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Returns the encoded byte estimate retained by the response mailbox.
    #[must_use]
    pub const fn pending_bytes(&self) -> usize {
        self.pending_bytes
    }

    /// Consumes the client and returns the underlying stream.
    #[must_use]
    pub fn into_inner(self) -> S {
        self.stream
    }
}

impl<S: Read + Write> LocalControlClient<S> {
    /// Sends one raw control request and flushes it.
    ///
    /// # Errors
    ///
    /// Returns a shared framing, bound, or I/O error.
    pub fn send(&mut self, request: &LocalControlRequest) -> Result<(), LocalControlError> {
        let payload = encode_request(request, self.limits)?;
        write_frame(&mut self.stream, &payload, self.limits)
    }

    /// Reads and decodes one raw control response, reusing the receive buffer.
    ///
    /// # Errors
    ///
    /// Returns a shared framing, bound, or response-admission error.
    pub fn receive(&mut self) -> Result<LocalControlResponse, LocalControlError> {
        read_frame_into(&mut self.stream, &mut self.receive, self.limits)?;
        decode_response(&self.receive, self.limits)
    }

    /// Sends a request and waits for its exact correlated response.
    ///
    /// Responses for other in-flight request IDs are retained in a bounded
    /// mailbox and returned by a later call to [`Self::response_for`]. This is
    /// what permits one stream to carry interleaved acknowledgement and batch
    /// traffic without making callers reconstruct a global state machine.
    ///
    /// # Errors
    ///
    /// Returns a shared framing, bound, correlation, or I/O error.
    pub fn request(
        &mut self,
        request: &LocalControlRequest,
    ) -> Result<LocalControlResponse, LocalControlError> {
        let request_id = request.request_id();
        if self.pending.contains_key(&request_id) {
            return Err(LocalControlError::Invalid("duplicate pending request id"));
        }
        self.send(request)?;
        self.response_for(request_id)
    }

    /// Waits for one exact response correlation, retaining interleaved replies.
    ///
    /// # Errors
    ///
    /// Returns [`LocalControlError::PendingLimit`] if a peer sends more
    /// unrelated responses than the bounded mailbox can retain.
    pub fn response_for(
        &mut self,
        request_id: u64,
    ) -> Result<LocalControlResponse, LocalControlError> {
        if let Some(response) = self.pending.remove(&request_id) {
            self.pending_bytes = self.pending_bytes.saturating_sub(response_size(&response));
            return Ok(response);
        }
        loop {
            let response = self.receive()?;
            if response.request_id() == request_id {
                return Ok(response);
            }
            let response_bytes = response_size(&response);
            if self.pending.len() >= LOCAL_CONTROL_MAX_PENDING {
                return Err(LocalControlError::PendingLimit);
            }
            let next_bytes = self
                .pending_bytes
                .checked_add(response_bytes)
                .ok_or(LocalControlError::PendingLimit)?;
            if next_bytes > LOCAL_CONTROL_MAX_PENDING_BYTES {
                return Err(LocalControlError::PendingLimit);
            }
            match self.pending.entry(response.request_id()) {
                Entry::Occupied(_) => {
                    return Err(LocalControlError::Invalid("duplicate response correlation"));
                }
                Entry::Vacant(entry) => {
                    entry.insert(response);
                }
            }
            self.pending_bytes = next_bytes;
        }
    }
}

fn response_size(response: &LocalControlResponse) -> usize {
    const HEADER: usize = super::LOCAL_CONTROL_HEADER_BYTES + 4;
    match response {
        LocalControlResponse::Accepted { .. } | LocalControlResponse::Queued { .. } => HEADER,
        LocalControlResponse::AcceptedPayload { payload, .. } => {
            HEADER.saturating_add(payload.len())
        }
        LocalControlResponse::Rejected { message, .. } => HEADER.saturating_add(message.len()),
        LocalControlResponse::Subscription(response) => {
            HEADER.saturating_add(subscription_response_size(response))
        }
    }
}

fn subscription_response_size(response: &super::LocalSubscriptionResponse) -> usize {
    const LEASE: usize = super::SUBSCRIPTION_ID_BYTES;
    match response {
        super::LocalSubscriptionResponse::Opened { cursor, .. }
        | super::LocalSubscriptionResponse::Resumed { cursor, .. }
        | super::LocalSubscriptionResponse::Acked { cursor, .. }
        | super::LocalSubscriptionResponse::Renewed { cursor, .. } => {
            LEASE.saturating_add(cursor.len()).saturating_add(24)
        }
        super::LocalSubscriptionResponse::Batch {
            previous,
            cursor,
            payload,
            ..
        } => LEASE
            .saturating_add(previous.len())
            .saturating_add(cursor.len())
            .saturating_add(payload.len())
            .saturating_add(24),
        super::LocalSubscriptionResponse::ResetWithRoot {
            cursor, payload, ..
        } => LEASE
            .saturating_add(cursor.len())
            .saturating_add(payload.len())
            .saturating_add(24),
        super::LocalSubscriptionResponse::Cancelled { .. } => LEASE,
        super::LocalSubscriptionResponse::SnapshotPage {
            page,
            next,
            payload,
            ..
        } => LEASE
            .saturating_add(page.len())
            .saturating_add(next.as_ref().map_or(0, |value| value.len()))
            .saturating_add(payload.len())
            .saturating_add(24),
    }
}
