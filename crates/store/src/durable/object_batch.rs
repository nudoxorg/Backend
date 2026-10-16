//! A bounded group of immutable CAS writes, without a publication capability.
//!
//! Private files are prepared under the normal store process lease, flushed
//! together, then linked and covered by one directory barrier. The lease also
//! prevents a concurrent store open from scavenging these live temporaries.
//! No object identity returned by a caller's input certifies durability: only
//! the completed batch receipt does, and it never selects a closure or HEAD.

use super::layout::ProcessLock;
use super::objects::create_temp;
use super::{
    FileStore, GcPinGuard, ObjectId, ObjectWriteReceipt, PathBuf, StoreError, TypedObject,
    encode_object, envelope_limit, fs, io_error, sync_directory,
};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::sync::Arc;

/// Maximum independent objects sharing one CAS directory barrier.
pub const MAX_OBJECT_BATCH_OBJECTS: usize = 32;

/// Storage facts returned only after every member and its name are durable.
/// This receipt admits neither a closure nor a selected workspace generation.
#[derive(Debug)]
#[must_use]
pub struct ObjectBatchWriteReceipt {
    writes: Box<[ObjectWriteReceipt]>,
    file_flushes: usize,
    directory_flushes: usize,
}

impl ObjectBatchWriteReceipt {
    /// Per-object immutable admission receipts in input order.
    #[must_use]
    pub fn writes(&self) -> &[ObjectWriteReceipt] {
        &self.writes
    }

    /// New private object files whose flushes completed before any link.
    #[must_use]
    pub const fn file_flushes(&self) -> usize {
        self.file_flushes
    }

    /// Completed barriers covering the objects directory's names.
    #[must_use]
    pub const fn directory_flushes(&self) -> usize {
        self.directory_flushes
    }
}

struct PendingObject {
    write_index: usize,
    temporary: PathBuf,
    target: PathBuf,
    file: File,
}

impl Drop for PendingObject {
    fn drop(&mut self) {
        // A failed or abandoned private batch cannot strand an active temp.
        // Crash recovery retains its existing process-lease scavenging rule.
        let _ = fs::remove_file(&self.temporary);
    }
}

struct BatchFiles {
    pending: Vec<PendingObject>,
    writes: Vec<ObjectWriteReceipt>,
    directory: PathBuf,
    // Drop private files before releasing either lease.
    _process_lock: Arc<ProcessLock>,
    _pin: GcPinGuard,
}

struct PreparedBatch(BatchFiles);
struct FlushedBatch(BatchFiles);

