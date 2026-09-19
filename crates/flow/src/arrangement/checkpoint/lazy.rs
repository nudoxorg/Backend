//! Checked lazy relation-root handoff for checkpoint adapters.
//!
//! The flow crate does not own a storage implementation.  This module keeps
//! the narrow capability boundary needed by a checkpoint reader: a caller
//! supplies a loader that admits one canonical node for one typed claim, and
//! flow supplies the O(1) persisted-root handoff plus path-copy facade.

use crate::{
    ArrangementCanonicalRelation, ArrangementKey, ArrangementRoot, CanonicalValue, ObjectIdentity,
    RelationIdentity, RowKey,
};
use backend_store::{FileStore, StoreError};
use backend_version::{
    CanonicalRelation, CanonicalRootAdmissionError, ChildCommitment, CommittedChild, IdContext,
    IdDecodeError, LazyPreparedUpdate, LazyTree, LazyTreeError, LazyTreeWork, PersistedTreeRoot,
    Schema, StateRoot, TreeNodeLoader, UntrustedId, admit_canonical_root_claim,
    canonical_branch_from_commitments, canonical_empty, canonical_leaf,
};
use std::{
    borrow::Borrow,
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

/// Creates an untrusted relation-root claim for an arrangement root.
///
/// The claim is only a typed wire label.  Callers must still pass the exact
/// canonical root bytes to [`LazyCheckpointRoot::open`], which performs the
/// digest and grammar admission before any node is trusted.
///
/// # Errors
///
/// Returns [`IdDecodeError`] when the root bytes cannot be represented as a
/// relation claim under the arrangement relation context.
pub fn arrangement_root_claim<V>(
    root: ArrangementRoot<V>,
) -> Result<UntrustedId<ArrangementCanonicalRelation<V>>, IdDecodeError>
where
    V: Clone + Ord + std::fmt::Debug + Eq + CanonicalValue + crate::CheckpointValue + 'static,
{
    UntrustedId::from_wire(
        root.as_bytes(),
        IdContext::relation::<ArrangementCanonicalRelation<V>>(),
    )
}

/// A storage adapter capable of reading one checked canonical relation node.
///
/// Implementations must validate the supplied claim against the exact bytes
/// before returning.  A loader should read only the requested node; branch
/// descendants remain authenticated summaries until a lookup or update path
/// asks for them.
pub trait CasNodeReader<R: CanonicalRelation> {
    /// Storage error reported before canonical admission.
    type Error;

    /// Reads and admits the node named by `claim`.
    ///
    /// # Errors
    ///
    /// Returns the adapter's storage error when the immutable node is absent
    /// or unreadable, or an admission error represented by the adapter's
    /// error type when the bytes do not reproduce `claim`.
    fn read_node(
        &self,
        claim: UntrustedId<R>,
    ) -> Result<backend_version::CheckedCanonicalRoot<R>, Self::Error>;
}

/// A store-backed checked node reader using the public relation-node seam.
///
/// This adapter deliberately receives an already opened [`FileStore`] and
/// delegates claim lookup and canonical admission to the store.  Flow never
/// reconstructs object paths or parses a private storage envelope.
pub struct FileStoreNodeReader<'a> {
    store: &'a FileStore,
}

struct OwnedFileStoreNodeReader {
    store: FileStore,
}

impl<R> CasNodeReader<R> for OwnedFileStoreNodeReader
where
    R: CanonicalRelation,
{
    type Error = StoreError;

    fn read_node(
        &self,
        claim: UntrustedId<R>,
    ) -> Result<backend_version::CheckedCanonicalRoot<R>, Self::Error> {
        self.store.read_relation_node_claim(claim)
    }
}

impl<R, L> CasNodeReader<R> for &L
where
    R: CanonicalRelation,
    L: CasNodeReader<R> + ?Sized,
{
    type Error = L::Error;

    fn read_node(
        &self,
        claim: UntrustedId<R>,
    ) -> Result<backend_version::CheckedCanonicalRoot<R>, Self::Error> {
        (*self).read_node(claim)
    }
}

