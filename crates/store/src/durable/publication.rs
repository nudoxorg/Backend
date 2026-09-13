//! Checked publication typestate and workspace composition.

use super::nodes::TreePublication;
use super::{
    ClosureManifest, FileStore, LayoutId, PREPARED_TAG, PUBLISHED_TAG, Pack, PackId,
    PublicationBase, PublicationDescriptor, RawRelation, SelectedHead, StateRoot, StoreError,
    StorePublicationAuthority, WorkspaceClosure, descriptor_matches_base,
};
use backend_version::WorkspaceRoot;

/// A validated publication that has not written any durable state.
#[derive(Clone, Debug)]
pub struct FilePrepared {
    pub(super) store: FileStore,
    pub(super) pack: Option<Pack>,
    pub(super) tree: Option<TreePublication>,
    pub(super) target: StateRoot<RawRelation>,
    pub(super) closure: ClosureManifest,
    pub(super) workspace: Option<WorkspaceClosure>,
    pub(super) descriptor: PublicationDescriptor,
}

/// A publication whose immutable objects and prepared journal record are synced.
#[derive(Clone, Debug)]
pub struct FileDurable {
    pub(super) store: FileStore,
    pub(super) descriptor: PublicationDescriptor,
    pub(super) target: StateRoot<RawRelation>,
    pub(super) prepared_sequence: u64,
}

/// A publication whose target is selected by the durable head.
#[derive(Clone, Debug)]
pub struct FilePublished {
    pub(super) head: SelectedHead,
    pub(super) target: StateRoot<RawRelation>,
}

/// A checked workspace-root publication that has not written durable state.
#[derive(Clone, Debug)]
pub struct WorkspaceFilePrepared {
    pub(super) store: FileStore,
    pub(super) closure: WorkspaceClosure,
    pub(super) descriptor: PublicationDescriptor,
}

/// A workspace publication whose closure and prepare frame are durable.
#[derive(Clone, Debug)]
pub struct WorkspaceFileDurable {
    pub(super) store: FileStore,
    pub(super) descriptor: PublicationDescriptor,
    pub(super) target: WorkspaceRoot,
    pub(super) prepared_sequence: u64,
}

/// A workspace publication whose typed root is selected durably.
#[derive(Clone, Debug)]
pub struct WorkspaceFilePublished {
    pub(super) head: SelectedHead,
    pub(super) target: WorkspaceRoot,
}

/// Checked backend-version evidence and physical identities for one workspace
/// publication.  The borrowed evidence keeps the version crate's checked
/// capabilities attached to the admission call without copying or weakening
/// them into raw bytes.
#[derive(Clone)]
pub struct CheckedWorkspacePublication<'a> {
    pub(super) target: &'a backend_version::WorkspaceManifest,
    pub(super) transition: Option<&'a backend_version::CheckedWorkspaceTransition>,
    pub(super) commit: Option<&'a backend_version::CheckedCommit>,
    pub(super) objects: ClosureManifest,
    pub(super) layout: LayoutId,
    pub(super) pack: PackId,
    pub(super) base: Option<PublicationBase>,
    pub(super) root_only: bool,
}

impl<'a> CheckedWorkspacePublication<'a> {
    /// Starts a publication from a checked target manifest.
    #[must_use]
    pub fn new(
        target: &'a backend_version::WorkspaceManifest,
        objects: ClosureManifest,
        layout: LayoutId,
        pack: PackId,
        base: Option<PublicationBase>,
    ) -> Self {
        Self {
            target,
            transition: None,
            commit: None,
            objects,
            layout,
            pack,
            base,
            root_only: false,
        }
    }

    /// Adds the checked transition and optional checked commit that produced
    /// the target manifest.
    #[must_use]
    pub fn with_transition(
        mut self,
        transition: &'a backend_version::CheckedWorkspaceTransition,
        commit: Option<&'a backend_version::CheckedCommit>,
    ) -> Self {
        self.transition = Some(transition);
        self.commit = commit;
        self
    }

    /// Preserves the root-only relation frontier of the checked source
    /// closure. The store re-derives the selected roots from the manifest's
    /// typed references before publication.
    #[must_use]
    pub fn with_root_only(mut self) -> Self {
        self.root_only = true;
        self
    }
}

impl FilePrepared {
    /// Returns the exact immutable identities this transaction will publish.
    #[must_use]
    pub const fn descriptor(&self) -> PublicationDescriptor {
        self.descriptor
    }

