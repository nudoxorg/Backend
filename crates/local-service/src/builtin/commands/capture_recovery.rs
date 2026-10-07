//! Recovery of a durable source capture whose owner work did not survive.
//!
//! Pending is not a lease. It must not be overwritten or cleared merely
//! because its process is absent. The current exclusive workspace owner first
//! proves absence of its admitted local job and of durable remote work, then
//! uses the ordinary authenticated terminal-capture transition. This preserves
//! the original capture tuple, selected predecessor and publication history.

use super::super::super::pending_stored::PendingStoredAckProductKey;
use super::*;
use backend_engine::builtin::{
    ProductSemanticCaptureOutcome, ProductSemanticCaptureRecord, SemanticUnavailableReason,
    semantic_capture_relation,
};

struct PendingCaptureCandidate {
    key: ProductSemanticPublicationKey,
    record: ProductSemanticCaptureRecord,
}

/// Owner-local proof, never serialized or accepted as a caller assertion.
/// The ACK journal lock remains held from its construction through commit.
struct OrphanedCapture {
    key: ProductSemanticPublicationKey,
    record: ProductSemanticCaptureRecord,
    owner_epoch: u64,
    owner_fence: [u8; 32],
}

impl CommandAdapter {
    /// Explicit retry/remove is the recovery action. A live local job or any
    /// possibly recoverable remote assignment keeps the marker Pending.
    pub(super) fn recover_orphaned_package_label(
        &mut self,
        daemon: &mut ProductDaemon,
        label: &str,
        request_id: u64,
    ) -> Result<(), BuiltinModelError> {
        if self.indexing.is_some() {
            return Ok(());
        }
        let (package_key, label) =
            canonical_local_package(backend_engine::package_key(label), label.to_owned())?;
        let package = backend_library::PackageReference::parse(label.clone())
            .map_err(|error| BuiltinModelError(format!("capture recovery package: {error}")))?;
        let owner = daemon.engine().daemon().owner();
        owner
            .lease()
            .assert_current()
            .map_err(|error| BuiltinModelError(format!("capture recovery owner lease: {error}")))?;
        let owner_epoch = owner.lease().epoch();
        let owner_fence = owner.lease().fence();
        let snapshot = owner.snapshot();
        let Some(relation) = semantic_capture_relation(&snapshot).map_err(|error| {
            BuiltinModelError(format!("open recovery capture relation: {error}"))
        })?
        else {
            return Ok(());
        };
        let mut candidates = Vec::new();
        let mut from = Some(ProductSemanticPublicationKey::package_lower_bound(
            package.clone(),
        ));
        let mut after = None;
        'pages: loop {
            let page = match from.take() {
                Some(start) => relation.page_from(&start, backend_engine::MAX_SNAPSHOT_PAGE_ROWS),
                None => relation.page(after.as_ref(), backend_engine::MAX_SNAPSHOT_PAGE_ROWS),
            }
            .map_err(|error| BuiltinModelError(format!("page recovery captures: {error}")))?;
            for (key, record) in page.entries() {
                if key.package() != &package {
                    break 'pages;
                }
                if key.is_selected()
                    && matches!(
                        record.outcome(),
                        ProductSemanticCaptureOutcome::Pending { .. }
                    )
                {
                    // Scratch is bounded independently of package history.
                    if candidates.len() == backend_engine::MAX_SNAPSHOT_PAGE_ROWS {
                        return Err(BuiltinModelError("Pending capture recovery exceeds its bounded owner page; no marker was changed".to_owned()));
                    }
                    candidates.push(PendingCaptureCandidate {
                        key: key.clone(),
                        record: record.clone(),
                    });
                }
            }
            let Some(next) = page.next().cloned() else {
                break;
            };
            after = Some(next);
        }
        if candidates.is_empty() {
            return Ok(());
        }

