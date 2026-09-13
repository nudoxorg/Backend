//! Effect phase records and zero-copy wire decoding.

use super::super::state::{AmbiguousReason, EffectFence};
use super::EffectLog;
use super::snapshot::{EffectSnapshotReceipt, decode_snapshot_receipt, encode_snapshot_receipt};
use crate::journal::{JournalCodec, JournalError};

/// Raw effect phase record carried inside the typed journal domain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EffectJournalRecord {
    /// Durable intent and its claimed key.
    Prepared {
        /// Canonical effect key claimed by the intent.
        key: [u8; 32],
        /// Canonical encoded intent bytes.
        intent: Vec<u8>,
    },
    /// Durable pre-call fence and ordinal.
    Executing {
        /// Effect key owning the call.
        key: [u8; 32],
        /// Full-width pre-call fence.
        fence: EffectFence,
        /// Monotonic execution ordinal.
        ordinal: u64,
    },
    /// Durable ambiguous outcome.
    Ambiguous {
        /// Effect key owning the uncertain call.
        key: [u8; 32],
        /// Full-width fence of the uncertain call.
        fence: EffectFence,
        /// Monotonic execution ordinal.
        ordinal: u64,
        /// Recovery reason recorded by the owner.
        reason: AmbiguousReason,
    },
    /// Durable checked receipt.
    Confirmed {
        /// Effect key bound to the receipt.
        key: [u8; 32],
        /// Canonical encoded sink receipt.
        receipt: Vec<u8>,
    },
    /// Authenticated generation marker at the head of a compacted journal.
    ///
    /// The marker binds the replacement journal to the selected snapshot. It
    /// lets recovery distinguish an atomic prefix replacement from a file
    /// that was truncated before the snapshot's checkpoint.
    Compacted {
        /// Exact fixed-width receipt of the selected effect snapshot.
        snapshot: EffectSnapshotReceipt,
    },
}

impl JournalCodec for EffectLog {
    type Record = EffectJournalRecord;

    fn encode(record: &Self::Record, output: &mut Vec<u8>) {
        match record {
            EffectJournalRecord::Prepared { key, intent } => {
                output.push(1);
                output.extend_from_slice(key);
                put_bytes(output, intent);
            }
            EffectJournalRecord::Executing {
                key,
                fence,
                ordinal,
            } => {
                output.push(2);
                output.extend_from_slice(key);
                output.extend_from_slice(&fence.as_bytes());
                output.extend_from_slice(&ordinal.to_be_bytes());
            }
            EffectJournalRecord::Ambiguous {
                key,
                fence,
                ordinal,
                reason,
            } => {
                output.push(3);
                output.extend_from_slice(key);
                output.extend_from_slice(&fence.as_bytes());
                output.extend_from_slice(&ordinal.to_be_bytes());
                output.push(reason_tag(*reason));
            }
            EffectJournalRecord::Confirmed { key, receipt } => {
                output.push(4);
                output.extend_from_slice(key);
                put_bytes(output, receipt);
            }
            EffectJournalRecord::Compacted { snapshot } => {
                output.push(5);
                output.extend_from_slice(&[0; 32]);
                encode_snapshot_receipt(*snapshot, output);
            }
        }
    }

    fn decode(bytes: &[u8]) -> Result<Self::Record, JournalError> {
        match decode_ref(bytes)? {
            EffectJournalRecordRef::Prepared { key, intent } => Ok(EffectJournalRecord::Prepared {
                key,
                intent: intent.to_vec(),
            }),
            EffectJournalRecordRef::Executing {
                key,
                fence,
                ordinal,
            } => Ok(EffectJournalRecord::Executing {
                key,
                fence,
                ordinal,
            }),
            EffectJournalRecordRef::Ambiguous {
                key,
                fence,
                ordinal,
                reason,
            } => Ok(EffectJournalRecord::Ambiguous {
                key,
                fence,
                ordinal,
                reason,
            }),
            EffectJournalRecordRef::Confirmed { key, receipt } => {
                Ok(EffectJournalRecord::Confirmed {
                    key,
                    receipt: receipt.to_vec(),
                })
            }
            EffectJournalRecordRef::Compacted { snapshot } => {
                Ok(EffectJournalRecord::Compacted { snapshot })
            }
        }
    }

    fn validate(bytes: &[u8]) -> Result<(), JournalError> {
        // The effect record grammar is self-delimiting.  Structural parsing is
        // enough to establish canonical bytes here and avoids constructing a
        // temporary Vec for every frame before replay decodes the typed field.
        decode_ref(bytes).map(|_| ())
    }
}

