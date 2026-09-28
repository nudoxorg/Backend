//! Durable sink for the canonical receiving CAS.

use super::CasGcBudget;
use backend_engine::{
    CanonicalDigest, ChunkChainDigest, ObjectKey, ObjectVersion, ReceivingCasSink,
    ReceivingCheckpoint, ReplicationError, Schema, SchemaWireObjectKey, SchemaWireObjectVersion,
    StagedExtent, TransferId, WireReceivingCheckpoint,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(super) struct SinkSession<T: Schema> {
    transfer: TransferId,
    key: Option<ObjectKey<T>>,
    version: Option<ObjectVersion<T>>,
    len: u64,
    extents: BTreeMap<[u8; 32], StagedExtent>,
    bytes: BTreeMap<[u8; 32], Arc<[u8]>>,
    temporary: Option<PathBuf>,
}

/// Durable extent sink shared by every receiving typestate in one worker.
#[derive(Debug)]
pub(super) struct DurableSink<T: Schema> {
    pub(super) root: Option<PathBuf>,
    pub(super) committed: BTreeMap<ObjectVersion<T>, Arc<[u8]>>,
    retained_objects: usize,
    retained_bytes: u64,
    max_retained_objects: usize,
    max_retained_bytes: u64,
    max_object_bytes: usize,
    reservations: BTreeMap<TransferId, Reservation<T>>,
    marker: PhantomData<fn() -> T>,
}

#[derive(Clone, Copy, Debug)]
struct Reservation<T: Schema> {
    version: Option<ObjectVersion<T>>,
    len: u64,
    replaced_bytes: u64,
    adds_object: bool,
}

impl<T: Schema> DurableSink<T> {
    pub(super) fn open_with_budget(
        path: impl AsRef<Path>,
        max_retained_objects: usize,
        max_retained_bytes: u64,
        max_object_bytes: u64,
    ) -> Result<Self, ReplicationError> {
        if max_retained_objects == 0 || max_retained_bytes == 0 || max_object_bytes == 0 {
            return Err(ReplicationError::InvalidLimits);
        }
        let max_object_bytes =
            usize::try_from(max_object_bytes).map_err(|_| ReplicationError::InvalidLimits)?;
        let path = path.as_ref();
        fs::create_dir_all(path).map_err(|_| ReplicationError::Disconnected)?;
        let root_metadata =
            fs::symlink_metadata(path).map_err(|_| ReplicationError::Disconnected)?;
        if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
            return Err(ReplicationError::CorruptFrame);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut mode = root_metadata.permissions();
            mode.set_mode(0o700);
            fs::set_permissions(path, mode).map_err(|_| ReplicationError::Disconnected)?;
        }
        let mut sink = Self {
            root: Some(path.to_owned()),
            committed: BTreeMap::new(),
            retained_objects: 0,
            retained_bytes: 0,
            max_retained_objects,
            max_retained_bytes,
            max_object_bytes,
            reservations: BTreeMap::new(),
            marker: PhantomData,
        };
        sink.refresh_usage()?;
        Ok(sink)
    }

    pub(super) fn usage(&self) -> (usize, u64) {
        (self.retained_objects, self.retained_bytes)
    }

    pub(super) fn restore_partial_reservation(
        &mut self,
        transfer: TransferId,
        len: u64,
    ) -> Result<(), ReplicationError> {
        let path = self
            .temporary_path(transfer)
            .ok_or(ReplicationError::CorruptFrame)?;
        let file = open_optional_readonly_nofollow(&path)?.ok_or(ReplicationError::CorruptFrame)?;
        if file
            .metadata()
            .map_err(|_| ReplicationError::Disconnected)?
            .len()
            != len
        {
            return Err(ReplicationError::CorruptFrame);
        }
        self.reserve(transfer, None, len)
    }

    pub(super) fn discard_partial(&mut self, transfer: TransferId) -> Result<(), ReplicationError> {
        self.release_reservation(transfer);
        let Some(path) = self.temporary_path(transfer) else {
            return Ok(());
        };
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(ReplicationError::Disconnected),
        }
        if let Some(root) = &self.root {
            backend_platform::durability::open_directory(root)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| ReplicationError::Disconnected)?;
        }
        Ok(())
    }

    pub(super) fn within_budget(&self) -> bool {
        self.retained_objects <= self.max_retained_objects
            && self.retained_bytes <= self.max_retained_bytes
    }

    fn refresh_usage(&mut self) -> Result<(), ReplicationError> {
        let Some(root) = self.root.as_ref() else {
            self.retained_objects = self.committed.len();
            self.retained_bytes = self.committed.values().try_fold(0_u64, |total, value| {
                total
                    .checked_add(value.len() as u64)
                    .ok_or(ReplicationError::Overflow)
            })?;
            return Ok(());
        };
        let mut objects = 0_usize;
        let mut bytes = 0_u64;
        for entry in fs::read_dir(root).map_err(|_| ReplicationError::Disconnected)? {
            let entry = entry.map_err(|_| ReplicationError::Disconnected)?;
            if entry.file_name().to_str().is_some_and(is_object_filename) {
                let metadata = entry
                    .file_type()
                    .map_err(|_| ReplicationError::Disconnected)?;
                if !metadata.is_file() {
                    return Err(ReplicationError::CorruptFrame);
                }
                let file = open_optional_readonly_nofollow(&entry.path())?
                    .ok_or(ReplicationError::CorruptFrame)?;
                objects = objects.checked_add(1).ok_or(ReplicationError::Overflow)?;
                bytes = bytes
                    .checked_add(
                        file.metadata()
                            .map_err(|_| ReplicationError::Disconnected)?
                            .len(),
                    )
                    .ok_or(ReplicationError::Overflow)?;
            }
        }
        self.retained_objects = objects;
        self.retained_bytes = bytes;
        Ok(())
    }

    fn object_path(&self, version: ObjectVersion<T>) -> Option<PathBuf> {
        self.root
            .as_ref()
            .map(|root| root.join(hex(version.to_bytes())))
    }

    pub(super) fn contains(&self, version: ObjectVersion<T>) -> bool {
        if let Some(bytes) = self.committed.get(&version) {
            return backend_engine::canonical_object_digest::<T>(
                bytes.len() as u64,
                [bytes.as_ref()],
            ) == version.to_bytes();
        }
        let Some(path) = self.object_path(version) else {
            return false;
        };
        let Ok(Some(file)) = open_optional_readonly_nofollow(&path) else {
            return false;
        };
        let Ok(metadata) = file.metadata() else {
            return false;
        };
        digest_open_file::<T>(file, metadata.len()).is_ok_and(|digest| digest == version.to_bytes())
    }

    pub(super) fn contains_claims(
        &self,
        key: SchemaWireObjectKey<T>,
        version: SchemaWireObjectVersion<T>,
    ) -> Result<bool, ReplicationError> {
        if key.context() != backend_engine::IdContext::object_key::<T>()
            || version.context() != backend_engine::IdContext::schema::<T>()
        {
            return Err(ReplicationError::IdentityContext);
        }
        if let Some(bytes) = self.committed.iter().find_map(|(stored, bytes)| {
            (stored.as_bytes() == version.as_bytes()).then_some(bytes.as_ref())
        }) {
            let mut digest = CanonicalDigest::<T>::new(bytes.len() as u64);
            digest.push(0, bytes)?;
            let (actual_key, actual_version) = digest.finish_identities()?;
            return Ok(actual_key.as_bytes() == key.as_bytes()
                && actual_version.as_bytes() == version.as_bytes());
        }
        let Some(root) = self.root.as_ref() else {
            return Ok(false);
        };
        let path = root.join(hex(*version.as_bytes()));
        let Some(mut file) = open_optional_readonly_nofollow(&path)? else {
            return Ok(false);
        };
        let metadata = file
            .metadata()
            .map_err(|_| ReplicationError::Disconnected)?;
        let mut digest = CanonicalDigest::<T>::new(metadata.len());
        let mut offset = 0_u64;
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|_| ReplicationError::Disconnected)?;
            if read == 0 {
                break;
            }
            digest.push(offset, &buffer[..read])?;
            offset = offset
                .checked_add(read as u64)
                .ok_or(ReplicationError::Overflow)?;
        }
        let (actual_key, actual_version) = digest.finish_identities()?;
        Ok(actual_key.as_bytes() == key.as_bytes()
            && actual_version.as_bytes() == version.as_bytes())
    }

    pub(super) fn contains_wire_claim(&self, claim: [u8; 32]) -> bool {
        if let Some((version, bytes)) = self
            .committed
            .iter()
            .find(|(version, _)| version.as_bytes() == &claim)
        {
            return backend_engine::canonical_object_digest::<T>(
                bytes.len() as u64,
                [bytes.as_ref()],
            ) == version.to_bytes();
        }
        let Some(root) = self.root.as_ref() else {
            return false;
        };
        let path = root.join(hex(claim));
        let Ok(Some(file)) = open_optional_readonly_nofollow(&path) else {
            return false;
        };
        let Ok(metadata) = file.metadata() else {
            return false;
        };
        digest_open_file::<T>(file, metadata.len()).is_ok_and(|digest| digest == claim)
    }

    /// Removes object files which are not reachable from the caller's live
    /// root lease.  The sweep is intentionally conservative about unknown
    /// sidecar files: only exact 64-hex object names are candidates, so a
    /// malformed marker can never authorize deletion of a canonical object.
    pub(super) fn reclaim_unleased(
        &mut self,
        live_versions: &BTreeSet<[u8; 32]>,
        budget: &mut CasGcBudget,
    ) -> Result<(usize, u64), ReplicationError> {
        // Refresh counters before sweeping so files materialized by a
        // recovery step or an external durable writer cannot make the
        // accounting subtraction underflow.
        self.refresh_usage()?;
        let mut removed_objects = 0_usize;
        let mut removed_bytes = 0_u64;
        if let Some(root) = self.root.clone() {
            for entry in fs::read_dir(&root)
                .map_err(|_| ReplicationError::Disconnected)?
                .take(super::MAX_GC_FILES_PER_CALL)
            {
                let entry = entry.map_err(|_| ReplicationError::Disconnected)?;
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                if !is_object_filename(name) {
                    continue;
                }
                let Some(version) = decode_hex(name) else {
                    continue;
                };
                if live_versions.contains(&version) {
                    continue;
                }
                let file_type = entry
                    .file_type()
                    .map_err(|_| ReplicationError::Disconnected)?;
                if !file_type.is_file() {
                    return Err(ReplicationError::CorruptFrame);
                }
                let file = open_optional_readonly_nofollow(&entry.path())?
                    .ok_or(ReplicationError::CorruptFrame)?;
                let metadata = file
                    .metadata()
                    .map_err(|_| ReplicationError::Disconnected)?;
                drop(file);
                if !budget.take(metadata.len()) {
                    break;
                }
                fs::remove_file(entry.path()).map_err(|_| ReplicationError::Disconnected)?;
                removed_objects = removed_objects
                    .checked_add(1)
                    .ok_or(ReplicationError::Overflow)?;
                removed_bytes = removed_bytes
                    .checked_add(metadata.len())
                    .ok_or(ReplicationError::Overflow)?;
                self.retained_objects = self.retained_objects.saturating_sub(1);
                self.retained_bytes = self
                    .retained_bytes
                    .checked_sub(metadata.len())
                    .ok_or(ReplicationError::CorruptFrame)?;
            }
            backend_platform::durability::open_directory(&root)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| ReplicationError::Disconnected)?;
        }
        self.committed
            .retain(|version, _| live_versions.contains(version.as_bytes()));
        Ok((removed_objects, removed_bytes))
    }

    fn reserve(
        &mut self,
        transfer: TransferId,
        version: Option<ObjectVersion<T>>,
        len: u64,
    ) -> Result<(), ReplicationError> {
        if self.reservations.contains_key(&transfer)
            || version.is_some_and(|version| {
                self.reservations
                    .values()
                    .any(|reservation| reservation.version == Some(version))
            })
        {
            return Err(ReplicationError::ReplayConflict);
        }
        let (replaced_bytes, object_exists) = if let Some(version) = version {
            let path = self
                .object_path(version)
                .ok_or(ReplicationError::Disconnected)?;
            let existing = open_optional_readonly_nofollow(&path)?;
            let replaced_bytes = if let Some(file) = existing.as_ref() {
                file.metadata()
                    .map_err(|_| ReplicationError::Disconnected)?
                    .len()
            } else {
                self.committed
                    .get(&version)
                    .map_or(0, |bytes| bytes.len() as u64)
            };
            (
                replaced_bytes,
                existing.is_some() || self.committed.contains_key(&version),
            )
        } else {
            (0, false)
        };
        let adds_object = !object_exists;
        let reserved_objects = self
            .reservations
            .values()
            .filter(|reservation| reservation.adds_object)
            .count();
        let reserved_bytes = self
            .reservations
            .values()
            .try_fold(0_u64, |total, reservation| {
                total
                    .checked_sub(reservation.replaced_bytes)
                    .and_then(|value| value.checked_add(reservation.len))
                    .ok_or(ReplicationError::Overflow)
            })?;
        let projected_objects = self
            .retained_objects
            .checked_add(reserved_objects)
            .and_then(|value| value.checked_add(usize::from(adds_object)))
            .ok_or(ReplicationError::Overflow)?;
        let projected_bytes = self
            .retained_bytes
            .checked_add(reserved_bytes)
            .and_then(|value| value.checked_sub(replaced_bytes))
            .and_then(|value| value.checked_add(len))
            .ok_or(ReplicationError::Overflow)?;
        if projected_objects > self.max_retained_objects
            || projected_bytes > self.max_retained_bytes
        {
            return Err(ReplicationError::Backpressure);
        }
        self.reservations.insert(
            transfer,
            Reservation {
                version,
                len,
                replaced_bytes,
                adds_object,
            },
        );
        Ok(())
    }

    fn release_reservation(&mut self, transfer: TransferId) -> Option<Reservation<T>> {
        self.reservations.remove(&transfer)
    }

    fn temporary_path(&self, transfer: TransferId) -> Option<PathBuf> {
        self.root
            .as_ref()
            .map(|root| root.join(format!(".{:016x}.part", transfer.get())))
    }

    fn create_sparse_staging(
        &self,
        transfer: TransferId,
        len: u64,
    ) -> Result<Option<PathBuf>, ReplicationError> {
        let Some(path) = self.temporary_path(transfer) else {
            return Ok(None);
        };
        let file = match OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(ReplicationError::ReplayConflict);
            }
            Err(_) => return Err(ReplicationError::Disconnected),
        };
        if file.set_len(len).and_then(|()| file.sync_all()).is_err() {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(ReplicationError::Disconnected);
        }
        Ok(Some(path))
    }

    pub(super) fn read(
        &mut self,
        version: ObjectVersion<T>,
    ) -> Result<Option<Arc<[u8]>>, ReplicationError> {
        if let Some(bytes) = self.committed.get(&version) {
            if backend_engine::canonical_object_digest::<T>(bytes.len() as u64, [bytes.as_ref()])
                != version.to_bytes()
            {
                return Err(ReplicationError::CorruptFrame);
            }
            return Ok(Some(Arc::clone(bytes)));
        }
        let Some(path) = self.object_path(version) else {
            return Ok(None);
        };
        let Some(file) = open_optional_readonly_nofollow(&path)? else {
            return Ok(None);
        };
        let len = file
            .metadata()
            .map_err(|_| ReplicationError::Disconnected)?
            .len();
        if len > self.max_object_bytes as u64 {
            return Err(ReplicationError::MessageTooLarge);
        }
        let capacity = usize::try_from(len).map_err(|_| ReplicationError::MessageTooLarge)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(capacity)
            .map_err(|_| ReplicationError::Backpressure)?;
        file.take((self.max_object_bytes as u64).saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| ReplicationError::Disconnected)?;
        if bytes.len() > self.max_object_bytes {
            return Err(ReplicationError::MessageTooLarge);
        }
        let bytes: Arc<[u8]> = Arc::from(bytes.into_boxed_slice());
        if backend_engine::canonical_object_digest::<T>(bytes.len() as u64, [bytes.as_ref()])
            != version.to_bytes()
        {
            return Err(ReplicationError::CorruptFrame);
        }
        self.committed.insert(version, Arc::clone(&bytes));
        Ok(Some(bytes))
    }
}

