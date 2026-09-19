//! Canonical transport message envelopes and shared ownership.

use std::sync::Arc;

use crate::{
    CapabilityManifest, Frame, NodeRequest, RangeRequest, RootSummary, TransportLimits,
    WireNodeRequest, WireRangeRequest, WireRecipeRequest, WireRecipeResult, WireRootSummary,
};

use super::closure::{
    ClosureNeedRequest, ClosurePageRequest, ClosurePageResponse, ClosureRootAck, ClosureRootOffer,
};
use super::control::CancelAttempt;
use super::error::ReplicationError;
use super::pack::{WirePack, WirePackClaim, coverage_wire_size};
use super::requests::{ResumeRequest, WireResumeRequest};

/// A typed local/remote protocol message. Local transport uses the same enum
/// as a remote implementation so tests exercise equivalent framing behavior.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportMessage {
    /// Untrusted immutable physical pack claim. The receiver must call
    /// [`WirePackClaim::admit_against`] with its expected layout before the
    /// pack enters backend-store.
    WirePack(WirePackClaim),
    /// Capability/schema/recipe advertisement.
    Capabilities(CapabilityManifest),
    /// Checked root summary.
    RootSummary(RootSummary),
    /// Untrusted root summary waiting for an expected workspace root.
    WireRootSummary(WireRootSummary),
    /// Correlated bounded Merkle page request.
    ClosurePageRequest(ClosurePageRequest),
    /// Correlated bounded Merkle page response.
    ClosurePageResponse(ClosurePageResponse),
    /// Constant-time authenticated closure root offer.
    ClosureRootOffer(ClosureRootOffer),
    /// Acknowledgement issued only after durable closure admission.
    ClosureRootAck(ClosureRootAck),
    /// Requests the next bounded object need batch.
    ClosureNeedRequest(ClosureNeedRequest),
    /// Immutable node request.
    NodeRequest(NodeRequest),
    /// Untrusted immutable node request.
    WireNodeRequest(WireNodeRequest),
    /// Relation range request.
    RangeRequest(RangeRequest),
    /// Untrusted relation range request.
    WireRangeRequest(WireRangeRequest),
    /// Unvalidated chunk frame at the wire boundary.
    Chunk(Frame),
    /// Resumable transfer request.
    Resume(ResumeRequest),
    /// Untrusted resumable transfer request.
    WireResumeRequest(WireResumeRequest),
    /// Opaque pure recipe request at the wire boundary.
    WireRecipeRequest(WireRecipeRequest),
    /// Opaque pure recipe result receipt at the wire boundary.
    WireRecipeResult(Box<WireRecipeResult>),
    /// Untrusted cancellation command for an active pure execution attempt.
    /// The receiver must call [`CancelAttempt::admit_against`] before
    /// notifying its execution-owned cancellation handle.
    CancelAttempt(CancelAttempt),
}

/// An immutable shared handle for a transport message.
///
/// The ordinary enum remains available for callers that need to inspect or
/// mutate a locally constructed claim. Once a message is ready for fan-out or
/// queue handoff, this owner lets clones share the complete envelope and its
/// potentially large result or pack payload instead of cloning nested `Vec`s.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SharedTransportMessage(Arc<TransportMessage>);
impl SharedTransportMessage {
    /// Wraps one transport message in an immutable shared owner.
    #[must_use]
    pub fn new(message: TransportMessage) -> Self {
        Self(Arc::new(message))
    }

    /// Borrows the underlying transport message for encoding or inspection.
    #[must_use]
    pub fn as_message(&self) -> &TransportMessage {
        &self.0
    }

    /// Returns the number of handles sharing this message.
    #[must_use]
    pub fn owner_count(&self) -> usize {
        Arc::strong_count(&self.0)
    }

    /// Consumes this owner, recovering the enum when it is unique.
    ///
    /// If another handle remains, recovering an independently mutable enum
    /// necessarily clones the message; callers that only need read access
    /// should retain [`Self::as_message`] instead.
    #[must_use]
    pub fn into_message(self) -> TransportMessage {
        Arc::try_unwrap(self.0).unwrap_or_else(|message| (*message).clone())
    }
}

impl TransportMessage {
    /// Wraps a transport message in an immutable shared owner for fan-out or
    /// queue handoff.
    #[must_use]
    pub fn into_shared(self) -> SharedTransportMessage {
        SharedTransportMessage::new(self)
    }

    /// Wraps a backend-store wire pack as an untrusted transport claim.
    #[must_use]
    pub fn from_wire_pack(pack: WirePack) -> Self {
        Self::WirePack(pack.into())
    }

    /// Encodes this message through the canonical versioned wire codec.
    ///
    /// Typed convenience variants are lowered to wire claims before bytes
    /// leave the process.
    ///
    /// # Errors
    ///
    /// Returns a validation, size, identity, or codec error when this message
    /// cannot be represented within `limits`.
    pub fn encode(&self, limits: TransportLimits) -> Result<Vec<u8>, ReplicationError> {
        crate::encode_message(self, limits)
    }

    /// Decodes one complete versioned wire message.
    ///
    /// The decoder returns claim variants when a message carries identities;
    /// callers must perform exact typed admission before using those claims.
    ///
    /// # Errors
    ///
    /// Returns a truncation, version, size, tag, or structural error when the
    /// bytes are not one complete message accepted by `limits`.
    pub fn decode(bytes: &[u8], limits: TransportLimits) -> Result<Self, ReplicationError> {
        crate::decode_message(bytes, limits)
    }

