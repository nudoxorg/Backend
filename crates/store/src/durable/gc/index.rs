//! Exact disk-backed indexes for the durable mark and work queue logs.
//!
//! The append-only logs remain authoritative.  These indexes are a bounded
//! memory acceleration structure: a missing or stale sidecar is rebuilt by
//! streaming fixed-size records through a sparse open-addressed table, and a
//! failed rebuild can never make a record appear live because the next open
//! rebuilds from the authenticated log again.

use super::super::super::digest;
use super::super::{StoreError, sync_directory};
use super::roots::QueueItem;
use super::state::{GcLimits, MarkKey};
use super::{GC_MARK_RECORD_BYTES, GC_QUEUE_RECORD_BYTES, MAX_PAGE_ITEMS, io_error};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const INDEX_MAGIC: &[u8] = b"LUNA_GC_INDEX_V1\0";
const INDEX_HEADER_BYTES: usize = INDEX_MAGIC.len() + 1 + 4 + 8 + 8 + 8 + 32;
const INDEX_DOMAIN: &[u8] = b"store.gc.index.slot.v1\0";
const INDEX_HEADER_DOMAIN: &[u8] = b"store.gc.index.header.v1\0";
const INITIAL_CAPACITY: u64 = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IndexKind {
    Mark = 1,
    Queue = 2,
}

impl IndexKind {
    const fn record_bytes(self) -> usize {
        match self {
            Self::Mark => GC_MARK_RECORD_BYTES,
            Self::Queue => GC_QUEUE_RECORD_BYTES,
        }
    }

    fn decode(value: u8) -> Result<Self, StoreError> {
        match value {
            1 => Ok(Self::Mark),
            2 => Ok(Self::Queue),
            _ => Err(StoreError::Corrupt),
        }
    }

    fn validate(self, bytes: &[u8]) -> Result<(), StoreError> {
        match self {
            Self::Mark => {
                let record: [u8; GC_MARK_RECORD_BYTES] =
                    bytes.try_into().map_err(|_| StoreError::Corrupt)?;
                MarkKey::decode(&record).map(|_| ())
            }
            Self::Queue => {
                let record: [u8; GC_QUEUE_RECORD_BYTES] =
                    bytes.try_into().map_err(|_| StoreError::Corrupt)?;
                QueueItem::decode(&record).map(|_| ())
            }
        }
    }
}

struct DiskIndex {
    log_path: PathBuf,
    kind: IndexKind,
    record_bytes: usize,
    capacity: u64,
    count: u64,
    max_items: u64,
    file: File,
}

impl DiskIndex {
    fn load(log_path: &Path, kind: IndexKind, max_items: u64) -> Result<Self, StoreError> {
        let index_path = index_path(log_path, kind);
        let log_length = log_length(log_path)?;
        if index_path.is_file() {
            match Self::open_existing(
                &index_path,
                log_path.to_owned(),
                kind,
                max_items,
                log_length,
            ) {
                Ok(index) => return Ok(index),
                Err(StoreError::Corrupt | StoreError::Bounds) => {}
                Err(error) => return Err(error),
            }
        }
        Self::rebuild(log_path, kind, max_items, log_length)
    }

    fn rebuild(
        log_path: &Path,
        kind: IndexKind,
        max_items: u64,
        log_length: u64,
    ) -> Result<Self, StoreError> {
        let index_path = index_path(log_path, kind);
        let temporary = index_temp_path(log_path, kind);
        let _ = fs::remove_file(&temporary);
        let _ = fs::remove_file(&index_path);
        let directory = log_path.parent().ok_or(StoreError::Bounds)?.to_owned();
        fs::create_dir_all(&directory).map_err(|error| io_error(&error))?;

        let record_bytes = kind.record_bytes();
        let record_width = u64::try_from(record_bytes).map_err(|_| StoreError::Bounds)?;
        if !log_length.is_multiple_of(record_width) {
            return Err(StoreError::Corrupt);
        }
        let record_count = log_length / record_width;
        if record_count > max_items {
            return Err(StoreError::Bounds);
        }
        let capacity = capacity_for(record_count, max_items)?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| io_error(&error))?;
        write_header(&mut file, kind, record_bytes, capacity, 0, log_length)?;
        let table_bytes = capacity
            .checked_mul(record_width)
            .ok_or(StoreError::Bounds)?;
        let total_bytes = u64::try_from(INDEX_HEADER_BYTES)
            .map_err(|_| StoreError::Bounds)?
            .checked_add(table_bytes)
            .ok_or(StoreError::Bounds)?;
        file.set_len(total_bytes)
            .map_err(|error| io_error(&error))?;
        file.sync_all().map_err(|error| io_error(&error))?;

