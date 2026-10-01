//! Oracle for one bounded `RPL2` replication message.
//!
//! The production default frame budget is 1 MiB and several counters default
//! to 4096. This harness passes a much smaller limit set so a hostile count
//! cannot reserve thousands of entries during `cargo test`.

use backend_replication::{
    ClosureNeedRequest, MerkleRoot, ReplicationError, TransportLimits, TransportMessage,
    decode_message, encode_message,
};
use backend_version::{ObjectVersion, Schema};

use crate::{OracleFailure, Verdict};

/// Largest input this harness will mutate or replay.
///
/// The integer lives in the sibling `max_len` file so Nix and Rust share it.
pub(crate) const MAX_LEN: usize = crate::decimal_usize(include_str!("max_len"));

struct SeedSchema;
impl Schema for SeedSchema {
    const DOMAIN: u8 = 0x51;
    const TYPE: u16 = 1;
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// Limits installed by every replication harness call.
///
/// Counters stay tiny because decoders reserve `Vec` capacity from the
/// counted field before they consume the following items.
const fn transport_limits() -> TransportLimits {
    TransportLimits {
        max_frame: MAX_LEN,
        max_chunk: 4 * 1024,
        max_object: 8 * 1024,
        max_objects: 32,
        max_ranges: 32,
        max_capabilities: 16,
        max_key_bytes: 256,
        max_inputs: 16,
    }
}

/// One valid closure-need message, the `canonical` seed.
///
/// # Errors
///
/// Returns when the negotiated limits or the encoded frame are rejected.
pub(crate) fn canonical() -> Result<Vec<u8>, String> {
    let limits = transport_limits()
        .validate()
        .map_err(|error: ReplicationError| error.to_string())?;
    let version = ObjectVersion::<SeedSchema>::from_value(&[1]);
    let message = TransportMessage::ClosureNeedRequest(ClosureNeedRequest {
        correlation: 1,
        root: MerkleRoot::from_admitted_manifest(1, version),
        cursor: 0,
    });
    encode_message(&message, limits).map_err(|error| error.to_string())
}

/// Decodes one buffer and requires every success to re-encode as a fixpoint.
///
/// # Errors
///
/// Returns when two decodes disagree or a decoded message is not stable under
/// `encode_message`.
pub(crate) fn exercise(bytes: &[u8]) -> Result<(), OracleFailure> {
    judge(bytes).map(|_| ())
}

/// Classifies one buffer after the replication fixpoint law runs.
///
/// # Errors
///
/// Returns when a decoded message is not a stable fixpoint. A clean rejection
/// is [`Verdict::Rejected`], not an error.
pub(crate) fn judge(bytes: &[u8]) -> Result<Verdict, OracleFailure> {
    let limits = transport_limits();
    let decoded = decode_message(bytes, limits);
    let again = decode_message(bytes, limits);
    if decoded != again {
        return Err(OracleFailure::new(
            "replication decode is not deterministic",
        ));
    }
    let Ok(message) = decoded else {
        return Ok(Verdict::Rejected);
    };
    let encoded = encode_message(&message, limits).map_err(|error| {
        OracleFailure::new(format!(
            "decoded replication message did not re-encode: {error}"
        ))
    })?;
    if encoded.len() > MAX_LEN {
        return Err(OracleFailure::new(
            "replication encoder exceeded the harness frame cap",
        ));
    }
    let round = decode_message(&encoded, limits).map_err(|error| {
        OracleFailure::new(format!("canonical replication frame was rejected: {error}"))
    })?;
    if round != message {
        return Err(OracleFailure::new(
            "replication encode/decode is not a fixpoint",
        ));
    }
    let stable = encode_message(&round, limits).map_err(|error| {
        OracleFailure::new(format!("second replication encode failed: {error}"))
    })?;
    if stable != encoded {
        return Err(OracleFailure::new("replication encode is not stable"));
    }
    Ok(Verdict::Accepted)
}
