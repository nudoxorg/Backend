//! Durable GC queue, mark, root, and validation records.

use super::super::super::digest;
use super::super::{Hash, StoreError, sync_directory};
use super::index::QueueIndex;
use super::roots::QueueItem;
use super::state::{GcLimits, GcState, MarkKey, Phase, read_u32_be};
use super::sweep::GcPaths;
use super::{
    GC_CANDIDATE_DIGEST_DOMAIN, GC_CANDIDATE_RECORD_BYTES, GC_MARK_DIGEST_DOMAIN,
    GC_MARK_RECORD_BYTES, GC_QUEUE_DIGEST_DOMAIN, GC_QUEUE_RECORD_BYTES, GC_ROOT_MAGIC,
    GC_ROOT_RECORD_BYTES, MAX_ROOTS, io_error,
};
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const MARK_SORT_CHUNK_RECORDS: usize = 4_096;
const MARK_SORT_FAN_IN: usize = 32;

/// Reads a small collector metadata file without allowing a corrupt length to
/// turn recovery into an unbounded allocation. Large logs are always streamed
/// by their fixed-record readers instead.
pub(super) fn read_bounded(path: &Path, maximum: usize) -> Result<Option<Vec<u8>>, StoreError> {
    let length = match fs::metadata(path) {
        Ok(metadata) => usize::try_from(metadata.len()).map_err(|_| StoreError::Bounds)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(&error)),
    };
    if length > maximum {
        return Err(StoreError::Bounds);
    }
    fs::read(path).map(Some).map_err(|error| io_error(&error))
}

pub(super) fn encode_roots(items: &[QueueItem], limits: GcLimits) -> Result<Vec<u8>, StoreError> {
    if items.len() > limits.max_roots || items.len() > MAX_ROOTS {
        return Err(StoreError::Bounds);
    }
    let count = items
        .len()
        .checked_mul(GC_ROOT_RECORD_BYTES)
        .and_then(|length| length.checked_add(GC_ROOT_MAGIC.len() + 4 + 32))
        .ok_or(StoreError::Bounds)?;
    let mut bytes = Vec::with_capacity(count);
    bytes.extend_from_slice(GC_ROOT_MAGIC);
    bytes.extend_from_slice(
        &u32::try_from(items.len())
            .map_err(|_| StoreError::Bounds)?
            .to_be_bytes(),
    );
    for item in items {
        bytes.extend_from_slice(&item.encode());
    }
    bytes.extend_from_slice(&digest(b"store.gc.roots.payload.v1\0", &bytes));
    Ok(bytes)
}

pub(super) fn persist_state(paths: &GcPaths, state: &GcState) -> Result<(), StoreError> {
    let bytes = state.encode()?;
    write_atomic_store(&paths.state, &bytes, &paths.dir)?;
    Ok(())
}

pub(super) fn write_atomic_store(
    path: &Path,
    bytes: &[u8],
    directory: &Path,
) -> Result<(), StoreError> {
    let parent = path.parent().ok_or(StoreError::Bounds)?;
    fs::create_dir_all(parent).map_err(|error| io_error(&error))?;
    super::super::objects::write_atomic(path, bytes, directory)
}

pub(super) fn empty_log_digest(domain: &[u8]) -> Hash {
    digest(domain, &[])
}

pub(super) fn extend_log_digest(previous: Hash, domain: &[u8], record: &[u8]) -> Hash {
    let mut input = Vec::with_capacity(previous.len().saturating_add(record.len()));
    input.extend_from_slice(&previous);
    input.extend_from_slice(record);
    digest(domain, &input)
}

pub(super) fn log_digest(
    path: &Path,
    record_bytes: usize,
    domain: &[u8],
) -> Result<Hash, StoreError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(empty_log_digest(domain));
        }
        Err(error) => return Err(io_error(&error)),
    };
    let length = file.metadata().map_err(|error| io_error(&error))?.len();
    let record_len = u64::try_from(record_bytes).map_err(|_| StoreError::Bounds)?;
    if !length.is_multiple_of(record_len) {
        return Err(StoreError::Corrupt);
    }
    let count = length / record_len;
    let mut current = empty_log_digest(domain);
    let mut record = vec![0; record_bytes];
    for _ in 0..count {
        file.read_exact(&mut record)
            .map_err(|error| io_error(&error))?;
        current = extend_log_digest(current, domain, &record);
    }
    Ok(current)
}

