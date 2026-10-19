//! Recovery observations for the workspace hash-chain journal.

use super::head::WorkspaceHead;
use super::model::WorkspaceModel;
use super::owner::{WorkspaceError, sync_directory};
use super::pack::{read_workspace_pack_index, verify_workspace_pack};
use super::record::WorkspaceRecord;
use super::transition::persisted_from_store_manifest;
use crate::fault::{Boundary, Faults};
use crate::journal::{ChainHash, HashChainJournal, JournalCodec, JournalError, JournalLimits};
use crate::schema::{RecordId, WorkspaceLog};
use backend_store::FileStore;
use std::fs;
use std::io;
use std::path::Path;

const MAX_STORE_BYTES: usize = 64 * 1024 * 1024;

/// Opens only the fixed authenticated recovery frontier of a publication.
/// Staged evidence membership is never exported as a complete object vector.
pub fn control_frontier(
    store: &FileStore,
    descriptor: &backend_store::PublicationDescriptor,
) -> Result<backend_store::ClosureManifest, WorkspaceError> {
    let membership = store
        .open_closure(descriptor.closure())
        .map_err(WorkspaceError::store)?;
    let index = read_workspace_pack_index(store, descriptor, &membership)?;
    let persisted = persisted_from_store_manifest(
        &membership,
        index.transaction,
        index.payloads,
        &index.auxiliary,
    )?;
    Ok(persisted.closure_manifest().clone())
}

/// Disposition of one durable transaction after journal replay.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransactionDisposition {
    /// Prepared bytes exist without a selected head.
    Pending,
    /// A selected transition can be reconstructed as the current head.
    Selected,
    /// A publication marker follows the selected transition.
    Published,
}

/// Compact restart report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryReport {
    /// Number of validated frames.
    pub frames: usize,
    /// Number of prepared records.
    pub prepared: usize,
    /// Number of selected records.
    pub selected: usize,
    /// Number of published records.
    pub published: usize,
    /// Whether a torn final frame was repaired.
    pub truncated_tail: bool,
    /// Last valid sequence.
    pub last_sequence: Option<u64>,
}

/// Replays and validates the workspace journal.  Record semantics are checked
/// by the owner when the associated typed closure is admitted.
/// # Errors
///
/// Returns an error when validation, persistence, or admission of the
/// supplied value fails.
pub fn inspect(journal: &HashChainJournal<WorkspaceLog>) -> Result<RecoveryReport, WorkspaceError> {
    let (mut report, scan) = journal
        .fold_stream(
            JournalLimits::default(),
            RecoveryReport {
                frames: 0,
                prepared: 0,
                selected: 0,
                published: 0,
                truncated_tail: false,
                last_sequence: None,
            },
            |report, frame| {
                report.frames = report.frames.checked_add(1).ok_or(JournalError::Bounds)?;
                match WorkspaceLog::decode(frame.payload)? {
                    WorkspaceRecord::Prepared { .. } | WorkspaceRecord::PreparedSummary { .. } => {
                        report.prepared =
                            report.prepared.checked_add(1).ok_or(JournalError::Bounds)?;
                    }
                    WorkspaceRecord::Select { .. } => {
                        report.selected =
                            report.selected.checked_add(1).ok_or(JournalError::Bounds)?;
                    }
                    WorkspaceRecord::Published { .. } => {
                        report.published = report
                            .published
                            .checked_add(1)
                            .ok_or(JournalError::Bounds)?;
                    }
                }
                Ok(())
            },
        )
        .map_err(WorkspaceError::Journal)?;
    report.truncated_tail = scan.truncated_tail;
    report.last_sequence = scan.last_sequence;
    Ok(report)
}

/// Materializes a bounded diagnostic history for tests and forensic tools.
/// Normal owner recovery uses the streaming fold and never calls this API.
/// # Errors
///
/// Returns an error when validation, persistence, or admission of the
/// supplied value fails.
pub fn scan_materialized(
    journal: &HashChainJournal<WorkspaceLog>,
    limits: JournalLimits,
) -> Result<crate::journal::JournalRecovery<WorkspaceLog>, JournalError> {
    journal.recover_with_limits(limits)
}

/// Opens the diagnostic journal without making it a prerequisite for the
/// store-owned workspace head.  A malformed diagnostic file is quarantined
/// and replaced with a fresh chain; filesystem failures still surface because
/// they would prevent future diagnostic acknowledgements from being written.
pub(crate) fn open_diagnostic_journal(
    path: &Path,
    directory: &Path,
    epoch: u64,
    limits: JournalLimits,
) -> Result<HashChainJournal<WorkspaceLog>, WorkspaceError> {
    open_diagnostic_journal_for::<WorkspaceLog>(path, directory, epoch, limits)
}