impl<'a> FileStoreNodeReader<'a> {
    /// Binds a reader to one immutable store handle.
    #[must_use]
    pub const fn new(store: &'a FileStore) -> Self {
        Self { store }
    }
}

impl<R> CasNodeReader<R> for FileStoreNodeReader<'_>
where
    R: CanonicalRelation,
{
    type Error = StoreError;

    fn read_node(
        &self,
        claim: UntrustedId<R>,
    ) -> Result<backend_version::CheckedCanonicalRoot<R>, Self::Error> {
        self.store.read_relation_node_claim(claim)
    }
}

/// An owned lazy root backed by the checked store relation index.
///
/// The root evidence is fetched and admitted once when [`Self::open`] is
/// called.  Subsequent lookups and updates construct a short-lived loader over
/// the owned store handle, so this type is not self-referential and can be
/// returned directly from a checkpoint reopen API.  No visible rows, history
/// runs, or descendant nodes are retained by the handle.
pub struct OwnedLazyCheckpointRoot<R: CanonicalRelation> {
    store: FileStore,
    root: StateRoot<R>,
    root_bytes: Arc<[u8]>,
    reader: Arc<dyn CasNodeReader<R, Error = StoreError> + Send + Sync>,
}

impl<R: CanonicalRelation> OwnedLazyCheckpointRoot<R> {
    /// Opens and admits one relation root through the store's typed index.
    ///
    /// Exactly the root object is read by this operation.  Child objects are
    /// loaded only by a later path probe.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the claim or its immutable root
    /// object cannot be admitted.
    pub fn open(store: &FileStore, claim: UntrustedId<R>) -> Result<Self, StoreError> {
        let checked = store.read_relation_node_claim(claim)?;
        let root =
            PersistedTreeRoot::admit(claim, checked.bytes()).map_err(|_| StoreError::Corrupt)?;
        Ok(Self {
            store: store.clone(),
            root: root.root(),
            root_bytes: checked.bytes().into(),
            reader: Arc::new(OwnedFileStoreNodeReader {
                store: store.clone(),
            }),
        })
    }

    /// Rebinds an already admitted root to a store without reading it.
    #[must_use]
    pub fn from_admitted(store: &FileStore, root: &PersistedTreeRoot<R>) -> Self {
        Self {
            store: store.clone(),
            root: root.root(),
            root_bytes: root.evidence().bytes().into(),
            reader: Arc::new(OwnedFileStoreNodeReader {
                store: store.clone(),
            }),
        }
    }

    /// Returns the exact authenticated relation root.
    #[must_use]
    pub const fn root(&self) -> StateRoot<R> {
        self.root
    }

    /// Returns an owned checked root handle for another store handoff.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] if the retained root evidence no
    /// longer admits under its relation context.
    pub fn root_handle(&self) -> Result<PersistedTreeRoot<R>, StoreError> {
        let claim = UntrustedId::from_wire(self.root.as_bytes(), IdContext::relation::<R>())
            .map_err(|_| StoreError::Corrupt)?;
        PersistedTreeRoot::admit(claim, &self.root_bytes).map_err(|_| StoreError::Corrupt)
    }

    /// Looks up one key through its authenticated root-to-leaf path.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when a requested node is absent or
    /// fails canonical admission.
    pub fn lookup(&self, key: &R::Key) -> Result<Option<R::Value>, StoreError> {
        let loader = CasNodeLoader::new(self.reader.as_ref());
        let tree = LazyCheckpointRoot::from_admitted(&loader, self.root_handle()?);
        tree.lookup(key).map_err(|_| StoreError::Corrupt)
    }

