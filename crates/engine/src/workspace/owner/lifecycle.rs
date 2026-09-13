//! Owner construction, restart binding, and public snapshots.

use super::{
    Arc, AtomicU64, BTreeMap, CatalogState, Faults, FileStore, JOURNAL_FILE, JournalLimits,
    MAX_JOURNAL_BYTES, MAX_JOURNAL_FRAMES, MAX_STORE_BYTES, Mutex, OwnerLease, Path, PathBuf,
    RelationAdmissionRegistry, WorkspaceError, WorkspaceHead, WorkspaceModel, WorkspaceOwner,
    WorkspaceSnapshot, open_diagnostic_journal, recover_store_head,
};
use crate::workspace::catalog::state_from_durable_manifest;
use backend_store::ClosureManifest;

impl<M: WorkspaceModel> WorkspaceOwner<M> {
    /// Opens a workspace, acquiring its owner lease and recovering the store
    /// selected head through the model's checked admission seam.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn open(
        directory: impl AsRef<Path>,
        model: M,
        genesis: WorkspaceHead,
    ) -> Result<Self, WorkspaceError> {
        Self::open_with_faults_and_registry(
            directory,
            model,
            genesis,
            Arc::new(Faults::default()),
            RelationAdmissionRegistry::default(),
        )
    }

    /// Opens the owner with an explicit relation decoder registry. Product
    /// models that persist a custom canonical relation must use this entry
    /// point so recovery validates the same typed bytes as publication.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn open_with_registry(
        directory: impl AsRef<Path>,
        model: M,
        genesis: WorkspaceHead,
        relation_registry: RelationAdmissionRegistry,
    ) -> Result<Self, WorkspaceError> {
        Self::open_with_faults_and_registry(
            directory,
            model,
            genesis,
            Arc::new(Faults::default()),
            relation_registry,
        )
    }

    /// Opens the owner with explicit production fault seams.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn open_with_faults(
        directory: impl AsRef<Path>,
        model: M,
        genesis: WorkspaceHead,
        faults: Arc<Faults>,
    ) -> Result<Self, WorkspaceError> {
        Self::open_with_faults_and_registry(
            directory,
            model,
            genesis,
            faults,
            RelationAdmissionRegistry::default(),
        )
    }

    /// Opens the owner with explicit fault hooks and relation decoders.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn open_with_faults_and_registry(
        directory: impl AsRef<Path>,
        model: M,
        genesis: WorkspaceHead,
        faults: Arc<Faults>,
        relation_registry: RelationAdmissionRegistry,
    ) -> Result<Self, WorkspaceError> {
        let directory = directory.as_ref().to_owned();
        let lease = OwnerLease::acquire(&directory)?;
        let opened =
            Self::open_with_lease(directory, model, &genesis, faults, lease, relation_registry);
        let _owned = genesis;
        opened
    }

    /// Acquires the kernel lease after a previous owner has released it and
    /// opens the owner under the newly fenced epoch. A live owner's lock is
    /// never renamed or guessed stale by this recovery path.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn reclaim(
        directory: impl AsRef<Path>,
        model: M,
        genesis: WorkspaceHead,
    ) -> Result<Self, WorkspaceError> {
        let directory = directory.as_ref().to_owned();
        let lease = OwnerLease::reclaim(&directory)?;
        let opened = Self::open_with_lease(
            directory,
            model,
            &genesis,
            Arc::new(Faults::default()),
            lease,
            RelationAdmissionRegistry::default(),
        );
        let _owned = genesis;
        opened
    }

    fn open_with_lease(
        directory: PathBuf,
        model: M,
        genesis: &WorkspaceHead,
        faults: Arc<Faults>,
        lease: OwnerLease,
        relation_registry: RelationAdmissionRegistry,
    ) -> Result<Self, WorkspaceError> {
        // The lower store owns the only authoritative workspace HEAD.  Its
        // journal and receipt live below `objects/`; the engine journal below
        // is a diagnostic/effect log and is never allowed to select a root.
        let relation_registry = relation_registry
            .with_relation::<crate::workspace::catalog::CatalogPrimaryRelation>()
            .map_err(WorkspaceError::store)?
            .with_relation::<crate::workspace::catalog::CatalogFreshnessRelation>()
            .map_err(WorkspaceError::store)?
            .with_relation::<crate::workspace::catalog::CatalogPayloadRefRelation>()
            .map_err(WorkspaceError::store)?;
        let store = Arc::new(
            FileStore::open_with_registry(
                directory.join("objects"),
                MAX_STORE_BYTES,
                relation_registry,
            )
            .map_err(WorkspaceError::store)?,
        );
        let journal_path = directory.join(JOURNAL_FILE);
        let limits = JournalLimits {
            max_frames: MAX_JOURNAL_FRAMES,
            max_bytes: MAX_JOURNAL_BYTES,
        };
        let head = recover_store_head(&store, &model, genesis, lease.epoch(), &faults)?;
        if store.head().map_err(WorkspaceError::store)?.is_none() {
            // An unselected genesis is still exposed through a lazy snapshot.
            // Materialize its checked closure before that snapshot can perform
            // a relation read. The immutable CAS writes are idempotent and
            // may be retried after a crash without selecting a physical HEAD.
            lease.assert_current()?;
            seed_unselected_genesis(&store, &head)?;
            lease.assert_current()?;
        }
        let catalog = if let Some(descriptor_id) = head.catalog_descriptor() {
            let durable_manifest = store
                .open_closure(head.closure().manifest().id())
                .map_err(WorkspaceError::store)?;
            state_from_durable_manifest(
                &store,
                &durable_manifest,
                descriptor_id,
                head.manifest().coverage(),
            )?
        } else {
            // A v3 workspace pack carries the descriptor identity whenever a
            // derived catalog exists.  An absent pointer therefore means an
            // empty catalog; do not fall back to materializing/scanning the
            // selected closure just to rediscover that fact.
            CatalogState::empty(head.manifest().coverage())
        };
        let journal = open_diagnostic_journal(&journal_path, &directory, lease.epoch(), limits)?;
        Ok(Self {
            model,
            directory,
            lease,
            store,
            journal,
            head,
            catalog,
            faults,
            gc_roots: Arc::new(Mutex::new(BTreeMap::new())),
            next_gc_pin: AtomicU64::new(0),
        })
    }

    /// Returns fault hooks used by production ordering tests.
    #[must_use]
    pub fn faults(&self) -> Arc<Faults> {
        Arc::clone(&self.faults)
    }

    /// Returns the current checked head.
    #[must_use]
    pub const fn head(&self) -> &WorkspaceHead {
        &self.head
    }

    /// Returns a checked immutable snapshot.
    #[must_use]
    pub fn snapshot(&self) -> WorkspaceSnapshot {
        self.head.snapshot().with_store(Arc::clone(&self.store))
    }

    /// Returns the current owner lease.
    #[must_use]
    pub const fn lease(&self) -> &OwnerLease {
        &self.lease
    }
}

