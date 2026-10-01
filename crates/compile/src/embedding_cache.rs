//! Bounded durable cache for exact embedding inputs.
//!
//! Each input identity has one checksummed file, so a changed view writes only
//! its new embeddings. Entries are keyed by `EmbeddingInputIdentity`; that key
//! binds the exact model, tokenizer, executable, runtime options, treatment,
//! and semantic text. Recipe metadata remains in the file for diagnostics but
//! does not partition the cache: exact identities can survive a recipe switch.
//! Cache failures are misses because the cache is an optimization.
//!
//! The engine mints a held no-follow capability while its workspace owner is live. A bounded
//! cache actor takes a separate kernel lock on that exact directory and owns inventory, eviction,
//! and atomic replacement. Cache reads and writes stay scoped to the borrowed actor session.

use crate::{EmbeddingInputIdentity, ProcessError};
use backend_platform::{DirectoryCapability, EntryKind};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::mem::size_of;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MAGIC: &[u8; 8] = b"BEMIC001";
const VERSION: u8 = 1;
const HEADER_BYTES: usize = 8 + 1 + 32 + 32 + 4;
const CHECKSUM_BYTES: usize = 32;
const CACHE_LOCK_FILE: &str = ".embedding-cache.lock";
const CACHE_DOMAIN: &str = "backend.compile.embedding-input-cache.v1";
const MAX_CACHE_BYTES: u64 = 520 * 1024 * 1024;
const MAX_CACHE_ENTRIES: usize = 65_536;
const COORDINATE_IO_CHUNK_BYTES: usize = 16 * 1024;
const COORDINATES_PER_IO_CHUNK: usize = COORDINATE_IO_CHUNK_BYTES / size_of::<f32>();

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static ACTIVE_BROKERS: AtomicUsize = AtomicUsize::new(0);
const CACHE_BROKER_LIMIT: usize = 16;
const CACHE_BROKER_QUEUE: usize = 1;
const CACHE_RPC_BATCH_ITEMS: usize = 256;

/// One held workspace-local directory of exact input-keyed vector entries.
pub(crate) struct EmbeddingCacheFile {
    directory: DirectoryCapability,
    _lease: File,
    recipe: [u8; 32],
    dimensions: u32,
    bytes_used: u64,
    stranded_temporary_bytes: u64,
    storage_poisoned: bool,
    entries: BTreeMap<[u8; 32], CacheEntry>,
    recency: u128,
}

#[derive(Clone, Copy)]
struct CacheEntry {
    bytes: u64,
    last_used: u128,
}

/// Non-cloneable owner-scoped handle to one bounded durable-cache actor.
///
/// The actor owns the pinned directory capability, kernel lock, inventory, and all file mutation.
/// Callers borrow the session for one inference; it never attaches itself to an executable runtime.
pub struct EmbeddingCacheSession {
    sender: SyncSender<CacheBrokerMessage>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for EmbeddingCacheSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EmbeddingCacheSession")
            .finish_non_exhaustive()
    }
}

enum CacheBrokerMessage {
    Lookup {
        identities: Vec<EmbeddingInputIdentity>,
        cancelled: Arc<AtomicBool>,
        deadline: Instant,
        reply: SyncSender<Vec<Option<Arc<[f32]>>>>,
    },
    Store {
        values: Vec<crate::embedding::ValidatedProducerVector>,
        cancelled: Arc<AtomicBool>,
        deadline: Instant,
    },
    Shutdown,
}

struct CacheBrokerPermit;

impl Drop for CacheBrokerPermit {
    fn drop(&mut self) {
        ACTIVE_BROKERS.fetch_sub(1, Ordering::AcqRel);
    }
}

impl EmbeddingCacheSession {
    pub(crate) fn open(
        directory: DirectoryCapability,
        recipe: [u8; 32],
        dimensions: u32,
    ) -> Option<Self> {
        if !reserve_broker() {
            return None;
        }
        let (sender, receiver) = mpsc::sync_channel(CACHE_BROKER_QUEUE);
        let permit = CacheBrokerPermit;
        let worker = thread::Builder::new()
            .name("embedding-cache-broker".into())
            .spawn(move || {
                let _permit = permit;
                run_cache_broker(receiver, directory, recipe, dimensions);
            })
            .ok()?;
        Some(Self {
            sender,
            worker: Mutex::new(Some(worker)),
        })
    }