impl<T: Schema> ReceivingCasSink<T> for DurableSink<T> {
    type Session = SinkSession<T>;
    type Receipt = ObjectVersion<T>;

    fn begin(
        &mut self,
        transfer: TransferId,
        key: ObjectKey<T>,
        version: ObjectVersion<T>,
        len: u64,
    ) -> Result<Self::Session, ReplicationError> {
        self.reserve(transfer, Some(version), len)?;
        let temporary = match self.create_sparse_staging(transfer, len) {
            Ok(path) => path,
            Err(error) => {
                let _ = self.release_reservation(transfer);
                return Err(error);
            }
        };
        Ok(SinkSession {
            transfer,
            key: Some(key),
            version: Some(version),
            len,
            extents: BTreeMap::new(),
            bytes: BTreeMap::new(),
            temporary,
        })
    }

    fn begin_unverified(
        &mut self,
        transfer: TransferId,
        len: u64,
    ) -> Result<Self::Session, ReplicationError> {
        self.reserve(transfer, None, len)?;
        let temporary = match self.create_sparse_staging(transfer, len) {
            Ok(path) => path,
            Err(error) => {
                let _ = self.release_reservation(transfer);
                return Err(error);
            }
        };
        Ok(SinkSession {
            transfer,
            key: None,
            version: None,
            len,
            extents: BTreeMap::new(),
            bytes: BTreeMap::new(),
            temporary,
        })
    }

