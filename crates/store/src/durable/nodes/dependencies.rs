//! Dependency completion is scoped to a held GC pin, never inferred from a reference file.

use super::super::{FileStore, Hash, ObjectId, ObjectWriteReceipt, StoreError, artifact_fs, io_error};
use super::super::layout::GcPinLease;
#[cfg(not(windows))]
use super::super::sync_directory;
use super::{NODE_MAX_COUNT, wire};
use crate::UntrustedObjectId;
use backend_version::SchemaIdentity;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Read as _;
use std::marker::PhantomData;

/// A checked dependency closure whose files and directory entries completed their barriers.
/// The receipt cannot outlive the GC custody that protected that observation.
pub(in crate::durable) struct DependencyComplete<'pin> {
    pub(super) object: ObjectId,
    pub(super) schema: SchemaIdentity,
    pub(super) version: Hash,
    _pin: PhantomData<&'pin GcPinLease>,
}

/// One bounded verification memo. Dropping the pin also ends every reuse decision it supports.
pub(in crate::durable) struct RelationAdmission<'pin> {
    store: &'pin FileStore,
    _pin: &'pin GcPinLease,
    complete: HashMap<ObjectId, (SchemaIdentity, Hash)>,
    complete_references: BTreeSet<(SchemaIdentity, Hash)>,
    written_objects: HashSet<ObjectId>,
    streamed_objects: Option<&'pin HashMap<ObjectId, artifact_fs::DurableObjectFile>>,
    #[cfg(windows)]
    directories: DependencyDirectories,
    #[cfg(windows)]
    repair: std::sync::OnceLock<Result<artifact_fs::DurabilityRepair<'pin>, StoreError>>,
    cancellation: Option<&'pin std::sync::atomic::AtomicBool>,
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
enum DependencyFile {
    Object(ObjectId),
    Reference(SchemaIdentity, Hash, ObjectId),
}

/// Two fixed directory capabilities per admission, rather than one retained handle per
/// member. All Windows file opens and the final directory barriers share these objects.
#[cfg(windows)]
struct DependencyDirectories {
    objects: std::fs::File,
    nodes: std::fs::File,
}

#[cfg(windows)]
impl DependencyDirectories {
    fn new(store: &FileStore) -> Result<Self, StoreError> {
        let root = backend_platform::durability::open_directory_readonly_nofollow(&store.root)
            .map_err(|error| io_error(&error))?;
        Ok(Self {
            objects: backend_platform::durability::open_directory_child_nofollow(&root, "objects")
                .map_err(|error| io_error(&error))?,
            nodes: backend_platform::durability::open_directory_child_nofollow(&root, "nodes")
                .map_err(|error| io_error(&error))?,
        })
    }

    fn parent(&self, member: DependencyFile) -> &std::fs::File {
        match member {
            DependencyFile::Object(_) => &self.objects,
            DependencyFile::Reference(..) => &self.nodes,
        }
    }

    fn sync(&self, directory: &str) -> Result<(), StoreError> {
        let parent = match directory {
            "objects" => &self.objects,
            "nodes" => &self.nodes,
            _ => return Err(StoreError::UnsafePath),
        };
        parent.sync_all().map_err(|error| io_error(&error))
    }
}

impl<'pin> RelationAdmission<'pin> {
    pub(in crate::durable) fn new(
        store: &'pin FileStore,
        pin: &'pin GcPinLease,
    ) -> Result<Self, StoreError> {
        if !pin.covers_root(&store.root)? {
            return Err(StoreError::UnsafePath);
        }
        Ok(Self {
            store,
            _pin: pin,
            complete: HashMap::new(),
            complete_references: BTreeSet::new(),
            written_objects: HashSet::new(),
            streamed_objects: None,
            #[cfg(windows)]
            directories: DependencyDirectories::new(store)?,
            #[cfg(windows)]
            repair: std::sync::OnceLock::new(),
            cancellation: None,
        })
    }

    /// Receipts stay in the receive session and are borrowed with its original, still-held
    /// GC lease. A hit is checked against an opened CAS identity again at the barrier.
    pub(in crate::durable) fn streamed(
        store: &'pin FileStore,
        pin: &'pin GcPinLease,
        receipts: &'pin HashMap<ObjectId, artifact_fs::DurableObjectFile>,
    ) -> Result<Self, StoreError> {
        let mut admission = Self::new(store, pin)?;
        admission.streamed_objects = Some(receipts);
        Ok(admission)
    }

