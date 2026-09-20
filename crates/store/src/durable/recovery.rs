//! Hash-chain publication recovery and selected-head persistence.

use super::{
    FileStore, HEAD_BYTES, HEAD_MAGIC, Hash, JOURNAL_MAGIC, JOURNAL_RECORD_BYTES, JOURNAL_VERSION,
    PREPARED_TAG, PUBLISHED_TAG, Path, PublicationBase, PublicationDescriptor, SelectedHead,
    StoreError, io_error, read_byte, read_hash, read_u64, sync_directory,
};
use std::{
    fs::{self, File, OpenOptions},
    io::{Seek, SeekFrom},
};

#[cfg(test)]
use std::sync::Mutex;

#[cfg(test)]
static TEST_FAULT: Mutex<Option<(std::thread::ThreadId, u8)>> = Mutex::new(None);

#[cfg(test)]
pub(crate) fn set_test_fault(point: u8) {
    if let Ok(mut fault) = TEST_FAULT.lock() {
        *fault = Some((std::thread::current().id(), point));
    }
}

#[cfg(test)]
fn take_test_fault(point: u8) -> bool {
    let Ok(mut fault) = TEST_FAULT.lock() else {
        return false;
    };
    let current = std::thread::current().id();
    if fault
        .as_ref()
        .is_some_and(|(owner, pending)| *owner == current && *pending == point)
    {
        *fault = None;
        true
    } else {
        false
    }
}

/// The fixed-memory journal state retained by an open [`FileStore`].
///
/// The selected head is the only publication history needed by the store's
/// linearization checks. The checksum and byte offset let a subsequent append
/// validate and continue the chain without rereading prior frames.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct JournalTail {
    pub(super) sequence: u64,
    pub(super) checksum: Hash,
    pub(super) offset: u64,
    pub(super) selected: Option<SelectedHead>,
}

#[path = "recovery_head.rs"]
mod head;
#[path = "recovery_journal.rs"]
mod journal;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct JournalRecord {
    pub(super) tag: u8,
    pub(super) sequence: u64,
    pub(super) paired_sequence: u64,
    pub(super) previous: Hash,
    pub(super) payload: PublicationDescriptor,
    pub(super) checksum: Hash,
}

/// Fixed-size receipt that checkpoints the validated journal tail alongside
/// the selected publication. A valid receipt makes warm reopen check only the
/// tail frame; a missing or malformed receipt explicitly falls back to a
/// streaming journal scan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct JournalCheckpoint {
    pub(super) head: SelectedHead,
    pub(super) tail: JournalTail,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct JournalState {
    pub(super) selected: Option<SelectedHead>,
}

pub(super) fn base_matches(
    current: Option<&PublicationDescriptor>,
    expected: Option<PublicationBase>,
) -> bool {
    match (current, expected) {
        (None, None) => true,
        (None, Some(_)) | (Some(_), None) => false,
        (Some(current), Some(expected)) => {
            expected.target == current.target && expected.generation == current.target_generation
        }
    }
}

pub(super) fn descriptor_matches_base(
    current: Option<&PublicationDescriptor>,
    expected: &PublicationDescriptor,
) -> bool {
    match current {
        None => expected.base.is_none() && expected.base_generation == 0,
        Some(current) => {
            expected.base == Some(current.target)
                && current.target_generation == expected.base_generation
        }
    }
}

impl FileStore {
    pub(super) fn read_state(&self) -> Result<JournalState, StoreError> {
        let head_file = head::read_head(&self.root.join("HEAD"))?;
        if let Some(receipt) = head_file {
            let mut cached = self.journal_tail.lock().map_err(|_| StoreError::Corrupt)?;
            if cached.offset < receipt.tail.offset {
                *cached = receipt.tail;
            }
        }
        let tail = self.refresh_journal_tail()?;
        let head = head_file.map(|receipt| receipt.head);
        if let Some(head) = head {
            head::validate_head_record(&self.root.join("journal"), &tail, &head)?;
        }
        if let Some(head) = tail.selected {
            if head_file.map(|receipt| receipt.head) != Some(head) {
                self.write_head(&head)?;
            }
        } else if head.is_some() {
            return Err(StoreError::Corrupt);
        }
        Ok(JournalState {
            selected: tail.selected,
        })
    }

    pub(super) fn ensure_head_file(&self, head: &SelectedHead) -> Result<(), StoreError> {
        let tail = self.refresh_journal_tail()?;
        let current = head::read_head(&self.root.join("HEAD"))?;
        if current != Some(JournalCheckpoint { head: *head, tail }) {
            self.write_head(head)?;
        }
        Ok(())
    }

    fn refresh_journal_tail(&self) -> Result<JournalTail, StoreError> {
        let path = self.root.join("journal");
        let mut guard = self.journal_tail.lock().map_err(|_| StoreError::Corrupt)?;
        refresh_journal_tail(&path, &mut guard)
    }