pub(super) fn read_queue_item(path: &Path, offset: u64) -> Result<Option<QueueItem>, StoreError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(&error)),
    };
    let length = file.metadata().map_err(|error| io_error(&error))?.len();
    if offset > length {
        return Err(StoreError::Corrupt);
    }
    if offset == length {
        return Ok(None);
    }
    let record_bytes = u64::try_from(GC_QUEUE_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
    if !offset.is_multiple_of(record_bytes) || length - offset < record_bytes {
        return Err(StoreError::Corrupt);
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| io_error(&error))?;
    let mut bytes = [0; GC_QUEUE_RECORD_BYTES];
    file.read_exact(&mut bytes)
        .map_err(|error| io_error(&error))?;
    QueueItem::decode(&bytes).map(Some)
}

pub(super) fn append_queue_items(
    paths: &GcPaths,
    items: &[QueueItem],
    limits: GcLimits,
    queue_index: &mut QueueIndex,
) -> Result<Vec<QueueItem>, StoreError> {
    let items = queue_index.retain_new(items)?;
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let mut bytes = Vec::with_capacity(
        items
            .len()
            .checked_mul(GC_QUEUE_RECORD_BYTES)
            .ok_or(StoreError::Bounds)?,
    );
    for item in &items {
        bytes.extend_from_slice(&item.encode());
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.queue)
        .map_err(|error| io_error(&error))?;
    let prior = file.metadata().map_err(|error| io_error(&error))?.len();
    let additions = u64::try_from(items.len()).map_err(|_| StoreError::Bounds)?;
    let existing = prior
        .checked_div(u64::try_from(GC_QUEUE_RECORD_BYTES).map_err(|_| StoreError::Bounds)?)
        .ok_or(StoreError::Bounds)?;
    if existing.checked_add(additions).ok_or(StoreError::Bounds)? > limits.max_queue_items {
        return Err(StoreError::Bounds);
    }
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        let _ = file.set_len(prior);
        let _ = file.sync_all();
        return Err(io_error(&error));
    }
    sync_directory(&paths.dir)?;
    for item in &items {
        queue_index.insert(*item)?;
    }
    Ok(items)
}

pub(super) fn append_mark(path: &Path, key: MarkKey, directory: &Path) -> Result<(), StoreError> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| io_error(&error))?;
    file.write_all(&key.encode())
        .and_then(|()| file.sync_all())
        .map_err(|error| io_error(&error))?;
    sync_directory(directory)
}

/// Tests membership in the frozen, sorted mark index using binary search.
/// The sweep never scans the mark log once per candidate; each lookup performs
/// only logarithmic fixed-record seeks and reads.
pub(super) fn mark_contains_sorted(path: &Path, key: MarkKey) -> Result<bool, StoreError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(io_error(&error)),
    };
    let length = file.metadata().map_err(|error| io_error(&error))?.len();
    let record_bytes = u64::try_from(GC_MARK_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
    if !length.is_multiple_of(record_bytes) {
        return Err(StoreError::Corrupt);
    }
    let count = length / record_bytes;
    let needle = key.encode();
    let mut low = 0u64;
    let mut high = count;
    let mut bytes = [0; GC_MARK_RECORD_BYTES];
    while low < high {
        let middle = low + (high - low) / 2;
        let offset = middle.checked_mul(record_bytes).ok_or(StoreError::Bounds)?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|error| io_error(&error))?;
        file.read_exact(&mut bytes)
            .map_err(|error| io_error(&error))?;
        // The index was frozen from admitted MarkKey values.  Decode here as
        // well so a torn/corrupt index cannot make a live object look absent.
        MarkKey::decode(&bytes)?;
        match bytes.cmp(&needle) {
            Ordering::Less => low = middle.checked_add(1).ok_or(StoreError::Bounds)?,
            Ordering::Greater => high = middle,
            Ordering::Equal => return Ok(true),
        }
    }
    Ok(false)
}

/// Sorts and deduplicates the append-only mark log before sweep begins.
///
/// Sorting is an external merge: at most one fixed-size chunk and one record
/// per merge input are resident at a time.  The replacement is atomic; a
/// crash leaves either the old valid log or the complete frozen index, and the
/// state transition is persisted only after this function returns.
pub(super) fn freeze_mark_index(
    path: &Path,
    limits: GcLimits,
    directory: &Path,
) -> Result<Hash, StoreError> {
    let run_dir = directory.join(".mark-runs");
    let _ = fs::remove_dir_all(&run_dir);
    let _ = fs::remove_file(path.with_file_name("mark.freeze.tmp"));
    fs::create_dir_all(&run_dir).map_err(|error| io_error(&error))?;

    let result = freeze_mark_index_inner(path, limits, directory, &run_dir);
    if result.is_err() {
        let _ = fs::remove_dir_all(&run_dir);
        let _ = fs::remove_file(path.with_file_name("mark.freeze.tmp"));
        let _ = sync_directory(directory);
    } else {
        fs::remove_dir_all(&run_dir).map_err(|error| io_error(&error))?;
        sync_directory(directory)?;
    }
    result
}

