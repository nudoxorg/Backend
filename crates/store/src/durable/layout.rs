//! Publication identities and filesystem store layout.

use super::{
    ClosureId, Hash, LayoutId, PackId, RelationAdmissionRegistry, TreeWriteStats, WorkspaceBinding,
};
use crate::digest;
use backend_version::WorkspaceRoot;
use std::{
    fs::{File, OpenOptions, TryLockError},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

static ACTIVE_GC_PINS: AtomicUsize = AtomicUsize::new(0);

/// Kernel-held capability required to select a durable store head.
///
/// The descriptor remains locked for the lifetime of this value.  A caller
/// may stage immutable objects and prepare journal records without it, but
/// publication requires this capability so an arbitrary `FileStore` clone
/// cannot become a second head writer.  The engine's [`OwnerLease`] and the
/// control plane each hold one for their entire lifetime.
///
/// [`OwnerLease`]: https://docs.rs/backend-engine/latest/backend_engine/struct.OwnerLease.html
#[derive(Clone, Debug)]
pub struct StorePublicationAuthority {
    inner: Arc<PublicationAuthorityInner>,
}

/// Failure to acquire a store's kernel-held publication authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublicationAuthorityError {
    /// Another process currently holds the authority.
    Busy,
    /// The authority file could not be opened or locked.
    Io(String),
}

impl std::fmt::Display for PublicationAuthorityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => formatter.write_str("publication authority is held by another process"),
            Self::Io(error) => formatter.write_str(error),
        }
    }
}

impl std::error::Error for PublicationAuthorityError {}

#[derive(Debug)]
struct PublicationAuthorityInner {
    lock_path: PathBuf,
    lock_file: File,
}

impl StorePublicationAuthority {
    /// Acquires the kernel lease at `lock_path`.
    ///
    /// The path must be the canonical `OWNER.lock` path associated with the
    /// store being published.  The store validates that association before a
    /// publish frame can be appended.
    ///
    /// # Errors
    ///
    /// Returns [`PublicationAuthorityError::Busy`] when another process
    /// holds the lease and [`PublicationAuthorityError::Io`] for other
    /// filesystem failures.
    pub fn acquire(lock_path: impl AsRef<Path>) -> Result<Self, PublicationAuthorityError> {
        let lock_path = lock_path.as_ref().to_owned();
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| PublicationAuthorityError::Io(error.to_string()))?;
        }
        let lock_file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|error| PublicationAuthorityError::Io(error.to_string()))?;
        match lock_file.try_lock() {
            Ok(()) => Ok(Self {
                inner: Arc::new(PublicationAuthorityInner {
                    lock_path,
                    lock_file,
                }),
            }),
            Err(TryLockError::WouldBlock) => Err(PublicationAuthorityError::Busy),
            Err(TryLockError::Error(error)) => {
                Err(PublicationAuthorityError::Io(error.to_string()))
            }
        }
    }

    pub(super) fn permits(&self, store_root: &Path) -> bool {
        self.inner.lock_path == publication_lock_path(store_root)
    }
}

impl Drop for PublicationAuthorityInner {
    fn drop(&mut self) {
        let _ = self.lock_file.unlock();
    }
}

pub(super) fn publication_lock_path(root: &Path) -> PathBuf {
    if root.file_name().is_some_and(|name| name == "objects") {
        root.parent().unwrap_or(root).join("OWNER.lock")
    } else {
        root.join("OWNER.lock")
    }
}

/// OS advisory lease held from publication preparation through selection.
///
/// The process-local mutex in [`FileStore`] protects the cached journal tail;
/// this file lock extends the same serialization rule to independent store
/// processes. The lease file contains no authority and may be recreated after
/// a crash; the operating system releases the lock with the file descriptor.
#[derive(Debug)]
pub(super) struct ProcessLock {
    file: File,
}

