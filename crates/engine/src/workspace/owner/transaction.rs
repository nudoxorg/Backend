//! Checked intent admission and derived-output staging/publication.

use super::{
    Arc, Boundary, CommitProvenance, DerivedOutputProof, DerivedOutputPublication, HeadExpectation,
    LatestQuery, PreparedPublication, PreparedTransition, Publication, PublicationData,
    PublicationStatus, StagedDerivedOutput, TransactionId, UntrustedObjectId, VersionObjectClosure,
    WorkspaceError, WorkspaceModel, WorkspaceOwner, append_to_closure, commit_capability,
    commit_checked, find_latest_in_state, find_proof_in_state, workspace_delta,
};

#[inline(never)]
fn consume_owned<T>(value: T) {
    // The owner receives these values from an owning queue boundary while
    // model/catalog operations borrow them. Keep the transfer explicit and
    // avoid cloning potentially large payloads merely to satisfy that API.
    std::hint::black_box(value);
}

impl<M: WorkspaceModel> WorkspaceOwner<M> {
    /// Prepares a checked model transition against the exact current head.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn prepare(
        &self,
        expected: HeadExpectation,
        intent: M::Intent,
    ) -> Result<PreparedPublication, WorkspaceError> {
        self.lease.assert_current()?;
        let request = self.model.request_id(&intent);
        // A retry can arrive with the caller's pre-commit expectation after
        // the selected HEAD has already advanced.  The request identity is
        // the owner-scoped idempotency key; reuse the exact selected head and
        // never mint a second store generation for it.
        if self.head.sequence() > 0 && self.head.request() == request {
            consume_owned(intent);
            return Ok(Publication::new(PublicationData {
                transition: self.head.transition_shared(),
                base_head: self.head.clone(),
                store_durable: None,
                store_published: None,
                prepared_receipt: None,
                post_selection: PublicationStatus::default(),
                existing_published: Some(self.head.clone()),
            }));
        }
        if expected != self.head.expectation() {
            return Err(WorkspaceError::HeadConflict);
        }
        let attempt = self
            .head
            .sequence()
            .checked_add(1)
            .ok_or(WorkspaceError::Bounds)?;
        let transaction =
            TransactionId::derive(self.lease.epoch(), self.head.root(), request, attempt);
        let transition = self
            .model
            .prepare(&self.snapshot(), &intent, transaction)
            .map_err(|error| WorkspaceError::Model(error.to_string()))?;
        let transition = if let Some(descriptor_id) = self.head.catalog_descriptor() {
            let transition = if transition
                .closure()
                .manifest()
                .contains_object_id(descriptor_id)
            {
                transition
            } else {
                let descriptor = self
                    .store
                    .read_object_claim(UntrustedObjectId::from_bytes(*descriptor_id.as_bytes()))
                    .map_err(WorkspaceError::store)?;
                transition.retain_objects([descriptor], self.store.relation_registry())?
            };
            transition.with_catalog_descriptor(descriptor_id)
        } else {
            transition
        };
        if transition.request() != request
            || transition.transaction() != transaction
            || transition.base() != self.head.root()
        {
            return Err(WorkspaceError::TransitionMismatch);
        }
        self.faults
            .trip(Boundary::Prepare)
            .map_err(WorkspaceError::Injected)?;
        consume_owned(intent);
        Ok(Publication::new(PublicationData {
            transition: Arc::new(transition),
            base_head: self.head.clone(),
            store_durable: None,
            store_published: None,
            prepared_receipt: None,
            post_selection: PublicationStatus::default(),
            existing_published: None,
        }))
    }

    /// Publishes an already admitted derived output by extending the current
    /// checked workspace closure and selecting one new store generation. The
    /// store publication is the sole linearization point; the diagnostic
    /// journal remains a non-authoritative acknowledgement.
    pub(crate) fn stage_derived_output(
        &self,
        expected: HeadExpectation,
        proof: DerivedOutputProof,
    ) -> Result<StagedDerivedOutput, WorkspaceError> {
        self.lease.assert_current()?;
        if expected != self.head.expectation() {
            return Err(WorkspaceError::HeadConflict);
        }
        let (output_object, manifest_object) = proof.payload_objects();
        let output_id = self
            .store
            .write_object(&output_object)
            .map_err(WorkspaceError::store)?;
        let manifest_id = self
            .store
            .write_object(&manifest_object)
            .map_err(WorkspaceError::store)?;
        Ok(StagedDerivedOutput {
            proof,
            base: expected,
            owner_epoch: self.lease.epoch(),
            output_object: output_id,
            manifest_object: manifest_id,
        })
    }

    /// Consumes an owner-issued staged result and performs the sole physical
    /// workspace publication. The stage capability is bound to the observed
    /// head and owner epoch, so a retry cannot publish behind a newer owner.
    pub(crate) fn publish_staged_derived_output(
        &mut self,
        staged: StagedDerivedOutput,
    ) -> Result<DerivedOutputPublication, WorkspaceError> {
        self.lease.assert_current()?;
        if staged.owner_epoch != self.lease.epoch() || staged.base != self.head.expectation() {
            return Err(WorkspaceError::HeadConflict);
        }
        self.publish_derived_output(staged.base, staged.proof)
    }

    fn prepare_derived_output_transition(
        &self,
        proof: &DerivedOutputProof,
        objects: backend_store::ClosureManifest,
        catalog_descriptor: super::ObjectId,
    ) -> Result<PreparedTransition, WorkspaceError> {
        // A durable derived-output catalog update is a real no-op workspace
        // transition with its own deterministic transaction and history
        // parent. Reusing the source commit would make the store select a new
        // generation under stale provenance and collapse two distinct writes.
        let attempt = self
            .head
            .sequence()
            .checked_add(1)
            .ok_or(WorkspaceError::Bounds)?;
        let transaction = TransactionId::derive(
            self.lease.epoch(),
            self.head.root(),
            proof.request(),
            attempt,
        );
        let manifest = self.head.manifest().clone();
        let delta = workspace_delta(&manifest, &manifest, Vec::new())
            .map_err(|error| WorkspaceError::Version(error.to_string()))?
            .into_checked();
        let authority = manifest
            .authority_closure()
            .ok_or(WorkspaceError::UnverifiedManifest)?;
        let provenance = CommitProvenance::new(
            authority,
            VersionObjectClosure::from_version(transaction.version()),
            b"backend.engine.derived-output.v1".to_vec(),
        );
        let commit = commit_checked(&manifest, vec![self.head.commit().id()], provenance)
            .map_err(|error| WorkspaceError::Version(error.to_string()))?;
        let checked_commit = commit_capability(
            &manifest,
            commit.parents().to_vec(),
            CommitProvenance::new(
                commit.authority(),
                commit.transaction(),
                commit.provenance().to_vec(),
            ),
        )
        .map_err(|error| WorkspaceError::Version(error.to_string()))?;
        // Carry the selected workspace's checked physical frontier into the
        // catalog transition before its provenance payloads are materialized.
        // Reconstructing from the logical index loses split-root child proof.
        let base_closure = self
            .head
            .closure()
            .rebind_checked_manifest_with_registry(
                &manifest,
                objects,
                self.store.relation_registry(),
            )
            .map_err(|error| {
                WorkspaceError::Store(format!("catalog manifest rebind: {error:?}"))
            })?;
        let closure = crate::workspace::transition::materialize_transition_closure_with_registry(
            &manifest,
            &delta,
            &checked_commit,
            transaction,
            proof.request(),
            &base_closure,
            self.store.relation_registry(),
        )?;
        PreparedTransition::from_checked_with_registry(
            crate::workspace::transition::CheckedTransitionInput {
                request: proof.request(),
                transaction,
                base: self.head.manifest(),
                manifest,
                delta,
                commit: checked_commit,
                closure,
                registry: self.store.relation_registry(),
            },
        )
        .map(|transition| transition.with_catalog_descriptor(catalog_descriptor))
    }

    /// Publishes an already admitted derived output by extending the current
    /// checked workspace closure and selecting one new store generation. The
    /// store publication is the sole linearization point; the diagnostic
    /// journal remains a non-authoritative acknowledgement.
    pub(crate) fn publish_derived_output(
        &mut self,
        expected: HeadExpectation,
        proof: DerivedOutputProof,
    ) -> Result<DerivedOutputPublication, WorkspaceError> {
        self.lease.assert_current()?;
        if expected != self.head.expectation() {
            return Err(WorkspaceError::HeadConflict);
        }
        if let Some(entry) = find_proof_in_state(&self.store, &self.catalog, &proof)? {
            consume_owned(proof);
            return Ok(DerivedOutputPublication::AlreadyPresent {
                entry,
                sequence: self.head.sequence(),
            });
        }
        let (objects, next_catalog) =
            append_to_closure(&self.store, self.head.closure(), &self.catalog, &proof)?;
        let catalog_descriptor = next_catalog
            .descriptor_object_id()?
            .ok_or(WorkspaceError::Corrupt("missing catalog descriptor"))?;
        let transition =
            self.prepare_derived_output_transition(&proof, objects, catalog_descriptor)?;
        let prepared = Publication::new(PublicationData {
            transition: Arc::new(transition),
            base_head: self.head.clone(),
            store_durable: None,
            store_published: None,
            prepared_receipt: None,
            post_selection: PublicationStatus::default(),
            existing_published: None,
        });
        let durable = self.durable(prepared)?;
        let published = self.publish(durable)?;
        self.catalog = next_catalog;
        let entry = find_proof_in_state(&self.store, &self.catalog, &proof)?
            .ok_or(WorkspaceError::Corrupt("published derived output missing"))?;
        consume_owned(proof);
        Ok(DerivedOutputPublication::Published {
            entry,
            sequence: self.head.sequence(),
            status: published.status(),
        })
    }

    /// Looks up the newest catalog entry for an exact semantic and authority
    /// binding. The dependency generation is selected from the authenticated
    /// catalog and must be restored by the dispatcher before reuse; callers
    /// cannot steer this lookup with a guessed generation.
    pub(crate) fn lookup_latest_derived_output(
        &self,
        query: &LatestQuery<'_>,
    ) -> Result<Option<crate::workspace::catalog::DerivedOutputEntry>, WorkspaceError> {
        find_latest_in_state(&self.store, &self.catalog, query)
    }

    /// Checks two staged immutable objects against the selected closure with
    /// two bounded radix-tree probes and one borrowed closure lookup. The
    /// publication recovery path uses this paired witness so it never builds
    /// the compatibility object export or traverses unrelated generations.
    pub(crate) fn selected_contains_objects(&self, output: [u8; 32], manifest: [u8; 32]) -> bool {
        let closure = self.head.closure().manifest();
        let Ok(output) = self
            .store
            .read_object_claim(UntrustedObjectId::from_bytes(output))
        else {
            return false;
        };
        let Ok(manifest) = self
            .store
            .read_object_claim(UntrustedObjectId::from_bytes(manifest))
        else {
            return false;
        };
        closure.contains_object_id(output.id()) && closure.contains_object_id(manifest.id())
    }
}
