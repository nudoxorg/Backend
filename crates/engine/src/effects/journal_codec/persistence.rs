//! Effect persistence lifecycle, replay driver, and uncertainty settlement.

use super::super::spec::{EffectKey, EffectSpec};
use super::super::state::{AmbiguousReason, EffectError, EffectFence, EffectPersistence, fence};
use super::admission::{
    checkpoint_frame_is_present, checkpoint_from_frame_ref, compaction_marker_checkpoint,
    max_ordinal_in_recovery, open_effect_journal, restore_snapshot, selected_snapshot,
    tail_from_recovery,
};
use super::pointer::snapshot_paths;
use super::record::{EffectJournalRecord, EffectJournalRecordRef, decode_ref};
use super::recovered::RecoveredEffects;
use super::replay::{replay_record, uncertain_receipt_error};
use super::snapshot::{
    EffectSnapshotLimits, EffectSnapshotReceipt, RawEffectSnapshot, StoredCheckpoint,
};
use super::{EffectCodec, EffectLog};
use crate::fault::{Boundary, Faults};
use crate::journal::{
    ChainHash, HashChainJournal, JournalCheckpoint, JournalError, JournalFrameRef, JournalLimits,
    JournalReceipt, JournalScan,
};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::marker::PhantomData;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Filesystem/hash-chain persistence for one effect specification.
pub struct JournalEffectPersistence<E: EffectSpec, C: EffectCodec<E>> {
    pub(super) journal: HashChainJournal<EffectLog>,
    pub(super) codec: C,
    pub(super) faults: Arc<Faults>,
    pub(super) max_ordinal: AtomicU64,
    pub(super) tail: Mutex<Option<JournalCheckpoint<EffectLog>>>,
    uncertain: Mutex<Option<PendingAppend>>,
    pub(super) snapshot_path: std::path::PathBuf,
    pub(super) pointer_path: std::path::PathBuf,
    _effect: PhantomData<fn() -> E>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingAppend {
    receipt: JournalReceipt<EffectLog>,
    record: EffectJournalRecord,
}

type StreamingEffectJournal = (
    HashChainJournal<EffectLog>,
    JournalScan<EffectLog>,
    u64,
    Option<JournalCheckpoint<EffectLog>>,
    Option<StoredCheckpoint>,
);
impl<E: EffectSpec, C: EffectCodec<E>> fmt::Debug for JournalEffectPersistence<E, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JournalEffectPersistence")
            .field("path", &self.journal.path())
            .finish_non_exhaustive()
    }
}