    fn resume(
        &mut self,
        transfer: TransferId,
        key: ObjectKey<T>,
        version: ObjectVersion<T>,
        len: u64,
        checkpoint: &ReceivingCheckpoint<T>,
    ) -> Result<Self::Session, ReplicationError> {
        self.reserve(transfer, Some(version), len)?;
        let temporary = self.temporary_path(transfer).ok_or_else(|| {
            let _ = self.release_reservation(transfer);
            ReplicationError::Disconnected
        })?;
        if open_optional_readonly_nofollow(&temporary)?.is_none() {
            let _ = self.release_reservation(transfer);
            return Err(ReplicationError::Disconnected);
        }
        let bytes = BTreeMap::new();
        let mut extents = BTreeMap::new();
        for extent in &checkpoint.extents {
            extents.insert(extent.id.as_bytes(), *extent);
        }
        Ok(SinkSession {
            transfer,
            key: Some(key),
            version: Some(version),
            len,
            extents,
            bytes,
            temporary: Some(temporary),
        })
    }

    fn resume_unverified(
        &mut self,
        transfer: TransferId,
        len: u64,
        checkpoint: &WireReceivingCheckpoint<T>,
    ) -> Result<Self::Session, ReplicationError> {
        if !self.reservations.contains_key(&transfer) {
            self.reserve(transfer, None, len)?;
        }
        let temporary = self.temporary_path(transfer).ok_or_else(|| {
            let _ = self.release_reservation(transfer);
            ReplicationError::Disconnected
        })?;
        let metadata = open_optional_readonly_nofollow(&temporary)?
            .ok_or(ReplicationError::Disconnected)?
            .metadata()
            .map_err(|_| ReplicationError::Disconnected)?;
        if metadata.len() != len {
            let _ = self.release_reservation(transfer);
            return Err(ReplicationError::CorruptFrame);
        }
        let mut extents = BTreeMap::new();
        for extent in &checkpoint.extents {
            let extent: StagedExtent = (*extent).into();
            if extents.insert(extent.id.as_bytes(), extent).is_some() {
                let _ = self.release_reservation(transfer);
                return Err(ReplicationError::ReplayConflict);
            }
            if let Err(error) = verify_extent_file(&temporary, extent) {
                let _ = self.release_reservation(transfer);
                return Err(error);
            }
        }
        Ok(SinkSession {
            transfer,
            key: None,
            version: None,
            len,
            extents,
            bytes: BTreeMap::new(),
            temporary: Some(temporary),
        })
    }