    /// Returns a conservative bounded size estimate for queue admission.
    pub fn estimated_size(&self) -> usize {
        match self {
            Self::WirePack(pack) => 10usize.saturating_add(pack.wire_size()),
            // Envelope + fixed capability fields + resource envelope. Each
            // schema is seven bytes and each recipe is one 37-byte wire
            // identity plus its four-byte version range.
            Self::Capabilities(capabilities) => 94usize
                .saturating_add(capabilities.schemas.len().saturating_mul(7))
                .saturating_add(capabilities.recipes.len().saturating_mul(41)),
            Self::RootSummary(summary) => 96usize
                .saturating_add(summary.objects().len().saturating_mul(80))
                .saturating_add(summary.relations().len().saturating_mul(64))
                .saturating_add(coverage_wire_size(summary.coverage()))
                .saturating_add(
                    summary
                        .relations()
                        .iter()
                        .map(|relation| coverage_wire_size(&relation.coverage))
                        .fold(0usize, usize::saturating_add),
                ),
            Self::WireRootSummary(summary) => 96usize
                .saturating_add(summary.objects.len().saturating_mul(80))
                .saturating_add(summary.relations.len().saturating_mul(64))
                .saturating_add(coverage_wire_size(&summary.coverage))
                .saturating_add(
                    summary
                        .relations
                        .iter()
                        .map(|relation| coverage_wire_size(&relation.coverage))
                        .fold(0usize, usize::saturating_add),
                ),
            Self::ClosurePageRequest(_) => 128,
            Self::ClosurePageResponse(response) => {
                let body = match &response.page.body {
                    crate::MerklePageBody::Branch(children) => children
                        .iter()
                        .map(|child| {
                            64usize
                                .saturating_add(child.first_key.len())
                                .saturating_add(child.end_key.as_ref().map_or(0, Vec::len))
                        })
                        .fold(0usize, usize::saturating_add),
                    crate::MerklePageBody::Leaf(entries) => entries
                        .iter()
                        .map(|entry| 80usize.saturating_add(entry.key.len()))
                        .fold(0usize, usize::saturating_add),
                };
                128usize
                    .saturating_add(response.proof.len())
                    .saturating_add(body)
            }
            Self::ClosureRootOffer(offer) => {
                128usize.saturating_add(offer.workspace_manifest.len())
            }
            Self::ClosureRootAck(ack) => 128usize
                .saturating_add(ack.missing.len().saturating_mul(40))
                .saturating_add(ack.next.map_or(1, |_| 5)),
            Self::ClosureNeedRequest(_) => 64,
            Self::NodeRequest(request) => {
                128usize.saturating_add(request.resume.ranges().len().saturating_mul(16))
            }
            Self::WireNodeRequest(request) => {
                128usize.saturating_add(request.resume.ranges().len().saturating_mul(16))
            }
            Self::RangeRequest(request) => 128usize
                .saturating_add(request.start.len())
                .saturating_add(request.end.as_ref().map_or(0, Vec::len))
                .saturating_add(request.resume.ranges().len().saturating_mul(16)),
            Self::WireRangeRequest(request) => 128usize
                .saturating_add(request.start.len())
                .saturating_add(request.end.as_ref().map_or(0, Vec::len))
                .saturating_add(request.resume.ranges().len().saturating_mul(16)),
            Self::Chunk(frame) => 192usize.saturating_add(frame.payload.len()),
            Self::Resume(request) => 128usize.saturating_add(coverage_wire_size(&request.coverage)),
            Self::WireResumeRequest(request) => {
                128usize.saturating_add(coverage_wire_size(&request.coverage))
            }
            Self::WireRecipeRequest(request) => 256usize
                .saturating_add(request.inputs.len().saturating_mul(32))
                .saturating_add(32),
            Self::WireRecipeResult(result) => 320usize
                .saturating_add(result.inputs.len().saturating_mul(32))
                .saturating_add(result.output_bytes.len())
                .saturating_add(64)
                .saturating_add(coverage_wire_size(&result.byte_coverage)),
            Self::CancelAttempt(_) => 119,
        }
    }
    /// Checks the message against transport allocation limits.
    ///
    /// # Errors
    ///
    /// Returns a size, capability, request, or coverage error when the message
    /// exceeds the negotiated limits.
    pub fn validate(&self, limits: TransportLimits) -> Result<(), ReplicationError> {
        limits.validate()?;
        if self.estimated_size() > limits.max_frame {
            return Err(ReplicationError::MessageTooLarge);
        }
        match self {
            Self::RootSummary(summary) => summary.validate(limits),
            Self::WireRootSummary(summary) => summary.validate(limits),
            Self::ClosurePageRequest(request) => request.validate(limits),
            Self::ClosurePageResponse(response) => response.validate(limits),
            Self::ClosureRootOffer(offer) => offer.validate(limits),
            Self::ClosureRootAck(ack) => ack.validate(limits),
            Self::ClosureNeedRequest(request) => request.validate(limits),
            Self::NodeRequest(request) => request.validate(limits),
            Self::WireNodeRequest(request) => request.validate(limits),
            Self::RangeRequest(request) => request.validate(limits),
            Self::WireRangeRequest(request) => request.validate(limits),
            Self::Resume(request) => request.validate(limits),
            Self::WireResumeRequest(request) => request.validate(limits),
            Self::WireRecipeRequest(request) => request.validate(limits),
            Self::WireRecipeResult(result) => result.validate(limits),
            Self::WirePack(pack) => pack.validate(limits),
            Self::Capabilities(capabilities) => capabilities.validate(limits),
            Self::Chunk(frame) => frame.validate(limits),
            Self::CancelAttempt(cancel) => cancel.validate(limits),
        }
    }
}