impl<E: EffectSpec, C: EffectCodec<E>> JournalEffectPersistence<E, C> {
    /// Opens a journal and repairs only a torn trailing frame.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn open(
        path: impl AsRef<Path>,
        codec: C,
    ) -> Result<(Self, crate::journal::JournalRecovery<EffectLog>), EffectError> {
        let path = path.as_ref().to_owned();
        let (snapshot_path, pointer_path) = snapshot_paths(&path);
        let selected = selected_snapshot(
            &snapshot_path,
            &pointer_path,
            &path,
            EffectSnapshotLimits::default(),
        )?;
        if selected.is_some() && !path.exists() {
            return Err(EffectError::Persistence(
                "effect journal missing beside selected snapshot".into(),
            ));
        }
        let base = selected.as_ref().and_then(|(_, snapshot)| snapshot.base);
        let (journal, recovery, used_base) = open_effect_journal(
            &path,
            base,
            selected.as_ref().map(|(receipt, _)| *receipt),
            JournalLimits::default(),
        )?;
        let mut max_ordinal = max_ordinal_in_recovery(&recovery)?;
        if let Some((_, snapshot)) = selected.as_ref() {
            max_ordinal = max_ordinal.max(snapshot.last_ordinal);
        }
        let tail =
            tail_from_recovery(&recovery).or_else(|| used_base.map(StoredCheckpoint::journal));
        Ok((
            Self {
                journal,
                codec,
                faults: Arc::new(Faults::default()),
                max_ordinal: AtomicU64::new(max_ordinal),
                tail: Mutex::new(tail),
                uncertain: Mutex::new(None),
                snapshot_path,
                pointer_path,
                _effect: PhantomData,
            },
            recovery,
        ))
    }

    /// Opens a journal while retaining only the authenticated scan result.
    ///
    /// Use this constructor when the caller does not need the compatibility
    /// frame vector returned by [`Self::open`].  Effect replay itself is
    /// streaming regardless of which constructor was used.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn open_streaming(
        path: impl AsRef<Path>,
        codec: C,
        limits: JournalLimits,
    ) -> Result<(Self, JournalScan<EffectLog>), EffectError> {
        let path = path.as_ref().to_owned();
        let (snapshot_path, pointer_path) = snapshot_paths(&path);
        let selected = selected_snapshot(
            &snapshot_path,
            &pointer_path,
            &path,
            EffectSnapshotLimits::default(),
        )?;
        if selected.is_some() && !path.exists() {
            return Err(EffectError::Persistence(
                "effect journal missing beside selected snapshot".into(),
            ));
        }
        let base = selected.as_ref().and_then(|(_, snapshot)| snapshot.base);
        let (journal, scan, mut max_ordinal, mut tail, used_base) =
            open_streaming_effect_journal(&path, selected.as_ref(), base, limits)?;
        if let Some((_, snapshot)) = selected.as_ref() {
            max_ordinal = max_ordinal.max(snapshot.last_ordinal);
        }
        if tail.is_none() {
            tail = used_base.map(StoredCheckpoint::journal);
        }
        Ok((
            Self {
                journal,
                codec,
                faults: Arc::new(Faults::default()),
                max_ordinal: AtomicU64::new(max_ordinal),
                tail: Mutex::new(tail),
                uncertain: Mutex::new(None),
                snapshot_path,
                pointer_path,
                _effect: PhantomData,
            },
            scan,
        ))
    }

    /// Installs production fault seams for effect crash-boundary tests.
    #[must_use]
    pub fn with_faults(mut self, faults: Arc<Faults>) -> Self {
        self.faults = faults;
        self
    }

    /// Replays and validates every effect. The fixed-width key index makes
    /// phase lookup O(log n) per frame instead of scanning all live entries.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn replay(&self, spec: &E) -> Result<RecoveredEffects<E>, EffectError> {
        self.replay_with_limits(spec, JournalLimits::default())
    }

    /// Replays with an explicit bounded history envelope. The generic journal
    /// validates frames before handing them over; this fold consumes frames
    /// as it advances, retaining only the active typed entries and the fixed
    /// width key index. A selected typed snapshot replaces the verified
    /// prefix, so warm recovery retains only the snapshot entries and tail.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn replay_with_limits(
        &self,
        spec: &E,
        limits: JournalLimits,
    ) -> Result<RecoveredEffects<E>, EffectError> {
        let selected = selected_snapshot(
            &self.snapshot_path,
            &self.pointer_path,
            self.journal.path(),
            EffectSnapshotLimits::default(),
        )?;
        let selected_receipt = selected.as_ref().map(|(receipt, _)| *receipt);
        let mut entries;
        let mut last_ordinal;
        let base = if let Some((_, snapshot)) = selected.as_ref() {
            (entries, last_ordinal) =
                restore_snapshot(snapshot, spec, &self.codec, EffectSnapshotLimits::default())?;
            snapshot.base
        } else {
            entries = BTreeMap::new();
            last_ordinal = None;
            None
        };
        let mut typed_failure = None;
        let mut apply = |frame: JournalFrameRef<'_, EffectLog>| {
            let record = decode_ref(frame.payload)?;
            let result = match record {
                EffectJournalRecordRef::Compacted { snapshot } => {
                    if frame.offset != 0
                        || frame.sequence != 0
                        || frame.previous != ChainHash::genesis()
                        || selected_receipt != Some(snapshot)
                    {
                        Err(EffectError::ConflictingHistory)
                    } else {
                        Ok(())
                    }
                }
                record => replay_record(record, spec, &self.codec, &mut entries, &mut last_ordinal),
            };
            match result {
                Ok(()) => Ok(()),
                Err(error) => {
                    typed_failure = Some(error);
                    // Stop the physical scan immediately after the first
                    // semantic conflict.  The original error is returned
                    // below, while this sentinel only crosses the journal
                    // visitor boundary.
                    Err(JournalError::Corrupt("effect replay"))
                }
            }
        };
        let scan_checkpoint = if let Some(base) = base {
            if let Ok(true) = checkpoint_frame_is_present(self.journal.path(), base) {
                Some(base.journal())
            } else {
                let Some(snapshot_receipt) = selected_receipt else {
                    return Err(EffectError::Journal(JournalError::Corrupt(
                        "checkpoint receipt",
                    )));
                };
                compaction_marker_checkpoint(self.journal.path(), snapshot_receipt)
                    .map_err(EffectError::Journal)?
                    .ok_or(EffectError::Journal(JournalError::Corrupt(
                        "checkpoint receipt",
                    )))
                    .map(Some)?
            }
        } else {
            None
        };
        let scan = if let Some(checkpoint) = scan_checkpoint {
            self.journal
                .scan_from_checkpoint(checkpoint, limits, &mut apply)
        } else {
            self.journal.scan_stream(limits, &mut apply)
        };
        if let Some(error) = typed_failure {
            return Err(error);
        }
        scan.map_err(EffectError::Journal)?;
        self.max_ordinal
            .store(last_ordinal.unwrap_or(0), Ordering::Release);
        Ok(entries.into_values().map(|(_, entry)| entry).collect())
    }

    pub(super) fn settle_uncertain(
        &self,
        record: &EffectJournalRecord,
    ) -> Result<Option<JournalReceipt<EffectLog>>, EffectError> {
        let mut pending = match self.uncertain.lock() {
            Ok(pending) => pending,
            Err(poisoned) => {
                let receipt = poisoned.get_ref().as_ref().map(|pending| pending.receipt);
                self.journal.mark_unusable();
                return match receipt {
                    Some(receipt) => Err(uncertain_receipt_error(receipt)),
                    None => Err(EffectError::Persistence(
                        "poisoned effect uncertainty".into(),
                    )),
                };
            }
        };
        let Some(candidate) = pending.as_ref() else {
            return Ok(None);
        };
        let receipt = self
            .journal
            .retry(candidate.receipt, &candidate.record)
            .map_err(EffectError::Journal)?;
        let checkpoint = receipt.checkpoint();
        if self.set_tail(checkpoint).is_err() {
            // The retry may already have adopted a complete durable frame;
            // preserve its fixed identity for a caller that must reopen and
            // reconcile after the cache poison.
            return Err(uncertain_receipt_error(receipt));
        }
        let same_record = candidate.record == *record;
        *pending = None;
        Ok(same_record.then_some(receipt))
    }

    pub(super) fn append(
        &self,
        record: &EffectJournalRecord,
    ) -> Result<JournalReceipt<EffectLog>, EffectError> {
        if let Some(receipt) = self.settle_uncertain(record)? {
            return Ok(receipt);
        }
        let receipt = match self.journal.append(record) {
            Ok(receipt) => receipt,
            Err(error) => {
                if let Some(receipt) = error.uncertain_receipt() {
                    let Ok(mut pending) = self.uncertain.lock() else {
                        self.journal.mark_unusable();
                        return Err(EffectError::Journal(error));
                    };
                    *pending = Some(PendingAppend {
                        receipt,
                        record: record.clone(),
                    });
                }
                return Err(EffectError::Journal(error));
            }
        };
        let checkpoint = receipt.checkpoint();
        if self.set_tail(checkpoint).is_err() {
            self.journal.mark_unusable();
            return Err(uncertain_receipt_error(receipt));
        }
        Ok(receipt)
    }
}

