//! Snapshot selection and authenticated journal admission.

use super::super::spec::EffectSpec;
use super::super::state::{EffectError, EffectState, fence};
use super::pointer::read_snapshot_pointer;
use super::record::{EffectJournalRecordRef, decode_ref};
use super::recovered::{RecoveredEffect, RecoveredIndex};
use super::snapshot::{
    EffectSnapshotLimits, EffectSnapshotLog, EffectSnapshotReceipt, RawEffectSnapshot,
    RawSnapshotState, StoredCheckpoint, decode_snapshot, snapshot_entry_count,
    validate_effect_snapshot_limits,
};
use super::{EffectCodec, EffectLog};
use crate::journal::{
    ChainHash, HashChainJournal, JournalCheckpoint, JournalError, JournalFrameRef, JournalLimits,
    read_frame_at_path,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

type OpenedEffectJournal = (
    HashChainJournal<EffectLog>,
    crate::journal::JournalRecovery<EffectLog>,
    Option<StoredCheckpoint>,
);

pub(super) fn selected_snapshot(
    snapshot_path: &Path,
    pointer_path: &Path,
    effect_path: &Path,
    limits: EffectSnapshotLimits,
) -> Result<Option<(EffectSnapshotReceipt, RawEffectSnapshot)>, EffectError> {
    validate_effect_snapshot_limits(limits)?;
    let receipt = if let Some(receipt) = read_snapshot_pointer(pointer_path)? {
        receipt
    } else {
        // Once a replacement journal has a complete generation marker,
        // the marker is a fixed, authenticated pointer of last resort if
        // the sidecar pointer was lost. This avoids a full historical
        // replay while keeping a corrupt/forged pointer fail closed.
        let Some(receipt) = compaction_marker_receipt(effect_path).map_err(EffectError::Journal)?
        else {
            return Ok(None);
        };
        receipt
    };
    let frame = read_frame_at_path::<EffectSnapshotLog>(snapshot_path, receipt.offset)
        .map_err(EffectError::Journal)?;
    if frame.start_offset() != receipt.offset
        || frame.sequence != receipt.sequence
        || frame.chain.as_bytes() != &receipt.chain
        || frame.record.as_bytes() != &receipt.record
    {
        return Err(EffectError::Journal(JournalError::Corrupt(
            "snapshot receipt",
        )));
    }
    let entry_count = snapshot_entry_count(&frame.payload).map_err(EffectError::Journal)?;
    if entry_count > limits.max_entries || frame.payload.len() > limits.max_bytes {
        return Err(EffectError::Journal(JournalError::Bounds));
    }
    let snapshot = decode_snapshot(&frame.payload).map_err(EffectError::Journal)?;
    Ok(Some((receipt, snapshot)))
}

pub(super) fn tail_from_recovery(
    recovery: &crate::journal::JournalRecovery<EffectLog>,
) -> Option<JournalCheckpoint<EffectLog>> {
    recovery.frames.last().map(checkpoint_from_frame)
}

pub(super) fn open_effect_journal(
    path: &Path,
    base: Option<StoredCheckpoint>,
    selected_snapshot: Option<EffectSnapshotReceipt>,
    limits: JournalLimits,
) -> Result<OpenedEffectJournal, EffectError> {
    let use_checkpoint = base
        .is_some_and(|base| fs::metadata(path).is_ok_and(|metadata| metadata.len() > base.offset));
    if let Some(base) = base {
        if use_checkpoint {
            match HashChainJournal::<EffectLog>::open_from_checkpoint(path, base.journal(), limits)
            {
                Ok((journal, recovery)) => return Ok((journal, recovery, Some(base))),
                Err(checkpoint_error) => {
                    // A selected snapshot can survive an atomic journal-prefix
                    // replacement. Only the authenticated generation marker
                    // permits falling back to a full scan; a random checkpoint
                    // mismatch must fail closed without replaying all history.
                    let compacted = selected_snapshot.is_some_and(|snapshot| {
                        matches!(compaction_marker_checkpoint(path, snapshot), Ok(Some(_)))
                    });
                    if compacted
                        && let Ok((journal, recovery)) =
                            HashChainJournal::<EffectLog>::open_with_limits(path, limits)
                    {
                        return Ok((journal, recovery, None));
                    }
                    return Err(EffectError::Journal(checkpoint_error));
                }
            }
        }
        // A missing checkpoint is valid only for an authenticated prefix
        // replacement. Open the physical file so a torn replacement can be
        // repaired at its verified boundary; replay will fail closed unless
        // the replacement marker authenticates this snapshot.
        let compacted = selected_snapshot.is_some_and(|snapshot| {
            matches!(compaction_marker_checkpoint(path, snapshot), Ok(Some(_)))
        });
        if !compacted {
            let (journal, recovery) = HashChainJournal::<EffectLog>::open_with_limits(path, limits)
                .map_err(EffectError::Journal)?;
            return Ok((journal, recovery, None));
        }
    }
    let (journal, recovery) = HashChainJournal::<EffectLog>::open_with_limits(path, limits)
        .map_err(EffectError::Journal)?;
    Ok((journal, recovery, None))
}

pub(super) fn restore_snapshot<E, C>(
    snapshot: &RawEffectSnapshot,
    spec: &E,
    codec: &C,
    limits: EffectSnapshotLimits,
) -> Result<(RecoveredIndex<E>, Option<u64>), EffectError>
where
    E: EffectSpec,
    C: EffectCodec<E>,
{
    validate_effect_snapshot_limits(limits)?;
    if snapshot.entries.len() > limits.max_entries {
        return Err(EffectError::Journal(JournalError::Bounds));
    }
    if snapshot.base.is_none() && !snapshot.entries.is_empty() {
        return Err(EffectError::ConflictingHistory);
    }
    let mut entries = BTreeMap::new();
    let mut ordinals = BTreeSet::new();
    for raw in &snapshot.entries {
        let intent = codec.decode_intent(&raw.intent)?;
        let canonical = codec.encode_intent(&intent)?;
        if canonical.as_slice() != raw.intent.as_slice() {
            return Err(EffectError::ConflictingHistory);
        }
        let key = spec.key(&intent);
        if key.as_bytes() != raw.key.as_ref() {
            return Err(EffectError::ConflictingHistory);
        }
        let state = match &raw.state {
            RawSnapshotState::Prepared => EffectState::Prepared,
            RawSnapshotState::Ambiguous {
                fence: effect_fence,
                ordinal,
                reason,
            } => {
                if *ordinal == 0
                    || *ordinal > snapshot.last_ordinal
                    || fence(key, *ordinal) != *effect_fence
                    || !ordinals.insert(*ordinal)
                {
                    return Err(EffectError::ConflictingHistory);
                }
                EffectState::Ambiguous {
                    fence: *effect_fence,
                    ordinal: *ordinal,
                    reason: *reason,
                }
            }
            RawSnapshotState::Confirmed { receipt } => {
                let typed_receipt = codec.decode_receipt(receipt)?;
                let canonical = codec.encode_receipt(&typed_receipt)?;
                if canonical.as_slice() != receipt.as_slice() {
                    return Err(EffectError::ConflictingHistory);
                }
                let request = spec.request(&intent);
                spec.validate_receipt(key, &request, &typed_receipt)?;
                EffectState::Confirmed {
                    receipt: typed_receipt,
                }
            }
        };
        if entries
            .insert(raw.key, (key, RecoveredEffect { intent, state }))
            .is_some()
        {
            return Err(EffectError::ConflictingHistory);
        }
    }
    Ok((entries, Some(snapshot.last_ordinal)))
}

pub(super) fn checkpoint_frame_is_present(
    path: &Path,
    checkpoint: StoredCheckpoint,
) -> Result<bool, JournalError> {
    let length = fs::metadata(path)?.len();
    if length <= checkpoint.offset {
        return Ok(false);
    }
    let frame = read_frame_at_path::<EffectLog>(path, checkpoint.offset)?;
    Ok(frame.start_offset() == checkpoint.offset
        && frame.sequence == checkpoint.sequence
        && frame.chain.as_bytes() == &checkpoint.chain
        && frame.record.as_bytes() == &checkpoint.record)
}

pub(super) fn compaction_marker_receipt(
    path: &Path,
) -> Result<Option<EffectSnapshotReceipt>, JournalError> {
    let length = match fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(JournalError::Io(error)),
    };
    if length == 0 {
        return Ok(None);
    }
    let frame = match read_frame_at_path::<EffectLog>(path, 0) {
        Ok(frame) => frame,
        Err(JournalError::Corrupt("frame header" | "frame payload")) => return Ok(None),
        Err(error) => return Err(error),
    };
    if frame.start_offset() != 0 || frame.sequence != 0 || frame.previous != ChainHash::genesis() {
        return Ok(None);
    }
    match decode_ref(&frame.payload)? {
        EffectJournalRecordRef::Compacted { snapshot } => Ok(Some(snapshot)),
        _ => Ok(None),
    }
}

