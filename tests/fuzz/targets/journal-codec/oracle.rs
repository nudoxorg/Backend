//! Oracle for effect and dispatch journal record bytes.
//!
//! Both codecs are [`JournalCodec`]. A buffer is accepted when either grammar
//! reaches a stable canonical encode. `canonical` is the effect record.
//! `corpus/dispatch` is the dispatch record. `corpus/cancelled-root` is a
//! minimized `Terminal::Cancelled` whose unused output root is not zero;
//! decode keeps the record and encode writes that root back as zeros.

use backend_engine::dispatch::{DISPATCH_RECORD_VERSION, DispatchAttemptKey, DispatchRecord};
use backend_engine::effects::EffectJournalRecord;
use backend_engine::{DispatchLog, EffectLog, JournalCodec};

use crate::{OracleFailure, Verdict};

/// Largest input this harness will mutate or replay.
///
/// The integer lives in the sibling `max_len` file so Nix and Rust share it.
/// Record length prefixes are checked against the remaining input, so this
/// cap is also the largest buffer a successful decode can copy.
pub(crate) const MAX_LEN: usize = crate::decimal_usize(include_str!("max_len"));

const _: () = assert!(MAX_LEN >= 64);
const _: () = assert!(MAX_LEN <= 1024 * 1024);

/// The committed `canonical` seed: one prepared effect record.
///
/// # Errors
///
/// Encoding this literal does not fail. The discovered target table still
/// requires a `Result`.
#[allow(
    clippy::unnecessary_wraps,
    reason = "Target::canonical is fn() -> Result<Vec<u8>, String>"
)]
pub(crate) fn canonical() -> Result<Vec<u8>, String> {
    Ok(effect_bytes())
}

/// Fresh encode of the committed `dispatch` seed.
///
/// The non-test library build does not read `corpus/dispatch`. The seed test
/// does, so this function is live only under `cfg(test)`.
///
/// # Errors
///
/// Returns when the attempt ordinal is rejected. The literal ordinal is 1.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "the seed test is the only caller")
)]
pub(crate) fn dispatch_bytes() -> Result<Vec<u8>, String> {
    let key = DispatchAttemptKey::new([0x21; 32], 1)
        .map_err(|_| "dispatch attempt key was rejected".to_string())?;
    let record = DispatchRecord::PublicationPending { key };
    let mut out = Vec::new();
    DispatchLog::encode(&record, &mut out);
    if out.first().copied() != Some(DISPATCH_RECORD_VERSION) {
        return Err("dispatch encode did not start with the record version".to_string());
    }
    Ok(out)
}

/// Replays both journal grammars.
///
/// # Errors
///
/// Returns when decode and validate disagree or an accepted record is not canonical.
pub(crate) fn exercise(bytes: &[u8]) -> Result<(), OracleFailure> {
    judge(bytes).map(|_| ())
}

/// Classifies one buffer after the effect and dispatch codecs.
///
/// # Errors
///
/// Returns when a law fails. A clean rejection is [`Verdict::Rejected`].
pub(crate) fn judge(bytes: &[u8]) -> Result<Verdict, OracleFailure> {
    let effect = effect_round_trip(bytes)?;
    let dispatch = dispatch_round_trip(bytes)?;
    if effect || dispatch {
        Ok(Verdict::Accepted)
    } else {
        Ok(Verdict::Rejected)
    }
}

fn effect_bytes() -> Vec<u8> {
    let record = EffectJournalRecord::Prepared {
        key: [0x11; 32],
        intent: b"seed".to_vec(),
    };
    let mut out = Vec::new();
    EffectLog::encode(&record, &mut out);
    out
}

fn effect_round_trip(bytes: &[u8]) -> Result<bool, OracleFailure> {
    let decoded = EffectLog::decode(bytes);
    let validated = EffectLog::validate(bytes);
    match (decoded, validated) {
        (Ok(record), Ok(())) => {
            let again = EffectLog::decode(bytes).map_err(|_| {
                OracleFailure::new("effect decode accepted and then rejected the same bytes")
            })?;
            if again != record {
                return Err(OracleFailure::new("effect decode is not deterministic"));
            }
            stable_encode(&record, EffectLog::decode, EffectLog::encode, "effect")?;
            Ok(true)
        }
        (Err(_), Err(_)) => Ok(false),
        _ => Err(OracleFailure::new("effect decode and validate disagree")),
    }
}