    /// Writes immutable objects and the exact prepared journal frame.
    ///
    /// # Errors
    ///
    /// Returns an error when an immutable object, closure, or journal frame
    /// cannot be admitted and synced.
    pub fn durable(self) -> Result<FileDurable, StoreError> {
        let FilePrepared {
            store,
            pack,
            tree,
            target,
            closure,
            workspace,
            descriptor,
        } = self;
        let prepared_sequence = {
            let _guard = store.lock.lock().map_err(|_| StoreError::Corrupt)?;
            let _process_lock = store.acquire_process_lock()?;
            match (pack.as_ref(), tree.as_ref()) {
                (Some(pack), None) => {
                    store.write_pack(pack)?;
                }
                (None, Some(tree)) => {
                    store.write_tree_publication(tree)?;
                    store.write_root_closure(&closure)?;
                }
                _ => return Err(StoreError::Corrupt),
            }
            if pack.is_some() {
                store.write_closure(&closure)?;
            }
            if let Some(workspace) = workspace.as_ref()
                && (workspace.manifest().id() != descriptor.closure
                    || workspace.binding().closure() != descriptor.closure)
            {
                return Err(StoreError::Corrupt);
            }
            store.append_record(PREPARED_TAG, &descriptor, 0)?
        };
        Ok(FileDurable {
            store,
            descriptor,
            target,
            prepared_sequence,
        })
    }
}

impl FileDurable {
    /// Returns the exact immutable identities this transaction will publish.
    #[must_use]
    pub const fn descriptor(&self) -> PublicationDescriptor {
        self.descriptor
    }

    /// Appends the paired publish frame and atomically advances the selected head.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::StaleHead`] when the observed base is no longer
    /// selected, or a filesystem/corruption error when publication cannot be
    /// completed.
    pub fn publish(self) -> Result<FilePublished, StoreError> {
        let authority = self
            .store
            .acquire_publication_authority()
            .map_err(map_publication_authority_error)?;
        self.publish_inner(&authority, || Ok::<(), ()>(()))
            .and_then(|result| result.map_err(|()| StoreError::Corrupt))
    }

    /// Appends the paired publish frame and atomically advances the selected
    /// head while holding the owner's kernel authority.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::StaleHead`] when the observed base is no longer
    /// selected, or a filesystem/corruption error when publication cannot be
    /// completed.
    pub fn publish_with_authority(
        self,
        authority: &StorePublicationAuthority,
    ) -> Result<FilePublished, StoreError> {
        self.publish_inner(authority, || Ok::<(), ()>(()))
            .and_then(|result| result.map_err(|()| StoreError::Corrupt))
    }

    /// Appends the paired publish frame and invokes `before_head_write` after
    /// the journal frame is durable but before the selected-head receipt is
    /// written. The callback error is returned as the inner result.
    ///
    /// # Errors
    ///
    /// The outer result reports store failures. A callback error means the
    /// journal publish frame may be present while the HEAD receipt has not
    /// yet been attempted.
    pub fn publish_with_authority_before_head<E, F>(
        self,
        authority: &StorePublicationAuthority,
        before_head_write: F,
    ) -> Result<Result<FilePublished, E>, StoreError>
    where
        F: FnOnce() -> Result<(), E>,
    {
        self.publish_inner(authority, before_head_write)
    }

    fn publish_inner<E, F>(
        self,
        authority: &StorePublicationAuthority,
        before_head_write: F,
    ) -> Result<Result<FilePublished, E>, StoreError>
    where
        F: FnOnce() -> Result<(), E>,
    {
        if !authority.permits(&self.store.root) {
            return Err(StoreError::Corrupt);
        }
        let _guard = self.store.lock.lock().map_err(|_| StoreError::Corrupt)?;
        let _process_lock = self.store.acquire_process_lock()?;
        let state = self.store.read_state()?;
        let current = state.selected.map(|head| head.descriptor);
        if !descriptor_matches_base(current.as_ref(), &self.descriptor) {
            return Err(StoreError::StaleHead);
        }
        let sequence =
            self.store
                .append_record(PUBLISHED_TAG, &self.descriptor, self.prepared_sequence)?;
        let head = SelectedHead {
            journal_sequence: sequence,
            descriptor: self.descriptor,
        };
        if let Err(error) = before_head_write() {
            return Ok(Err(error));
        }
        if let Err(error) = self.store.write_head(&head) {
            return Err(self.store.published_sync_error(&head, error));
        }
        Ok(Ok(FilePublished {
            head,
            target: self.target,
        }))
    }
}

impl FilePublished {
    /// Returns the selected target root.
    #[must_use]
    pub const fn root(&self) -> StateRoot<RawRelation> {
        self.target
    }

    /// Returns the selected head and all of its bindings.
    #[must_use]
    pub const fn head(&self) -> SelectedHead {
        self.head
    }