    fn write(
        &mut self,
        session: &mut Self::Session,
        extent: StagedExtent,
        bytes: Arc<[u8]>,
    ) -> Result<(), ReplicationError> {
        if bytes.len() as u64 != extent.len {
            return Err(ReplicationError::Range);
        }
        if let Some(path) = &session.temporary {
            let mut file = open_existing_writeonly_nofollow(path)?;
            file.seek(SeekFrom::Start(extent.offset))
                .and_then(|_| file.write_all(&bytes))
                .and_then(|()| file.sync_data())
                .map_err(|_| ReplicationError::Disconnected)?;
        }
        session.extents.insert(extent.id.as_bytes(), extent);
        if session.temporary.is_none() {
            session.bytes.insert(extent.id.as_bytes(), bytes);
        }
        Ok(())
    }

    fn read_extent(
        &mut self,
        session: &mut Self::Session,
        extent: StagedExtent,
        visitor: &mut dyn FnMut(&[u8]) -> Result<(), ReplicationError>,
    ) -> Result<(), ReplicationError> {
        if let Some(bytes) = session.bytes.get(&extent.id.as_bytes()) {
            return visitor(bytes);
        }
        let Some(path) = &session.temporary else {
            return Err(ReplicationError::CorruptFrame);
        };
        let mut file =
            open_optional_readonly_nofollow(path)?.ok_or(ReplicationError::Disconnected)?;
        let mut bytes =
            vec![0_u8; usize::try_from(extent.len).map_err(|_| ReplicationError::Overflow)?];
        file.seek(SeekFrom::Start(extent.offset))
            .and_then(|_| file.read_exact(&mut bytes))
            .map_err(|_| ReplicationError::Disconnected)?;
        visitor(&bytes)
    }

