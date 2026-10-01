//! Oracle for untrusted native authority envelopes.
//!
//! [`NativeEnvelope::decode`] is the public decoder. The canonical seed is
//! [`NativeEnvelope::encode`] of one declaration record.

use backend_compile::{
    MAX_NATIVE_PAYLOAD_BYTES, NativeCoverage, NativeEnvelope, NativeRecord, NativeRecordKind,
};

use crate::{OracleFailure, Verdict};

/// Largest input this harness will mutate or replay.
///
/// The integer lives in the sibling `max_len` file so Nix and Rust share it.
/// It is far below [`MAX_NATIVE_PAYLOAD_BYTES`], so a counted record field
/// cannot reserve a protocol-sized buffer from a short campaign input.
pub(crate) const MAX_LEN: usize = crate::decimal_usize(include_str!("max_len"));

const _: () = assert!(MAX_LEN >= 128);
const _: () = assert!(MAX_LEN < MAX_NATIVE_PAYLOAD_BYTES);
const _: () = assert!(MAX_NATIVE_PAYLOAD_BYTES == 256 * 1024);

/// The committed `canonical` seed.
///
/// # Errors
///
/// Returns when the production encoder rejects the one-record envelope.
pub(crate) fn canonical() -> Result<Vec<u8>, String> {
    let record = NativeRecord::new(NativeRecordKind::Declaration, "item", vec![0x01])
        .map_err(|error| error.to_string())?;
    let envelope = NativeEnvelope::unbound(
        "rs",
        [0x11; 32],
        [0x22; 32],
        [0x33; 32],
        1,
        NativeCoverage::Partial,
        vec![record],
    )
    .map_err(|error| error.to_string())?;
    envelope.encode().map_err(|error| error.to_string())
}

/// Replays envelope decoding.
///
/// # Errors
///
/// Returns when two decodes disagree or an accepted envelope is not canonical.
pub(crate) fn exercise(bytes: &[u8]) -> Result<(), OracleFailure> {
    judge(bytes).map(|_| ())
}

/// Classifies one buffer after [`NativeEnvelope::decode`].
///
/// # Errors
///
/// Returns when a law fails. A clean rejection is [`Verdict::Rejected`].
pub(crate) fn judge(bytes: &[u8]) -> Result<Verdict, OracleFailure> {
    let first = NativeEnvelope::decode(bytes);
    let second = NativeEnvelope::decode(bytes);
    match (first, second) {
        (Ok(left), Ok(right)) => {
            if left != right {
                return Err(OracleFailure::new("native decode is not deterministic"));
            }
            if bytes.len() > MAX_NATIVE_PAYLOAD_BYTES {
                return Err(OracleFailure::new(
                    "native decode accepted a payload over the protocol byte cap",
                ));
            }
            let encoded = left.encode().map_err(|error| {
                OracleFailure::new(format!(
                    "accepted native envelope did not re-encode: {error}"
                ))
            })?;
            if encoded.as_slice() != bytes {
                return Err(OracleFailure::new(
                    "native decode accepted non-canonical bytes",
                ));
            }
            Ok(Verdict::Accepted)
        }
        (Err(_), Err(_)) => Ok(Verdict::Rejected),
        _ => Err(OracleFailure::new("native decode is not deterministic")),
    }
}
