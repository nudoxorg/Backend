//! Durable sink for the canonical receiving CAS.

use super::CasGcBudget;
use backend_engine::{
    CanonicalDigest, ObjectKey, ObjectVersion, ReceivingCasSink, ReceivingCheckpoint,
    ReplicationError, Schema, StagedExtent, TransferId,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(super) struct SinkSession<T: Schema> {
    transfer: TransferId,
    key: ObjectKey<T>,
    version: ObjectVersion<T>,
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
    reservations: BTreeMap<TransferId, Reservation<T>>,
    marker: PhantomData<fn() -> T>,
}

#[derive(Clone, Copy, Debug)]
struct Reservation<T: Schema> {
    version: ObjectVersion<T>,
    len: u64,
    replaced_bytes: u64,
    adds_object: bool,
}

impl<T: Schema> DurableSink<T> {
    pub(super) fn open_with_budget(
        path: impl AsRef<Path>,
        max_retained_objects: usize,
        max_retained_bytes: u64,
    ) -> Result<Self, ReplicationError> {
        if max_retained_objects == 0 || max_retained_bytes == 0 {
            return Err(ReplicationError::InvalidLimits);
        }
        let path = path.as_ref();
        fs::create_dir_all(path).map_err(|_| ReplicationError::Disconnected)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut mode = fs::metadata(path)
                .map_err(|_| ReplicationError::Disconnected)?
                .permissions();
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
            reservations: BTreeMap::new(),
            marker: PhantomData,
        };
        sink.refresh_usage()?;
        Ok(sink)
    }

    pub(super) fn usage(&self) -> (usize, u64) {
        (self.retained_objects, self.retained_bytes)
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
            let metadata = entry
                .metadata()
                .map_err(|_| ReplicationError::Disconnected)?;
            if metadata.is_file() && entry.file_name().to_str().is_some_and(is_object_filename) {
                objects = objects.checked_add(1).ok_or(ReplicationError::Overflow)?;
                bytes = bytes
                    .checked_add(metadata.len())
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
        self.object_path(version).is_some_and(|path| {
            let Ok(metadata) = fs::metadata(&path) else {
                return false;
            };
            metadata.is_file()
                && digest_file::<T>(&path, metadata.len())
                    .is_ok_and(|digest| digest == version.to_bytes())
        })
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
                let metadata = entry
                    .metadata()
                    .map_err(|_| ReplicationError::Disconnected)?;
                if metadata.is_file() {
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
        version: ObjectVersion<T>,
        len: u64,
    ) -> Result<(), ReplicationError> {
        if self.reservations.contains_key(&transfer)
            || self
                .reservations
                .values()
                .any(|reservation| reservation.version == version)
        {
            return Err(ReplicationError::ReplayConflict);
        }
        let path = self.object_path(version);
        let replaced_bytes = path
            .as_ref()
            .and_then(|path| fs::metadata(path).ok())
            .filter(Metadata::is_file)
            .map_or_else(
                || {
                    self.committed
                        .get(&version)
                        .map_or(0, |bytes| bytes.len() as u64)
                },
                |metadata| metadata.len(),
            );
        let object_exists = path.as_ref().is_some_and(|path| path.is_file())
            || self.committed.contains_key(&version);
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
        if !path.is_file() {
            return Ok(None);
        }
        let bytes: Arc<[u8]> = Arc::from(
            fs::read(path)
                .map_err(|_| ReplicationError::Disconnected)?
                .into_boxed_slice(),
        );
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
        self.reserve(transfer, version, len)?;
        let temporary = self.temporary_path(transfer);
        if let Some(path) = &temporary
            && let Err(error) = OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(path)
        {
            let _ = self.release_reservation(transfer);
            return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
                ReplicationError::ReplayConflict
            } else {
                ReplicationError::Disconnected
            });
        }
        Ok(SinkSession {
            transfer,
            key,
            version,
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
        self.reserve(transfer, version, len)?;
        let temporary = self
            .temporary_path(transfer)
            .filter(|path| path.is_file())
            .ok_or_else(|| {
                let _ = self.release_reservation(transfer);
                ReplicationError::Disconnected
            })?;
        if let Err(_error) = File::open(&temporary) {
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
            key,
            version,
            len,
            extents,
            bytes,
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
            let mut file = OpenOptions::new()
                .write(true)
                .open(path)
                .map_err(|_| ReplicationError::Disconnected)?;
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
        let mut file = File::open(path).map_err(|_| ReplicationError::Disconnected)?;
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
        if session.key != key || session.version != version || session.len != len {
            let _ = self.release_reservation(session.transfer);
            return Err(ReplicationError::IdentityMismatch);
        }
        if let Some(path) = session.temporary.as_ref() {
            let result = (|| {
                let destination = self
                    .object_path(version)
                    .ok_or(ReplicationError::Disconnected)?;
                if destination.is_file() {
                    if digest_file::<T>(&destination, len)? == version.to_bytes() {
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
                let new_bytes = if self
                    .object_path(version)
                    .as_ref()
                    .and_then(|path| fs::metadata(path).ok())
                    .is_some_and(|metadata| metadata.is_file())
                {
                    len
                } else {
                    0
                };
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

fn digest_file<T: Schema>(path: &Path, len: u64) -> Result<[u8; 32], ReplicationError> {
    let mut file = File::open(path).map_err(|_| ReplicationError::Disconnected)?;
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