/// Borrowed effect record used by the streaming replay fold.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum EffectJournalRecordRef<'a> {
    Prepared {
        key: [u8; 32],
        intent: &'a [u8],
    },
    Executing {
        key: [u8; 32],
        fence: EffectFence,
        ordinal: u64,
    },
    Ambiguous {
        key: [u8; 32],
        fence: EffectFence,
        ordinal: u64,
        reason: AmbiguousReason,
    },
    Confirmed {
        key: [u8; 32],
        receipt: &'a [u8],
    },
    Compacted {
        snapshot: EffectSnapshotReceipt,
    },
}

pub(super) fn decode_ref(bytes: &[u8]) -> Result<EffectJournalRecordRef<'_>, JournalError> {
    let tag = *bytes.first().ok_or(JournalError::Corrupt("effect tag"))?;
    let mut at = 1usize;
    let key = read_fixed::<32>(bytes, &mut at)?;
    let record = match tag {
        1 => EffectJournalRecordRef::Prepared {
            key,
            intent: read_bytes_ref(bytes, &mut at)?,
        },
        2 => EffectJournalRecordRef::Executing {
            key,
            fence: EffectFence(read_fixed::<32>(bytes, &mut at)?),
            ordinal: u64::from_be_bytes(read_fixed::<8>(bytes, &mut at)?),
        },
        3 => {
            let fence = EffectFence(read_fixed::<32>(bytes, &mut at)?);
            let ordinal = u64::from_be_bytes(read_fixed::<8>(bytes, &mut at)?);
            let reason = reason_from_tag(
                *bytes
                    .get(at)
                    .ok_or(JournalError::Corrupt("effect reason"))?,
            )?;
            at += 1;
            EffectJournalRecordRef::Ambiguous {
                key,
                fence,
                ordinal,
                reason,
            }
        }
        4 => EffectJournalRecordRef::Confirmed {
            key,
            receipt: read_bytes_ref(bytes, &mut at)?,
        },
        5 => {
            // A compacted journal marker has no logical effect state. It is
            // admitted only by the generation check in replay, but remains a
            // normal canonical frame for physical journal validation.
            if key != [0; 32] {
                return Err(JournalError::Corrupt("compaction marker key"));
            }
            EffectJournalRecordRef::Compacted {
                snapshot: decode_snapshot_receipt(bytes, &mut at)?,
            }
        }
        _ => return Err(JournalError::Corrupt("effect tag")),
    };
    if at != bytes.len() {
        return Err(JournalError::Corrupt("effect trailing bytes"));
    }
    Ok(record)
}

pub(super) fn reason_tag(reason: AmbiguousReason) -> u8 {
    match reason {
        AmbiguousReason::Timeout => 1,
        AmbiguousReason::CrashAfterCall => 2,
        AmbiguousReason::UnknownOutcome => 3,
        AmbiguousReason::InvalidReceipt => 4,
    }
}
pub(super) fn reason_from_tag(tag: u8) -> Result<AmbiguousReason, JournalError> {
    match tag {
        1 => Ok(AmbiguousReason::Timeout),
        2 => Ok(AmbiguousReason::CrashAfterCall),
        3 => Ok(AmbiguousReason::UnknownOutcome),
        4 => Ok(AmbiguousReason::InvalidReceipt),
        _ => Err(JournalError::Corrupt("effect reason")),
    }
}
pub(super) fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    out.extend_from_slice(bytes);
}
pub(super) fn read_fixed<const N: usize>(
    bytes: &[u8],
    at: &mut usize,
) -> Result<[u8; N], JournalError> {
    let end = at.checked_add(N).ok_or(JournalError::Bounds)?;
    let value = bytes
        .get(*at..end)
        .ok_or(JournalError::Corrupt("effect fixed field"))?;
    *at = end;
    value
        .try_into()
        .map_err(|_| JournalError::Corrupt("effect fixed field"))
}
pub(super) fn read_bytes_ref<'a>(
    bytes: &'a [u8],
    at: &mut usize,
) -> Result<&'a [u8], JournalError> {
    let len = usize::try_from(u64::from_be_bytes(read_fixed::<8>(bytes, at)?))
        .map_err(|_| JournalError::Bounds)?;
    let end = at.checked_add(len).ok_or(JournalError::Bounds)?;
    let value = bytes
        .get(*at..end)
        .ok_or(JournalError::Corrupt("effect bytes"))?;
    *at = end;
    Ok(value)
}
