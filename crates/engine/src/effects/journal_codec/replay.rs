//! Semantic replay and snapshot materialization.

use super::super::spec::EffectSpec;
use super::super::state::{AmbiguousReason, EffectError, EffectFence, EffectState, fence};
use super::record::EffectJournalRecordRef;
use super::recovered::{RecoveredEffect, RecoveredIndex};
use super::snapshot::{
    EffectSnapshotLimits, RawEffectSnapshot, RawSnapshotEntry, RawSnapshotState,
    SNAPSHOT_MAX_BYTES, StoredCheckpoint, snapshot_entry_bytes, snapshot_header_bytes,
    validate_effect_snapshot_limits,
};
use super::{EffectCodec, EffectLog};
use crate::journal::{JournalError, JournalReceipt};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn snapshot_from_recovered<E, C, I>(
    recovered: I,
    spec: &E,
    codec: &C,
    base: Option<StoredCheckpoint>,
    persisted_last_ordinal: u64,
    limits: EffectSnapshotLimits,
) -> Result<RawEffectSnapshot, EffectError>
where
    E: EffectSpec,
    C: EffectCodec<E>,
    I: IntoIterator<Item = RecoveredEffect<E::Intent, E::Receipt>>,
{
    validate_effect_snapshot_limits(limits)?;
    let mut entries = BTreeMap::new();
    let mut ordinals = BTreeSet::new();
    let mut last_ordinal = persisted_last_ordinal;
    let mut encoded_bytes = snapshot_header_bytes(base);
    if encoded_bytes > limits.max_bytes {
        return Err(EffectError::Journal(JournalError::Bounds));
    }
    for recovered in recovered {
        if entries.len() >= limits.max_entries {
            return Err(EffectError::Journal(JournalError::Bounds));
        }
        let key = spec.key(&recovered.intent);
        let key_bytes = key.to_bytes();
        let intent = codec.encode_intent(&recovered.intent)?;
        let state = match recovered.state {
            EffectState::Prepared => RawSnapshotState::Prepared,
            EffectState::Executing {
                fence: effect_fence,
                ordinal,
            } => {
                if ordinal == 0 || fence(key, ordinal) != effect_fence {
                    return Err(EffectError::ConflictingHistory);
                }
                if !ordinals.insert(ordinal) {
                    return Err(EffectError::ConflictingHistory);
                }
                last_ordinal = last_ordinal.max(ordinal);
                RawSnapshotState::Ambiguous {
                    fence: effect_fence,
                    ordinal,
                    reason: AmbiguousReason::CrashAfterCall,
                }
            }
            EffectState::Ambiguous {
                fence: effect_fence,
                ordinal,
                reason,
            } => {
                if ordinal == 0 || fence(key, ordinal) != effect_fence {
                    return Err(EffectError::ConflictingHistory);
                }
                if !ordinals.insert(ordinal) {
                    return Err(EffectError::ConflictingHistory);
                }
                last_ordinal = last_ordinal.max(ordinal);
                RawSnapshotState::Ambiguous {
                    fence: effect_fence,
                    ordinal,
                    reason,
                }
            }
            EffectState::Confirmed { receipt } => {
                let request = spec.request(&recovered.intent);
                spec.validate_receipt(key, &request, &receipt)?;
                RawSnapshotState::Confirmed {
                    receipt: codec.encode_receipt(&receipt)?,
                }
            }
        };
        let entry_bytes =
            snapshot_entry_bytes(intent.len(), &state).map_err(EffectError::Journal)?;
        encoded_bytes = encoded_bytes
            .checked_add(entry_bytes)
            .ok_or(EffectError::Journal(JournalError::Bounds))?;
        if encoded_bytes > limits.max_bytes || encoded_bytes > SNAPSHOT_MAX_BYTES {
            return Err(EffectError::Journal(JournalError::Bounds));
        }
        if entries
            .insert(
                key_bytes,
                RawSnapshotEntry {
                    key: key_bytes,
                    intent,
                    state,
                },
            )
            .is_some()
        {
            return Err(EffectError::ConflictingHistory);
        }
    }
    let snapshot = RawEffectSnapshot {
        base,
        last_ordinal,
        root: [0; 32],
        entries: entries.into_values().collect(),
    };
    if snapshot.base.is_none() && !snapshot.entries.is_empty() {
        // A nonempty state must be linked to a durable effect frame. Without
        // that link a caller could select arbitrary stale entries while the
        // journal is empty or was replaced by another generation.
        return Err(EffectError::ConflictingHistory);
    }
    Ok(snapshot)
}

pub(super) fn uncertain_receipt_error(receipt: JournalReceipt<EffectLog>) -> EffectError {
    EffectError::Journal(JournalError::AppendUncertain {
        offset: receipt.offset,
        sequence: receipt.sequence,
        chain: *receipt.chain.as_bytes(),
        record: *receipt.record.as_bytes(),
    })
}

pub(super) fn replay_record<E, C>(
    record: EffectJournalRecordRef<'_>,
    spec: &E,
    codec: &C,
    entries: &mut RecoveredIndex<E>,
    last_ordinal: &mut Option<u64>,
) -> Result<(), EffectError>
where
    E: EffectSpec,
    C: EffectCodec<E>,
{
    match record {
        EffectJournalRecordRef::Prepared { key, intent } => {
            replay_prepared(key, intent, spec, codec, entries)
        }
        EffectJournalRecordRef::Executing {
            key,
            fence,
            ordinal,
        } => replay_executing::<E>(key, fence, ordinal, entries, last_ordinal),
        EffectJournalRecordRef::Ambiguous {
            key,
            fence,
            ordinal,
            reason,
        } => replay_ambiguous::<E>(key, fence, ordinal, reason, entries),
        EffectJournalRecordRef::Confirmed { key, receipt } => {
            replay_confirmed(key, receipt, spec, codec, entries)
        }
        EffectJournalRecordRef::Compacted { .. } => Err(EffectError::ConflictingHistory),
    }
}