fn open_streaming_effect_journal(
    path: &Path,
    selected: Option<&(EffectSnapshotReceipt, RawEffectSnapshot)>,
    base: Option<StoredCheckpoint>,
    limits: JournalLimits,
) -> Result<StreamingEffectJournal, EffectError> {
    let mut max_ordinal = 0;
    let mut tail = None;
    let visit = |frame: JournalFrameRef<'_, EffectLog>| {
        update_streaming_frame(&frame, &mut tail, &mut max_ordinal)
    };
    if let Some(base) =
        base.filter(|base| fs::metadata(path).is_ok_and(|metadata| metadata.len() > base.offset))
    {
        match HashChainJournal::<EffectLog>::open_from_checkpoint_streaming_with(
            path,
            base.journal(),
            limits,
            visit,
        ) {
            Ok((journal, scan)) => {
                return Ok((journal, scan, max_ordinal, tail, Some(base)));
            }
            Err(checkpoint_error) => {
                let compacted = selected.is_some_and(|(receipt, _)| {
                    matches!(compaction_marker_checkpoint(path, *receipt), Ok(Some(_)))
                });
                if !compacted {
                    return Err(EffectError::Journal(checkpoint_error));
                }
                max_ordinal = 0;
                tail = None;
            }
        }
    }
    let (journal, scan) =
        HashChainJournal::<EffectLog>::open_streaming_with(path, limits, |frame| {
            update_streaming_frame(&frame, &mut tail, &mut max_ordinal)
        })
        .map_err(EffectError::Journal)?;
    Ok((journal, scan, max_ordinal, tail, None))
}