    pub(crate) fn lookup_batch(
        &self,
        identities: &[EmbeddingInputIdentity],
        cancelled: Option<&AtomicBool>,
        deadline: Instant,
    ) -> Result<Option<Vec<Option<Arc<[f32]>>>>, ProcessError> {
        check_request(cancelled, deadline)?;
        let mut result = Vec::new();
        result
            .try_reserve_exact(identities.len())
            .map_err(|_| ProcessError::OutputLimit)?;
        for batch in identities.chunks(CACHE_RPC_BATCH_ITEMS) {
            check_request(cancelled, deadline)?;
            let (reply, response) = mpsc::sync_channel(1);
            let request_cancelled = Arc::new(AtomicBool::new(false));
            let message = CacheBrokerMessage::Lookup {
                identities: batch.to_vec(),
                cancelled: Arc::clone(&request_cancelled),
                deadline,
                reply,
            };
            match self.sender.try_send(message) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                    check_request(cancelled, deadline)?;
                    return Ok(None);
                }
            }
            let values = loop {
                if let Err(error) = check_request(cancelled, deadline) {
                    request_cancelled.store(true, Ordering::Release);
                    return Err(error);
                }
                match response.try_recv() {
                    Ok(values) => break values,
                    Err(TryRecvError::Disconnected) => return Ok(None),
                    Err(TryRecvError::Empty) => thread::sleep(
                        deadline
                            .saturating_duration_since(Instant::now())
                            .min(Duration::from_millis(1)),
                    ),
                }
            };
            if values.len() != batch.len() {
                return Ok(None);
            }
            result.extend(values);
        }
        check_request(cancelled, deadline)?;
        Ok(Some(result))
    }

    pub(crate) fn store_validated(
        &self,
        values: Vec<crate::embedding::ValidatedProducerVector>,
        cancelled: Option<&AtomicBool>,
        deadline: Instant,
    ) -> Result<(), ProcessError> {
        check_request(cancelled, deadline)?;
        for batch in values.chunks(CACHE_RPC_BATCH_ITEMS) {
            check_request(cancelled, deadline)?;
            let request_cancelled = Arc::new(AtomicBool::new(false));
            let message = CacheBrokerMessage::Store {
                values: batch.to_vec(),
                cancelled: Arc::clone(&request_cancelled),
                deadline,
            };
            match self.sender.try_send(message) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => break,
            }
            if let Err(error) = check_request(cancelled, deadline) {
                request_cancelled.store(true, Ordering::Release);
                return Err(error);
            }
        }
        Ok(())
    }
}

impl Drop for EmbeddingCacheSession {
    fn drop(&mut self) {
        let _ = self.sender.try_send(CacheBrokerMessage::Shutdown);
        let worker = self
            .worker
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(handle) = worker.take() else {
            return;
        };
        let join_deadline = Instant::now() + Duration::from_millis(50);
        loop {
            if handle.is_finished() {
                let _ = handle.join();
                return;
            }
            let _ = self.sender.try_send(CacheBrokerMessage::Shutdown);
            if Instant::now() >= join_deadline {
                // A blocked filesystem call keeps its actor permit, held directory, and kernel
                // lease until the call returns. Global admission bounds detached actors.
                drop(handle);
                return;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
}

fn reserve_broker() -> bool {
    ACTIVE_BROKERS
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
            (active < CACHE_BROKER_LIMIT).then_some(active + 1)
        })
        .is_ok()
}

fn run_cache_broker(
    receiver: Receiver<CacheBrokerMessage>,
    directory: DirectoryCapability,
    recipe: [u8; 32],
    dimensions: u32,
) {
    let Some(mut cache) = EmbeddingCacheFile::open_directory(directory, recipe, dimensions) else {
        return;
    };
    while let Ok(message) = receiver.recv() {
        match message {
            CacheBrokerMessage::Lookup {
                identities,
                cancelled,
                deadline,
                reply,
            } => {
                let mut values = Vec::with_capacity(identities.len());
                for identity in identities {
                    if cache_checkpoint(&cancelled, deadline).is_err() {
                        values.clear();
                        break;
                    }
                    values.push(
                        cache
                            .load_until(identity, &cancelled, deadline)
                            .ok()
                            .flatten(),
                    );
                }
                let _ = reply.try_send(values);
            }
            CacheBrokerMessage::Store {
                values,
                cancelled,
                deadline,
            } => {
                if cache_checkpoint(&cancelled, deadline).is_ok() {
                    let _ = cache.store_validated(&values, &cancelled, deadline);
                }
            }
            CacheBrokerMessage::Shutdown => break,
        }
    }
}

fn check_request(cancelled: Option<&AtomicBool>, deadline: Instant) -> Result<(), ProcessError> {
    if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        Err(ProcessError::Cancelled)
    } else if Instant::now() >= deadline {
        Err(ProcessError::Deadline)
    } else {
        Ok(())
    }
}

fn cache_checkpoint(cancelled: &AtomicBool, deadline: Instant) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "cache request cancelled",
        ))
    } else if Instant::now() >= deadline {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "cache request deadline",
        ))
    } else {
        Ok(())
    }
}