    /// Returns the exact publication descriptor.
    #[must_use]
    pub const fn descriptor(&self) -> PublicationDescriptor {
        self.head.descriptor
    }
}

impl WorkspaceFilePrepared {
    /// Returns the exact identities bound by this workspace publication.
    #[must_use]
    pub const fn descriptor(&self) -> PublicationDescriptor {
        self.descriptor
    }

    /// Writes the complete typed closure and the prepared journal frame.
    ///
    /// # Errors
    ///
    /// Returns an admission, bounds, corruption, or filesystem error.
    pub fn durable(self) -> Result<WorkspaceFileDurable, StoreError> {
        let WorkspaceFilePrepared {
            store,
            closure,
            descriptor,
        } = self;
        let prepared_sequence = {
            let _guard = store.lock.lock().map_err(|_| StoreError::Corrupt)?;
            let _process_lock = store.acquire_process_lock()?;
            if closure.is_root_only() {
                store.write_workspace_root_closure(&closure)?;
            } else {
                store.write_closure(closure.manifest())?;
            }
            store.append_record(PREPARED_TAG, &descriptor, 0)?
        };
        Ok(WorkspaceFileDurable {
            store,
            descriptor,
            target: closure.root(),
            prepared_sequence,
        })
    }
}

impl WorkspaceFileDurable {
    /// Returns the exact identities bound by this workspace publication.
    #[must_use]
    pub const fn descriptor(&self) -> PublicationDescriptor {
        self.descriptor
    }

    /// Appends the paired publish frame and selects the checked workspace root
    /// while holding the owner's kernel authority.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::StaleHead`] when the observed base is no longer
    /// selected and a filesystem, authority, or corruption error during
    /// publication.
    pub fn publish_with_authority(
        self,
        authority: &StorePublicationAuthority,
    ) -> Result<WorkspaceFilePublished, StoreError> {
        self.publish_inner(authority, || Ok::<(), ()>(()))
            .and_then(|result| result.map_err(|()| StoreError::Corrupt))
    }

    /// Appends the paired publish frame and invokes `before_head_write` after
    /// the journal frame is durable but before the selected-head receipt is
    /// written. The callback error is returned as the inner result.
    ///
    /// # Errors
    ///
    /// The outer result reports store failures. A callback error means the
    /// journal publish frame may be present while the HEAD receipt has not
    /// yet been attempted.
    pub fn publish_with_authority_before_head<E, F>(
        self,
        authority: &StorePublicationAuthority,
        before_head_write: F,
    ) -> Result<Result<WorkspaceFilePublished, E>, StoreError>
    where
        F: FnOnce() -> Result<(), E>,
    {
        self.publish_inner(authority, before_head_write)
    }

    fn publish_inner<E, F>(
        self,
        authority: &StorePublicationAuthority,
        before_head_write: F,
    ) -> Result<Result<WorkspaceFilePublished, E>, StoreError>
    where
        F: FnOnce() -> Result<(), E>,
    {
        if !authority.permits(&self.store.root) {
            return Err(StoreError::Corrupt);
        }
        let _guard = self.store.lock.lock().map_err(|_| StoreError::Corrupt)?;
        let _process_lock = self.store.acquire_process_lock()?;
        let state = self.store.read_state()?;
        let current = state.selected.map(|head| head.descriptor);
        if !descriptor_matches_base(current.as_ref(), &self.descriptor) {
            return Err(StoreError::StaleHead);
        }
        let sequence =
            self.store
                .append_record(PUBLISHED_TAG, &self.descriptor, self.prepared_sequence)?;
        let head = SelectedHead {
            journal_sequence: sequence,
            descriptor: self.descriptor,
        };
        if let Err(error) = before_head_write() {
            return Ok(Err(error));
        }
        if let Err(error) = self.store.write_head(&head) {
            return Err(self.store.published_sync_error(&head, error));
        }
        Ok(Ok(WorkspaceFilePublished {
            head,
            target: self.target,
        }))
    }
}

fn map_publication_authority_error(error: super::PublicationAuthorityError) -> StoreError {
    match error {
        super::PublicationAuthorityError::Busy => StoreError::PublicationAuthorityBusy,
        super::PublicationAuthorityError::Io(error) => StoreError::Io(error),
    }
}

impl WorkspaceFilePublished {
    /// Returns the checked typed workspace root selected by this publication.
    #[must_use]
    pub const fn root(&self) -> WorkspaceRoot {
        self.target
    }

    /// Returns the selected durable head.
    #[must_use]
    pub const fn head(&self) -> SelectedHead {
        self.head
    }

    /// Returns the exact publication descriptor.
    #[must_use]
    pub const fn descriptor(&self) -> PublicationDescriptor {
        self.head.descriptor
    }
}
