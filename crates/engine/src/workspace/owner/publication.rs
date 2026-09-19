//! Durable workspace publication and post-selection acknowledgement.

use super::super::transition::PersistedTransition;
use super::{
    Arc, Boundary, CatalogState, ChainHash, DurablePublication, JournalCodec, PreparedPublication,
    Publication, PublicationStatus, PublishedPublication, RecordId, StoreError, WorkspaceError,
    WorkspaceHead, WorkspaceLog, WorkspaceModel, WorkspaceOwner, encode_prepared, encode_published,
    encode_select, store_head_matches, write_diagnostic,
};
use backend_store::ClosureManifest;

impl<M: WorkspaceModel> WorkspaceOwner<M> {
    /// Makes an admitted transition durable without selecting its new head.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn durable(
        &self,
        prepared: PreparedPublication,
    ) -> Result<DurablePublication, WorkspaceError> {
        self.lease.assert_current()?;
        if self.head != prepared.data.base_head {
            return Err(WorkspaceError::HeadConflict);
        }
        let mut data = prepared.data;
        if data.existing_published.is_some() {
            return Ok(Publication {
                data,
                _state: std::marker::PhantomData,
            });
        }
        let transition = &data.transition;
        let store_base = self
            .store
            .head()
            .map_err(WorkspaceError::store)?
            .map(backend_store::SelectedHead::as_base);
        if let Some(base) = store_base
            && (base.target() != self.head.root().to_bytes()
                || base.generation() != self.head.sequence())
        {
            return Err(WorkspaceError::HeadConflict);
        }
        self.faults
            .trip(Boundary::ObjectWrite)
            .map_err(WorkspaceError::Injected)?;
        self.faults
            .trip(Boundary::ClosureWrite)
            .map_err(WorkspaceError::Injected)?;
        self.faults
            .trip(Boundary::ObjectFlush)
            .map_err(WorkspaceError::Injected)?;
        if store_base.is_none() {
            // A workspace transition carries a persistent delta over the
            // genesis closure.  The store's incremental writer intentionally
            // writes only that delta's changed frontier, so seed the initial
            // immutable closure once before the first physical publication.
            // Without this step an authority or relation root inherited from
            // genesis could be named by the new manifest but absent from the
            // object CAS after a cold-start crash.
            let seed = ClosureManifest::new_with_registry(
                self.head.closure().manifest().objects().to_vec(),
                self.store.relation_registry(),
            )
            .map_err(WorkspaceError::store)?;
            self.store
                .write_closure(&seed)
                .map_err(WorkspaceError::store)?;
        }
        let store_prepared = transition
            .store_publication(&self.store, store_base)
            .map_err(WorkspaceError::store)?;
        let store_durable = store_prepared.durable().map_err(WorkspaceError::store)?;
        data.store_durable = Some(store_durable);
        self.faults
            .trip(Boundary::Transfer)
            .map_err(WorkspaceError::Injected)?;
        let persisted = transition.persisted();
        self.faults
            .trip(Boundary::JournalPrepared)
            .map_err(WorkspaceError::Injected)?;
        let prepared_receipt = self
            .journal
            .append_encoded(|output| encode_prepared(output, persisted))
            .map_err(WorkspaceError::Journal)?;
        data.prepared_receipt = Some(prepared_receipt);
        self.faults
            .trip(Boundary::JournalFlush)
            .map_err(WorkspaceError::Injected)?;
        Ok(Publication {
            data,
            _state: std::marker::PhantomData,
        })
    }

    /// Publishes the checked store transaction and records diagnostics.
    ///
    /// `FileStore::WorkspaceFileDurable::publish` is the sole
    /// linearization point.  The engine journal and diagnostic sidecar are
    /// intentionally written after that point and can therefore only report
    /// pending acknowledgement; they can never make an unselected root
    /// visible or make a selected store root disappear.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn publish(
        &mut self,
        durable: DurablePublication,
    ) -> Result<PublishedPublication, WorkspaceError> {
        self.lease.assert_current()?;
        if self.head != durable.data.base_head {
            return Err(WorkspaceError::HeadConflict);
        }
        let mut data = durable.data;
        if let Some(existing) = data.existing_published.take() {
            if existing != self.head {
                return Err(WorkspaceError::HeadConflict);
            }
            data.post_selection = PublicationStatus::default();
            return Ok(Publication {
                data,
                _state: std::marker::PhantomData,
            });
        }
        let store_durable = data
            .store_durable
            .take()
            .ok_or(WorkspaceError::Corrupt("missing store publication"))?;
        let sequence = self
            .head
            .sequence()
            .checked_add(1)
            .ok_or(WorkspaceError::Bounds)?;
        let persisted = data.transition.persisted_shared();
        let prepared_receipt = data
            .prepared_receipt
            .ok_or(WorkspaceError::Corrupt("missing prepared journal receipt"))?;

        let (store_published, status) = self.publish_store(
            &data.transition,
            store_durable,
            persisted.transaction(),
            sequence,
            prepared_receipt,
        )?;
        data.store_published = store_published;

        data.post_selection = self.acknowledge_selection(
            &data.transition,
            &persisted,
            sequence,
            prepared_receipt,
            status,
        );
        Ok(Publication {
            data,
            _state: std::marker::PhantomData,
        })
    }
}