impl EmbeddingCacheFile {
    /// Opens the cache below the exact data directory owned by locald.
    ///
    /// A missing/invalid directory disables persistence while leaving the
    /// caller's bounded process-local cache available.
    pub(crate) fn open_directory(
        directory: DirectoryCapability,
        recipe: [u8; 32],
        dimensions: u32,
    ) -> Option<Self> {
        if dimensions == 0 || directory.validate_private().is_err() {
            return None;
        }
        let lease = directory
            .open_private_file_read_write(CACHE_LOCK_FILE, true)
            .ok()?;
        lease.try_lock().ok()?;
        let (bytes_used, entries, recency) = inventory(&directory).ok()?;
        if bytes_used > MAX_CACHE_BYTES || entries.len() > MAX_CACHE_ENTRIES {
            return None;
        }
        Some(Self {
            directory,
            _lease: lease,
            recipe,
            dimensions,
            bytes_used,
            stranded_temporary_bytes: 0,
            storage_poisoned: false,
            entries,
            recency,
        })
    }

    /// Opens a cache directly in an absolute fixture directory.
    ///
    /// This is intended for isolated fixtures. Production callers should use a directory
    /// capability minted by the live owner.
    #[cfg(test)]
    fn open_in_directory(directory: &Path, recipe: [u8; 32], dimensions: u32) -> Option<Self> {
        if !directory.is_absolute() || dimensions == 0 {
            return None;
        }
        let capability = DirectoryCapability::open_or_create_private(directory).ok()?;
        Self::open_directory(capability, recipe, dimensions)
    }

    /// Reads and validates one exact cache key. Corrupt entries become misses.
    #[cfg(test)]
    fn load(&mut self, identity: EmbeddingInputIdentity) -> Option<Arc<[f32]>> {
        self.load_until(
            identity,
            &AtomicBool::new(false),
            Instant::now() + Duration::from_secs(24 * 60 * 60),
        )
        .ok()
        .flatten()
    }

    fn load_until(
        &mut self,
        identity: EmbeddingInputIdentity,
        cancelled: &AtomicBool,
        deadline: Instant,
    ) -> io::Result<Option<Arc<[f32]>>> {
        let identity = identity.as_bytes();
        let name = entry_name(&identity);
        let mut checkpoint = || cache_checkpoint(cancelled, deadline);
        match self.load_checked(&name, identity, &mut checkpoint) {
            Ok(values) => {
                self.touch(identity);
                Ok(Some(values))
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::TimedOut
                ) =>
            {
                Err(error)
            }
            Err(_) => {
                self.remove_entry(identity, &name);
                Ok(None)
            }
        }
    }

    /// Stores newly computed entries, evicting least-recently-used entries
    /// only when the byte or entry cap requires it.
    #[cfg(test)]
    fn store_batch(&mut self, values: &[(EmbeddingInputIdentity, &[f32])]) -> io::Result<()> {
        self.store_batch_with_checkpoint(values, || Ok(()))
    }

    fn store_validated(
        &mut self,
        values: &[crate::embedding::ValidatedProducerVector],
        cancelled: &AtomicBool,
        deadline: Instant,
    ) -> io::Result<()> {
        let mut borrowed = Vec::new();
        borrowed
            .try_reserve_exact(values.len())
            .map_err(|_| io::Error::other("embedding cache batch allocation failed"))?;
        for value in values {
            cache_checkpoint(cancelled, deadline)?;
            borrowed.push((value.identity(), value.values()));
        }
        self.store_batch_with_checkpoint(&borrowed, || cache_checkpoint(cancelled, deadline))
    }