        let mut index = Self {
            log_path: log_path.to_owned(),
            kind,
            record_bytes,
            capacity,
            count: 0,
            max_items,
            file,
        };
        if log_length != 0 {
            let mut log = File::open(log_path).map_err(|error| io_error(&error))?;
            let mut record = vec![0; record_bytes];
            for _ in 0..record_count {
                log.read_exact(&mut record)
                    .map_err(|error| io_error(&error))?;
                index.kind.validate(&record)?;
                index.insert_slot(&record, false)?;
            }
        }
        write_header(
            &mut index.file,
            kind,
            record_bytes,
            capacity,
            index.count,
            log_length,
        )?;
        index.file.sync_all().map_err(|error| io_error(&error))?;
        drop(index.file);
        fs::rename(&temporary, &index_path).map_err(|error| io_error(&error))?;
        sync_directory(&directory)?;
        Self::open_existing(
            &index_path,
            log_path.to_owned(),
            kind,
            max_items,
            log_length,
        )
    }

    fn open_existing(
        index_path: &Path,
        log_path: PathBuf,
        kind: IndexKind,
        max_items: u64,
        expected_log_length: u64,
    ) -> Result<Self, StoreError> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(index_path)
            .map_err(|error| io_error(&error))?;
        let (record_bytes, capacity, count, log_length) = read_header(&mut file, kind)?;
        if record_bytes != kind.record_bytes() {
            return Err(StoreError::Corrupt);
        }
        let maximum = maximum_capacity(max_items)?;
        if capacity < INITIAL_CAPACITY || capacity > maximum || count > max_items {
            return Err(StoreError::Bounds);
        }
        let record_width = u64::try_from(record_bytes).map_err(|_| StoreError::Bounds)?;
        if log_length != expected_log_length || !log_length.is_multiple_of(record_width) {
            return Err(StoreError::Corrupt);
        }
        let expected = u64::try_from(INDEX_HEADER_BYTES)
            .map_err(|_| StoreError::Bounds)?
            .checked_add(
                capacity
                    .checked_mul(u64::try_from(record_bytes).map_err(|_| StoreError::Bounds)?)
                    .ok_or(StoreError::Bounds)?,
            )
            .ok_or(StoreError::Bounds)?;
        if file.metadata().map_err(|error| io_error(&error))?.len() != expected {
            return Err(StoreError::Corrupt);
        }
        Ok(Self {
            log_path,
            kind,
            record_bytes,
            capacity,
            count,
            max_items,
            file,
        })
    }

    fn contains(&mut self, bytes: &[u8]) -> Result<bool, StoreError> {
        self.kind.validate(bytes)?;
        let mut slot = slot_for(bytes, self.capacity);
        let mut observed = vec![0; self.record_bytes];
        for _ in 0..self.capacity {
            self.read_slot(slot, &mut observed)?;
            if observed.iter().all(|byte| *byte == 0) {
                return Ok(false);
            }
            self.kind.validate(&observed)?;
            if observed == bytes {
                return Ok(true);
            }
            slot = (slot + 1) % self.capacity;
        }
        Err(StoreError::Corrupt)
    }

    fn insert(&mut self, bytes: &[u8]) -> Result<bool, StoreError> {
        self.kind.validate(bytes)?;
        if self.count.saturating_mul(10) >= self.capacity.saturating_mul(7) {
            let replacement = Self::rebuild(
                &self.log_path,
                self.kind,
                self.max_items,
                log_length(&self.log_path)?,
            )?;
            *self = replacement;
        }
        let inserted = self.insert_slot(bytes, true)?;
        if inserted {
            write_header(
                &mut self.file,
                self.kind,
                self.record_bytes,
                self.capacity,
                self.count,
                log_length(&self.log_path)?,
            )?;
            self.file.sync_data().map_err(|error| io_error(&error))?;
        }
        Ok(inserted)
    }

    fn insert_slot(&mut self, bytes: &[u8], sync: bool) -> Result<bool, StoreError> {
        let mut slot = slot_for(bytes, self.capacity);
        let mut observed = vec![0; self.record_bytes];
        for _ in 0..self.capacity {
            self.read_slot(slot, &mut observed)?;
            if observed.iter().all(|byte| *byte == 0) {
                let offset = self.slot_offset(slot)?;
                self.file
                    .seek(SeekFrom::Start(offset))
                    .map_err(|error| io_error(&error))?;
                self.file
                    .write_all(bytes)
                    .map_err(|error| io_error(&error))?;
                if sync {
                    self.file.sync_data().map_err(|error| io_error(&error))?;
                }
                self.count = self.count.checked_add(1).ok_or(StoreError::Bounds)?;
                return Ok(true);
            }
            self.kind.validate(&observed)?;
            if observed == bytes {
                return Ok(false);
            }
            slot = (slot + 1) % self.capacity;
        }
        Err(StoreError::Bounds)
    }

    fn read_slot(&mut self, slot: u64, output: &mut [u8]) -> Result<(), StoreError> {
        let offset = self.slot_offset(slot)?;
        self.file
            .seek(SeekFrom::Start(offset))
            .map_err(|error| io_error(&error))?;
        self.file
            .read_exact(output)
            .map_err(|error| io_error(&error))
    }

    fn slot_offset(&self, slot: u64) -> Result<u64, StoreError> {
        if slot >= self.capacity {
            return Err(StoreError::Bounds);
        }
        u64::try_from(INDEX_HEADER_BYTES)
            .map_err(|_| StoreError::Bounds)?
            .checked_add(
                slot.checked_mul(u64::try_from(self.record_bytes).map_err(|_| StoreError::Bounds)?)
                    .ok_or(StoreError::Bounds)?,
            )
            .ok_or(StoreError::Bounds)
    }
}