impl FileStore {
    /// Durably admits at most 32 distinct objects using shared barriers.
    ///
    /// Every object uses the same wire-version and existing value-target
    /// validators as [`Self::write_object`]. No sibling-reference exemption is
    /// introduced. All file flushes complete before final names are linked;
    /// the directory is synchronized once before any receipt is returned.
    /// An interrupted batch can leave durable, unselected objects for normal
    /// GC, but can never publish a closure or advance HEAD.
    ///
    /// # Errors
    /// Returns bounds, admission, corruption or filesystem errors. Duplicate
    /// input identities are rejected before creating any temporary file.
    pub fn write_object_batch(
        &self,
        objects: &[TypedObject],
    ) -> Result<ObjectBatchWriteReceipt, StoreError> {
        if objects.len() > MAX_OBJECT_BATCH_OBJECTS {
            return Err(StoreError::Bounds);
        }
        if objects.is_empty() {
            return Ok(ObjectBatchWriteReceipt {
                writes: Box::new([]),
                file_flushes: 0,
                directory_flushes: 0,
            });
        }
        let pin = self.pin_garbage_collection()?;
        let mut seen = BTreeSet::<ObjectId>::new();
        for object in objects {
            if !seen.insert(object.id()) {
                return Err(StoreError::Corrupt);
            }
            object.verify_wire_version(&self.relation_registry)?;
            self.verify_object_value_targets(object)?;
        }
        let process_lock = self.acquire_process_lock()?;
        let mut files = BatchFiles {
            pending: Vec::with_capacity(objects.len()),
            writes: Vec::with_capacity(objects.len()),
            directory: self.root.join("objects"),
            _process_lock: process_lock,
            _pin: pin,
        };
        let limit = envelope_limit(self.max_pack_bytes)?;
        for object in objects {
            let encoded = encode_object(object, limit)?;
            let target = self.object_path(object.id());
            let created = match fs::symlink_metadata(&target) {
                Ok(metadata) => {
                    if !metadata.is_file() || metadata.file_type().is_symlink() {
                        return Err(StoreError::Corrupt);
                    }
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt as _;
                        if metadata.nlink() != 1 {
                            return Err(StoreError::Corrupt);
                        }
                    }
                    if metadata.len() != encoded.len() as u64 {
                        return Err(StoreError::Corrupt);
                    }
                    let file = backend_platform::durability::open_regular_file_nofollow(&target)
                        .map_err(|error| io_error(&error))?;
                    let mut existing = Vec::with_capacity(encoded.len());
                    file.take(encoded.len() as u64 + 1)
                        .read_to_end(&mut existing)
                        .map_err(|error| io_error(&error))?;
                    if existing != encoded {
                        return Err(StoreError::Corrupt);
                    }
                    false
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let (temporary, file) = create_temp(&target)?;
                    let mut pending = PendingObject {
                        write_index: files.writes.len(),
                        temporary,
                        target,
                        file,
                    };
                    pending
                        .file
                        .write_all(&encoded)
                        .map_err(|error| io_error(&error))?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt as _;
                        pending
                            .file
                            .set_permissions(fs::Permissions::from_mode(0o400))
                            .map_err(|error| io_error(&error))?;
                    }
                    files.pending.push(pending);
                    true
                }
                Err(error) => return Err(io_error(&error)),
            };
            files.writes.push(ObjectWriteReceipt::new(
                object.id(),
                created,
                u64::try_from(encoded.len()).map_err(|_| StoreError::Bounds)?,
            ));
        }
        checkpoint(BatchPhase::Prepared)?;
        PreparedBatch(files).flush()?.link()
    }
}

impl PreparedBatch {
    fn flush(self) -> Result<FlushedBatch, StoreError> {
        let inject_failure = injected_file_failure();
        backend_platform::durability::flush_concurrently(&self.0.pending, |pending| {
            if inject_failure {
                return Err(std::io::Error::other(
                    "injected object batch file-flush failure",
                ));
            }
            pending.file.sync_all()
        })
        .map_err(|error| io_error(&error))?;
        checkpoint(BatchPhase::FilesFlushed)?;
        Ok(FlushedBatch(self.0))
    }
}