impl ProcessLock {
    pub(super) fn acquire(path: &Path) -> Result<Arc<Self>, super::StoreError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| super::io_error(&error))?;
        file.lock().map_err(|error| super::io_error(&error))?;
        Ok(Arc::new(Self { file }))
    }
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Shared live-capture pin, separate from the store mutation lock so a long
/// compiler stream does not block head reads or unrelated immutable writes.
#[derive(Debug)]
pub(super) struct GcPinLease {
    file: File,
}

impl GcPinLease {
    fn open(root: &Path) -> Result<File, super::StoreError> {
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("GC-PINS.lock"))
            .map_err(|error| super::io_error(&error))
    }

    pub(super) fn shared(root: &Path) -> Result<Self, super::StoreError> {
        let file = Self::open(root)?;
        file.lock_shared()
            .map_err(|error| super::io_error(&error))?;
        Ok(Self { file })
    }

    pub(super) fn exclusive(root: &Path) -> Result<Self, super::StoreError> {
        let file = Self::open(root)?;
        file.lock().map_err(|error| super::io_error(&error))?;
        Ok(Self { file })
    }

    pub(super) fn try_exclusive(root: &Path) -> Result<Option<Self>, super::StoreError> {
        let file = Self::open(root)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(super::io_error(&error)),
        }
    }
}

impl Drop for GcPinLease {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Shared lease that prevents garbage collection while an external product
/// reference is being published or read for a collection root snapshot.
/// Product stores acquire this before their own mutation lock and hold it
/// until the reference or root snapshot is durable.
#[derive(Debug)]
pub struct GcPinGuard {
    _lease: GcPinLease,
    root: PathBuf,
    acquired_at: Instant,
}

impl GcPinGuard {
    pub(super) fn covers_root(&self, root: &Path) -> bool {
        self.root == root
    }

    /// Elapsed time since this shared collection pin was acquired.
    #[must_use]
    pub fn held_for(&self) -> Duration {
        self.acquired_at.elapsed()
    }

    /// Number of shared collection-pin guards held in this process.
    ///
    /// Pins held by other processes are not included; a failed nonblocking
    /// exclusive lease still reports that another process may be holding one.
    #[must_use]
    pub fn active_count() -> usize {
        ACTIVE_GC_PINS.load(Ordering::Relaxed)
    }
}

impl Drop for GcPinGuard {
    fn drop(&mut self) {
        ACTIVE_GC_PINS.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Opaque identity for one exact publication transaction.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TransactionId(pub(super) Hash);

impl TransactionId {
    /// Returns the transaction's fixed-width identity bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &Hash {
        &self.0
    }
}

/// The selected base observed while preparing a publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationBase {
    pub(super) target: Hash,
    pub(super) generation: u64,
}

impl PublicationBase {
    /// Returns the selected target root used as the publication base.
    #[must_use]
    pub const fn target(self) -> Hash {
        self.target
    }

    /// Creates a base token from a checked workspace root.
    #[must_use]
    pub fn from_workspace(root: WorkspaceRoot, generation: u64) -> Self {
        Self {
            target: root.to_bytes(),
            generation,
        }
    }

    /// Returns the selected publication generation.
    #[must_use]
    pub const fn generation(self) -> u64 {
        self.generation
    }
}

/// Exact immutable identities bound by a publication transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationDescriptor {
    pub(super) transaction: TransactionId,
    pub(super) base: Option<Hash>,
    pub(super) target: Hash,
    pub(super) layout: LayoutId,
    pub(super) pack: PackId,
    pub(super) closure: ClosureId,
    pub(super) workspace: Option<WorkspaceBinding>,
    pub(super) base_generation: u64,
    pub(super) target_generation: u64,
}

