use super::{
    DTO_VERSION, EventEnvelopeWire, WireCertificate, decode_event_with_certificate, ensure_version,
};
use crate::{CoverageCapability, Cursor, CursorEvent};

/// Versioned event DTO. Events carry the cursor at which they were observed;
/// view transition details remain in the checked in-process event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventDto {
    /// Correlated cursor position.
    pub cursor: Cursor,
    /// Event observed at this cursor.
    pub event: CursorEvent,
    /// Optional producer certificate carrying every logical identity
    /// preimage needed by a standalone receiver.
    certificate: Option<WireCertificate>,
}

impl EventDto {
    /// Creates an event DTO.
    #[must_use]
    pub const fn new(cursor: Cursor, event: CursorEvent) -> Self {
        Self {
            cursor,
            event,
            certificate: None,
        }
    }

    /// Attaches a producer certificate to this event envelope.
    #[must_use]
    pub fn with_certificate(mut self, certificate: WireCertificate) -> Self {
        self.certificate = Some(certificate);
        self
    }

    /// Returns the producer certificate, when one was attached.
    #[must_use]
    pub const fn certificate(&self) -> Option<&WireCertificate> {
        self.certificate.as_ref()
    }

    /// Returns the encoded DTO version.
    #[must_use]
    pub const fn version(&self) -> u16 {
        DTO_VERSION
    }

    /// Decodes a wire event by comparing it with a caller-owned accepted
    /// event. The returned event is the accepted typed value; no wire digest
    /// is ever used as an identity constructor.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed, unsupported, unknown-field, or changed
    /// wire payloads.
    pub fn decode_against(bytes: &[u8], expected: &Self) -> Result<Self, String> {
        let envelope: EventEnvelopeWire =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        ensure_version(envelope.version, "event")?;
        let observed = serde_json::to_value(&envelope).map_err(|error| error.to_string())?;
        let accepted = serde_json::to_value(expected).map_err(|error| error.to_string())?;
        if observed != accepted {
            return Err("wire event claims do not match the caller-owned event".to_owned());
        }
        Ok(expected.clone())
    }

    /// Decodes an event using producer preimages and optional complete source
    /// coverage capability.
    ///
    /// # Errors
    ///
    /// Returns an error when the cursor, intent, or checked view transition is
    /// not independently admitted.
    pub fn decode_with_certificate(
        bytes: &[u8],
        capability: Option<CoverageCapability>,
    ) -> Result<Self, String> {
        decode_event_with_certificate(bytes, capability)
    }
}