fn seed_unselected_genesis(
    store: &FileStore,
    genesis: &WorkspaceHead,
) -> Result<(), WorkspaceError> {
    let manifest = ClosureManifest::new_with_registry(
        genesis.closure().manifest().objects().to_vec(),
        store.relation_registry(),
    )
    .map_err(WorkspaceError::store)?;
    store
        .write_closure(&manifest)
        .map_err(WorkspaceError::store)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_execution::AuthorityVersion;
    use backend_store::{ClosureManifest, TypedObject, WorkspaceClosure};
    use backend_version::ObjectKey;
    use std::error::Error;

    #[derive(Clone, Copy, Debug, Default)]
    struct ColdStartModel;

    #[derive(Debug)]
    struct ColdStartError;

    impl std::fmt::Display for ColdStartError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("cold-start model is not used by this regression")
        }
    }

    impl Error for ColdStartError {}

    impl WorkspaceModel for ColdStartModel {
        type Intent = Vec<u8>;
        type Error = ColdStartError;

        fn request_id(&self, _intent: &Self::Intent) -> [u8; 32] {
            [0; 32]
        }

        fn prepare(
            &self,
            _base: &WorkspaceSnapshot,
            _intent: &Self::Intent,
            _transaction: crate::workspace::owner::TransactionId,
        ) -> Result<crate::workspace::owner::PreparedTransition, Self::Error> {
            Err(ColdStartError)
        }

        fn admit_persisted(
            &self,
            _persisted: &crate::workspace::transition::PersistedTransition,
        ) -> Result<crate::workspace::owner::PreparedTransition, Self::Error> {
            Err(ColdStartError)
        }
    }

    fn product_genesis() -> (WorkspaceHead, RelationAdmissionRegistry) {
        let descriptor = crate::builtin::profile_descriptor(crate::builtin::Profile::Product)
            .expect("product profile descriptor");
        let authority: AuthorityVersion = descriptor.ids.authority;
        let relation = crate::builtin::product_source_fixture_with_authority(false, authority)
            .expect("product source relation");
        let manifest = crate::builtin::execution_input_manifest(&relation, authority)
            .expect("product input manifest");
        let registry = RelationAdmissionRegistry::default()
            .with_relation::<crate::builtin::ProductSourceRelation>()
            .expect("product source relation registry");
        let relation_object =
            TypedObject::from_relation_state(&relation).expect("product relation object");
        let authority_key = ObjectKey::<backend_execution::AuthorityVersionSchema>::from_value(
            crate::builtin::PRODUCT_AUTHORITY_BYTES,
        );
        let authority_object =
            TypedObject::from_value(&authority_key, crate::builtin::PRODUCT_AUTHORITY_BYTES);
        let mut objects = vec![relation_object, authority_object];
        objects.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
        let closure_manifest =
            ClosureManifest::new_with_registry(objects, &registry).expect("genesis closure");
        let closure = WorkspaceClosure::from_checked_manifest_root_only_with_registry(
            &manifest,
            closure_manifest,
            &registry,
        )
        .expect("admit genesis closure");
        let head = WorkspaceHead::genesis_with_registry(manifest, closure, &registry)
            .expect("product genesis");
        (head, registry)
    }

    #[test]
    fn empty_owner_seeds_custom_genesis_for_lazy_read_and_reopen() {
        let (genesis, registry) = product_genesis();
        let directory = std::env::temp_dir().join(format!(
            "backend-engine-owner-genesis-{}-{}",
            std::process::id(),
            genesis.root().to_bytes()[..8]
                .iter()
                .fold(0_u64, |value, byte| value * 256 + u64::from(*byte)),
        ));
        let _ = std::fs::remove_dir_all(&directory);

        let owner = WorkspaceOwner::open_with_registry(
            &directory,
            ColdStartModel,
            genesis.clone(),
            registry.clone(),
        )
        .expect("open empty owner");
        let key = backend_library::package_key("backend-builtin").to_bytes();
        let relation = owner
            .snapshot()
            .lazy_relation::<crate::builtin::ProductSourceRelation>()
            .expect("open lazily seeded relation");
        assert!(
            relation
                .lookup(&key)
                .expect("read seeded relation")
                .is_some()
        );
        drop(owner);

        let reopened =
            WorkspaceOwner::open_with_registry(&directory, ColdStartModel, genesis, registry)
                .expect("reopen unselected genesis");
        let relation = reopened
            .snapshot()
            .lazy_relation::<crate::builtin::ProductSourceRelation>()
            .expect("reopen lazily seeded relation");
        assert!(
            relation
                .lookup(&key)
                .expect("read reopened relation")
                .is_some()
        );

        drop(reopened);
        std::fs::remove_dir_all(directory).expect("remove owner fixture");
    }
}