impl FlushedBatch {
    fn link(mut self) -> Result<ObjectBatchWriteReceipt, StoreError> {
        for (index, pending) in self.0.pending.iter().enumerate() {
            match fs::hard_link(&pending.temporary, &pending.target) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let mine = fs::read(&pending.temporary).map_err(|error| io_error(&error))?;
                    let file =
                        backend_platform::durability::open_regular_file_nofollow(&pending.target)
                            .map_err(|error| io_error(&error))?;
                    let metadata = file.metadata().map_err(|error| io_error(&error))?;
                    if !metadata.is_file() {
                        return Err(StoreError::Corrupt);
                    }
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt as _;
                        if metadata.nlink() != 1 {
                            return Err(StoreError::Corrupt);
                        }
                    }
                    let mut existing = Vec::with_capacity(mine.len());
                    file.take(mine.len() as u64 + 1)
                        .read_to_end(&mut existing)
                        .map_err(|error| io_error(&error))?;
                    if mine != existing {
                        return Err(StoreError::Corrupt);
                    }
                    // A legacy individual writer need not hold our batch
                    // lease. Report its winning physical file as shared,
                    // even when it appeared after this batch's preflight.
                    let write = self.0.writes[pending.write_index];
                    self.0.writes[pending.write_index] =
                        ObjectWriteReceipt::new(write.id(), false, write.bytes());
                }
                Err(error) => return Err(io_error(&error)),
            }
            fs::remove_file(&pending.temporary).map_err(|error| io_error(&error))?;
            if index == 0 {
                checkpoint(BatchPhase::FirstObjectLinked)?;
            }
        }
        checkpoint(BatchPhase::BeforeDirectoryFlush)?;
        // Cover even pre-existing names: another immutable writer may have
        // linked an already-flushed file without completing its parent sync.
        sync_directory(&self.0.directory)?;
        checkpoint(BatchPhase::DirectoryFlushed)?;
        Ok(ObjectBatchWriteReceipt {
            file_flushes: self.0.pending.len(),
            directory_flushes: 1,
            writes: self.0.writes.into_boxed_slice(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BatchPhase {
    Prepared,
    FilesFlushed,
    FirstObjectLinked,
    BeforeDirectoryFlush,
    DirectoryFlushed,
}

#[cfg(not(test))]
fn checkpoint(_phase: BatchPhase) -> Result<(), StoreError> {
    Ok(())
}
#[cfg(not(test))]
const fn injected_file_failure() -> bool {
    false
}

#[cfg(test)]
std::thread_local! {
    static TEST_CONTROL: std::cell::RefCell<(Option<BatchPhase>, bool, Option<Vec<BatchPhase>>)> = const { std::cell::RefCell::new((None, false, None)) };
    static PROCESS_LEASE_PROBE: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    static LEGACY_WRITER: std::cell::RefCell<Option<(FileStore, TypedObject)>> = const { std::cell::RefCell::new(None) };
    static LEGACY_WRITER_SHADOW: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
    static CRASH_PHASE: std::cell::Cell<Option<BatchPhase>> = const { std::cell::Cell::new(None) };
}
#[cfg(test)]
fn checkpoint(phase: BatchPhase) -> Result<(), StoreError> {
    if CRASH_PHASE.with(|value| value.get() == Some(phase)) {
        std::process::exit(77);
    }
    if phase == BatchPhase::Prepared {
        if let Some((store, object)) = LEGACY_WRITER.with_borrow_mut(Option::take) {
            store.write_object(&object)?;
            if let Some(shadow) = LEGACY_WRITER_SHADOW.with_borrow_mut(Option::take) {
                fs::hard_link(store.object_path(object.id()), shadow)
                    .map_err(|error| io_error(&error))?;
            }
        }
    }
    PROCESS_LEASE_PROBE.with_borrow(|path| {
        if let Some(path) = path {
            let probe = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .map_err(|error| io_error(&error))?;
            match probe.try_lock() {
                Err(fs::TryLockError::WouldBlock) => {}
                _ => return Err(StoreError::Corrupt),
            }
        }
        Ok::<_, StoreError>(())
    })?;
    TEST_CONTROL.with_borrow_mut(|control| {
        if let Some(events) = &mut control.2 {
            events.push(phase);
        }
        if control.0 == Some(phase) {
            Err(StoreError::Io(format!(
                "injected object batch failure at {phase:?}"
            )))
        } else {
            Ok(())
        }
    })
}
#[cfg(test)]
fn injected_file_failure() -> bool {
    TEST_CONTROL.with_borrow(|control| control.1)
}

#[cfg(test)]
std::thread_local! {
    static DIRECTORY_FLUSHES: std::cell::RefCell<Option<Vec<PathBuf>>> = const { std::cell::RefCell::new(None) };
}
#[cfg(test)]
pub(super) fn record_directory_flush(path: &std::path::Path) {
    DIRECTORY_FLUSHES.with_borrow_mut(|paths| {
        if let Some(paths) = paths {
            paths.push(path.to_path_buf());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_version::{ObjectKey, Schema};
    use std::error::Error;

    #[derive(Debug)]
    enum TestFailure {
        Store(StoreError),
        Io(std::io::Error),
    }
    impl std::fmt::Display for TestFailure {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "{self:?}")
        }
    }
    impl Error for TestFailure {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            match self {
                Self::Store(_) => None,
                Self::Io(error) => Some(error),
            }
        }
    }
    impl From<StoreError> for TestFailure {
        fn from(error: StoreError) -> Self {
            Self::Store(error)
        }
    }
    impl From<std::io::Error> for TestFailure {
        fn from(error: std::io::Error) -> Self {
            Self::Io(error)
        }
    }
    type TestResult = Result<(), TestFailure>;

    struct BytesSchema;
    impl Schema for BytesSchema {
        const DOMAIN: u8 = 0xf2;
        const TYPE: u16 = 0x5102;
        type Value = Vec<u8>;
        fn encode(value: &Self::Value, output: &mut Vec<u8>) {
            output.extend_from_slice(value);
        }
    }
    struct Directory(PathBuf);
    impl Directory {
        fn new(label: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Self(std::env::temp_dir().join(format!(
                "backend-store-batch-{label}-{}-{n}",
                std::process::id()
            )))
        }
        fn store(&self) -> Result<FileStore, StoreError> {
            FileStore::open(&self.0, 1024 * 1024)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn objects(count: usize) -> Vec<TypedObject> {
        (0..count)
            .map(|n| {
                let bytes = format!("bounded batch {n}").into_bytes();
                TypedObject::from_value(&ObjectKey::<BytesSchema>::from_value(&bytes), &bytes)
            })
            .collect()
    }
    fn reset(fault: Option<BatchPhase>, file_failure: bool) {
        TEST_CONTROL.with_borrow_mut(|c| *c = (fault, file_failure, Some(Vec::new())));
        DIRECTORY_FLUSHES.with_borrow_mut(|paths| *paths = Some(Vec::new()));
        PROCESS_LEASE_PROBE.with_borrow_mut(|path| *path = None);
        LEGACY_WRITER.with_borrow_mut(|writer| *writer = None);
        LEGACY_WRITER_SHADOW.with_borrow_mut(|path| *path = None);
        CRASH_PHASE.with(|phase| phase.set(None));
    }
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            reset(None, false);
            TEST_CONTROL.with_borrow_mut(|control| control.2 = None);
            DIRECTORY_FLUSHES.with_borrow_mut(|paths| *paths = None);
        }
    }
    fn temporaries(store: &FileStore) -> std::io::Result<usize> {
        let mut count = 0;
        for entry in fs::read_dir(store.root().join("objects"))? {
            if entry?.file_name().to_string_lossy().ends_with(".tmp") {
                count += 1;
            }
        }
        Ok(count)
    }
    fn object_directory_flushes(store: &FileStore) -> usize {
        DIRECTORY_FLUSHES.with_borrow(|paths| {
            paths.as_ref().map_or(0, |paths| {
                paths
                    .iter()
                    .filter(|p| **p == store.root().join("objects"))
                    .count()
            })
        })
    }

    #[test]
    fn a_batch_completes_the_same_objects_with_one_actual_directory_barrier() -> TestResult {
        let baseline_dir = Directory::new("baseline");
        let baseline = baseline_dir.store()?;
        let candidate_dir = Directory::new("candidate");
        let candidate = candidate_dir.store()?;
        let objects = objects(MAX_OBJECT_BATCH_OBJECTS);
        reset(None, false);
        let _reset = Reset;
        for object in &objects {
            assert!(baseline.write_object_with_receipt(object)?.created());
        }
        let individual_flushes = object_directory_flushes(&baseline);
        reset(None, false);
        let receipt = candidate.write_object_batch(&objects)?;
        assert_eq!(receipt.writes().len(), objects.len());
        assert!(receipt.writes().iter().all(|write| write.created()));
        assert_eq!(receipt.file_flushes(), objects.len());
        assert_eq!(receipt.directory_flushes(), 1);
        assert_eq!(individual_flushes, objects.len());
        assert_eq!(object_directory_flushes(&candidate), 1);
        assert_eq!(
            TEST_CONTROL.with_borrow(|c| c.2.clone().unwrap_or_default()),
            vec![
                BatchPhase::Prepared,
                BatchPhase::FilesFlushed,
                BatchPhase::FirstObjectLinked,
                BatchPhase::BeforeDirectoryFlush,
                BatchPhase::DirectoryFlushed
            ]
        );
        for object in &objects {
            assert_eq!(candidate.read_object(object.id())?, *object);
        }
        assert_eq!(temporaries(&candidate)?, 0);
        assert!(candidate.head()?.is_none());
        eprintln!(
            "object_batch_actual_directory_flushes individual={individual_flushes} batch=1 objects={}",
            objects.len()
        );
        Ok(())
    }

    #[test]
    fn every_interrupted_batch_keeps_head_unselected_and_retry_is_exact() -> TestResult {
        for fault in [
            BatchPhase::Prepared,
            BatchPhase::FilesFlushed,
            BatchPhase::FirstObjectLinked,
            BatchPhase::BeforeDirectoryFlush,
            BatchPhase::DirectoryFlushed,
        ] {
            let directory = Directory::new("fault");
            let store = directory.store()?;
            let objects = objects(5);
            reset(Some(fault), false);
            let _reset = Reset;
            assert!(store.write_object_batch(&objects).is_err());
            let mut present = 0;
            for object in &objects {
                if store.contains_object(object.id())? {
                    present += 1;
                }
            }
            assert_eq!(
                present,
                match fault {
                    BatchPhase::Prepared | BatchPhase::FilesFlushed => 0,
                    BatchPhase::FirstObjectLinked => 1,
                    _ => objects.len(),
                }
            );
            assert_eq!(temporaries(&store)?, 0);
            reset(None, false);
            let cold = directory.store()?;
            assert!(cold.head()?.is_none());
            let receipt = cold.write_object_batch(&objects)?;
            assert_eq!(
                receipt.writes().iter().filter(|w| w.created()).count(),
                objects.len() - present
            );
            for object in &objects {
                assert_eq!(cold.read_object(object.id())?, *object);
            }
            assert_eq!(temporaries(&cold)?, 0);
        }
        Ok(())
    }

    #[test]
    fn a_failed_file_barrier_cannot_create_any_cas_name() -> TestResult {
        let directory = Directory::new("file-failure");
        let store = directory.store()?;
        let objects = objects(7);
        reset(None, true);
        let _reset = Reset;
        assert!(store.write_object_batch(&objects).is_err());
        for object in &objects {
            assert!(!store.contains_object(object.id())?);
        }
        assert_eq!(
            TEST_CONTROL.with_borrow(|c| c.2.clone().unwrap_or_default()),
            vec![BatchPhase::Prepared]
        );
        assert_eq!(temporaries(&store)?, 0);
        Ok(())
    }

    #[test]
    fn replay_is_idempotent_and_bounds_and_corruption_fail_before_linking() -> TestResult {
        let directory = Directory::new("admission");
        let store = directory.store()?;
        let objects = objects(33);
        assert!(matches!(
            store.write_object_batch(&objects),
            Err(StoreError::Bounds)
        ));
        assert!(matches!(
            store.write_object_batch(&[objects[0].clone(), objects[0].clone()]),
            Err(StoreError::Corrupt)
        ));
        assert_eq!(temporaries(&store)?, 0);
        let first = store.write_object_batch(&objects[..32])?;
        assert!(first.writes().iter().all(|w| w.created()));
        let second = store.write_object_batch(&objects[..32])?;
        assert!(second.writes().iter().all(|w| !w.created()));
        assert_eq!(second.file_flushes(), 0);
        assert_eq!(second.directory_flushes(), 1);
        let conflict = store.object_path(objects[32].id());
        fs::write(conflict, b"conflict")?;
        assert!(matches!(
            store.write_object_batch(&objects[32..]),
            Err(StoreError::Corrupt)
        ));
        assert!(store.head()?.is_none());
        Ok(())
    }

    #[test]
    fn concurrent_batches_of_the_same_objects_preserve_cas_identity() -> TestResult {
        let directory = Directory::new("race");
        let store = directory.store()?;
        let objects = objects(32);
        std::thread::scope(|scope| -> TestResult {
            let handles = (0..3)
                .map(|_| scope.spawn(|| store.write_object_batch(&objects)))
                .collect::<Vec<_>>();
            for handle in handles {
                let _receipt = handle
                    .join()
                    .map_err(|_| std::io::Error::other("batch writer panicked"))??;
            }
            Ok(())
        })?;
        for object in &objects {
            assert_eq!(store.read_object(object.id())?, *object);
        }
        assert_eq!(temporaries(&store)?, 0);
        assert!(store.head()?.is_none());
        Ok(())
    }

    #[test]
    fn a_legacy_writer_winning_after_preflight_is_reported_as_shared() -> TestResult {
        let directory = Directory::new("legacy-race");
        let store = directory.store()?;
        let objects = objects(4);
        // Keep a pre-existing item before the pending objects, so the
        // pending-file index differs from its input receipt index.
        store.write_object(&objects[0])?;
        reset(None, false);
        let _reset = Reset;
        LEGACY_WRITER.with_borrow_mut(|writer| {
            *writer = Some((store.clone(), objects[2].clone()));
        });
        let receipt = store.write_object_batch(&objects)?;
        assert_eq!(
            receipt
                .writes()
                .iter()
                .map(|write| write.created())
                .collect::<Vec<_>>(),
            vec![false, true, false, true]
        );
        assert_eq!(receipt.file_flushes(), 3);
        assert_eq!(receipt.directory_flushes(), 1);
        for (write, object) in receipt.writes().iter().zip(&objects) {
            assert_eq!(write.id(), object.id());
            assert_eq!(store.read_object(write.id())?, *object);
        }
        assert_eq!(temporaries(&store)?, 0);
        assert!(store.head()?.is_none());
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn a_hardlinked_legacy_race_target_is_rejected() -> TestResult {
        use std::os::unix::fs::MetadataExt as _;

        let directory = Directory::new("legacy-hardlink-race");
        let store = directory.store()?;
        let objects = objects(2);
        let shadow = store.root().join("legacy-race-shadow");
        reset(None, false);
        let _reset = Reset;
        LEGACY_WRITER.with_borrow_mut(|writer| {
            *writer = Some((store.clone(), objects[0].clone()));
        });
        LEGACY_WRITER_SHADOW.with_borrow_mut(|path| *path = Some(shadow.clone()));

        assert!(matches!(
            store.write_object_batch(&objects),
            Err(StoreError::Corrupt)
        ));
        assert_eq!(
            fs::symlink_metadata(store.object_path(objects[0].id()))?.nlink(),
            2
        );
        assert_eq!(temporaries(&store)?, 0);
        assert!(store.head()?.is_none());
        Ok(())
    }

    #[test]
    fn live_temporaries_hold_the_same_process_lease_as_store_recovery() -> TestResult {
        let directory = Directory::new("live-lease");
        let store = directory.store()?;
        reset(None, false);
        let _reset = Reset;
        PROCESS_LEASE_PROBE.with_borrow_mut(|path| *path = Some(store.root().join("LOCK")));
        let _durable = store.write_object_batch(&objects(3))?;
        let probe = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(store.root().join("LOCK"))?;
        probe.try_lock()?;
        assert_eq!(temporaries(&store)?, 0);
        Ok(())
    }

    #[test]
    fn crash_during_a_batch_is_unselected_and_restart_reaps_only_private_temps() -> TestResult {
        const CHILD_DIRECTORY: &str = "NUDOX_OBJECT_BATCH_CRASH_DIRECTORY";
        if let Some(path) = std::env::var_os(CHILD_DIRECTORY) {
            let store = FileStore::open(path, 1024 * 1024)?;
            reset(None, false);
            CRASH_PHASE.with(|phase| phase.set(Some(BatchPhase::FirstObjectLinked)));
            let _durable = store.write_object_batch(&objects(32))?;
            std::process::exit(78);
        }
        let directory = Directory::new("crash");
        let status = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "durable::object_batch::tests::crash_during_a_batch_is_unselected_and_restart_reaps_only_private_temps",
                "--nocapture",
            ])
            .env(CHILD_DIRECTORY, &directory.0)
            .status()?;
        assert_eq!(status.code(), Some(77));
        let cold = directory.store()?;
        assert!(cold.head()?.is_none());
        assert_eq!(
            temporaries(&cold)?,
            0,
            "ordinary process-lease recovery reaps the interrupted group"
        );
        let objects = objects(32);
        let mut present = 0;
        for object in &objects {
            if cold.contains_object(object.id())? {
                present += 1;
            }
        }
        assert_eq!(present, 1);
        let retry = cold.write_object_batch(&objects)?;
        assert_eq!(
            retry
                .writes()
                .iter()
                .filter(|write| write.created())
                .count(),
            31
        );
        for object in &objects {
            assert_eq!(cold.read_object(object.id())?, *object);
        }
        assert!(cold.head()?.is_none());
        Ok(())
    }
}