    /// Prepares one path-copy replacement without writing it.
    ///
    /// The returned update contains only the changed canonical frontier and
    /// can be passed to [`Self::apply`].
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when a requested path node is absent,
    /// malformed, or the key is not present.
    pub fn prepare_replace(
        &self,
        key: &R::Key,
        value: R::Value,
    ) -> Result<LazyPreparedUpdate<R>, StoreError> {
        let loader = CasNodeLoader::new(self.reader.as_ref());
        let tree = LazyCheckpointRoot::from_admitted(&loader, self.root_handle()?);
        tree.prepare_replace(key, value)
            .map_err(|_| StoreError::Corrupt)
    }

    /// Publishes a prepared path-copy update and retains its new root.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] if the immutable changed frontier is
    /// rejected by the store or its target root cannot be admitted.
    pub fn apply(&mut self, update: &LazyPreparedUpdate<R>) -> Result<LazyTreeWork, StoreError> {
        let target_bytes = update.target().bytes().to_vec();
        let work = update.work();
        self.store.write_lazy_relation_update(update)?;
        let target = PersistedTreeRoot::admit(
            UntrustedId::from_wire(
                update.target().root().as_bytes(),
                IdContext::relation::<R>(),
            )
            .map_err(|_| StoreError::Corrupt)?,
            &target_bytes,
        )
        .map_err(|_| StoreError::Corrupt)?;
        self.root = target.root();
        self.root_bytes = target_bytes.into();
        Ok(work)
    }
}

impl<V> OwnedLazyCheckpointRoot<ArrangementCanonicalRelation<V>>
where
    V: Clone + Ord + std::fmt::Debug + Eq + CanonicalValue + crate::CheckpointValue + 'static,
{
    /// Opens a flow checkpoint node persisted in the legacy flow node schema.
    ///
    /// The persisted node is converted to the shared backend-version grammar
    /// at the requested path. The conversion retains only authenticated
    /// branch summaries and a bounded map from logical node to physical object,
    /// so opening this handle reads one node and never rebuilds the relation.
    pub(crate) fn open_flow(
        store: &FileStore,
        claim: UntrustedId<ArrangementCanonicalRelation<V>>,
        root_object: [u8; 32],
    ) -> Result<Self, StoreError> {
        let reader = Arc::new(FlowNodeReader::<V>::new(
            store,
            *claim.as_bytes(),
            root_object,
        ));
        let checked = reader.read_node(claim)?;
        let root = checked.root();
        let root_bytes: Arc<[u8]> = checked.bytes().into();
        Ok(Self {
            store: store.clone(),
            root,
            root_bytes,
            reader,
        })
    }

    /// Looks up one logical arrangement row without materializing the visible
    /// relation. The logical key is converted to the shared canonical wire
    /// key used by the persisted relation root.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the requested path cannot be
    /// admitted by the store-backed canonical reader.
    pub fn lookup_arrangement(&self, key: &(RowKey, V)) -> Result<Option<i64>, StoreError> {
        let wire = arrangement_wire_key(key);
        self.lookup(&wire)
    }

    /// Prepares a support replacement for one logical arrangement row.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Corrupt`] when the requested path cannot be
    /// admitted or the row is absent.
    pub fn prepare_arrangement_replace(
        &self,
        key: &(RowKey, V),
        support: i64,
    ) -> Result<LazyPreparedUpdate<ArrangementCanonicalRelation<V>>, StoreError> {
        let wire = arrangement_wire_key(key);
        self.prepare_replace(&wire, support)
    }
}

struct FlowNodeReader<V> {
    store: FileStore,
    objects: Mutex<BTreeMap<[u8; 32], [u8; 32]>>,
    marker: std::marker::PhantomData<fn() -> V>,
}

impl<V> FlowNodeReader<V> {
    fn new(store: &FileStore, root_logical: [u8; 32], root_object: [u8; 32]) -> Self {
        let mut objects = BTreeMap::new();
        objects.insert(root_logical, root_object);
        Self {
            store: store.clone(),
            objects: Mutex::new(objects),
            marker: std::marker::PhantomData,
        }
    }
}