fn update_streaming_frame(
    frame: &JournalFrameRef<'_, EffectLog>,
    tail: &mut Option<JournalCheckpoint<EffectLog>>,
    max_ordinal: &mut u64,
) -> Result<(), JournalError> {
    *tail = Some(checkpoint_from_frame_ref(frame));
    match decode_ref(frame.payload)? {
        EffectJournalRecordRef::Executing { ordinal, .. }
        | EffectJournalRecordRef::Ambiguous { ordinal, .. } => {
            *max_ordinal = (*max_ordinal).max(ordinal);
        }
        EffectJournalRecordRef::Prepared { .. }
        | EffectJournalRecordRef::Confirmed { .. }
        | EffectJournalRecordRef::Compacted { .. } => {}
    }
    Ok(())
}

impl<E: EffectSpec, C: EffectCodec<E>> EffectPersistence<E> for JournalEffectPersistence<E, C> {
    fn prepared(&mut self, key: EffectKey, intent: &E::Intent) -> Result<(), EffectError> {
        let intent = self.codec.encode_intent(intent)?;
        self.faults
            .trip(Boundary::EffectPrepared)
            .map_err(EffectError::Injected)?;
        self.append(&EffectJournalRecord::Prepared {
            key: key.to_bytes(),
            intent,
        })
        .map(|_| ())
    }
    fn executing(&mut self, key: EffectKey, fence: EffectFence) -> Result<(), EffectError> {
        self.executing_with_ordinal(key, fence, 0)
    }
    fn executing_with_ordinal(
        &mut self,
        key: EffectKey,
        effect_fence: EffectFence,
        ordinal: u64,
    ) -> Result<(), EffectError> {
        if ordinal == 0
            || ordinal <= self.max_ordinal.load(Ordering::Acquire)
            || fence(key, ordinal) != effect_fence
        {
            return Err(EffectError::ConflictingHistory);
        }
        self.faults
            .trip(Boundary::EffectExecuting)
            .map_err(EffectError::Injected)?;
        self.append(&EffectJournalRecord::Executing {
            key: key.to_bytes(),
            fence: effect_fence,
            ordinal,
        })?;
        let _ = self.max_ordinal.fetch_max(ordinal, Ordering::AcqRel);
        Ok(())
    }
    fn ambiguous(
        &mut self,
        key: EffectKey,
        fence: EffectFence,
        reason: AmbiguousReason,
    ) -> Result<(), EffectError> {
        self.ambiguous_with_ordinal(key, fence, 0, reason)
    }
    fn ambiguous_with_ordinal(
        &mut self,
        key: EffectKey,
        effect_fence: EffectFence,
        ordinal: u64,
        reason: AmbiguousReason,
    ) -> Result<(), EffectError> {
        if ordinal == 0 || fence(key, ordinal) != effect_fence {
            return Err(EffectError::ConflictingHistory);
        }
        self.faults
            .trip(Boundary::EffectAmbiguous)
            .map_err(EffectError::Injected)?;
        self.append(&EffectJournalRecord::Ambiguous {
            key: key.to_bytes(),
            fence: effect_fence,
            ordinal,
            reason,
        })
        .map(|_| ())
    }
    fn confirmed(&mut self, key: EffectKey, receipt: &E::Receipt) -> Result<(), EffectError> {
        let receipt = self.codec.encode_receipt(receipt)?;
        self.faults
            .trip(Boundary::EffectConfirmed)
            .map_err(EffectError::Injected)?;
        self.append(&EffectJournalRecord::Confirmed {
            key: key.to_bytes(),
            receipt,
        })
        .map(|_| ())
    }

    fn fault_controller(&self) -> Option<Arc<Faults>> {
        Some(Arc::clone(&self.faults))
    }

    fn max_ordinal(&self) -> u64 {
        self.max_ordinal.load(Ordering::Acquire)
    }
}