impl PublicationDescriptor {
    #[allow(
        clippy::too_many_arguments,
        reason = "the descriptor fields are the complete publication identity"
    )]
    pub(super) fn from_parts(
        base: Option<Hash>,
        target: Hash,
        layout: LayoutId,
        pack: PackId,
        closure: ClosureId,
        workspace: Option<WorkspaceBinding>,
        base_generation: u64,
        target_generation: u64,
    ) -> Self {
        let transaction = TransactionId(derive_transaction(
            base,
            target,
            layout,
            pack,
            closure,
            workspace,
            base_generation,
            target_generation,
        ));
        Self {
            transaction,
            base,
            target,
            layout,
            pack,
            closure,
            workspace,
            base_generation,
            target_generation,
        }
    }

    pub(super) fn derived_transaction(self) -> Hash {
        derive_transaction(
            self.base,
            self.target,
            self.layout,
            self.pack,
            self.closure,
            self.workspace,
            self.base_generation,
            self.target_generation,
        )
    }

    /// Returns the deterministic identity of this exact transition.
    #[must_use]
    pub const fn transaction(self) -> TransactionId {
        self.transaction
    }

    /// Returns the exact base root, or `None` for a genesis publication.
    #[must_use]
    pub const fn base(self) -> Option<Hash> {
        self.base
    }

    /// Returns the exact target root.
    #[must_use]
    pub const fn target(self) -> Hash {
        self.target
    }

    /// Returns the physical layout identity.
    #[must_use]
    pub const fn layout(self) -> LayoutId {
        self.layout
    }

    /// Returns the immutable pack identity.
    #[must_use]
    pub const fn pack(self) -> PackId {
        self.pack
    }

    /// Returns the complete closure identity.
    #[must_use]
    pub const fn closure(self) -> ClosureId {
        self.closure
    }

    /// Returns the typed workspace binding, when this publication carries one.
    #[must_use]
    pub const fn workspace(self) -> Option<WorkspaceBinding> {
        self.workspace
    }

    /// Returns the selected generation at preparation time.
    #[must_use]
    pub const fn base_generation(self) -> u64 {
        self.base_generation
    }

    /// Returns the generation selected by this publication.
    #[must_use]
    pub const fn target_generation(self) -> u64 {
        self.target_generation
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the transaction digest covers every publication identity field"
)]
fn derive_transaction(
    base: Option<Hash>,
    target: Hash,
    layout: LayoutId,
    pack: PackId,
    closure: ClosureId,
    workspace: Option<WorkspaceBinding>,
    base_generation: u64,
    target_generation: u64,
) -> Hash {
    let mut bytes = Vec::with_capacity(256);
    bytes.push(u8::from(base.is_some()));
    bytes.extend_from_slice(&base.unwrap_or([0; 32]));
    bytes.extend_from_slice(&target);
    bytes.extend_from_slice(layout.as_bytes());
    bytes.extend_from_slice(pack.as_bytes());
    bytes.extend_from_slice(closure.as_bytes());
    bytes.push(u8::from(workspace.is_some()));
    if let Some(workspace) = workspace {
        bytes.extend_from_slice(workspace.root());
        bytes.extend_from_slice(workspace.closure().as_bytes());
        bytes.extend_from_slice(workspace.proof());
    } else {
        bytes.extend_from_slice(&[0; 96]);
    }
    bytes.extend_from_slice(&base_generation.to_le_bytes());
    bytes.extend_from_slice(&target_generation.to_le_bytes());
    digest(b"store.publication.transaction.v2\0", &bytes)
}

/// The currently selected durable publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SelectedHead {
    pub(super) journal_sequence: u64,
    pub(super) descriptor: PublicationDescriptor,
}

impl SelectedHead {
    /// Returns the journal sequence of the selected publish record.
    #[must_use]
    pub const fn journal_sequence(self) -> u64 {
        self.journal_sequence
    }

    /// Returns the publication generation.
    #[must_use]
    pub const fn generation(self) -> u64 {
        self.descriptor.target_generation
    }

    /// Returns all identities bound by the selected publication.
    #[must_use]
    pub const fn descriptor(self) -> PublicationDescriptor {
        self.descriptor
    }