// Keep the rollback policy independent of the closed record grammar. This is
// also exercised with the previous reader's exact tag set below.
fn open_diagnostic_journal_for<D: JournalCodec>(
    path: &Path,
    directory: &Path,
    epoch: u64,
    limits: JournalLimits,
) -> Result<HashChainJournal<D>, WorkspaceError> {
    match HashChainJournal::<D>::open_streaming(path, limits) {
        Ok((journal, _scan)) => Ok(journal),
        Err(error)
            if matches!(
                &error,
                JournalError::Corrupt(_)
                    | JournalError::Bounds
                    | JournalError::DomainMismatch
                    | JournalError::Record(_)
            ) =>
        {
            let quarantine = directory.join(format!(
                "workspace.diagnostic.corrupt.{epoch}-{}",
                std::process::id()
            ));
            match fs::rename(path, &quarantine) {
                Ok(()) => sync_directory(directory)?,
                Err(rename) if rename.kind() == io::ErrorKind::NotFound => {}
                Err(rename) => return Err(WorkspaceError::io(rename)),
            }
            HashChainJournal::<D>::open_streaming(path, limits)
                .map(|(journal, _scan)| journal)
                .map_err(WorkspaceError::Journal)
        }
        Err(error) => Err(WorkspaceError::Journal(error)),
    }
}

