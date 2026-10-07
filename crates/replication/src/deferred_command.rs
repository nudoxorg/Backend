//! Explicit opt-in for one authenticated, finite deferred local command.
//!
//! This envelope is independent of the terminal library DTO grammar. The
//! exact original DTO bytes include its identity, root, query, and certificate.
//! An ACK has authority only after admission by the affine local peer token;
//! decoding a frame, matching a claimed principal, or seeing busy is insufficient.

use std::time::Duration;

/// Local command opt-in envelope magic. Old endpoints reject this grammar.
pub const DEFERRED_COMMAND_REQUEST_MAGIC: [u8; 4] = *b"LDQ1";
/// Deferred admission response magic, distinct from JSON and control frames.
pub const DEFERRED_COMMAND_ACK_MAGIC: [u8; 4] = *b"LDA1";
/// Closed envelope version; unknown versions never renew a read lease.
pub const DEFERRED_COMMAND_VERSION: u8 = 1;
/// Existing finite owner deadline; this protocol never grants more.
pub const MAX_DEFERRED_COMMAND_WAIT: Duration = Duration::from_secs(15 * 60);
const HEADER: usize = 8;
const ACK_BYTES: usize = 80;

/// Failure to decode or correlate a deferred command envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeferredCommandError(pub &'static str);
impl std::fmt::Display for DeferredCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for DeferredCommandError {}

fn header(bytes: &[u8], magic: [u8; 4], dto_version: u16) -> Result<(), DeferredCommandError> {
    if bytes.len() < HEADER
        || bytes[..4] != magic
        || bytes[4] != DEFERRED_COMMAND_VERSION
        || bytes[5] != 0
        || bytes[6..8] != dto_version.to_be_bytes()
    {
        return Err(DeferredCommandError(
            "deferred command frame/version mismatch",
        ));
    }
    Ok(())
}

/// Wraps an exact admitted library request, explicitly opting into one ACK.
/// # Errors
/// Refuses an empty or oversized body.
pub fn wrap_deferred_command(
    body: &[u8],
    dto_version: u16,
) -> Result<Vec<u8>, DeferredCommandError> {
    if body.is_empty() || body.len() > crate::LOCAL_CONTROL_MAX_FRAME - HEADER {
        return Err(DeferredCommandError(
            "deferred command body exceeds frame bound",
        ));
    }
    let mut bytes = Vec::with_capacity(HEADER + body.len());
    bytes.extend_from_slice(&DEFERRED_COMMAND_REQUEST_MAGIC);
    bytes.extend_from_slice(&[DEFERRED_COMMAND_VERSION, 0]);
    bytes.extend_from_slice(&dto_version.to_be_bytes());
    bytes.extend_from_slice(body);
    Ok(bytes)
}

/// Returns the exact original command only for this closed opt-in grammar.
/// # Errors
/// Refuses malformed, incompatible, empty, or oversized opt-in frames.
pub fn unwrap_deferred_command(
    bytes: &[u8],
    dto_version: u16,
) -> Result<Option<&[u8]>, DeferredCommandError> {
    if !bytes.starts_with(&DEFERRED_COMMAND_REQUEST_MAGIC) {
        return Ok(None);
    }
    header(bytes, DEFERRED_COMMAND_REQUEST_MAGIC, dto_version)?;
    if bytes.len() <= HEADER || bytes.len() > crate::LOCAL_CONTROL_MAX_FRAME {
        return Err(DeferredCommandError(
            "deferred command body exceeds frame bound",
        ));
    }
    Ok(Some(&bytes[HEADER..]))
}

