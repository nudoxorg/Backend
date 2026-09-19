//! Durable workspace admission and restart binding.

use super::{
    BoundedFileImage, CheckedWorkspaceManifest, File, InputCas, ReplicationError, Schema,
    UntrustedWorkspaceManifest, WorkspaceRoot, Write, fs, hex,
};

impl<T: Schema> InputCas<T> {
    /// Records the exact workspace closure that authenticated the current
    /// input set. The marker is replaced atomically and fsynced before it is
    /// visible, so a reconnect can safely reuse the immutable object index.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn record_workspace(&mut self, workspace: WorkspaceRoot) -> Result<(), ReplicationError> {
        let Some(root) = self.sink.root.as_ref() else {
            self.admitted_workspace = Some(workspace);
            return Ok(());
        };
        let temporary = root.join(".WORKSPACE.part");
        let target = root.join("WORKSPACE");
        let mut file = File::create(&temporary).map_err(|_| ReplicationError::Disconnected)?;
        file.write_all(workspace.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|_| ReplicationError::Disconnected)?;
        fs::rename(&temporary, &target).map_err(|_| ReplicationError::Disconnected)?;
        let directory = backend_platform::durability::open_directory(root)
            .map_err(|_| ReplicationError::Disconnected)?;
        directory
            .sync_all()
            .map_err(|_| ReplicationError::Disconnected)?;
        self.admitted_workspace = Some(workspace);
        Ok(())
    }

    /// Returns the last authenticated workspace closure, if one is durable.
    #[must_use]
    pub const fn admitted_workspace(&self) -> Option<WorkspaceRoot> {
        self.admitted_workspace
    }

    /// Returns the last authenticated workspace claim without manufacturing a
    /// typed root from wire bytes. This is the reconnect index used by the
    /// closure protocol; execution still receives a typed workspace root from
    /// its checked request path.
    #[must_use]
    pub const fn admitted_workspace_claim(&self) -> Option<[u8; 32]> {
        self.admitted_workspace_claim
    }

    /// Checks that a reconnect offer carries the exact manifest evidence that
    /// was durably committed with the warm root.
    #[must_use]
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn workspace_manifest_matches(&self, bytes: &[u8]) -> bool {
        self.workspace_manifest_bytes
            .as_deref()
            .is_some_and(|stored| stored == bytes)
    }

    /// Persists a workspace claim whose exact bytes were authenticated by the
    /// closure offer/page exchange.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn record_workspace_claim(&mut self, workspace: [u8; 32]) -> Result<(), ReplicationError> {
        let Some(root) = self.sink.root.as_ref() else {
            self.admitted_workspace_claim = Some(workspace);
            return Ok(());
        };
        let temporary = root.join(".WORKSPACE.part");
        let target = root.join("WORKSPACE");
        let mut file = File::create(&temporary).map_err(|_| ReplicationError::Disconnected)?;
        file.write_all(&workspace)
            .and_then(|()| file.sync_all())
            .map_err(|_| ReplicationError::Disconnected)?;
        fs::rename(&temporary, &target).map_err(|_| ReplicationError::Disconnected)?;
        backend_platform::durability::open_directory(root)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| ReplicationError::Disconnected)?;
        self.admitted_workspace_claim = Some(workspace);
        Ok(())
    }

    /// Publishes the canonical checked workspace evidence after all relation
    /// proofs and immutable inputs are durable. The marker contains the
    /// version-owned manifest bytes; the typed root is derived only from the
    /// checked value supplied by the admission state machine.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn record_workspace_manifest(
        &mut self,
        manifest: &CheckedWorkspaceManifest,
    ) -> Result<(), ReplicationError> {
        manifest
            .validate()
            .map_err(|_| ReplicationError::IdentityMismatch)?;
        let Some(directory) = self.sink.root.as_ref() else {
            self.workspace_manifest_bytes = Some(manifest.encode());
            self.admitted_workspace = Some(manifest.root());
            self.admitted_workspace_claim = Some(*manifest.root().as_bytes());
            return Ok(());
        };
        let bytes = manifest.encode();
        let temporary = directory.join(".WORKSPACE_MANIFEST.part");
        let target = directory.join("WORKSPACE_MANIFEST");
        let mut file = File::create(&temporary).map_err(|_| ReplicationError::Disconnected)?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| ReplicationError::Disconnected)?;
        fs::rename(&temporary, &target).map_err(|_| ReplicationError::Disconnected)?;
        self.record_workspace_claim(*manifest.root().as_bytes())?;
        self.workspace_manifest_bytes = Some(bytes);
        self.admitted_workspace = Some(manifest.root());
        Ok(())
    }

    /// Reopens checked workspace evidence after process restart. The relation
    /// proof is loaded directly by digest and rebound through the version
    /// `PersistedTreeRoot` seam; no workspace root is reconstructed from raw
    /// bytes.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn reopen_workspace(
        &mut self,
        authority: backend_engine::AuthorityVersion,
        admitted_authority: &backend_engine::AdmittedAuthority,
    ) -> Result<(), ReplicationError> {
        let Some(directory) = self.sink.root.as_ref() else {
            return Ok(());
        };
        let bytes = match BoundedFileImage::read_optional(
            &directory.join("WORKSPACE_MANIFEST"),
            self.limits.max_frame,
        )? {
            Some(bytes) => bytes,
            None => return Ok(()),
        };
        let manifest = UntrustedWorkspaceManifest::decode_untrusted(bytes.as_slice())
            .map_err(|_| ReplicationError::CorruptFrame)?;
        if manifest.relations().len() != 1 || !manifest.basis().is_empty() {
            return Err(ReplicationError::IdentityMismatch);
        }
        let relation_digest = manifest.relations()[0].root();
        let relation_claim =
            backend_engine::UntrustedId::<
                backend_engine::ProductSemanticPublicationRelation,
            >::from_wire(
                &relation_digest,
                backend_engine::IdContext::relation::<
                    backend_engine::ProductSemanticPublicationRelation,
                >(),
            )
            .map_err(|_| ReplicationError::CorruptFrame)?;
        let proof = BoundedFileImage::read_optional(
            &directory.join(format!(".proof-{}", hex(relation_digest))),
            64 * 1024,
        )?
        .ok_or(ReplicationError::Disconnected)?;
        let persisted = backend_engine::PersistedTreeRoot::<
            backend_engine::ProductSemanticPublicationRelation,
        >::admit(relation_claim, proof.as_slice())
        .map_err(|_| ReplicationError::CorruptFrame)?;
        let coverage =
            backend_engine::coverage_from_admitted_authority(authority, admitted_authority)
                .map_err(|_| ReplicationError::IdentityMismatch)?;
        let binding = backend_engine::RelationBinding::from_persisted_root(&persisted, coverage);
        let checked = manifest
            .admit_checked(
                vec![binding],
                Vec::new(),
                backend_engine::ObjectClosure::from_version(authority),
                coverage,
            )
            .map_err(|_| ReplicationError::IdentityMismatch)?;
        if self.admitted_workspace_claim != Some(*checked.root().as_bytes()) {
            return Err(ReplicationError::IdentityMismatch);
        }
        self.admitted_workspace = Some(checked.root());
        Ok(())
    }
}