    fn commit(
        &mut self,
        session: Self::Session,
        key: ObjectKey<T>,
        version: ObjectVersion<T>,
        len: u64,
        _digest: [u8; 32],
    ) -> Result<Self::Receipt, ReplicationError> {
        let durable = session.temporary.is_some();
        if session.key.is_some_and(|session_key| session_key != key)
            || session
                .version
                .is_some_and(|session_version| session_version != version)
            || session.len != len
        {
            let _ = self.release_reservation(session.transfer);
            return Err(ReplicationError::IdentityMismatch);
        }
        if self
            .reservations
            .get(&session.transfer)
            .is_some_and(|reservation| reservation.version.is_none())
        {
            if self.reservations.iter().any(|(transfer, existing)| {
                *transfer != session.transfer && existing.version == Some(version)
            }) {
                let _ = self.release_reservation(session.transfer);
                return Err(ReplicationError::ReplayConflict);
            }
            let path = self.object_path(version);
            let existing = path
                .as_ref()
                .map(|path| open_optional_readonly_nofollow(path))
                .transpose()?
                .flatten();
            let replaced_bytes = existing
                .as_ref()
                .map(|file| {
                    file.metadata()
                        .map(|metadata| metadata.len())
                        .map_err(|_| ReplicationError::Disconnected)
                })
                .transpose()?
                .unwrap_or_else(|| {
                    self.committed
                        .get(&version)
                        .map_or(0, |bytes| bytes.len() as u64)
                });
            let adds_object = existing.is_none() && !self.committed.contains_key(&version);
            let Some(reservation) = self.reservations.get_mut(&session.transfer) else {
                return Err(ReplicationError::Disconnected);
            };
            reservation.replaced_bytes = replaced_bytes;
            reservation.adds_object = adds_object;
            reservation.version = Some(version);
        }
        if let Some(path) = session.temporary.as_ref() {
            let result = (|| {
                let destination = self
                    .object_path(version)
                    .ok_or(ReplicationError::Disconnected)?;
                if let Some(existing) = open_optional_readonly_nofollow(&destination)? {
                    let existing_len = existing
                        .metadata()
                        .map_err(|_| ReplicationError::Disconnected)?
                        .len();
                    if digest_open_file::<T>(existing, existing_len)? == version.to_bytes() {
                        fs::remove_file(path).map_err(|_| ReplicationError::Disconnected)?;
                    } else {
                        // A corrupt prior object is a repair target. The staged
                        // replacement has already passed the receiving CAS
                        // checks, so it can replace the bad file atomically.
                        fs::remove_file(&destination)
                            .map_err(|_| ReplicationError::Disconnected)?;
                        fs::rename(path, &destination)
                            .map_err(|_| ReplicationError::Disconnected)?;
                    }
                } else {
                    fs::rename(path, &destination).map_err(|_| ReplicationError::Disconnected)?;
                }
                if let Some(root) = &self.root {
                    backend_platform::durability::open_directory(root)
                        .and_then(|directory| directory.sync_all())
                        .map_err(|_| ReplicationError::Disconnected)?;
                }
                Ok::<(), ReplicationError>(())
            })();
            if let Err(error) = result {
                let _ = self.release_reservation(session.transfer);
                return Err(error);
            }
        }
        let reservation = self.release_reservation(session.transfer);
        let bytes: Arc<[u8]> = if durable {
            // Durable objects are intentionally left on disk. Recipe
            // admission maps them on demand, so committing a large object
            // never allocates a second full value merely to warm a process
            // local cache.
            if let Some(reservation) = reservation {
                let old_bytes = reservation.replaced_bytes;
                let new_bytes = self
                    .object_path(version)
                    .map(|path| open_optional_readonly_nofollow(&path))
                    .transpose()?
                    .flatten()
                    .map(|file| {
                        file.metadata()
                            .map(|metadata| metadata.len())
                            .map_err(|_| ReplicationError::Disconnected)
                    })
                    .transpose()?
                    .unwrap_or(0);
                if reservation.adds_object {
                    self.retained_objects = self.retained_objects.saturating_add(1);
                }
                self.retained_bytes = self
                    .retained_bytes
                    .saturating_sub(old_bytes)
                    .saturating_add(new_bytes);
            }
            return Ok(version);
        } else if let Some(bytes) = self.read(version)? {
            bytes
        } else {
            let mut value =
                Vec::with_capacity(usize::try_from(len).map_err(|_| ReplicationError::Overflow)?);
            let mut by_offset = session.extents.values().copied().collect::<Vec<_>>();
            by_offset.sort_unstable_by_key(|extent| extent.offset);
            for extent in by_offset {
                value.extend_from_slice(
                    session
                        .bytes
                        .get(&extent.id.as_bytes())
                        .ok_or(ReplicationError::CorruptFrame)?,
                );
            }
            Arc::from(value.into_boxed_slice())
        };
        self.committed.insert(version, bytes);
        if let Some(reservation) = reservation {
            if reservation.adds_object {
                self.retained_objects = self.retained_objects.saturating_add(1);
            }
            self.retained_bytes = self
                .retained_bytes
                .saturating_sub(reservation.replaced_bytes)
                .saturating_add(len);
        }
        Ok(version)
    }