/// One owner's admitted response wait. The wire claim alone conveys no authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeferredCommandAck {
    principal: [u8; 32],
    original: [u8; 32],
    budget_ms: u64,
}
impl DeferredCommandAck {
    /// Creates an ACK only after the owner has accepted the exact request.
    /// # Errors
    /// Refuses a zero or unbounded admitted deadline.
    pub fn admitted(
        original: &[u8],
        principal: [u8; 32],
        budget: Duration,
    ) -> Result<Self, DeferredCommandError> {
        let budget_ms = u64::try_from(budget.as_millis())
            .map_err(|_| DeferredCommandError("deferred deadline overflow"))?;
        if budget_ms == 0 || budget > MAX_DEFERRED_COMMAND_WAIT {
            return Err(DeferredCommandError(
                "deferred deadline is not finite and bounded",
            ));
        }
        Ok(Self {
            principal,
            original: *blake3::hash(original).as_bytes(),
            budget_ms,
        })
    }
    /// Encodes this bounded ACK under an explicit terminal DTO version guard.
    #[must_use]
    pub fn encode(&self, dto_version: u16) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(ACK_BYTES);
        bytes.extend_from_slice(&DEFERRED_COMMAND_ACK_MAGIC);
        bytes.extend_from_slice(&[DEFERRED_COMMAND_VERSION, 0]);
        bytes.extend_from_slice(&dto_version.to_be_bytes());
        bytes.extend_from_slice(&self.principal);
        bytes.extend_from_slice(&self.original);
        bytes.extend_from_slice(&self.budget_ms.to_be_bytes());
        bytes
    }
    pub(crate) fn admit(
        bytes: &[u8],
        original: &[u8],
        principal: [u8; 32],
        dto_version: u16,
    ) -> Result<Duration, DeferredCommandError> {
        header(bytes, DEFERRED_COMMAND_ACK_MAGIC, dto_version)?;
        if bytes.len() != ACK_BYTES
            || bytes[8..40] != principal
            || bytes[40..72] != *blake3::hash(original).as_bytes()
        {
            return Err(DeferredCommandError(
                "deferred ACK does not bind this authenticated request",
            ));
        }
        let mut budget = [0; 8];
        budget.copy_from_slice(&bytes[72..80]);
        let duration = Duration::from_millis(u64::from_be_bytes(budget));
        if duration.is_zero() || duration > MAX_DEFERRED_COMMAND_WAIT {
            return Err(DeferredCommandError(
                "deferred ACK deadline exceeds owner bound",
            ));
        }
        Ok(duration)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deferred_command_ack_closed_grammar_binds_all_original_bytes_and_finite_budget() {
        let original = br#"{"request_id":7,"basis":"root","query":"needle","certificate":"exact"}"#;
        let ack = DeferredCommandAck::admitted(original, [9; 32], Duration::from_secs(90))
            .expect("finite ack")
            .encode(22);
        assert_eq!(
            DeferredCommandAck::admit(&ack, original, [9; 32], 22).expect("exact"),
            Duration::from_secs(90)
        );
        for changed in [
            br#"{"request_id":8,"basis":"root","query":"needle","certificate":"exact"}"#.as_slice(),
            br#"{"request_id":7,"basis":"other","query":"needle","certificate":"exact"}"#
                .as_slice(),
            br#"{"request_id":7,"basis":"root","query":"different","certificate":"exact"}"#
                .as_slice(),
            br#"{"request_id":7,"basis":"root","query":"needle","certificate":"other"}"#.as_slice(),
        ] {
            assert!(DeferredCommandAck::admit(&ack, changed, [9; 32], 22).is_err());
        }
        assert!(DeferredCommandAck::admit(&ack, original, [8; 32], 22).is_err());
        assert!(DeferredCommandAck::admit(&ack, original, [9; 32], 23).is_err());
        assert!(DeferredCommandAck::admitted(original, [9; 32], Duration::ZERO).is_err());
        assert!(
            DeferredCommandAck::admitted(
                original,
                [9; 32],
                MAX_DEFERRED_COMMAND_WAIT + Duration::from_millis(1)
            )
            .is_err()
        );
        let wrapped = wrap_deferred_command(original, 22).expect("opt in");
        assert_eq!(
            unwrap_deferred_command(&wrapped, 22).expect("closed request"),
            Some(original.as_slice())
        );
        assert!(unwrap_deferred_command(&wrapped, 23).is_err());
        assert!(
            unwrap_deferred_command(original, 22)
                .expect("legacy")
                .is_none()
        );
        assert!(unwrap_deferred_command(&wrapped[..7], 22).is_err());
    }
}