impl<M: WorkspaceModel> WorkspaceOwner<M> {
    fn publish_store(
        &self,
        transition: &Arc<super::super::transition::PreparedTransition>,
        store_durable: backend_store::WorkspaceFileDurable,
        transaction: super::TransactionId,
        sequence: u64,
        prepared_receipt: crate::journal::JournalReceipt<WorkspaceLog>,
    ) -> Result<
        (
            Option<backend_store::WorkspaceFilePublished>,
            PublicationStatus,
        ),
        WorkspaceError,
    > {
        let mut select_probe = Vec::new();
        encode_select(&mut select_probe, transaction, sequence, prepared_receipt);
        WorkspaceLog::validate(&select_probe).map_err(WorkspaceError::Journal)?;
        let descriptor = store_durable.descriptor();
        if descriptor.target() != transition.target().to_bytes()
            || descriptor.target_generation() != sequence
            || descriptor.closure().as_bytes() != transition.closure().manifest().id().as_bytes()
        {
            return Err(WorkspaceError::ClosureMismatch);
        }
        let mut status = PublicationStatus::default();
        let store_attempt = store_durable.publish_with_authority_before_head(
            self.lease.publication_authority(),
            || {
                self.faults
                    .trip(Boundary::HeadWrite)
                    .map_err(WorkspaceError::Injected)
            },
        );
        let store_published = match store_attempt {
            Ok(Err(error)) => {
                return Err(WorkspaceError::PublicationPending {
                    target: transition.target(),
                    sequence,
                    status: status
                        .with_store_publish_pending()
                        .with_head_write_pending(),
                    detail: format!("store head acknowledgement: {error}"),
                });
            }
            Ok(Ok(published)) => Some(published),
            Err(error) => match store_head_matches(&self.store, transition, sequence) {
                Ok(true) => {
                    status = status.with_store_publish_pending();
                    None
                }
                Ok(false) if matches!(error, StoreError::StaleHead) => {
                    return Err(WorkspaceError::store(error));
                }
                Ok(false) => {
                    return Err(WorkspaceError::PublicationPending {
                        target: transition.target(),
                        sequence,
                        status: status.with_store_publish_pending(),
                        detail: format!("store publication acknowledgement: {error:?}"),
                    });
                }
                Err(observation) => {
                    return Err(WorkspaceError::PublicationPending {
                        target: transition.target(),
                        sequence,
                        status: status.with_store_publish_pending(),
                        detail: format!(
                            "store publication observation failed: {error:?}; {observation}"
                        ),
                    });
                }
            },
        };
        if let Some(published) = store_published.as_ref()
            && published.root() != transition.target()
        {
            return Err(WorkspaceError::PublicationPending {
                target: transition.target(),
                sequence,
                status: status.with_store_publish_pending(),
                detail: "store selected an unexpected workspace root".to_owned(),
            });
        }
        Ok((store_published, status))
    }

    fn acknowledge_selection(
        &mut self,
        transition: &Arc<super::super::transition::PreparedTransition>,
        persisted: &PersistedTransition,
        sequence: u64,
        prepared_receipt: crate::journal::JournalReceipt<WorkspaceLog>,
        mut status: PublicationStatus,
    ) -> PublicationStatus {
        let mut head = WorkspaceHead::from_shared_transition(
            Arc::clone(transition),
            sequence,
            0,
            ChainHash::genesis(),
            RecordId::from_payload(&[]),
            self.lease.epoch(),
        );
        self.head = head.clone();
        if transition.catalog_descriptor().is_none() {
            self.catalog = CatalogState::empty(transition.manifest().coverage());
        }
        let selected_receipt = if self.faults.trip(Boundary::JournalSelect).is_err() {
            status = status.with_journal_select_pending();
            None
        } else {
            match self.journal.append_encoded(|output| {
                encode_select(output, persisted.transaction(), sequence, prepared_receipt);
            }) {
                Ok(receipt) => Some(receipt),
                Err(_error) => {
                    status = status.with_journal_select_pending();
                    None
                }
            }
        };
        if let Some(receipt) = selected_receipt {
            head = WorkspaceHead::from_shared_transition(
                Arc::clone(transition),
                sequence,
                receipt.sequence,
                receipt.chain,
                receipt.record,
                self.lease.epoch(),
            );
            self.head = head.clone();
        }
        if self.faults.trip(Boundary::JournalFlush).is_err() {
            status = status.with_journal_flush_pending();
        }
        if self.faults.trip(Boundary::HeadSelection).is_err() {
            status = status.with_head_selection_pending();
        }
        if write_diagnostic(&self.directory, &head, &self.faults).is_err() {
            status = status.with_head_write_pending();
        }
        let journal_published = self.faults.trip(Boundary::JournalPublished).is_ok()
            && self
                .journal
                .append_encoded(|output| {
                    encode_published(output, persisted.transaction(), sequence);
                })
                .is_ok();
        if !journal_published {
            status = status.with_journal_published_pending();
        }
        if self.faults.trip(Boundary::Notification).is_err() {
            status = status.with_notification_pending();
        }
        status
    }
}