    fn abort(&mut self, session: Self::Session) {
        let _ = self.release_reservation(session.transfer);
        if let Some(path) = session.temporary {
            let _ = fs::remove_file(path);
        }
    }
}

fn open_optional_readonly_nofollow(path: &Path) -> Result<Option<File>, ReplicationError> {
    let link_metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ReplicationError::Disconnected),
    };
    if !link_metadata.is_file() || link_metadata.file_type().is_symlink() {
        return Err(ReplicationError::CorruptFrame);
    }
    let file = open_readonly_nofollow(path).map_err(|_| ReplicationError::CorruptFrame)?;
    let metadata = file
        .metadata()
        .map_err(|_| ReplicationError::Disconnected)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(ReplicationError::CorruptFrame);
    }
    Ok(Some(file))
}

fn open_existing_writeonly_nofollow(path: &Path) -> Result<File, ReplicationError> {
    let link_metadata = fs::symlink_metadata(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            ReplicationError::Disconnected
        } else {
            ReplicationError::CorruptFrame
        }
    })?;
    if !link_metadata.is_file() || link_metadata.file_type().is_symlink() {
        return Err(ReplicationError::CorruptFrame);
    }
    let file = open_writeonly_nofollow(path).map_err(|_| ReplicationError::CorruptFrame)?;
    let metadata = file
        .metadata()
        .map_err(|_| ReplicationError::Disconnected)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(ReplicationError::CorruptFrame);
    }
    Ok(file)
}

