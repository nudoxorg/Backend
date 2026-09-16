//! Snapshot writer and journal replacement protocol.

use super::persistence::JournalEffectPersistence;
use super::pointer::{
    NEXT_SNAPSHOT_TEMP, containing_directory, read_snapshot_pointer, write_snapshot_pointer,
};
use super::record::EffectJournalRecord;
use super::recovered::RecoveredEffect;
use super::replay::snapshot_from_recovered;
use super::snapshot::{
    EffectSnapshotLimits, EffectSnapshotLog, EffectSnapshotReceipt, SNAPSHOT_MAX_BYTES,
    SNAPSHOT_MAX_ENTRIES, StoredCheckpoint, snapshot_checkpoint, snapshot_receipt_from_journal,
};
use super::{EffectCodec, EffectLog};
use crate::effects::spec::EffectSpec;
use crate::effects::state::EffectError;
use crate::fault::Boundary;
use crate::journal::{HashChainJournal, JournalCheckpoint, JournalLimits};
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::sync::atomic::Ordering;

pub(super) fn open_snapshot_writer(
    snapshot_path: &Path,
    pointer_path: &Path,
) -> Result<HashChainJournal<EffectSnapshotLog>, EffectError> {
    let limits = JournalLimits {
        max_frames: SNAPSHOT_MAX_ENTRIES,
        max_bytes: SNAPSHOT_MAX_BYTES,
    };
    if let Some(receipt) = read_snapshot_pointer(pointer_path)? {
        let (journal, _) = HashChainJournal::<EffectSnapshotLog>::open_from_checkpoint_streaming(
            snapshot_path,
            snapshot_checkpoint(receipt),
            limits,
        )
        .map_err(EffectError::Journal)?;
        return Ok(journal);
    }
    let (journal, _) = HashChainJournal::<EffectSnapshotLog>::open_streaming(snapshot_path, limits)
        .map_err(EffectError::Journal)?;
    Ok(journal)
}

pub(super) fn temporary_compaction_path(path: &Path) -> std::path::PathBuf {
    let nonce = NEXT_SNAPSHOT_TEMP.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .map_or_else(|| OsString::from("journal"), OsString::from);
    let mut temporary = file_name;
    temporary.push(format!(".compact.{}.{}", std::process::id(), nonce));
    containing_directory(path).join(temporary)
}