fn replay_prepared<E, C>(
    key: [u8; 32],
    intent_bytes: &[u8],
    spec: &E,
    codec: &C,
    entries: &mut RecoveredIndex<E>,
) -> Result<(), EffectError>
where
    E: EffectSpec,
    C: EffectCodec<E>,
{
    let intent = codec.decode_intent(intent_bytes)?;
    if codec.encode_intent(&intent)?.as_slice() != intent_bytes {
        return Err(EffectError::ConflictingHistory);
    }
    let typed_key = spec.key(&intent);
    if typed_key.as_bytes() != &key {
        return Err(EffectError::ConflictingHistory);
    }
    if let Some((existing_key, entry)) = entries.get_mut(&key) {
        if *existing_key != typed_key
            || codec.encode_intent(&entry.intent)?.as_slice() != intent_bytes
        {
            return Err(EffectError::ConflictingHistory);
        }
        if matches!(
            entry.state,
            EffectState::Ambiguous { .. } | EffectState::Executing { .. }
        ) {
            entry.state = EffectState::Prepared;
        }
        return Ok(());
    }
    if entries.contains_key(&key) {
        return Err(EffectError::ConflictingHistory);
    }
    entries.insert(
        key,
        (
            typed_key,
            RecoveredEffect {
                intent,
                state: EffectState::Prepared,
            },
        ),
    );
    Ok(())
}

fn replay_executing<E: EffectSpec>(
    key: [u8; 32],
    persisted_fence: EffectFence,
    ordinal: u64,
    entries: &mut RecoveredIndex<E>,
    last_ordinal: &mut Option<u64>,
) -> Result<(), EffectError> {
    if ordinal == 0 || last_ordinal.is_some_and(|previous| ordinal <= previous) {
        return Err(EffectError::ConflictingHistory);
    }
    let (typed_key, entry) = entries
        .get_mut(&key)
        .ok_or(EffectError::ConflictingHistory)?;
    let typed_key = *typed_key;
    if fence(typed_key, ordinal) != persisted_fence || !matches!(entry.state, EffectState::Prepared)
    {
        return Err(EffectError::ConflictingHistory);
    }
    entry.state = EffectState::Ambiguous {
        fence: persisted_fence,
        ordinal,
        reason: AmbiguousReason::CrashAfterCall,
    };
    *last_ordinal = Some(ordinal);
    Ok(())
}

fn replay_ambiguous<E: EffectSpec>(
    key: [u8; 32],
    persisted_fence: EffectFence,
    ordinal: u64,
    reason: AmbiguousReason,
    entries: &mut RecoveredIndex<E>,
) -> Result<(), EffectError> {
    if ordinal == 0 {
        return Err(EffectError::ConflictingHistory);
    }
    let (typed_key, entry) = entries
        .get_mut(&key)
        .ok_or(EffectError::ConflictingHistory)?;
    let typed_key = *typed_key;
    if fence(typed_key, ordinal) != persisted_fence {
        return Err(EffectError::ConflictingHistory);
    }
    let valid_state = matches!(
        entry.state,
        EffectState::Ambiguous { fence, ordinal: old, .. }
            | EffectState::Executing { fence, ordinal: old }
            if fence == persisted_fence && old == ordinal
    );
    if !valid_state {
        return Err(EffectError::ConflictingHistory);
    }
    entry.state = EffectState::Ambiguous {
        fence: persisted_fence,
        ordinal,
        reason,
    };
    Ok(())
}

fn replay_confirmed<E, C>(
    key: [u8; 32],
    receipt: &[u8],
    spec: &E,
    codec: &C,
    entries: &mut RecoveredIndex<E>,
) -> Result<(), EffectError>
where
    E: EffectSpec,
    C: EffectCodec<E>,
{
    let typed_key = entries
        .get(&key)
        .map(|(typed_key, _)| *typed_key)
        .ok_or(EffectError::ConflictingHistory)?;
    let existing = entries
        .get(&key)
        .map(|(_, entry)| entry.state.clone())
        .ok_or(EffectError::ConflictingHistory)?;
    if let EffectState::Confirmed { receipt: old } = existing {
        return (codec.encode_receipt(&old)?.as_slice() == receipt)
            .then_some(())
            .ok_or(EffectError::ConflictingHistory);
    }
    if !matches!(
        existing,
        EffectState::Ambiguous { .. } | EffectState::Executing { .. }
    ) {
        return Err(EffectError::ConflictingHistory);
    }
    let typed_receipt = codec.decode_receipt(receipt)?;
    if codec.encode_receipt(&typed_receipt)?.as_slice() != receipt {
        return Err(EffectError::ConflictingHistory);
    }
    let request = {
        let entry = entries.get(&key).ok_or(EffectError::ConflictingHistory)?;
        spec.request(&entry.1.intent)
    };
    spec.validate_receipt(typed_key, &request, &typed_receipt)?;
    let entry = entries
        .get_mut(&key)
        .ok_or(EffectError::ConflictingHistory)?;
    entry.1.state = EffectState::Confirmed {
        receipt: typed_receipt,
    };
    Ok(())
}