#[cfg(unix)]
fn open_readonly_nofollow(path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, open};
    open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(io::Error::from)
}

#[cfg(windows)]
fn open_readonly_nofollow(path: &Path) -> io::Result<File> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "symbolic link"));
    }
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_readonly_nofollow(path: &Path) -> io::Result<File> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "symbolic link"));
    }
    File::open(path)
}

#[cfg(unix)]
fn open_writeonly_nofollow(path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, open};
    open(
        path,
        OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(io::Error::from)
}

#[cfg(windows)]
fn open_writeonly_nofollow(path: &Path) -> io::Result<File> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "symbolic link"));
    }
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_writeonly_nofollow(path: &Path) -> io::Result<File> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "symbolic link"));
    }
    OpenOptions::new().write(true).open(path)
}

fn hex(bytes: [u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn is_object_filename(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn decode_hex(name: &str) -> Option<[u8; 32]> {
    if !is_object_filename(name) {
        return None;
    }
    let mut bytes = [0_u8; 32];
    for (index, pair) in name.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        bytes[index] = (high << 4) | low;
    }
    Some(bytes)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn digest_open_file<T: Schema>(mut file: File, len: u64) -> Result<[u8; 32], ReplicationError> {
    let mut digest = CanonicalDigest::<T>::new(len);
    let mut offset = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| ReplicationError::Disconnected)?;
        if read == 0 {
            break;
        }
        digest.push(offset, &buffer[..read])?;
        offset = offset
            .checked_add(u64::try_from(read).map_err(|_| ReplicationError::Overflow)?)
            .ok_or(ReplicationError::Overflow)?;
    }
    digest.finish()
}

fn verify_extent_file(path: &Path, extent: StagedExtent) -> Result<(), ReplicationError> {
    if extent.len == 0 {
        return Err(ReplicationError::Range);
    }
    let mut file = open_optional_readonly_nofollow(path)?.ok_or(ReplicationError::Disconnected)?;
    file.seek(SeekFrom::Start(extent.offset))
        .map_err(|_| ReplicationError::Disconnected)?;
    let mut remaining = extent.len;
    let mut buffer = vec![
        0_u8;
        usize::try_from(extent.len.min(64 * 1024))
            .map_err(|_| ReplicationError::Overflow)?
    ];
    let mut chain = ChunkChainDigest::new(extent.previous_chain, extent.sequence, extent.len);
    while remaining > 0 {
        let take = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| ReplicationError::Overflow)?;
        file.read_exact(&mut buffer[..take])
            .map_err(|_| ReplicationError::CorruptFrame)?;
        chain.push(&buffer[..take])?;
        remaining = remaining
            .checked_sub(take as u64)
            .ok_or(ReplicationError::Overflow)?;
    }
    if chain.finish()? != extent.chain {
        return Err(ReplicationError::CorruptFrame);
    }
    Ok(())
}