    pub(in crate::durable) fn controlled(
        store: &'pin FileStore,
        pin: &'pin GcPinLease,
        cancellation: Option<&'pin std::sync::atomic::AtomicBool>,
    ) -> Result<Self, StoreError> {
        let mut admission = Self::new(store, pin)?;
        admission.cancellation = cancellation;
        Ok(admission)
    }

    pub(super) fn verify_for(&self, store: &FileStore) -> Result<(), StoreError> {
        if self.store.gc_identity == store.gc_identity { Ok(()) } else { Err(StoreError::UnsafePath) }
    }

    pub(super) fn object_written(&mut self, receipt: ObjectWriteReceipt) {
        if receipt.created() { self.written_objects.insert(receipt.id()); }
    }

    pub(super) fn checkpoint(&self, point: u8, object: ObjectId) -> Result<(), StoreError> {
        if self.cancellation.is_some_and(|flag| flag.load(std::sync::atomic::Ordering::Acquire)) {
            return Err(StoreError::Io("relation dependency admission cancelled".to_owned()));
        }
        #[cfg(test)]
        if TEST_DEPENDENCY_FAULT.with(|fault| {
            if fault.get() == Some((point, object)) { fault.take(); true } else { false }
        }) {
            if std::env::var("BACKEND_STORE_DEPENDENCY_CRASH_POINT").ok()
                .and_then(|value| value.parse::<u8>().ok()) == Some(point)
            {
                std::process::exit(160 + i32::from(point));
            }
            return Err(StoreError::Io(format!("injected relation dependency failure {point}")));
        }
        #[cfg(not(test))]
        let _ = (point, object);
        Ok(())
    }