    fn store_batch_with_checkpoint(
        &mut self,
        values: &[(EmbeddingInputIdentity, &[f32])],
        mut checkpoint: impl FnMut() -> io::Result<()>,
    ) -> io::Result<()> {
        checkpoint()?;
        if self.storage_poisoned {
            return Ok(());
        }
        let dimension_count = usize::try_from(self.dimensions).map_err(|_| cache_size_error())?;
        let entry_bytes = u64::try_from(
            HEADER_BYTES
                .checked_add(
                    dimension_count
                        .checked_mul(4)
                        .ok_or_else(cache_size_error)?,
                )
                .and_then(|bytes| bytes.checked_add(CHECKSUM_BYTES))
                .ok_or_else(cache_size_error)?,
        )
        .map_err(|_| cache_size_error())?;
        let mut unique = BTreeMap::new();
        for (identity, coordinates) in values {
            checkpoint()?;
            if coordinates.len() != dimension_count
                || coordinates.iter().any(|value| !value.is_finite())
                || !is_unit_vector(coordinates)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid cached vector",
                ));
            }
            let key = identity.as_bytes();
            if let Some(previous) = unique.get(&key)
                && previous
                    .iter()
                    .zip(*coordinates)
                    .any(|(left, right)| left.to_bits() != right.to_bits())
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "conflicting vectors share one embedding identity",
                ));
            }
            unique.insert(key, *coordinates);
        }
        let new_entries = unique
            .keys()
            .filter(|identity| !self.entries.contains_key(*identity))
            .count();
        let additional_bytes = entry_bytes
            .checked_mul(u64::try_from(new_entries).map_err(|_| cache_size_error())?)
            .ok_or_else(cache_size_error)?;
        let mut projected_bytes = self
            .bytes_used
            .checked_add(self.stranded_temporary_bytes)
            .ok_or_else(cache_size_error)?;
        projected_bytes = projected_bytes
            .checked_add(additional_bytes)
            .ok_or_else(cache_size_error)?;
        let mut projected_entries = self.entries.len().saturating_add(new_entries);
        if unique.len() > MAX_CACHE_ENTRIES || entry_bytes >= MAX_CACHE_BYTES {
            return Ok(());
        }

        if projected_bytes > MAX_CACHE_BYTES || projected_entries > MAX_CACHE_ENTRIES {
            let mut eviction = self
                .entries
                .iter()
                .filter(|(identity, _)| !unique.contains_key(*identity))
                .map(|(identity, entry)| (*identity, entry.last_used))
                .collect::<Vec<_>>();
            eviction.sort_by_key(|(_, last_used)| *last_used);
            checkpoint()?;
            for (identity, _) in eviction {
                checkpoint()?;
                if projected_bytes <= MAX_CACHE_BYTES - entry_bytes
                    && projected_entries < MAX_CACHE_ENTRIES
                {
                    break;
                }
                let Some(entry) = self.entries.remove(&identity) else {
                    continue;
                };
                let name = entry_name(&identity);
                let removed = match self.directory.remove_file(&name) {
                    Ok(()) => true,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => true,
                    Err(_) => false,
                };
                if removed {
                    self.bytes_used = self.bytes_used.saturating_sub(entry.bytes);
                } else {
                    self.entries.insert(identity, entry);
                    continue;
                }
                projected_bytes = projected_bytes.saturating_sub(entry.bytes);
                projected_entries = projected_entries.saturating_sub(1);
            }
        }
        if projected_bytes > MAX_CACHE_BYTES - entry_bytes || projected_entries >= MAX_CACHE_ENTRIES
        {
            return Ok(());
        }

        for (identity, coordinates) in unique {
            checkpoint()?;
            self.store_one(identity, coordinates, entry_bytes, &mut checkpoint)?;
        }
        Ok(())
    }

    fn store_one(
        &mut self,
        identity: [u8; 32],
        values: &[f32],
        entry_bytes: u64,
        checkpoint: &mut impl FnMut() -> io::Result<()>,
    ) -> io::Result<()> {
        let prior = self.entries.get(&identity).copied();
        let destination = entry_name(&identity);
        let temporary = temporary_name(&identity);
        let mut committed = false;
        let mut temporary_created = false;
        let result = (|| {
            checkpoint()?;
            let mut file = self.directory.create_file_exclusive(&temporary)?;
            temporary_created = true;
            let mut hasher = blake3::Hasher::new_derive_key(CACHE_DOMAIN);
            let mut header = [0_u8; HEADER_BYTES];
            header[..8].copy_from_slice(MAGIC);
            header[8] = VERSION;
            header[9..41].copy_from_slice(&self.recipe);
            header[41..73].copy_from_slice(&identity);
            header[73..77].copy_from_slice(&self.dimensions.to_be_bytes());
            checkpoint()?;
            write_hashed(&mut file, &mut hasher, &header)?;
            let mut encoded = [0_u8; COORDINATE_IO_CHUNK_BYTES];
            for chunk in values.chunks(COORDINATES_PER_IO_CHUNK) {
                checkpoint()?;
                let encoded_len = chunk.len() * size_of::<f32>();
                for (value, bytes) in chunk
                    .iter()
                    .zip(encoded[..encoded_len].chunks_exact_mut(size_of::<f32>()))
                {
                    bytes.copy_from_slice(&value.to_le_bytes());
                }
                write_hashed(&mut file, &mut hasher, &encoded[..encoded_len])?;
            }
            checkpoint()?;
            file.write_all(hasher.finalize().as_bytes())?;
            file.sync_all()?;
            checkpoint()?;
            match self
                .directory
                .rename_with_outcome(&temporary, &destination, true)
            {
                Ok(()) => committed = true,
                Err(error @ backend_platform::DirectoryRenameError::CommittedButNotDurable(_)) => {
                    committed = true;
                    return Err(error.into_io_error());
                }
                Err(error @ backend_platform::DirectoryRenameError::NotCommitted(_)) => {
                    return Err(error.into_io_error());
                }
            }
            Ok(())
        })();
        if !committed && temporary_created {
            match self.directory.remove_file(&temporary) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(_) => {
                    self.stranded_temporary_bytes = self
                        .stranded_temporary_bytes
                        .saturating_add(entry_bytes)
                        .min(MAX_CACHE_BYTES);
                    self.storage_poisoned = true;
                }
            }
        }
        if committed {
            let last_used = self.tick();
            self.bytes_used = self
                .bytes_used
                .saturating_sub(prior.map_or(0, |entry| entry.bytes))
                .saturating_add(entry_bytes);
            self.entries.insert(
                identity,
                CacheEntry {
                    bytes: entry_bytes,
                    last_used,
                },
            );
        }
        result
    }

    fn touch(&mut self, identity: [u8; 32]) {
        let last_used = self.tick();
        if let Some(entry) = self.entries.get_mut(&identity) {
            entry.last_used = last_used;
        }
        // Reading the checksummed file naturally advances filesystem access
        // time where supported. Avoid a second write-open for every hit.
    }

    fn tick(&mut self) -> u128 {
        let now = timestamp(std::time::SystemTime::now());
        self.recency = self.recency.max(now).saturating_add(1);
        self.recency
    }

    fn remove_entry(&mut self, identity: [u8; 32], name: &str) {
        match self.directory.remove_file(name) {
            Ok(()) => {
                if let Some(entry) = self.entries.remove(&identity) {
                    self.bytes_used = self.bytes_used.saturating_sub(entry.bytes);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if let Some(entry) = self.entries.remove(&identity) {
                    self.bytes_used = self.bytes_used.saturating_sub(entry.bytes);
                }
            }
            Err(_) => {}
        }
    }

    fn load_checked(
        &self,
        name: &str,
        identity: [u8; 32],
        checkpoint: &mut impl FnMut() -> io::Result<()>,
    ) -> io::Result<Arc<[f32]>> {
        checkpoint()?;
        let mut file = self.directory.open_private_file(name)?;
        let dimensions = usize::try_from(self.dimensions).map_err(|_| cache_data_error())?;
        let expected = HEADER_BYTES
            .checked_add(dimensions.checked_mul(4).ok_or_else(cache_data_error)?)
            .and_then(|bytes| bytes.checked_add(CHECKSUM_BYTES))
            .ok_or_else(cache_data_error)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() != u64::try_from(expected).unwrap_or(u64::MAX) {
            return Err(cache_data_error());
        }
        let mut hasher = blake3::Hasher::new_derive_key(CACHE_DOMAIN);
        let mut header = [0_u8; HEADER_BYTES];
        checkpoint()?;
        read_hashed(&mut file, &mut hasher, &mut header)?;
        if &header[..8] != MAGIC
            || header[8] != VERSION
            || header[41..73] != identity
            || header[73..77] != self.dimensions.to_be_bytes()
        {
            return Err(cache_data_error());
        }

        let mut values = Vec::new();
        values
            .try_reserve_exact(dimensions)
            .map_err(|_| io::Error::other("embedding cache allocation failed"))?;
        let mut encoded = [0_u8; COORDINATE_IO_CHUNK_BYTES];
        let mut norm_squared = 0.0_f64;
        let mut remaining = dimensions;
        while remaining > 0 {
            checkpoint()?;
            let coordinate_count = remaining.min(COORDINATES_PER_IO_CHUNK);
            let encoded_len = coordinate_count
                .checked_mul(size_of::<f32>())
                .ok_or_else(cache_data_error)?;
            let encoded_chunk = &mut encoded[..encoded_len];
            read_hashed(&mut file, &mut hasher, encoded_chunk)?;
            for coordinate in encoded_chunk.chunks_exact(size_of::<f32>()) {
                let value =
                    f32::from_le_bytes(coordinate.try_into().map_err(|_| cache_data_error())?);
                if !value.is_finite() {
                    return Err(cache_data_error());
                }
                norm_squared += f64::from(value) * f64::from(value);
                values.push(value);
            }
            remaining -= coordinate_count;
        }
        if (norm_squared - 1.0).abs() > 0.001 {
            return Err(cache_data_error());
        }
        let expected_checksum = hasher.finalize();
        let mut observed_checksum = [0_u8; CHECKSUM_BYTES];
        checkpoint()?;
        file.read_exact(&mut observed_checksum)?;
        if observed_checksum != *expected_checksum.as_bytes() {
            return Err(cache_data_error());
        }
        Ok(Arc::from(values))
    }
}

