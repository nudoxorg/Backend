//! Bounded physical-root pins and store collection.

use super::{
    Arc, GcLimits, GcReport, GcRoot, GcRoots, MAX_OWNER_GC_PINS, ObjectId, TypedObject,
    WorkspaceError, WorkspaceGcPin, WorkspaceModel, WorkspaceOwner,
};
use std::sync::atomic::Ordering;

impl<M: WorkspaceModel> WorkspaceOwner<M> {
    /// Admits and stores one immutable object for a later checked publication.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn write_object(&self, object: &TypedObject) -> Result<ObjectId, WorkspaceError> {
        self.lease.assert_current()?;
        self.store
            .write_object(object)
            .map_err(WorkspaceError::store)
    }

    /// Checks whether an admitted immutable object is present in the owner
    /// store without exposing a mutable or publication capable store handle.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn contains_object(&self, object: ObjectId) -> Result<bool, WorkspaceError> {
        self.lease.assert_current()?;
        self.store
            .contains_object(object)
            .map_err(WorkspaceError::store)
    }

    /// Stores one pack admitted by the daemon's replication boundary.
    ///
    /// Pack storage is staging only; selecting a pack still requires an
    /// owner-issued checked workspace publication.
    pub(crate) fn write_admitted_pack(
        &self,
        pack: &backend_store::Pack,
    ) -> Result<backend_store::PackId, WorkspaceError> {
        self.lease.assert_current()?;
        self.store.write_pack(pack).map_err(WorkspaceError::store)
    }

    /// Retains one checked physical root until the returned token is dropped.
    /// The owner refuses collection if this registry is poisoned or full, so
    /// a partial live-root snapshot can never authorize deletion.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn pin_gc_root(&self, root: GcRoot) -> Result<WorkspaceGcPin, WorkspaceError> {
        self.lease.assert_current()?;
        let id = self
            .next_gc_pin
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .map_err(|_| WorkspaceError::Bounds)?;
        let mut roots = self
            .gc_roots
            .lock()
            .map_err(|_| WorkspaceError::Corrupt("GC root registry"))?;
        if roots.len() >= MAX_OWNER_GC_PINS {
            return Err(WorkspaceError::Bounds);
        }
        roots.insert(id, root);
        Ok(WorkspaceGcPin {
            id,
            roots: Arc::clone(&self.gc_roots),
        })
    }

    /// Retains a reader or subscription root for the next complete GC pass.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn pin_reader_root(&self, root: GcRoot) -> Result<WorkspaceGcPin, WorkspaceError> {
        self.pin_gc_root(root)
    }

    /// Retains a view or subscription root.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn pin_view_root(&self, root: GcRoot) -> Result<WorkspaceGcPin, WorkspaceError> {
        self.pin_gc_root(root)
    }

    /// Retains a subscription root.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn pin_subscription_root(&self, root: GcRoot) -> Result<WorkspaceGcPin, WorkspaceError> {
        self.pin_gc_root(root)
    }

    /// Retains an in-flight dispatch input or output root.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn pin_pending_dispatch_root(
        &self,
        root: GcRoot,
    ) -> Result<WorkspaceGcPin, WorkspaceError> {
        self.pin_gc_root(root)
    }

    /// Retains a transfer or replication lease root.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn pin_transfer_root(&self, root: GcRoot) -> Result<WorkspaceGcPin, WorkspaceError> {
        self.pin_gc_root(root)
    }

    /// Retains a durable checkpoint root.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn pin_checkpoint_root(&self, root: GcRoot) -> Result<WorkspaceGcPin, WorkspaceError> {
        self.pin_gc_root(root)
    }

    /// Retains a derived-output/catalog payload root.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn pin_derived_output_root(&self, root: GcRoot) -> Result<WorkspaceGcPin, WorkspaceError> {
        self.pin_gc_root(root)
    }

    /// Retains an arrangement or layout root.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn pin_layout_root(&self, root: GcRoot) -> Result<WorkspaceGcPin, WorkspaceError> {
        self.pin_gc_root(root)
    }

    /// Retains a replication claim root.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn pin_replication_root(&self, root: GcRoot) -> Result<WorkspaceGcPin, WorkspaceError> {
        self.pin_gc_root(root)
    }

    /// Collects unreachable immutable store files from one atomically checked
    /// owner snapshot. The selected store head, all live engine pins, and the
    /// authenticated derived-output catalog are captured before collection;
    /// a head change or poisoned root registry fails closed.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn collect_garbage(&self, limits: GcLimits) -> Result<GcReport, WorkspaceError> {
        self.lease.assert_current()?;
        let selected = self.store.head().map_err(WorkspaceError::store)?;
        let mut roots = GcRoots::new();
        match selected {
            Some(head) => {
                let descriptor = head.descriptor();
                if descriptor.target() != self.head.root().to_bytes()
                    || descriptor.target_generation() != self.head.sequence()
                    || descriptor.closure().as_bytes()
                        != self.head.closure().manifest().id().as_bytes()
                {
                    return Err(WorkspaceError::HeadConflict);
                }
                roots.add_selected_head(head);
            }
            None if self.head.sequence() != 0 => {
                return Err(WorkspaceError::Corrupt("owner head is not selected"));
            }
            None => {}
        }
        // Keep the registry guard through the store collection. A new reader
        // pin cannot become live between this snapshot and the first mark
        // page, and dropping an existing pin waits until collection finishes.
        let pinned = self
            .gc_roots
            .lock()
            .map_err(|_| WorkspaceError::Corrupt("GC root registry"))?;
        if pinned.len() > MAX_OWNER_GC_PINS {
            return Err(WorkspaceError::Bounds);
        }
        for &root in pinned.values() {
            roots.add(root);
        }
        roots.extend(crate::workspace::catalog::catalog_gc_roots(
            &self.store,
            &self.catalog,
        )?);
        self.store
            .collect_garbage(&roots, limits)
            .map_err(WorkspaceError::store)
    }
}