    /// A legacy reference is only a locator. Its entire reachable closure must pass admission.
    pub(in crate::durable) fn existing(
        &mut self,
        schema: SchemaIdentity,
        version: &Hash,
    ) -> Result<Option<DependencyComplete<'pin>>, StoreError> {
        let Some(object) = self.store.read_relation_ref_if_present(schema, version)? else {
            return Ok(None);
        };
        let complete = self.admit(object, Some((schema, *version)))?;
        if complete.is_some() {
            self.reference_installed(schema, version, object, false)?;
        }
        Ok(complete)
    }

    pub(super) fn reference_installed(
        &mut self,
        schema: SchemaIdentity,
        version: &Hash,
        object: ObjectId,
        durable_new: bool,
    ) -> Result<(), StoreError> {
        if self.complete_references.contains(&(schema, *version)) { return Ok(()); }
        if self.complete.get(&object) != Some(&(schema, *version)) {
            return Err(StoreError::Corrupt);
        }
        self.checkpoint(0, object)?;
        if !durable_new {
            self.flush_file(DependencyFile::Reference(schema, *version, object))?;
            #[cfg(windows)]
            self.directories.sync("nodes")?;
            #[cfg(not(windows))]
            sync_directory(&self.store.root.join("nodes"))?;
        }
        self.complete_references.insert((schema, *version));
        Ok(())
    }

    pub(in crate::durable) fn admit_object(
        &mut self,
        object: ObjectId,
    ) -> Result<Option<DependencyComplete<'pin>>, StoreError> {
        self.admit(object, None)
    }

    fn admit(
        &mut self,
        root: ObjectId,
        expected: Option<(SchemaIdentity, Hash)>,
    ) -> Result<Option<DependencyComplete<'pin>>, StoreError> {
        self.checkpoint(0, root)?;
        if let Some(descriptor) = self.complete.get(&root) {
            if expected.is_some_and(|expected| expected != *descriptor) {
                return Err(StoreError::Corrupt);
            }
            return Ok(Some(DependencyComplete { object: root, schema: descriptor.0, version: descriptor.1, _pin: PhantomData }));
        }
        let mut pending = vec![(root, expected)];
        let mut discovered = HashSet::from([root]);
        let mut verified = HashMap::new();
        let mut files = Vec::new();
        if let Some((schema, version)) = expected {
            if !self.complete_references.contains(&(schema, version)) {
                files.push(DependencyFile::Reference(schema, version, root));
            }
        }
        while let Some((object, expected)) = pending.pop() {
            self.checkpoint(0, root)?;
            if let Some(descriptor) = self.complete.get(&object).or_else(|| verified.get(&object)) {
                if expected.is_some_and(|expected| expected != *descriptor) {
                    return Err(StoreError::Corrupt);
                }
                continue;
            }
            let Some((schema, version, references)) = self.store
                .with_verified_object_claim_if_present(UntrustedObjectId::from_bytes(*object.as_bytes()), |view| {
                    let descriptor = (view.schema(), *view.version());
                    if expected.is_some_and(|expected| expected != descriptor) {
                        return Err(StoreError::Corrupt);
                    }
                    let references = if self.store.relation_registry.contains_schema(view.schema()) {
                        self.store.relation_registry.node_references(view.schema(), view.version(), view.bytes())?
                    } else {
                        Default::default()
                    };
                    Ok((view.schema(), *view.version(), references))
                })? else {
                    return Ok(None);
                };
            verified.insert(object, (schema, version));
            if !self.written_objects.contains(&object) {
                files.push(DependencyFile::Object(object));
            }
            for child in references.children {
                let Some(child_object) = self.store.read_relation_ref_if_present(schema, &child)? else {
                    return Ok(None);
                };
                if !self.complete_references.contains(&(schema, child)) {
                    files.push(DependencyFile::Reference(schema, child, child_object));
                }
                discovered.insert(child_object);
                pending.push((child_object, Some((schema, child))));
            }
            for reference in references.value_references {
                let target = ObjectId::from_bytes(reference);
                if discovered.insert(target) {
                    pending.push((target, None));
                }
            }
            if discovered.len().checked_add(self.complete.len()).ok_or(StoreError::Bounds)? > NODE_MAX_COUNT
                || files.len() > NODE_MAX_COUNT.checked_mul(4).ok_or(StoreError::Bounds)?
            {
                return Err(StoreError::Bounds);
            }
        }
        // Legacy files may be visible after a failed directory barrier. Re-observe every file
        // barrier before granting a receipt, then flush names only after all file work joined.
        // A DAG can repeat a reference. Repairing the same read-only name concurrently would
        // make one worker invalidate the other's identity observation, so each name gets one
        // barrier per batch. Sorting/deduplication needs no second per-member allocation.
        files.sort_unstable();
        files.dedup();
        let file_failure = self.checkpoint(3, root).err();
        backend_platform::durability::try_flush_concurrently_bounded(&files, 16, |file| {
            self.checkpoint(0, root)?;
            if matches!(file, DependencyFile::Object(object) if *object == root)
                && let Some(error) = file_failure.as_ref()
            {
                return Err(error.clone());
            }
            self.flush_file(*file)
        }).map_err(|error| match error {
            backend_platform::durability::BarrierError::Operation(error) => error,
            backend_platform::durability::BarrierError::WorkerPanicked => {
                StoreError::Io("durability barrier worker panicked".to_owned())
            }
        })?;
        if let Some(error) = file_failure { return Err(error); }
        for directory in ["objects", "nodes"].into_iter().filter(|_| !files.is_empty()) {
            if directory == "nodes" { self.checkpoint(4, root)?; }
            #[cfg(windows)]
            self.directories.sync(directory)?;
            #[cfg(not(windows))]
            sync_directory(&self.store.root.join(directory))?;
        }
        self.checkpoint(0, root)?;
        self.complete.extend(verified);
        self.complete_references.extend(files.iter().filter_map(|file| match file {
            DependencyFile::Reference(schema, version, _) => Some((*schema, *version)),
            DependencyFile::Object(_) => None,
        }));
        let (schema, version) = self.complete.get(&root).copied().ok_or(StoreError::Corrupt)?;
        Ok(Some(DependencyComplete { object: root, schema, version, _pin: PhantomData }))
    }

    fn flush_file(&self, member: DependencyFile) -> Result<(), StoreError> {
        #[cfg(windows)]
        let parent = self.directories.parent(member);
        #[allow(unused_mut)]
        let mut file = match member {
            DependencyFile::Object(object) => {
                #[cfg(windows)]
                { artifact_fs::open_directory_file(parent, &format!("{}.object", super::super::hex(object.as_bytes())))? }
                #[cfg(not(windows))]
                { artifact_fs::open_object(self.store, object)? }
            }
            DependencyFile::Reference(schema, version, object) => {
                let path = self.store.relation_ref_path(schema, &version);
                let name = path.file_name().and_then(|name| name.to_str()).ok_or(StoreError::Corrupt)?;
                #[cfg(windows)]
                let opened = artifact_fs::open_directory_file(parent, name)?;
                #[cfg(not(windows))]
                let opened = artifact_fs::open_store_file(self.store, "nodes", name)?;
                let Some(mut file) = opened else {
                    return Err(StoreError::Corrupt);
                };
                let mut bytes = Vec::new();
                (&mut file).take(u64::try_from(wire::relation_ref_encoded_len()).map_err(|_| StoreError::Bounds)? + 1)
                    .read_to_end(&mut bytes).map_err(|error| io_error(&error))?;
                if wire::decode_relation_ref(&bytes, schema, &version)? != object {
                    return Err(StoreError::Corrupt);
                }
                Some(file)
            }
        }.ok_or(StoreError::Corrupt)?;
        if let DependencyFile::Object(object) = member
            && let Some(receipt) = self
                .streamed_objects
                .and_then(|objects| objects.get(&object))
            && receipt.matches(object, &file)?
        {
            return Ok(());
        }
        #[cfg(windows)]
        {
            let (_directory, path, maximum) = match member {
                DependencyFile::Object(object) => (
                    "objects",
                    self.store.object_path(object),
                    u64::try_from(self.store.object_envelope_limit()?)
                        .map_err(|_| StoreError::Bounds)?,
                ),
                DependencyFile::Reference(schema, version, _) => (
                    "nodes",
                    self.store.relation_ref_path(schema, &version),
                    u64::try_from(wire::relation_ref_encoded_len())
                        .map_err(|_| StoreError::Bounds)?,
                ),
            };
            let name = path.file_name().and_then(|name| name.to_str()).ok_or(StoreError::UnsafePath)?;
            // Ordinary references are writable and need no replacement. Read-only references
            // and all objects without a matching inode receipt need a fresh actual barrier;
            // their read-only bit, content hash, and metadata are never a durability receipt.
            if matches!(member, DependencyFile::Reference(..))
                && !file
                    .metadata()
                    .map_err(|error| io_error(&error))?
                    .permissions()
                    .readonly()
            {
                let writable =
                    backend_platform::durability::open_regular_file_readwrite_child_nofollow(parent, name)
                        .map_err(|error| io_error(&error))?;
                if backend_platform::FileIdentity::of_file(&file)
                    .map_err(|error| io_error(&error))?
                    != backend_platform::FileIdentity::of_file(&writable)
                        .map_err(|error| io_error(&error))?
                {
                    return Err(StoreError::UnsafePath);
                }
                return writable.sync_all().map_err(|error| io_error(&error));
            }
            let repair = self
                .repair
                .get_or_init(|| artifact_fs::DurabilityRepair::new(self.store, self._pin))
                .as_ref()
                .map_err(Clone::clone)?;
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or(StoreError::UnsafePath)?;
            return repair.replace_verified(parent, name, &mut file, maximum, |mut copy| {
                match member {
                    DependencyFile::Object(object) => {
                        super::super::artifact::verify_object_file_handle(self.store, object, copy)
                    }
                    DependencyFile::Reference(schema, version, object) => {
                        let mut bytes = Vec::new();
                        (&mut copy)
                            .take(maximum + 1)
                            .read_to_end(&mut bytes)
                            .map_err(|error| io_error(&error))?;
                        if wire::decode_relation_ref(&bytes, schema, &version)? != object {
                            return Err(StoreError::Corrupt);
                        }
                        Ok(())
                    }
                }
            });
        }
        #[cfg(not(windows))]
        file.sync_all().map_err(|error| io_error(&error))
    }
}

#[cfg(test)]
std::thread_local! {
    static TEST_DEPENDENCY_FAULT: std::cell::Cell<Option<(u8, ObjectId)>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) fn set_test_fault(point: u8, object: ObjectId) {
    TEST_DEPENDENCY_FAULT.with(|fault| fault.set(Some((point, object))));
}