impl<V> CasNodeReader<ArrangementCanonicalRelation<V>> for FlowNodeReader<V>
where
    V: Clone + Ord + std::fmt::Debug + Eq + CanonicalValue + crate::CheckpointValue + 'static,
{
    type Error = StoreError;

    fn read_node(
        &self,
        claim: UntrustedId<ArrangementCanonicalRelation<V>>,
    ) -> Result<backend_version::CheckedCanonicalRoot<ArrangementCanonicalRelation<V>>, Self::Error>
    {
        let logical = *claim.as_bytes();
        let object = self
            .objects
            .lock()
            .map_err(|_| StoreError::Corrupt)?
            .get(&logical)
            .copied()
            .ok_or(StoreError::Corrupt)?;
        let typed = self
            .store
            .read_object_claim(backend_store::UntrustedObjectId::from_bytes(object))?;
        if typed.schema()
            != backend_version::SchemaIdentity::new(
                super::NodeSchema::DOMAIN,
                super::NodeSchema::TYPE,
                super::NodeSchema::VERSION,
            )
        {
            return Err(StoreError::Corrupt);
        }
        let resolver = |relation: &[u8; 32], object: &[u8; 32], key: u64| {
            Ok(RowKey {
                relation: RelationIdentity::from_bytes(*relation),
                object: ObjectIdentity::from_bytes(*object),
                key,
            })
        };
        let node = super::codec::decode_node::<V, _>(typed.bytes(), &resolver)
            .map_err(|_| StoreError::Corrupt)?;
        if node.logical != logical {
            return Err(StoreError::Corrupt);
        }
        let child_objects = node
            .children
            .iter()
            .map(|child| (child.logical, child.object))
            .collect::<Vec<_>>();
        let canonical = if let Some(entries) = node.entries {
            if entries.is_empty() {
                canonical_empty::<ArrangementCanonicalRelation<V>>()
            } else {
                let items = entries.into_iter().collect::<Vec<_>>();
                canonical_leaf::<ArrangementCanonicalRelation<V>>(&items)
                    .map_err(|_| StoreError::Corrupt)?
            }
        } else {
            let children = node
                .children
                .iter()
                .map(|child| -> Result<_, StoreError> {
                    let first = child.first.clone();
                    Ok(CommittedChild {
                        first_key: first,
                        commitment: ChildCommitment::from_bytes(&child.logical)
                            .map_err(|_| StoreError::Corrupt)?,
                        level: node.level.checked_sub(1).ok_or(StoreError::Corrupt)?,
                        row_count: child.row_count,
                    })
                })
                .collect::<Result<Vec<_>, StoreError>>()?;
            canonical_branch_from_commitments::<ArrangementCanonicalRelation<V>>(
                node.level, &children,
            )
            .map_err(|_| StoreError::Corrupt)?
        };
        if canonical.commitment().as_bytes() != &logical {
            return Err(StoreError::Corrupt);
        }
        if !child_objects.is_empty() {
            let mut objects = self.objects.lock().map_err(|_| StoreError::Corrupt)?;
            for (child_logical, child_object) in child_objects {
                objects.insert(child_logical, child_object);
            }
        }
        admit_canonical_root_claim(claim, canonical.as_bytes()).map_err(|_| StoreError::Corrupt)
    }
}

fn arrangement_wire_key<V>(key: &(RowKey, V)) -> ArrangementKey<V>
where
    V: CanonicalValue + Clone,
{
    ArrangementKey::new(key.0, key.1.clone())
}

/// Adapts a [`CasNodeReader`] to the version kernel's loader trait.
pub struct CasNodeLoader<L>(L);

impl<L> CasNodeLoader<L> {
    /// Wraps a checked CAS reader.
    #[must_use]
    pub const fn new(reader: L) -> Self {
        Self(reader)
    }

    /// Returns the wrapped reader.
    #[must_use]
    pub fn into_inner(self) -> L {
        self.0
    }
}

