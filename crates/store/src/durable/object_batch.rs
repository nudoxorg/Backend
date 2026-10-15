//! Batched admission of immutable files with shared durability barriers.
//!
//! [`FileStore::write_object`] makes each object durable on its own: a flush of the new file,
//! then a flush of the directory that names it. A writer that admits thousands of immutable
//! files before it can commit anything therefore waits for two device flushes per file, one
//! after another. A [`DurableBatch`] keeps the same per-file guarantee while paying for the
//! flushes together:
//!
//! 1. each file is written to a private temporary file that no reader can name, and sealed
//!    read-only where the caller asks for it;
//! 2. the temporary files are flushed concurrently, so the device merges them into very few
//!    cache flushes;
//! 3. only then is each file linked under its final name, so a final name never refers to bytes
//!    that were not already flushed (a crash cannot leave a torn file that a retry would mistake
//!    for corruption);
//! 4. every directory that gained an entry is flushed once, covering all of its links.
//!
//! [`DurableBatch::commit`] returns only after step 4. A caller must commit the batch before it
//! writes any record, closure, or selection that makes the batch's files reachable. Dropping a
//! batch without committing abandons it: its temporary files are removed, and files it already
//! linked stay unreferenced until something names them.

use super::objects::create_temp;
use super::{
    FileStore, ObjectId, PathBuf, StoreError, TypedObject, encode_object, envelope_limit, fs,
    io_error,
};
use std::collections::HashSet;
use std::fs::File;
use std::io::Write as _;
use std::path::Path;

/// The most temporary files a batch keeps open before it seals them.
///
/// Bounds the open descriptors and the dirty data one batch holds. Sealing flushes the files
/// concurrently, so a larger bound only trades memory for fewer flush rounds.
const SEAL_FILES: usize = 64;

struct PendingFile {
    temporary: PathBuf,
    target: PathBuf,
    directory: usize,
    file: File,
}

/// Immutable files being admitted together; see the module documentation for the protocol.
pub(super) struct DurableBatch {
    open: Vec<PendingFile>,
    /// Directories that gained, or whose existing entries this batch depends on.
    directories: Vec<PathBuf>,
    /// Whether each directory has entries this batch has not flushed yet.
    dirty: Vec<bool>,
    /// Objects admitted by this batch: durable once it commits, so a sibling object in the same
    /// batch may name them even before they are linked.
    objects: HashSet<ObjectId>,
    /// Test fault: the next file flush fails, to prove nothing is linked before it succeeds.
    #[cfg(test)]
    fail_flush: bool,
}

impl DurableBatch {
    pub(super) fn new() -> Self {
        Self {
            open: Vec::new(),
            directories: Vec::new(),
            dirty: Vec::new(),
            objects: HashSet::new(),
            #[cfg(test)]
            fail_flush: false,
        }
    }

    /// Whether this batch admitted the object (it is durable once the batch commits).
    pub(super) fn has_object(&self, id: ObjectId) -> bool {
        self.objects.contains(&id)
    }

    pub(super) fn note_object(&mut self, id: ObjectId) {
        self.objects.insert(id);
    }

    fn directory_slot(&mut self, directory: &Path) -> usize {
        if let Some(slot) = self.directories.iter().position(|known| known == directory) {
            return slot;
        }
        self.directories.push(directory.to_path_buf());
        self.dirty.push(false);
        self.directories.len() - 1
    }

    /// Adds one immutable file and returns whether this call staged a new file.
    ///
    /// `false` means `target` already holds exactly `bytes`. The directory is still marked for
    /// the commit flush, because the existing entry may have been linked by a writer that has
    /// not flushed it yet and this batch's commit record will depend on it. A different file at
    /// `target` is corruption. `read_only` seals the file owner-read-only before it is flushed,
    /// so its first read needs no permission change and no second flush.
    pub(super) fn add(
        &mut self,
        directory: &Path,
        target: PathBuf,
        bytes: &[u8],
        read_only: bool,
    ) -> Result<bool, StoreError> {
        let slot = self.directory_slot(directory);
        if let Ok(existing) = fs::read(&target) {
            if existing != bytes {
                return Err(StoreError::Corrupt);
            }
            self.dirty[slot] = true;
            return Ok(false);
        }
        let (temporary, mut file) = create_temp(&target)?;
        let written = file.write_all(bytes).and_then(|()| {
            #[cfg(unix)]
            if read_only {
                use std::os::unix::fs::PermissionsExt as _;
                file.set_permissions(fs::Permissions::from_mode(0o400))?;
            }
            #[cfg(not(unix))]
            let _ = read_only;
            Ok(())
        });
        if let Err(error) = written {
            let _ = fs::remove_file(&temporary);
            return Err(io_error(&error));
        }
        self.open.push(PendingFile {
            temporary,
            target,
            directory: slot,
            file,
        });
        if self.open.len() >= SEAL_FILES {
            self.seal()?;
        }
        Ok(true)
    }