/// Reconstructs the typed owner view from the store's authenticated HEAD.
///
/// The complete transition envelope is retained under the store closure ID,
/// so this path does not scan or trust the diagnostic journal.  The selected
/// store descriptor, closure manifest, object files, and pack index are the
/// sole durable authority for the visible root and its typed evidence.
pub(crate) fn recover_store_head<M: WorkspaceModel>(
    store: &FileStore,
    model: &M,
    genesis: &WorkspaceHead,
    owner_epoch: u64,
    faults: &Faults,
) -> Result<WorkspaceHead, WorkspaceError> {
    faults
        .trip(Boundary::Recovery)
        .map_err(WorkspaceError::Injected)?;
    let Some(physical) = store.head().map_err(WorkspaceError::store)? else {
        // No store selection exists. Any engine Prepared/Select records are
        // abandoned diagnostics and cannot advance the owner.
        return Ok(genesis.with_owner_epoch(owner_epoch));
    };
    let descriptor = physical.descriptor();
    let Some(binding) = descriptor.workspace() else {
        return Err(WorkspaceError::Store(
            "physical store head lacks a typed workspace binding".to_owned(),
        ));
    };
    if binding.root() != &descriptor.target()
        || binding.closure().as_bytes() != descriptor.closure().as_bytes()
    {
        return Err(WorkspaceError::Store(
            "physical workspace binding does not match its descriptor".to_owned(),
        ));
    }
    // Open the authenticated closure index without materializing its flat
    // compatibility export. The fixed workspace pack contains the only
    // payload identities needed for typed recovery; each is admitted through
    // a bounded manifest lookup below.
    let durable_manifest = store
        .open_closure(descriptor.closure())
        .map_err(WorkspaceError::store)?;
    let index = read_workspace_pack_index(store, &descriptor, &durable_manifest)?;
    let persisted = persisted_from_store_manifest(
        &durable_manifest,
        index.transaction,
        index.payloads,
        &index.auxiliary,
    )?;
    let transition = model
        .admit_persisted_with_store(&persisted, store)
        .map_err(|error| {
            WorkspaceError::Model(format!("persisted store-head admission: {error}"))
        })?;
    let transition = if let Some(descriptor_id) = index.catalog_descriptor {
        if durable_manifest
            .get(descriptor_id)
            .map_err(WorkspaceError::store)?
            .is_none()
        {
            return Err(WorkspaceError::Corrupt(
                "catalog descriptor outside closure",
            ));
        }
        transition.with_catalog_descriptor(descriptor_id)
    } else {
        transition
    };
    // Old root-only publications retained fewer auxiliary pointers in their
    // fixed pack than their authenticated closure. Keep the original frontier
    // for exact pack verification after typed closure admission, rather than
    // silently rewriting a selected publication into the newer writer form.
    let transition = transition.with_recovery_frontier(index.frontier)?;
    if transition.transaction() != index.transaction {
        return Err(WorkspaceError::Corrupt(
            "store/typed transition transaction mismatch",
        ));
    }
    if transition.target().to_bytes() != descriptor.target() {
        return Err(WorkspaceError::Corrupt(
            "store/typed transition target mismatch",
        ));
    }
    if transition.closure().membership_id().as_bytes() != descriptor.closure().as_bytes() {
        return Err(WorkspaceError::Corrupt(
            "store/typed transition closure mismatch",
        ));
    }
    verify_workspace_pack(store, &transition, &descriptor, MAX_STORE_BYTES)?;
    Ok(WorkspaceHead::from_transition(
        transition,
        descriptor.target_generation(),
        0,
        ChainHash::genesis(),
        RecordId::from_payload(&[]),
        owner_epoch,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::TransactionId;
    use crate::workspace::record::tests::LegacyWorkspaceLog;

    #[test]
    fn mixed_prepared_grammars_append_reopen_and_count_exact_control_links()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("workspace.journal");
        let transaction = TransactionId::from_bytes([2; 32]);
        let legacy = WorkspaceRecord::Prepared {
            request: [1; 32],
            transaction,
            manifest: b"manifest".to_vec(),
            delta: b"delta".to_vec(),
            commit: b"commit".to_vec(),
            closure: b"closure".to_vec(),
        };
        let (journal, _) = HashChainJournal::<WorkspaceLog>::open(&path)?;
        journal.append(&legacy)?;
        drop(journal);
        // Historical records retain their original closed grammar and bytes.
        let (old, history) = HashChainJournal::<LegacyWorkspaceLog>::open(&path)?;
        assert_eq!(history.frames.len(), 1);
        assert_eq!(
            LegacyWorkspaceLog::decode(&history.frames[0].payload)?,
            legacy
        );
        drop(old);
        let (journal, _) = HashChainJournal::<WorkspaceLog>::open(&path)?;
        let summary = WorkspaceRecord::PreparedSummary {
            request: [1; 32],
            transaction,
            manifest_bytes: 8,
            manifest_digest: *blake3::hash(b"manifest").as_bytes(),
            delta_bytes: 5,
            delta_digest: *blake3::hash(b"delta").as_bytes(),
            commit_bytes: 6,
            commit_digest: *blake3::hash(b"commit").as_bytes(),
            closure_bytes: 7,
            closure_digest: *blake3::hash(b"closure").as_bytes(),
        };
        let prepared = journal.append(&summary)?;
        journal.append_encoded(|output| {
            crate::workspace::record::encode_select(output, transaction, 7, prepared)
        })?;
        journal.append(&WorkspaceRecord::Published {
            transaction,
            sequence: 7,
        })?;
        drop(journal);
        let (journal, history) = HashChainJournal::<WorkspaceLog>::open(&path)?;
        assert_eq!(history.frames.len(), 4);
        assert_eq!(WorkspaceLog::decode(&history.frames[1].payload)?, summary);
        let report = inspect(&journal)?;
        assert_eq!(
            (
                report.frames,
                report.prepared,
                report.selected,
                report.published
            ),
            (4, 2, 1, 1)
        );
        assert!(!report.truncated_tail);
        assert_eq!(report.last_sequence, Some(3));
        let WorkspaceRecord::Select {
            prepared_sequence,
            prepared_offset,
            prepared_chain,
            prepared_record,
            ..
        } = WorkspaceLog::decode(&history.frames[2].payload)?
        else {
            panic!("select link expected")
        };
        assert_eq!(prepared_sequence, prepared.sequence);
        assert_eq!(prepared_offset, prepared.offset);
        assert_eq!(prepared_chain, *prepared.chain.as_bytes());
        assert_eq!(prepared_record, *prepared.record.as_bytes());
        drop(journal);

        // Downgrades reject this closed grammar, preserve the entire original
        // history, then resume a fresh diagnostic chain. This is the owner's
        // real quarantine policy, not a test-local rename approximation.
        let original = fs::read(&path)?;
        assert!(matches!(
            HashChainJournal::<LegacyWorkspaceLog>::open(&path),
            Err(JournalError::Corrupt("record trailing bytes"))
        ));
        assert_eq!(fs::read(&path)?, original);
        let old = open_diagnostic_journal_for::<LegacyWorkspaceLog>(
            &path,
            root.path(),
            41,
            JournalLimits::default(),
        )?;
        let quarantine = root.path().join(format!(
            "workspace.diagnostic.corrupt.41-{}",
            std::process::id()
        ));
        assert_eq!(fs::read(&quarantine)?, original);
        assert_eq!(old.recover()?.frames.len(), 0);
        drop(old);
        let (retained, _) = HashChainJournal::<WorkspaceLog>::open(&quarantine)?;
        assert_eq!(inspect(&retained)?.prepared, 2);
        Ok(())
    }
}