    /// Returns a token that can be supplied as an exact publication base.
    #[must_use]
    pub const fn as_base(self) -> PublicationBase {
        PublicationBase {
            target: self.descriptor.target,
            generation: self.descriptor.target_generation,
        }
    }
}

/// Filesystem-backed immutable packs, closure manifests, and publication state.
#[derive(Clone, Debug)]
pub struct FileStore {
    pub(super) root: PathBuf,
    pub(super) max_pack_bytes: usize,
    pub(super) lock: Arc<Mutex<()>>,
    pub(super) journal_tail: Arc<Mutex<super::recovery::JournalTail>>,
    pub(super) relation_registry: Arc<RelationAdmissionRegistry>,
    pub(super) tree_write_stats: Arc<Mutex<TreeWriteStats>>,
}

impl FileStore {
    pub(super) fn acquire_process_lock(&self) -> Result<Arc<ProcessLock>, super::StoreError> {
        ProcessLock::acquire(&self.root.join("LOCK"))
    }

    pub(super) fn acquire_gc_pin(&self) -> Result<GcPinLease, super::StoreError> {
        GcPinLease::shared(&self.root)
    }

    pub(super) fn acquire_gc_exclusive(&self) -> Result<GcPinLease, super::StoreError> {
        GcPinLease::exclusive(&self.root)
    }

    /// Acquires the shared collection lease for an external product mutation.
    /// A collector waits for this guard before resolving external roots.
    /// Product writers should acquire it before their owner lock, persist the
    /// product ref, then drop it to preserve lock ordering with root resolvers.
    pub fn pin_garbage_collection(&self) -> Result<GcPinGuard, super::StoreError> {
        let lease = self.acquire_gc_pin()?;
        ACTIVE_GC_PINS.fetch_add(1, Ordering::Relaxed);
        Ok(GcPinGuard {
            _lease: lease,
            root: self.root.clone(),
            acquired_at: Instant::now(),
        })
    }

    /// Runs a product metadata operation while holding the exclusive garbage
    /// collection lease.
    ///
    /// This is for external metadata which must be reclaimed under the same
    /// cross-process barrier as FileStore objects. The callback must not call
    /// a FileStore method that acquires a shared GC pin or starts collection;
    /// either would wait on this lease. Product owners should acquire their
    /// own lock inside the callback, preserving the global `GC-PINS.lock`
    /// before owner-lock ordering used by readers and writers.
    pub fn with_gc_exclusive_lease<T>(
        &self,
        operation: impl FnOnce() -> Result<T, super::StoreError>,
    ) -> Result<T, super::StoreError> {
        let _gc_exclusive = self.acquire_gc_exclusive()?;
        operation()
    }

    /// Attempts a product metadata operation under the exclusive collection
    /// lease without waiting for shared readers. `Ok(None)` signals that a
    /// reader or another process currently holds a pin, so the caller should
    /// apply backpressure and retry later.
    pub fn try_with_gc_exclusive_lease<T>(
        &self,
        operation: impl FnOnce() -> Result<T, super::StoreError>,
    ) -> Result<Option<T>, super::StoreError> {
        let Some(_gc_exclusive) = GcPinLease::try_exclusive(&self.root)? else {
            return Ok(None);
        };
        operation().map(Some)
    }

    /// Acquires this store's canonical head-selection capability.
    ///
    /// Immutable object admission and publication preparation do not require
    /// this capability.  The returned kernel lease must remain live through
    /// every call to `publish_with_authority`.
    ///
    /// # Errors
    ///
    /// Returns [`PublicationAuthorityError::Busy`] when another owner holds
    /// the head-selection lease and [`PublicationAuthorityError::Io`] for
    /// other filesystem failures.
    pub fn acquire_publication_authority(
        &self,
    ) -> Result<StorePublicationAuthority, PublicationAuthorityError> {
        StorePublicationAuthority::acquire(publication_lock_path(&self.root))
    }
}