fn inventory(
    directory: &DirectoryCapability,
) -> io::Result<(u64, BTreeMap<[u8; 32], CacheEntry>, u128)> {
    let entries = directory.entries(MAX_CACHE_ENTRIES.saturating_add(2))?;
    let mut bytes = 0_u64;
    let mut inventory = BTreeMap::new();
    let mut recency = 0_u128;
    for (index, entry) in entries.into_iter().enumerate() {
        if index >= MAX_CACHE_ENTRIES.saturating_add(1) {
            return Err(cache_data_error());
        }
        let name = entry.name.to_str().ok_or_else(cache_data_error)?;
        if name == CACHE_LOCK_FILE && entry.kind == EntryKind::File {
            continue;
        }
        if entry.kind != EntryKind::File {
            return Err(cache_data_error());
        }
        let Some(identity) = parse_identity_filename(name) else {
            if name.starts_with('.') && name.ends_with(".tmp") {
                directory.remove_file(name)?;
                continue;
            }
            directory.remove_file(name)?;
            continue;
        };
        let expected_name = entry_name(&identity);
        if name != expected_name {
            directory.remove_file(name)?;
            continue;
        }
        let metadata = directory.open_private_file(name)?.metadata()?;
        let size = metadata.len();
        let last_used = metadata
            .accessed()
            .ok()
            .or_else(|| metadata.modified().ok())
            .map(timestamp)
            .unwrap_or_default();
        if inventory
            .insert(
                identity,
                CacheEntry {
                    bytes: size,
                    last_used,
                },
            )
            .is_some()
        {
            return Err(cache_data_error());
        }
        bytes = bytes.checked_add(size).ok_or_else(cache_data_error)?;
        recency = recency.max(last_used);
    }
    if inventory.len() > MAX_CACHE_ENTRIES {
        return Err(cache_data_error());
    }
    Ok((bytes, inventory, recency))
}