    pub(super) fn published_sync_error(
        &self,
        head: &SelectedHead,
        error: StoreError,
    ) -> StoreError {
        let refreshed = self.refresh_journal_tail();
        if refreshed.is_ok_and(|tail| tail.selected == Some(*head)) {
            return StoreError::PublishedWithSyncPending(Box::new(*head));
        }
        match head::read_head(&self.root.join("HEAD")) {
            Ok(Some(observed)) if observed.head == *head => {
                StoreError::PublishedWithSyncPending(Box::new(*head))
            }
            Ok(Some(_) | None) => error,
            Err(_) => StoreError::PublishedWithSyncPending(Box::new(*head)),
        }
    }
}

fn refresh_journal_tail(path: &Path, cached: &mut JournalTail) -> Result<JournalTail, StoreError> {
    let length = journal_length(path)?;
    let record_bytes = u64::try_from(JOURNAL_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
    let cached_valid = cached.offset <= length
        && cached.offset.is_multiple_of(record_bytes)
        && (cached.offset == 0 || tail_record_matches(path, cached)?);
    let refreshed = if cached_valid && cached.offset < length {
        extend_journal(path, cached, length)?
    } else if cached_valid && cached.offset == length {
        *cached
    } else {
        scan_journal(path, length)?
    };
    *cached = refreshed;
    Ok(refreshed)
}

fn journal_length(path: &Path) -> Result<u64, StoreError> {
    let length = match fs::metadata(path) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(io_error(&error)),
    };
    let record_bytes = u64::try_from(JOURNAL_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
    let complete = length / record_bytes * record_bytes;
    if complete != length {
        let file = OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|error| io_error(&error))?;
        file.set_len(complete).map_err(|error| io_error(&error))?;
        file.sync_all().map_err(|error| io_error(&error))?;
        sync_directory(path.parent().ok_or(StoreError::Corrupt)?)?;
    }
    Ok(complete)
}

fn tail_record_matches(path: &Path, tail: &JournalTail) -> Result<bool, StoreError> {
    let record_bytes = u64::try_from(JOURNAL_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
    let offset = tail
        .offset
        .checked_sub(record_bytes)
        .ok_or(StoreError::Corrupt)?;
    let record = journal::read_record_at_offset(path, offset)?;
    Ok(record.sequence == tail.sequence && record.checksum == tail.checksum)
}

fn scan_journal(path: &Path, length: u64) -> Result<JournalTail, StoreError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(JournalTail::default());
        }
        Err(error) => return Err(io_error(&error)),
    };
    validate_stream(path, &mut file, 0, length, 1, [0; 32], None)
}

fn extend_journal(
    path: &Path,
    cached: &JournalTail,
    length: u64,
) -> Result<JournalTail, StoreError> {
    let mut file = File::open(path).map_err(|error| io_error(&error))?;
    validate_stream(
        path,
        &mut file,
        cached.offset,
        length,
        cached.sequence.checked_add(1).ok_or(StoreError::Bounds)?,
        cached.checksum,
        cached.selected,
    )
}

fn validate_stream(
    path: &Path,
    file: &mut File,
    mut offset: u64,
    length: u64,
    mut expected_sequence: u64,
    mut previous: Hash,
    mut selected: Option<SelectedHead>,
) -> Result<JournalTail, StoreError> {
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| io_error(&error))?;
    let record_bytes = u64::try_from(JOURNAL_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
    while offset < length {
        let frame = journal::read_record(file)?;
        if frame.sequence != expected_sequence || frame.previous != previous {
            return Err(StoreError::Corrupt);
        }
        selected = apply_frame(path, &frame, selected.as_ref())?;
        previous = frame.checksum;
        expected_sequence = expected_sequence.checked_add(1).ok_or(StoreError::Bounds)?;
        offset = offset.checked_add(record_bytes).ok_or(StoreError::Bounds)?;
    }
    if offset != length {
        return Err(StoreError::Corrupt);
    }
    Ok(JournalTail {
        sequence: expected_sequence
            .checked_sub(1)
            .ok_or(StoreError::Corrupt)?,
        checksum: previous,
        offset,
        selected,
    })
}

fn apply_frame(
    path: &Path,
    frame: &JournalRecord,
    selected: Option<&SelectedHead>,
) -> Result<Option<SelectedHead>, StoreError> {
    match frame.tag {
        PREPARED_TAG => Ok(selected.copied()),
        PUBLISHED_TAG => {
            let prepared = journal::read_record_at(path, frame.paired_sequence)?;
            if prepared.tag != PREPARED_TAG
                || prepared.sequence != frame.paired_sequence
                || prepared.paired_sequence != 0
                || prepared.payload != frame.payload
            {
                return Err(StoreError::Corrupt);
            }
            let prior_generation = selected.map_or(0, |head| head.generation());
            let expected_base = selected.map(|head| head.descriptor.target);
            if frame.payload.base_generation != prior_generation
                || frame.payload.target_generation
                    != prior_generation.checked_add(1).ok_or(StoreError::Bounds)?
                || frame.payload.base != expected_base
            {
                return Err(StoreError::Corrupt);
            }
            Ok(Some(SelectedHead {
                journal_sequence: frame.sequence,
                descriptor: frame.payload,
            }))
        }
        _ => Err(StoreError::Corrupt),
    }
}