pub(super) struct MarkIndex {
    disk: DiskIndex,
}

impl MarkIndex {
    pub(super) fn load(path: &Path, limits: GcLimits) -> Result<Self, StoreError> {
        Ok(Self {
            disk: DiskIndex::load(path, IndexKind::Mark, limits.max_mark_items)?,
        })
    }

    pub(super) fn contains(&mut self, key: MarkKey) -> Result<bool, StoreError> {
        self.disk.contains(&key.encode())
    }

    pub(super) fn insert(&mut self, key: MarkKey) -> Result<bool, StoreError> {
        self.disk.insert(&key.encode())
    }

    pub(super) fn len(&self) -> usize {
        match usize::try_from(self.disk.count) {
            Ok(value) => value,
            Err(_) => usize::MAX,
        }
    }
}

pub(super) struct QueueIndex {
    disk: DiskIndex,
}

impl QueueIndex {
    pub(super) fn load(path: &Path, limits: GcLimits) -> Result<Self, StoreError> {
        Ok(Self {
            disk: DiskIndex::load(path, IndexKind::Queue, limits.max_queue_items)?,
        })
    }

    pub(super) fn retain_new(&mut self, items: &[QueueItem]) -> Result<Vec<QueueItem>, StoreError> {
        if items.len() > MAX_PAGE_ITEMS {
            return Err(StoreError::Bounds);
        }
        let mut seen = std::collections::HashSet::with_capacity(items.len());
        let mut new_items = Vec::with_capacity(items.len());
        for &item in items {
            if seen.insert(item) && !self.disk.contains(&item.encode())? {
                new_items.push(item);
            }
        }
        Ok(new_items)
    }

    pub(super) fn insert(&mut self, item: QueueItem) -> Result<bool, StoreError> {
        self.disk.insert(&item.encode())
    }
}

fn index_path(log_path: &Path, kind: IndexKind) -> PathBuf {
    log_path.with_file_name(match kind {
        IndexKind::Mark => "mark.index",
        IndexKind::Queue => "queue.index",
    })
}

fn index_temp_path(log_path: &Path, kind: IndexKind) -> PathBuf {
    log_path.with_file_name(match kind {
        IndexKind::Mark => "mark.index.tmp",
        IndexKind::Queue => "queue.index.tmp",
    })
}

fn capacity_for(count: u64, max_items: u64) -> Result<u64, StoreError> {
    let desired = count
        .checked_mul(2)
        .and_then(|value| value.checked_add(INITIAL_CAPACITY))
        .ok_or(StoreError::Bounds)?
        .max(INITIAL_CAPACITY);
    let maximum = maximum_capacity(max_items)?;
    let mut capacity = INITIAL_CAPACITY;
    while capacity < desired {
        capacity = capacity.checked_mul(2).ok_or(StoreError::Bounds)?;
    }
    if capacity > maximum {
        return Err(StoreError::Bounds);
    }
    Ok(capacity)
}