    /// Flushes the pending temporary files together, then links them under their final names.
    fn seal(&mut self) -> Result<(), StoreError> {
        let pending = std::mem::take(&mut self.open);
        if pending.is_empty() {
            return Ok(());
        }
        #[cfg(test)]
        let fail_flush = self.fail_flush;
        #[cfg(not(test))]
        let fail_flush = false;
        let result = (|| {
            backend_platform::durability::flush_concurrently_bounded(
                &pending,
                SEAL_FILES,
                |pending| {
                    if fail_flush {
                        return Err(std::io::Error::other("injected flush failure"));
                    }
                    pending.file.sync_all()
                },
            )
            .map_err(|error| io_error(&error))?;
            for pending in &pending {
                match fs::hard_link(&pending.temporary, &pending.target) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        // A concurrent writer won the name; it must hold identical bytes.
                        let mine =
                            fs::read(&pending.temporary).map_err(|error| io_error(&error))?;
                        let existing =
                            fs::read(&pending.target).map_err(|error| io_error(&error))?;
                        if mine != existing {
                            return Err(StoreError::Corrupt);
                        }
                    }
                    Err(error) => return Err(io_error(&error)),
                }
                // Readers accept a second link only while the temporary name is being removed,
                // so it is removed right after its link, not after the whole group.
                let _ = fs::remove_file(&pending.temporary);
                self.dirty[pending.directory] = true;
            }
            Ok(())
        })();
        for pending in &pending {
            let _ = fs::remove_file(&pending.temporary);
        }
        result
    }

    /// Makes every file of the batch durable and reachable by name.
    ///
    /// Returns only after the temporary files, and then each directory that gained entries, are
    /// flushed.
    pub(super) fn commit(mut self) -> Result<(), StoreError> {
        self.seal()?;
        let dirty = self
            .directories
            .iter()
            .zip(&self.dirty)
            .filter_map(|(directory, dirty)| dirty.then_some(directory))
            .collect::<Vec<_>>();
        backend_platform::durability::flush_concurrently(&dirty, |directory| {
            backend_platform::durability::open_directory(directory)
                .and_then(|directory| directory.sync_all())
        })
        .map_err(|error| io_error(&error))?;
        self.dirty.fill(false);
        Ok(())
    }
}

impl Drop for DurableBatch {
    fn drop(&mut self) {
        for pending in self.open.drain(..) {
            let _ = fs::remove_file(&pending.temporary);
        }
    }
}

/// A group of immutable-object admissions that share their durability barriers.
///
/// Obtain one with [`FileStore::object_batch`]. Objects are checked like
/// [`FileStore::write_object`] and become durable together at [`Self::commit`].
pub struct ObjectBatch<'store> {
    store: &'store FileStore,
    batch: DurableBatch,
}

impl FileStore {
    /// Starts a batch that admits immutable objects with shared durability barriers.
    ///
    /// Commit the batch before writing any record that makes its objects reachable.
    #[must_use]
    pub fn object_batch(&self) -> ObjectBatch<'_> {
        ObjectBatch {
            store: self,
            batch: DurableBatch::new(),
        }
    }

    /// Adds an object to `batch` like [`Self::write_object`] writes it. Returns the object's
    /// identity, whether this call staged a new file, and the encoded length.
    pub(super) fn batch_object(
        &self,
        batch: &mut DurableBatch,
        object: &TypedObject,
        limit: usize,
    ) -> Result<(ObjectId, bool, usize), StoreError> {
        object.verify_wire_version(&self.relation_registry)?;
        self.verify_object_value_targets_in(object, Some(&*batch))?;
        let encoded = encode_object(object, limit)?;
        let created = batch.add(
            &self.root.join("objects"),
            self.object_path(object.id()),
            &encoded,
            true,
        )?;
        batch.note_object(object.id());
        Ok((object.id(), created, encoded.len()))
    }
}