impl<R, L> TreeNodeLoader<R> for CasNodeLoader<L>
where
    R: CanonicalRelation,
    L: CasNodeReader<R>,
{
    type Error = L::Error;

    fn load(
        &self,
        claim: UntrustedId<R>,
    ) -> Result<backend_version::CheckedCanonicalRoot<R>, Self::Error> {
        self.0.read_node(claim)
    }
}

/// A lazy, typed view over one admitted checkpoint root.
///
/// Constructing this value authenticates the supplied root bytes once and
/// retains no visible row map.  Lookups and prepared updates load only the
/// affected root-to-leaf path through the caller's [`CasNodeReader`].
pub struct LazyCheckpointRoot<'a, R: CanonicalRelation, L: TreeNodeLoader<R>> {
    tree: LazyTree<'a, R, L>,
}

impl<'a, R: CanonicalRelation, L: TreeNodeLoader<R>> LazyCheckpointRoot<'a, R, L> {
    /// Opens a root from its checked wire claim and canonical root bytes.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalRootAdmissionError`] when the bytes do not reproduce
    /// the supplied relation-root claim.
    pub fn open(
        loader: &'a L,
        claim: UntrustedId<R>,
        root_bytes: &[u8],
    ) -> Result<Self, CanonicalRootAdmissionError> {
        let root = PersistedTreeRoot::admit(claim, root_bytes)?;
        Ok(Self::from_admitted(loader, root))
    }

    /// Opens an already admitted root without reading any node from storage.
    #[must_use]
    pub fn from_admitted(loader: &'a L, root: PersistedTreeRoot<R>) -> Self {
        Self {
            tree: LazyTree::from_admitted(loader, root),
        }
    }

    /// Returns the exact checked relation root.
    #[must_use]
    pub const fn root(&self) -> StateRoot<R> {
        self.tree.root().root()
    }

    /// Returns an owned checked root handle for another loader handoff.
    #[must_use]
    pub fn root_handle(&self) -> PersistedTreeRoot<R> {
        self.tree.root_handle()
    }

    /// Looks up one relation key through its authenticated path.
    ///
    /// # Errors
    ///
    /// Returns the loader or canonical admission error for an affected node.
    pub fn lookup<Q: ?Sized + Ord>(
        &self,
        key: &Q,
    ) -> Result<Option<R::Value>, LazyTreeError<L::Error>>
    where
        R::Key: Borrow<Q>,
    {
        self.tree.lookup(key)
    }

    /// Prepares a checked replacement through one authenticated path copy.
    ///
    /// # Errors
    ///
    /// Returns the loader, duplicate/missing-key, or canonical admission
    /// error reported by the version kernel.
    pub fn prepare_replace<Q: ?Sized + Ord>(
        &self,
        key: &Q,
        value: R::Value,
    ) -> Result<LazyPreparedUpdate<R>, LazyTreeError<L::Error>>
    where
        R::Key: Borrow<Q>,
    {
        self.tree.prepare_replace(key, value)
    }

    /// Prepares one checked insertion through an authenticated path copy.
    ///
    /// # Errors
    ///
    /// Returns the loader, duplicate-key, or canonical admission error
    /// reported by the version kernel.
    pub fn prepare_insert(
        &self,
        key: &R::Key,
        value: R::Value,
    ) -> Result<LazyPreparedUpdate<R>, LazyTreeError<L::Error>> {
        self.tree.prepare_insert(key, value)
    }

    /// Prepares one checked replacement or removal through an authenticated
    /// path copy.
    ///
    /// # Errors
    ///
    /// Returns the loader, missing-key, or canonical admission error reported
    /// by the version kernel.
    pub fn prepare_change(
        &self,
        key: &R::Key,
        value: Option<R::Value>,
    ) -> Result<LazyPreparedUpdate<R>, LazyTreeError<L::Error>> {
        self.tree.prepare_change(key, value)
    }
}

#[cfg(test)]
#[path = "lazy_tests.rs"]
mod tests;