impl<E: EffectSpec, C: EffectCodec<E>> JournalEffectPersistence<E, C> {
    /// Writes a bounded typed snapshot and atomically selects it for warm
    /// recovery. The selected snapshot is paired with the exact effect
    /// journal tail observed before its frame is written.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn checkpoint<I>(
        &mut self,
        spec: &E,
        recovered: I,
        limits: EffectSnapshotLimits,
    ) -> Result<EffectSnapshotReceipt, EffectError>
    where
        I: IntoIterator<Item = RecoveredEffect<E::Intent, E::Receipt>>,
    {
        let base = match self.tail.lock() {
            Ok(tail) => tail.map(StoredCheckpoint::from_journal),
            Err(poisoned) => poisoned.into_inner().map(StoredCheckpoint::from_journal),
        };
        let last_ordinal = self.max_ordinal.load(Ordering::Acquire);
        let snapshot =
            snapshot_from_recovered(recovered, spec, &self.codec, base, last_ordinal, limits)?;
        let (snapshot_path, pointer_path) = (&self.snapshot_path, &self.pointer_path);
        let snapshot_journal = open_snapshot_writer(snapshot_path, pointer_path)?;
        let receipt = snapshot_journal
            .append(&snapshot)
            .map_err(EffectError::Journal)?;
        let receipt = snapshot_receipt_from_journal(receipt);
        // Pointer selection is the linearization point. A crash before the
        // rename leaves the previous pointer and therefore the previous
        // snapshot/journal pairing authoritative.
        write_snapshot_pointer(pointer_path, receipt, &self.faults)?;
        Ok(receipt)
    }

    /// Writes and selects a typed snapshot, then replaces the effect journal
    /// with an empty authenticated suffix. Pointer durability precedes the
    /// replacement; recovery can therefore use the snapshot if a crash lands
    /// between the two operations.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn checkpoint_and_compact<I>(
        &mut self,
        spec: &E,
        recovered: I,
        snapshot_limits: EffectSnapshotLimits,
        journal_limits: JournalLimits,
    ) -> Result<EffectSnapshotReceipt, EffectError>
    where
        I: IntoIterator<Item = RecoveredEffect<E::Intent, E::Receipt>>,
    {
        let receipt = self.checkpoint(spec, recovered, snapshot_limits)?;
        self.compact_journal(receipt, journal_limits)?;
        Ok(receipt)
    }

    pub(crate) fn compact_journal(
        &mut self,
        snapshot: EffectSnapshotReceipt,
        limits: JournalLimits,
    ) -> Result<(), EffectError> {
        let path = self.journal.path().to_owned();
        let temporary = temporary_compaction_path(&path);
        let _ = fs::remove_file(&temporary);
        self.faults
            .trip(Boundary::TempCreate)
            .map_err(EffectError::Injected)?;
        let (replacement, _) =
            match HashChainJournal::<EffectLog>::open_streaming(&temporary, limits) {
                Ok(value) => value,
                Err(error) => {
                    let _ = fs::remove_file(&temporary);
                    return Err(EffectError::Journal(error));
                }
            };
        // Keep a complete, synced generation marker in the replacement. An
        // empty file is indistinguishable from a truncation, while this
        // marker binds the new journal to the exact selected snapshot.
        let marker = match replacement.append(&EffectJournalRecord::Compacted { snapshot }) {
            Ok(marker) => marker,
            Err(error) => {
                drop(replacement);
                let _ = fs::remove_file(&temporary);
                return Err(EffectError::Journal(error));
            }
        };
        if let Err(error) = self.faults.trip(Boundary::TempWrite) {
            drop(replacement);
            let _ = fs::remove_file(&temporary);
            return Err(EffectError::Injected(error));
        }
        if let Err(error) = self.faults.trip(Boundary::FileSync) {
            drop(replacement);
            let _ = fs::remove_file(&temporary);
            return Err(EffectError::Injected(error));
        }
        drop(replacement);
        let mut replaced = false;
        let result = (|| -> Result<(), EffectError> {
            self.faults
                .trip(Boundary::Rename)
                .map_err(EffectError::Injected)?;
            fs::rename(&temporary, &path)
                .map_err(|error| EffectError::Persistence(error.to_string()))?;
            replaced = true;
            let parent = containing_directory(&path);
            self.faults
                .trip(Boundary::DirSync)
                .map_err(EffectError::Injected)?;
            backend_platform::durability::open_directory(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| EffectError::Persistence(error.to_string()))?;
            Ok(())
        })();
        if let Err(error) = result {
            if replaced {
                match HashChainJournal::<EffectLog>::open_streaming(&path, limits) {
                    Ok((journal, _)) => {
                        let _old = std::mem::replace(&mut self.journal, journal);
                        if self.set_tail(marker.checkpoint()).is_err() {
                            self.journal.mark_unusable();
                        }
                    }
                    Err(_) => self.journal.mark_unusable(),
                }
            }
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        let (journal, _) = match HashChainJournal::<EffectLog>::open_streaming(&path, limits) {
            Ok(value) => value,
            Err(error) => {
                self.journal.mark_unusable();
                return Err(EffectError::Journal(error));
            }
        };
        let _old = std::mem::replace(&mut self.journal, journal);
        if let Err(error) = self.set_tail(marker.checkpoint()) {
            self.journal.mark_unusable();
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn set_tail(
        &self,
        checkpoint: JournalCheckpoint<EffectLog>,
    ) -> Result<(), EffectError> {
        if let Ok(mut tail) = self.tail.lock() {
            *tail = Some(checkpoint);
            Ok(())
        } else {
            // The tail cache is part of checkpoint selection. Recovering
            // a poisoned value could anchor a future snapshot before a
            // durable suffix and make compaction discard committed work.
            self.journal.mark_unusable();
            Err(EffectError::Persistence("poisoned effect tail".into()))
        }
    }
}