/// Returns the authenticated head frame of a compacted replacement when it
/// binds the currently selected snapshot. An empty or ordinary effect journal
/// is deliberately not treated as compacted: accepting a missing checkpoint
/// without this marker would let a truncated file silently resurrect stale
/// snapshot state.
pub(super) fn compaction_marker_checkpoint(
    path: &Path,
    snapshot: EffectSnapshotReceipt,
) -> Result<Option<JournalCheckpoint<EffectLog>>, JournalError> {
    let Some(marker) = compaction_marker_receipt(path)? else {
        return Ok(None);
    };
    if marker != snapshot {
        return Err(JournalError::Corrupt("compaction marker"));
    }
    let frame = read_frame_at_path::<EffectLog>(path, 0)?;
    Ok(Some(checkpoint_from_frame(&frame)))
}

pub(super) fn max_ordinal_in_recovery(
    recovery: &crate::journal::JournalRecovery<EffectLog>,
) -> Result<u64, EffectError> {
    let mut max_ordinal = 0;
    for frame in &recovery.frames {
        match decode_ref(&frame.payload).map_err(EffectError::Journal)? {
            EffectJournalRecordRef::Executing { ordinal, .. }
            | EffectJournalRecordRef::Ambiguous { ordinal, .. } => {
                max_ordinal = max_ordinal.max(ordinal);
            }
            EffectJournalRecordRef::Prepared { .. }
            | EffectJournalRecordRef::Confirmed { .. }
            | EffectJournalRecordRef::Compacted { .. } => {}
        }
    }
    Ok(max_ordinal)
}

pub(super) fn checkpoint_from_frame(
    frame: &crate::journal::JournalFrame<EffectLog>,
) -> JournalCheckpoint<EffectLog> {
    JournalCheckpoint {
        offset: frame.start_offset(),
        sequence: frame.sequence,
        chain: frame.chain,
        record: frame.record,
    }
}

pub(super) fn checkpoint_from_frame_ref(
    frame: &JournalFrameRef<'_, EffectLog>,
) -> JournalCheckpoint<EffectLog> {
    JournalCheckpoint {
        offset: frame.offset,
        sequence: frame.sequence,
        chain: frame.chain,
        record: frame.record,
    }
}