fn maximum_capacity(max_items: u64) -> Result<u64, StoreError> {
    let target = max_items
        .checked_mul(2)
        .and_then(|value| value.checked_add(INITIAL_CAPACITY))
        .ok_or(StoreError::Bounds)?;
    let mut capacity = INITIAL_CAPACITY;
    while capacity < target {
        capacity = capacity.checked_mul(2).ok_or(StoreError::Bounds)?;
    }
    Ok(capacity)
}

fn slot_for(bytes: &[u8], capacity: u64) -> u64 {
    let hash = digest(INDEX_DOMAIN, bytes);
    let prefix = [
        hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6], hash[7],
    ];
    u64::from_be_bytes(prefix) % capacity
}

fn log_length(path: &Path) -> Result<u64, StoreError> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(io_error(&error)),
    }
}

fn write_header(
    file: &mut File,
    kind: IndexKind,
    record_bytes: usize,
    capacity: u64,
    count: u64,
    log_length: u64,
) -> Result<(), StoreError> {
    let mut header = Vec::with_capacity(INDEX_HEADER_BYTES);
    header.extend_from_slice(INDEX_MAGIC);
    header.push(kind as u8);
    header.extend_from_slice(
        &u32::try_from(record_bytes)
            .map_err(|_| StoreError::Bounds)?
            .to_be_bytes(),
    );
    header.extend_from_slice(&capacity.to_be_bytes());
    header.extend_from_slice(&count.to_be_bytes());
    header.extend_from_slice(&log_length.to_be_bytes());
    header.extend_from_slice(&digest(INDEX_HEADER_DOMAIN, &header));
    file.seek(SeekFrom::Start(0))
        .map_err(|error| io_error(&error))?;
    file.write_all(&header).map_err(|error| io_error(&error))
}

fn read_header(file: &mut File, expected: IndexKind) -> Result<(usize, u64, u64, u64), StoreError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| io_error(&error))?;
    let mut bytes = vec![0; INDEX_HEADER_BYTES];
    file.read_exact(&mut bytes)
        .map_err(|error| io_error(&error))?;
    if !bytes.starts_with(INDEX_MAGIC) {
        return Err(StoreError::Corrupt);
    }
    let checksum_at = INDEX_HEADER_BYTES
        .checked_sub(32)
        .ok_or(StoreError::Bounds)?;
    if digest(INDEX_HEADER_DOMAIN, &bytes[..checksum_at]) != bytes[checksum_at..] {
        return Err(StoreError::Corrupt);
    }
    let mut at = INDEX_MAGIC.len();
    let kind = IndexKind::decode(*bytes.get(at).ok_or(StoreError::Corrupt)?)?;
    if kind != expected {
        return Err(StoreError::Corrupt);
    }
    at = at.checked_add(1).ok_or(StoreError::Bounds)?;
    let record_bytes = u32::from_be_bytes(
        bytes
            .get(at..at.checked_add(4).ok_or(StoreError::Bounds)?)
            .ok_or(StoreError::Corrupt)?
            .try_into()
            .map_err(|_| StoreError::Corrupt)?,
    );
    at = at.checked_add(4).ok_or(StoreError::Bounds)?;
    let capacity = u64::from_be_bytes(
        bytes
            .get(at..at.checked_add(8).ok_or(StoreError::Bounds)?)
            .ok_or(StoreError::Corrupt)?
            .try_into()
            .map_err(|_| StoreError::Corrupt)?,
    );
    at = at.checked_add(8).ok_or(StoreError::Bounds)?;
    let count = u64::from_be_bytes(
        bytes
            .get(at..at.checked_add(8).ok_or(StoreError::Bounds)?)
            .ok_or(StoreError::Corrupt)?
            .try_into()
            .map_err(|_| StoreError::Corrupt)?,
    );
    at = at.checked_add(8).ok_or(StoreError::Bounds)?;
    let log_length = u64::from_be_bytes(
        bytes
            .get(at..at.checked_add(8).ok_or(StoreError::Bounds)?)
            .ok_or(StoreError::Corrupt)?
            .try_into()
            .map_err(|_| StoreError::Corrupt)?,
    );
    Ok((
        usize::try_from(record_bytes).map_err(|_| StoreError::Bounds)?,
        capacity,
        count,
        log_length,
    ))
}