fn freeze_mark_index_inner(
    path: &Path,
    limits: GcLimits,
    directory: &Path,
    run_dir: &Path,
) -> Result<Hash, StoreError> {
    let mut runs = make_mark_runs(path, limits, run_dir)?;
    let mut pass = 0usize;
    while runs.len() > 1 {
        let mut next = Vec::with_capacity(runs.len().div_ceil(MARK_SORT_FAN_IN));
        for (group, inputs) in runs.chunks(MARK_SORT_FAN_IN).enumerate() {
            let output = run_dir.join(format!("merge-{pass:08}-{group:08}"));
            merge_mark_runs(inputs, &output)?;
            for input in inputs {
                fs::remove_file(input).map_err(|error| io_error(&error))?;
            }
            next.push(output);
        }
        runs = next;
        pass = pass.checked_add(1).ok_or(StoreError::Bounds)?;
    }

    match runs.first() {
        Some(run) => atomic_copy_mark_run(run, path, directory)?,
        None => write_atomic_store(path, &[], directory)?,
    }
    #[cfg(test)]
    if super::take_test_fault(5) {
        return Err(StoreError::Io(
            "injected GC failure during mark index freeze".to_owned(),
        ));
    }
    log_digest(path, GC_MARK_RECORD_BYTES, GC_MARK_DIGEST_DOMAIN)
}

fn make_mark_runs(
    path: &Path,
    limits: GcLimits,
    run_dir: &Path,
) -> Result<Vec<PathBuf>, StoreError> {
    let mut source = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io_error(&error)),
    };
    let length = source.metadata().map_err(|error| io_error(&error))?.len();
    let record_bytes = u64::try_from(GC_MARK_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
    if !length.is_multiple_of(record_bytes) {
        return Err(StoreError::Corrupt);
    }
    let count = length / record_bytes;
    if count > limits.max_mark_items {
        return Err(StoreError::Bounds);
    }
    let mut runs = Vec::new();
    let mut records = Vec::with_capacity(MARK_SORT_CHUNK_RECORDS);
    let mut remaining = count;
    let mut run_number = 0usize;
    while remaining != 0 {
        records.clear();
        let take = remaining.min(MARK_SORT_CHUNK_RECORDS as u64);
        for _ in 0..take {
            let mut record = [0; GC_MARK_RECORD_BYTES];
            source
                .read_exact(&mut record)
                .map_err(|error| io_error(&error))?;
            MarkKey::decode(&record)?;
            records.push(record);
        }
        records.sort_unstable();
        records.dedup();
        let run = run_dir.join(format!("run-{run_number:08}"));
        write_mark_run(&run, &records)?;
        runs.push(run);
        run_number = run_number.checked_add(1).ok_or(StoreError::Bounds)?;
        remaining -= take;
    }
    Ok(runs)
}

fn write_mark_run(path: &Path, records: &[[u8; GC_MARK_RECORD_BYTES]]) -> Result<(), StoreError> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|error| io_error(&error))?;
    for record in records {
        file.write_all(record).map_err(|error| io_error(&error))?;
    }
    file.sync_all().map_err(|error| io_error(&error))
}

struct MarkRunReader {
    file: File,
    remaining: u64,
}