        // An enabled remote compiler without its durable ownership journal
        // cannot establish absence. Poison or I/O is likewise not absence.
        if self.owner_cluster.is_some() && self.pending_stored_acks.is_none() {
            return Err(BuiltinModelError("Pending capture recovery requires the configured remote-work ownership journal; retry after it is available".to_owned()));
        }
        let journal = self.pending_stored_acks.clone();
        let pending = journal.as_ref().map(|journal| journal.lock()).transpose()
            .map_err(|_| BuiltinModelError("Pending capture remote-work ownership journal is unavailable; no marker was changed".to_owned()))?;
        let mut orphaned = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let profile = <[u8; 2]>::from(candidate.key.profile());
            let remote_key = PendingStoredAckProductKey {
                package: candidate.key.package().as_str().into(),
                coordinate: candidate.key.coordinate().as_str().into(),
                profile: format!("{:02x}{:02x}/lower-ir", profile[0], profile[1]).into_boxed_str(),
            };
            if pending
                .as_ref()
                .is_some_and(|journal| journal.protects_capture(&remote_key))
            {
                return Err(BuiltinModelError("Pending capture still has admitted remote work or an unresolved stored result; wait for its terminal receipt before retrying or removing".to_owned()));
            }
            self.admit_interrupted_operation(daemon, &candidate)?;
            orphaned.push(OrphanedCapture {
                key: candidate.key,
                record: candidate.record,
                owner_epoch,
                owner_fence,
            });
        }
        for candidate in orphaned {
            let owner = daemon.engine().daemon().owner();
            owner.lease().assert_current().map_err(|error| {
                BuiltinModelError(format!("capture recovery owner lease: {error}"))
            })?;
            if owner.lease().epoch() != candidate.owner_epoch
                || owner.lease().fence() != candidate.owner_fence
                || self.indexing.is_some()
                || semantic_capture_relation(&owner.snapshot())
                    .map_err(|error| BuiltinModelError(error.to_string()))?
                    .ok_or_else(|| {
                        BuiltinModelError("recovery capture relation disappeared".to_owned())
                    })?
                    .lookup(&candidate.key)
                    .map_err(|error| BuiltinModelError(error.to_string()))?
                    .as_ref()
                    != Some(&candidate.record)
            {
                return Err(BuiltinModelError("Pending capture ownership or exact source marker changed before recovery; retry".to_owned()));
            }
            let captures = BTreeMap::from([(candidate.key.clone(), candidate.record.capture())]);
            commit_pending_capture_failure(
                daemon,
                package_key,
                &label,
                request_id,
                &captures,
                SemanticUnavailableReason::Rejected,
                None,
            )?;
            if let Some(bytes) = candidate.record.operation_key() {
                let operation = backend_library::IndexOperationKey::from_bytes(*bytes)
                    .map_err(|error| BuiltinModelError(error.to_string()))?;
                if let Some(JournalEntry::Retained(entry)) = self
                    .index_operations
                    .entry(operation)
                    .map_err(|error| BuiltinModelError(error.to_string()))?
                    && let Some(original) = entry.source_capture.as_ref()
                {
                    self.refresh_index_operation_source_capture(
                        daemon,
                        operation,
                        &entry.package,
                        original,
                    )
                    .map_err(|error| BuiltinModelError(error.to_string()))?;
                    // A mixed operation may have a genuine published profile.
                    // Its missing prepared publication receipt stays unresolved.
                    if let Some(JournalEntry::Retained(updated)) = self.index_operations.entry(operation)
                        .map_err(|error| BuiltinModelError(error.to_string()))?
                        && updated.source_capture.as_ref().is_some_and(|receipt| receipt.profiles().iter().all(|profile| matches!(profile.state,
                            backend_library::IndexOperationSemanticProfileState::Failed { .. }
                            | backend_library::IndexOperationSemanticProfileState::Unavailable { .. })))
                    {
                        self.index_operations.failed(operation,
                            backend_library::IndexOperationFailureReason::WorkerFailed,
                            backend_library::ProductText::from_static("interrupted Pending source capture recovered: no live owner job or admitted remote work remains; retry indexing with a new operation key; semantic publication was not completed"))
                            .map_err(|error| BuiltinModelError(error.to_string()))?;
                    }
                }
            }
        }
        drop(pending);
        self.publish_view(daemon, None)
    }

    fn admit_interrupted_operation(
        &mut self,
        daemon: &ProductDaemon,
        candidate: &PendingCaptureCandidate,
    ) -> Result<(), BuiltinModelError> {
        let Some(bytes) = candidate.record.operation_key() else {
            return Ok(());
        };
        let key = backend_library::IndexOperationKey::from_bytes(*bytes)
            .map_err(|error| BuiltinModelError(error.to_string()))?;
        let Some(JournalEntry::Retained(entry)) =
            self.index_operations.entry(key).map_err(|error| {
                BuiltinModelError(format!("read interrupted operation ownership: {error}"))
            })?
        else {
            return Err(BuiltinModelError("Pending capture lacks its exact retained operation ownership; no marker was changed".to_owned()));
        };
        if entry.package != *candidate.key.package()
            || !matches!(entry.state, StoredOperationState::Accepted)
            || entry.source_capture_base.is_none_or(|base| {
                &base.workspace_root != candidate.record.base_workspace_root()
                    || base.workspace_sequence != candidate.record.base_workspace_sequence()
            })
        {
            return Err(BuiltinModelError("Pending capture operation ownership or captured base is inconsistent; no marker was changed".to_owned()));
        }
        let receipt = super::super::index::source_capture_receipt_for_root(
            daemon,
            &entry.package,
            key,
            Some(*candidate.record.request_identity()),
        )?
        .ok_or_else(|| {
            BuiltinModelError(
                "Pending capture has no exact authenticated operation receipt".to_owned(),
            )
        })?;
        if receipt.commit_identity() != candidate.record.source_commit()
            || receipt.workspace_root() != candidate.record.source_workspace_root()
            || receipt.workspace_sequence() != candidate.record.source_workspace_sequence()
            || entry.source_capture.as_ref().is_some_and(|stored| {
                stored.commit_identity() != receipt.commit_identity()
                    || stored.workspace_root() != receipt.workspace_root()
                    || stored.workspace_sequence() != receipt.workspace_sequence()
            })
        {
            return Err(BuiltinModelError("Pending capture operation receipt substituted a different source identity; no marker was changed".to_owned()));
        }
        if let Some(original) = entry.source_capture.as_ref() {
            self.refresh_index_operation_source_capture(daemon, key, &entry.package, original)
                .map_err(|error| {
                    BuiltinModelError(format!("admit exact interrupted profile tuple: {error}"))
                })?;
        } else {
            self.index_operations
                .source_captured(key, receipt)
                .map_err(|error| {
                    BuiltinModelError(format!("retain exact interrupted capture receipt: {error}"))
                })?;
        }
        Ok(())
    }
}