impl ObjectBatch<'_> {
    /// Admits one typed object into the batch and returns its identity.
    ///
    /// The object is not crash-durable, and an object that was not already in the store is not
    /// readable by name, until the batch commits.
    ///
    /// # Errors
    ///
    /// Returns an admission, bounds, corruption, or filesystem error.
    pub fn write(&mut self, object: &TypedObject) -> Result<ObjectId, StoreError> {
        let limit = envelope_limit(self.store.max_pack_bytes)?;
        self.store
            .batch_object(&mut self.batch, object, limit)
            .map(|(id, _, _)| id)
    }

    /// Makes every object of the batch durable and reachable by name.
    ///
    /// Returns only after the objects' files, and then the `objects` directory entries naming
    /// them, are flushed. Write the record that references the batch's objects afterwards.
    ///
    /// # Errors
    ///
    /// Returns a filesystem or corruption error; the batch's objects must then not be
    /// referenced.
    pub fn commit(self) -> Result<(), StoreError> {
        self.batch.commit()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use backend_version::{ObjectKey, Schema};

    struct BytesSchema;

    impl Schema for BytesSchema {
        const DOMAIN: u8 = 0xf2;
        const TYPE: u16 = 0x5102;
        type Value = Vec<u8>;

        fn encode(value: &Self::Value, output: &mut Vec<u8>) {
            output.extend_from_slice(value);
        }
    }

    fn test_store(label: &str) -> (FileStore, PathBuf) {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nonce = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "backend-store-object-batch-{label}-{}-{nonce}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let store = FileStore::open(&root, 1024 * 1024).expect("open test store");
        (store, root)
    }

    fn object(seed: usize) -> TypedObject {
        let bytes = format!("batched object {seed}").into_bytes();
        let key = ObjectKey::<BytesSchema>::from_value(&bytes);
        TypedObject::from_value(&key, &bytes)
    }

    fn temporaries(store: &FileStore) -> usize {
        fs::read_dir(store.root().join("objects"))
            .expect("list objects")
            .filter(|entry| {
                entry
                    .as_ref()
                    .is_ok_and(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            })
            .count()
    }

    #[test]
    fn committed_objects_read_back_exactly_across_several_seals() {
        let (store, root) = test_store("commit");
        // More than two seal groups, so objects become nameable before the commit too.
        let objects = (0..SEAL_FILES * 2 + 5).map(object).collect::<Vec<_>>();
        let mut batch = store.object_batch();
        for object in &objects {
            assert_eq!(batch.write(object).expect("stage object"), object.id());
        }
        batch.commit().expect("commit batch");
        assert_eq!(temporaries(&store), 0, "no temporary survives a commit");
        for object in &objects {
            assert_eq!(store.read_object(object.id()).expect("read back"), *object);
        }
        // A second batch of the same objects finds them present and stays consistent.
        let mut again = store.object_batch();
        for object in &objects {
            again.write(object).expect("present object");
        }
        again.commit().expect("commit present objects");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn an_uncommitted_batch_leaves_no_temporary_and_no_unsealed_object() {
        let (store, root) = test_store("abandon");
        let objects = (0..3).map(object).collect::<Vec<_>>();
        let mut batch = store.object_batch();
        for object in &objects {
            batch.write(object).expect("stage object");
        }
        assert!(
            objects
                .iter()
                .all(|object| !store.contains_object(object.id()).expect("probe")),
            "an unsealed object is not nameable"
        );
        drop(batch);
        assert_eq!(temporaries(&store), 0, "dropping removes the temporaries");
        assert!(
            objects
                .iter()
                .all(|object| !store.contains_object(object.id()).expect("probe")),
            "an abandoned batch admits nothing"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn nothing_is_linked_under_its_address_unless_its_file_flush_succeeded() {
        let (store, root) = test_store("flush-failure");
        let objects = (0..5).map(object).collect::<Vec<_>>();
        let mut batch = DurableBatch::new();
        batch.fail_flush = true;
        let limit = envelope_limit(store.max_pack_bytes).expect("limit");
        for object in &objects {
            store.batch_object(&mut batch, object, limit).expect("stage");
        }
        assert!(batch.commit().is_err(), "a failed flush fails the commit");
        assert!(
            objects
                .iter()
                .all(|object| !store.contains_object(object.id()).expect("probe")),
            "no address names bytes whose flush failed"
        );
        assert_eq!(temporaries(&store), 0, "the failed batch cleans its temporaries");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_conflicting_object_at_the_address_is_corruption() {
        let (store, root) = test_store("conflict");
        let object = object(1);
        let path = store.object_path(object.id());
        fs::write(&path, b"not the object").expect("plant conflicting file");
        let mut batch = store.object_batch();
        assert!(matches!(batch.write(&object), Err(StoreError::Corrupt)));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn batch_objects_are_sealed_and_single_linked_like_individual_writes() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let (store, root) = test_store("sealed");
        let object = object(9);
        let mut batch = store.object_batch();
        batch.write(&object).expect("stage object");
        batch.commit().expect("commit");
        let metadata = fs::metadata(store.object_path(object.id())).expect("object metadata");
        assert_eq!(metadata.permissions().mode() & 0o7777, 0o400);
        assert_eq!(metadata.nlink(), 1, "the temporary link is gone");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn concurrent_batches_of_the_same_objects_agree() {
        let (store, root) = test_store("race");
        let objects = (0..100).map(object).collect::<Vec<_>>();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    let mut batch = store.object_batch();
                    for object in &objects {
                        batch.write(object).expect("stage object");
                    }
                    batch.commit().expect("commit racing batch");
                });
            }
        });
        for object in &objects {
            assert_eq!(store.read_object(object.id()).expect("read back"), *object);
        }
        assert_eq!(temporaries(&store), 0);
        let _ = fs::remove_dir_all(root);
    }
}