fn dispatch_round_trip(bytes: &[u8]) -> Result<bool, OracleFailure> {
    let decoded = DispatchLog::decode(bytes);
    let validated = DispatchLog::validate(bytes);
    match (decoded, validated) {
        (Ok(record), Ok(())) => {
            let again = DispatchLog::decode(bytes).map_err(|_| {
                OracleFailure::new("dispatch decode accepted and then rejected the same bytes")
            })?;
            if again != record {
                return Err(OracleFailure::new("dispatch decode is not deterministic"));
            }
            stable_encode(
                &record,
                DispatchLog::decode,
                DispatchLog::encode,
                "dispatch",
            )?;
            Ok(true)
        }
        (Err(_), Err(_)) => Ok(false),
        _ => Err(OracleFailure::new("dispatch decode and validate disagree")),
    }
}

fn stable_encode<Record, DecodeError>(
    record: &Record,
    decode: fn(&[u8]) -> Result<Record, DecodeError>,
    encode: fn(&Record, &mut Vec<u8>),
    name: &str,
) -> Result<(), OracleFailure>
where
    Record: PartialEq,
{
    let mut encoded = Vec::new();
    encode(record, &mut encoded);
    let recoded = decode(&encoded)
        .map_err(|_| OracleFailure::new(format!("{name} canonical bytes did not decode")))?;
    if &recoded != record {
        return Err(OracleFailure::new(format!(
            "{name} encode changed the decoded record"
        )));
    }
    let mut second = Vec::new();
    encode(&recoded, &mut second);
    if second != encoded {
        return Err(OracleFailure::new(format!(
            "{name} canonical encode is not stable"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod dispatch_seed {
    use super::dispatch_bytes;
    use crate::{Verdict, target};
    use backend_engine::{DispatchLog, JournalCodec};

    #[test]
    fn dispatch_seed_is_the_fresh_encode() -> Result<(), String> {
        let fresh = dispatch_bytes()?;
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/targets/journal-codec/corpus/dispatch"
        );
        let stored = std::fs::read(path).map_err(|error| format!("reading {path}: {error}"))?;
        if stored != fresh {
            return Err(format!(
                "dispatch seed drifted (stored {} bytes, fresh {} bytes, fresh hex {})",
                stored.len(),
                fresh.len(),
                crate::hex_bytes(&fresh)
            ));
        }
        let Some(harness) = target("journal-codec") else {
            return Err("journal-codec was not discovered".to_string());
        };
        match (harness.judge)(&stored) {
            Ok(Verdict::Accepted) => Ok(()),
            Ok(Verdict::Rejected) => Err("dispatch seed was rejected".to_string()),
            Err(error) => Err(error.to_string()),
        }
    }

    #[test]
    fn cancelled_terminal_root_canonicalizes() -> Result<(), String> {
        let bytes = include_bytes!("corpus/cancelled-root");
        let record = DispatchLog::decode(bytes)
            .map_err(|error| format!("minimized terminal did not decode: {error:?}"))?;
        let mut once = Vec::new();
        DispatchLog::encode(&record, &mut once);
        if once.as_slice() == bytes {
            return Err(
                "minimized Cancelled terminal was already canonical; replace corpus/cancelled-root"
                    .to_string(),
            );
        }
        let again = DispatchLog::decode(&once)
            .map_err(|error| format!("canonical terminal did not decode: {error:?}"))?;
        if again != record {
            return Err("dispatch encode changed the minimized terminal record".to_string());
        }
        let mut twice = Vec::new();
        DispatchLog::encode(&again, &mut twice);
        if twice != once {
            return Err(
                "dispatch canonical encode of the minimized terminal was not stable".to_string(),
            );
        }
        Ok(())
    }
}