fn entry_name(identity: &[u8; 32]) -> String {
    format!("{}.vec", hexadecimal(identity))
}

fn temporary_name(identity: &[u8; 32]) -> String {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!(
        ".{}-{}-{sequence}.tmp",
        hexadecimal(identity),
        std::process::id()
    )
}

fn parse_identity_filename(name: &str) -> Option<[u8; 32]> {
    let stem = name.strip_suffix(".vec")?;
    if stem.len() != 64 {
        return None;
    }
    let mut identity = [0_u8; 32];
    for (index, pair) in stem.as_bytes().chunks_exact(2).enumerate() {
        let pair = std::str::from_utf8(pair).ok()?;
        identity[index] = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(identity)
}

fn hexadecimal(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn is_unit_vector(values: &[f32]) -> bool {
    let norm_squared = values
        .iter()
        .map(|value| f64::from(*value) * f64::from(*value))
        .sum::<f64>();
    (norm_squared - 1.0).abs() <= 0.001
}

fn timestamp(time: std::time::SystemTime) -> u128 {
    time.duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn write_hashed(
    writer: &mut impl Write,
    hasher: &mut blake3::Hasher,
    bytes: &[u8],
) -> io::Result<()> {
    writer.write_all(bytes)?;
    hasher.update(bytes);
    Ok(())
}

fn read_hashed(
    reader: &mut impl Read,
    hasher: &mut blake3::Hasher,
    bytes: &mut [u8],
) -> io::Result<()> {
    reader.read_exact(bytes)?;
    hasher.update(bytes);
    Ok(())
}

fn cache_size_error() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "embedding cache size overflow")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EmbeddingInvocation, EmbeddingPurpose};
    use std::collections::BTreeMap;
    use std::fs;

    fn input_identity(seed: u8) -> EmbeddingInputIdentity {
        EmbeddingInputIdentity::for_configuration(
            [seed; 32],
            EmbeddingInvocation {
                purpose: EmbeddingPurpose::Document,
                text: "cache fixture",
            },
        )
    }

    struct Fixture {
        directory: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("fixture clock")
                .as_nanos();
            let directory = std::env::temp_dir().join(format!(
                "backend-embedding-cache-{}-{stamp}",
                std::process::id()
            ));
            fs::create_dir(&directory).expect("fixture directory");
            Self { directory }
        }

        fn cache(&self) -> EmbeddingCacheFile {
            self.cache_with_dimensions(2)
        }

        fn cache_with_dimensions(&self, dimensions: u32) -> EmbeddingCacheFile {
            self.cache_with_recipe([7; 32], dimensions)
        }

        fn cache_with_recipe(&self, recipe: [u8; 32], dimensions: u32) -> EmbeddingCacheFile {
            EmbeddingCacheFile::open_in_directory(&self.directory, recipe, dimensions)
                .expect("open cache capability")
        }
    }

    fn entry_path(directory: &Path, identity: &[u8; 32]) -> PathBuf {
        directory.join(entry_name(identity))
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn exact_entries_survive_view_changes_and_cold_reopen() {
        let fixture = Fixture::new();
        let first_identity = input_identity(1);
        let later_identity = input_identity(2);
        let first_vector = [0.6, 0.8];
        let later_vector = [0.8, 0.6];
        let mut first_view = fixture.cache();
        first_view
            .store_batch(&[(first_identity, &first_vector)])
            .expect("store first view");

        // A second view adds a new input but leaves the absent old input
        // reusable; durable entries are not projection membership.
        first_view
            .store_batch(&[(later_identity, &later_vector)])
            .expect("store changed view");
        drop(first_view);

        let mut reopened = fixture.cache();
        assert_eq!(
            reopened.load(first_identity).as_deref(),
            Some(&first_vector[..])
        );
        assert_eq!(
            reopened.load(later_identity).as_deref(),
            Some(&later_vector[..])
        );
        assert!(reopened.load(input_identity(3)).is_none());
    }

    #[test]
    fn chunked_coordinate_io_preserves_the_canonical_file_and_checksum() {
        let fixture = Fixture::new();
        let dimensions = u32::try_from(COORDINATES_PER_IO_CHUNK + 17).expect("dimension");
        let identity = input_identity(0x27);
        let mut vector = vec![0.0_f32; dimensions as usize];
        vector[0] = 1.0;
        let mut cache = fixture.cache_with_dimensions(dimensions);
        cache
            .store_batch(&[(identity, &vector)])
            .expect("store vector across chunk boundaries");

        let path = entry_path(&fixture.directory, &identity.as_bytes());
        let observed = fs::read(path).expect("read canonical entry");
        let mut expected =
            Vec::with_capacity(HEADER_BYTES + vector.len() * size_of::<f32>() + CHECKSUM_BYTES);
        expected.extend_from_slice(MAGIC);
        expected.push(VERSION);
        expected.extend_from_slice(&[7; 32]);
        expected.extend_from_slice(&identity.as_bytes());
        expected.extend_from_slice(&dimensions.to_be_bytes());
        for value in &vector {
            expected.extend_from_slice(&value.to_le_bytes());
        }
        let mut hasher = blake3::Hasher::new_derive_key(CACHE_DOMAIN);
        hasher.update(&expected);
        expected.extend_from_slice(hasher.finalize().as_bytes());
        assert_eq!(observed, expected);
        assert_eq!(cache.load(identity).as_deref(), Some(vector.as_slice()));
    }

    #[test]
    fn repeated_small_batches_keep_existing_entries_when_caps_allow() {
        let fixture = Fixture::new();
        let first_identity = input_identity(0x51);
        let second_identity = input_identity(0x52);
        let mut cache = fixture.cache();
        cache
            .store_batch(&[(first_identity, &[0.6, 0.8])])
            .expect("store first small batch");
        cache
            .store_batch(&[(second_identity, &[0.8, 0.6])])
            .expect("store second small batch");
        let first_before = fs::read(entry_path(&fixture.directory, &first_identity.as_bytes()))
            .expect("first entry after second batch");
        for _ in 0..8 {
            cache
                .store_batch(&[(input_identity(0x53), &[1.0, 0.0])])
                .expect("store repeated small batch");
        }
        assert_eq!(
            fs::read(entry_path(&fixture.directory, &first_identity.as_bytes()))
                .expect("first entry remains"),
            first_before
        );
        assert!(entry_path(&fixture.directory, &second_identity.as_bytes()).exists());
        assert!(entry_path(&fixture.directory, &input_identity(0x53).as_bytes()).exists());
        assert_eq!(cache.entries.len(), 3);
    }

    #[test]
    fn recipe_switch_keeps_old_entries_but_new_identity_misses() {
        let fixture = Fixture::new();
        let identity = input_identity(0x31);
        let changed_recipe_identity = input_identity(0x32);
        let vector = [0.6, 0.8];
        let mut original = fixture.cache();
        original
            .store_batch(&[(identity, &vector)])
            .expect("store exact entry");
        drop(original);
        let mut reopened = fixture.cache_with_recipe([8; 32], 2);
        assert!(
            reopened.load(changed_recipe_identity).is_none(),
            "a changed execution recipe supplies a different input identity"
        );
        assert_eq!(
            reopened.load(identity).as_deref(),
            Some(&vector[..]),
            "unreferenced exact entries remain available for their original identity"
        );
    }

    #[test]
    fn global_cache_evicts_the_least_recently_used_exact_entry() {
        let fixture = Fixture::new();
        let first_identity = input_identity(0x41);
        let second_identity = input_identity(0x42);
        let third_identity = input_identity(0x43);
        let mut cache = fixture.cache();
        cache
            .store_batch(&[(first_identity, &[0.6, 0.8])])
            .expect("store first");
        cache
            .store_batch(&[(second_identity, &[0.8, 0.6])])
            .expect("store second");
        cache
            .entries
            .get_mut(&first_identity.as_bytes())
            .expect("first entry")
            .last_used = 1;
        cache
            .entries
            .get_mut(&second_identity.as_bytes())
            .expect("second entry")
            .last_used = 2;
        // Force the byte cap to require one eviction without allocating a
        // half-gigabyte fixture on disk.
        cache.bytes_used = MAX_CACHE_BYTES;
        cache
            .store_batch(&[(third_identity, &[1.0, 0.0])])
            .expect("store third under cap");
        assert!(!entry_path(&fixture.directory, &first_identity.as_bytes()).exists());
        assert!(entry_path(&fixture.directory, &second_identity.as_bytes()).exists());
        assert!(entry_path(&fixture.directory, &third_identity.as_bytes()).exists());
        assert_eq!(cache.bytes_used, MAX_CACHE_BYTES);
    }

    #[test]
    fn corrupt_entries_are_withdrawn_as_cache_misses() {
        let fixture = Fixture::new();
        let identity = input_identity(9);
        let mut cache = fixture.cache();
        cache
            .store_batch(&[(identity, &[1.0, 0.0])])
            .expect("store entry");
        let path = entry_path(&fixture.directory, &identity.as_bytes());
        let mut bytes = fs::read(&path).expect("entry bytes");
        bytes[HEADER_BYTES] ^= 0x01;
        fs::write(&path, bytes).expect("corrupt entry");

        assert!(cache.load(identity).is_none());
        assert!(!path.exists());
    }

    #[test]
    fn filename_collision_cannot_authorize_a_different_input_identity() {
        let fixture = Fixture::new();
        let original = input_identity(0x61);
        let colliding_name = input_identity(0x62);
        let mut cache = fixture.cache();
        cache
            .store_batch(&[(original, &[0.6, 0.8])])
            .expect("store original exact entry");
        fs::rename(
            entry_path(&fixture.directory, &original.as_bytes()),
            entry_path(&fixture.directory, &colliding_name.as_bytes()),
        )
        .expect("place valid original entry at colliding filename");

        assert!(cache.load(colliding_name).is_none());
        assert!(!entry_path(&fixture.directory, &colliding_name.as_bytes()).exists());
    }

    #[test]
    fn only_finite_unit_vectors_are_persisted() {
        let fixture = Fixture::new();
        let mut cache = fixture.cache();
        assert!(
            cache
                .store_batch(&[(input_identity(1), &[f32::NAN, 0.0])])
                .is_err()
        );
        assert!(
            cache
                .store_batch(&[(input_identity(2), &[1.0, 1.0])])
                .is_err()
        );
        assert_eq!(
            fs::read_dir(&fixture.directory).expect("directory").count(),
            1,
            "the exclusive cache lock is the only file"
        );
    }

    #[test]
    fn one_held_cache_directory_has_only_one_kernel_locked_writer() {
        let fixture = Fixture::new();
        let first = fixture.cache();
        let directory = DirectoryCapability::open(&fixture.directory).expect("held directory");
        assert!(EmbeddingCacheFile::open_directory(directory, [7; 32], 2).is_none());
        drop(first);
        let directory = DirectoryCapability::open(&fixture.directory).expect("held directory");
        assert!(EmbeddingCacheFile::open_directory(directory, [7; 32], 2).is_some());
    }

    #[test]
    fn one_input_identity_cannot_name_conflicting_vectors_in_a_batch() {
        let fixture = Fixture::new();
        let identity = input_identity(0x74);
        let mut cache = fixture.cache();
        assert!(
            cache
                .store_batch(&[(identity, &[0.6, 0.8]), (identity, &[0.8, 0.6]),])
                .is_err()
        );
        assert!(!entry_path(&fixture.directory, &identity.as_bytes()).exists());
    }

    #[test]
    fn interrupted_store_cleans_temporary_file_and_keeps_inventory_unchanged() {
        let fixture = Fixture::new();
        let identity = input_identity(0x75);
        let mut cache = fixture.cache();

        let result = cache.store_batch_with_checkpoint(&[(identity, &[0.6, 0.8])], || {
            let temporary_exists = fs::read_dir(&fixture.directory)?
                .filter_map(Result::ok)
                .any(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.ends_with(".tmp"))
                });
            if temporary_exists {
                Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "simulated cancellation after temporary creation",
                ))
            } else {
                Ok(())
            }
        });

        assert_eq!(
            result
                .expect_err("injected checkpoint must stop store")
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert!(!entry_path(&fixture.directory, &identity.as_bytes()).exists());
        assert_eq!(
            fs::read_dir(&fixture.directory)
                .expect("cache directory")
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.ends_with(".tmp"))
                })
                .count(),
            0,
            "the interrupted write removes its temporary entry"
        );
        assert_eq!(cache.bytes_used, 0);
        assert!(cache.entries.is_empty());
        assert_eq!(cache.stranded_temporary_bytes, 0);
        assert!(!cache.storage_poisoned);
    }
}

fn cache_data_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "embedding cache entry is invalid",
    )
}