impl MarkRunReader {
    fn open(path: &Path) -> Result<Self, StoreError> {
        let file = File::open(path).map_err(|error| io_error(&error))?;
        let length = file.metadata().map_err(|error| io_error(&error))?.len();
        let record_bytes = u64::try_from(GC_MARK_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
        if !length.is_multiple_of(record_bytes) {
            return Err(StoreError::Corrupt);
        }
        Ok(Self {
            file,
            remaining: length / record_bytes,
        })
    }

    fn next(&mut self) -> Result<Option<[u8; GC_MARK_RECORD_BYTES]>, StoreError> {
        if self.remaining == 0 {
            return Ok(None);
        }
        let mut record = [0; GC_MARK_RECORD_BYTES];
        self.file
            .read_exact(&mut record)
            .map_err(|error| io_error(&error))?;
        self.remaining -= 1;
        MarkKey::decode(&record)?;
        Ok(Some(record))
    }
}

fn merge_mark_runs(inputs: &[PathBuf], output: &Path) -> Result<(), StoreError> {
    let mut readers = Vec::with_capacity(inputs.len());
    let mut heap = BinaryHeap::new();
    for (index, input) in inputs.iter().enumerate() {
        let mut reader = MarkRunReader::open(input)?;
        if let Some(record) = reader.next()? {
            heap.push(std::cmp::Reverse((record, index)));
        }
        readers.push(reader);
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(output)
        .map_err(|error| io_error(&error))?;
    let mut previous = None;
    while let Some(std::cmp::Reverse((record, index))) = heap.pop() {
        if previous != Some(record) {
            file.write_all(&record).map_err(|error| io_error(&error))?;
            previous = Some(record);
        }
        let reader = readers.get_mut(index).ok_or(StoreError::Corrupt)?;
        if let Some(next) = reader.next()? {
            heap.push(std::cmp::Reverse((next, index)));
        }
    }
    file.sync_all().map_err(|error| io_error(&error))
}

fn atomic_copy_mark_run(
    run: &Path,
    destination: &Path,
    directory: &Path,
) -> Result<(), StoreError> {
    let temporary = destination.with_file_name("mark.freeze.tmp");
    let mut source = File::open(run).map_err(|error| io_error(&error))?;
    let mut target = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(|error| io_error(&error))?;
    io::copy(&mut source, &mut target).map_err(|error| io_error(&error))?;
    target.sync_all().map_err(|error| io_error(&error))?;
    fs::rename(&temporary, destination).map_err(|error| io_error(&error))?;
    sync_directory(directory)?;
    #[cfg(test)]
    if super::take_test_fault(6) {
        return Err(StoreError::Io(
            "injected GC failure after mark index rename".to_owned(),
        ));
    }
    Ok(())
}

fn validate_sorted_mark(path: &Path) -> Result<(), StoreError> {
    let mut file = File::open(path).map_err(|error| io_error(&error))?;
    let length = file.metadata().map_err(|error| io_error(&error))?.len();
    let record_bytes = u64::try_from(GC_MARK_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
    if !length.is_multiple_of(record_bytes) {
        return Err(StoreError::Corrupt);
    }
    let mut prior: Option<[u8; GC_MARK_RECORD_BYTES]> = None;
    let mut remaining = length / record_bytes;
    while remaining != 0 {
        let mut record = [0; GC_MARK_RECORD_BYTES];
        file.read_exact(&mut record)
            .map_err(|error| io_error(&error))?;
        MarkKey::decode(&record)?;
        if prior.is_some_and(|previous| previous >= record) {
            return Err(StoreError::Corrupt);
        }
        prior = Some(record);
        remaining -= 1;
    }
    Ok(())
}

fn validate_candidate_state(
    paths: &GcPaths,
    state: &GcState,
    limits: GcLimits,
) -> Result<(), StoreError> {
    if state.phase == Phase::Mark {
        return if state.candidate_length == 0
            && state.candidate_digest == empty_log_digest(GC_CANDIDATE_DIGEST_DOMAIN)
        {
            Ok(())
        } else {
            Err(StoreError::Corrupt)
        };
    }
    if state.phase == Phase::Finished {
        return Ok(());
    }
    let kind = match state.phase {
        Phase::SweepPacks => super::sweep::SweepKind::Pack,
        Phase::SweepClosures => super::sweep::SweepKind::Closure,
        Phase::SweepObjects => super::sweep::SweepKind::Object,
        Phase::Mark | Phase::Finished => return Err(StoreError::Corrupt),
    };
    let candidate = paths.candidate_index(kind);
    let length = super::sweep::validate_candidate_index(&candidate, kind, limits)?;
    if length != state.candidate_length {
        return Err(StoreError::Corrupt);
    }
    if log_digest(
        &candidate,
        GC_CANDIDATE_RECORD_BYTES,
        GC_CANDIDATE_DIGEST_DOMAIN,
    )? != state.candidate_digest
    {
        return Err(StoreError::Corrupt);
    }
    let offset = if state.cursor.is_empty() {
        0
    } else {
        u64::from_be_bytes(
            state
                .cursor
                .as_slice()
                .try_into()
                .map_err(|_| StoreError::Corrupt)?,
        )
    };
    let record_bytes = u64::try_from(GC_CANDIDATE_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
    if offset > length || !offset.is_multiple_of(record_bytes) {
        return Err(StoreError::Corrupt);
    }
    Ok(())
}

pub(super) fn validate_gc_files(
    paths: &GcPaths,
    state: &GcState,
    limits: GcLimits,
) -> Result<(), StoreError> {
    if !state
        .queue_offset
        .is_multiple_of(u64::try_from(GC_QUEUE_RECORD_BYTES).map_err(|_| StoreError::Bounds)?)
    {
        return Err(StoreError::Corrupt);
    }
    if matches!(
        state.phase,
        Phase::SweepPacks | Phase::SweepClosures | Phase::SweepObjects
    ) && !state.cursor.is_empty()
        && state.cursor.len() != 8
    {
        return Err(StoreError::Corrupt);
    }
    if state.swept_items > state.examined_items
        || state.examined_items
            > limits
                .max_queue_items
                .checked_mul(3)
                .ok_or(StoreError::Bounds)?
    {
        return Err(StoreError::Corrupt);
    }
    let root_bytes = fs::read(&paths.roots).map_err(|error| io_error(&error))?;
    if digest(b"store.gc.roots.v1\0", &root_bytes) != state.roots_digest {
        return Err(StoreError::Corrupt);
    }
    validate_roots_bytes(&root_bytes, limits)?;
    let roots = fs::metadata(&paths.roots).map_err(|error| io_error(&error))?;
    if roots.len()
        > u64::try_from(
            GC_ROOT_MAGIC
                .len()
                .checked_add(4)
                .and_then(|n| n.checked_add(32))
                .and_then(|n| n.checked_add(limits.max_roots.checked_mul(GC_ROOT_RECORD_BYTES)?))
                .ok_or(StoreError::Bounds)?,
        )
        .map_err(|_| StoreError::Bounds)?
    {
        return Err(StoreError::Bounds);
    }
    let queue = fs::metadata(&paths.queue).map_err(|error| io_error(&error))?;
    let queue_record_bytes =
        u64::try_from(GC_QUEUE_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
    if queue.len() % queue_record_bytes != 0
        || queue.len() / queue_record_bytes > limits.max_queue_items
        || state.queue_offset > queue.len()
    {
        return Err(StoreError::Corrupt);
    }
    let mark = fs::metadata(&paths.mark).map_err(|error| io_error(&error))?;
    let mark_record_bytes = u64::try_from(GC_MARK_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
    if mark.len() % mark_record_bytes != 0
        || mark.len() / mark_record_bytes > limits.max_mark_items
        || state.marked_items > limits.max_mark_items
    {
        return Err(StoreError::Corrupt);
    }
    if log_digest(&paths.queue, GC_QUEUE_RECORD_BYTES, GC_QUEUE_DIGEST_DOMAIN)?
        != state.queue_digest
        || log_digest(&paths.mark, GC_MARK_RECORD_BYTES, GC_MARK_DIGEST_DOMAIN)?
            != state.mark_digest
    {
        return Err(StoreError::Corrupt);
    }
    if state.phase != Phase::Mark {
        validate_sorted_mark(&paths.mark)?;
    }
    validate_candidate_state(paths, state, limits)?;
    Ok(())
}

pub(super) fn validate_roots_bytes(bytes: &[u8], limits: GcLimits) -> Result<(), StoreError> {
    let header = GC_ROOT_MAGIC
        .len()
        .checked_add(4)
        .ok_or(StoreError::Bounds)?;
    if bytes.len() < header + 32 || !bytes.starts_with(GC_ROOT_MAGIC) {
        return Err(StoreError::Corrupt);
    }
    let mut at = GC_ROOT_MAGIC.len();
    let count = usize::try_from(read_u32_be(bytes, &mut at)?).map_err(|_| StoreError::Bounds)?;
    if count > limits.max_roots || count > MAX_ROOTS {
        return Err(StoreError::Bounds);
    }
    let records = count
        .checked_mul(GC_ROOT_RECORD_BYTES)
        .ok_or(StoreError::Bounds)?;
    let checksum_at = header.checked_add(records).ok_or(StoreError::Bounds)?;
    if checksum_at.checked_add(32).ok_or(StoreError::Bounds)? != bytes.len() {
        return Err(StoreError::Corrupt);
    }
    for chunk in bytes[header..checksum_at].chunks_exact(GC_ROOT_RECORD_BYTES) {
        let record: [u8; GC_QUEUE_RECORD_BYTES] =
            chunk.try_into().map_err(|_| StoreError::Corrupt)?;
        QueueItem::decode(&record)?;
    }
    let checksum: Hash = bytes[checksum_at..]
        .try_into()
        .map_err(|_| StoreError::Corrupt)?;
    if digest(b"store.gc.roots.payload.v1\0", &bytes[..checksum_at]) != checksum {
        return Err(StoreError::Corrupt);
    }
    Ok(())
}
