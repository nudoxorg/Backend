//! Journal frame encoding, append linearization, and ambiguous-write recovery.

use super::super::{ClosureId, Hash, LayoutId, PackId, TransactionId, WorkspaceBinding};
#[cfg(test)]
use super::take_test_fault;
use super::{
    FileStore, JOURNAL_MAGIC, JOURNAL_RECORD_BYTES, JOURNAL_VERSION, JournalRecord, JournalTail,
    PREPARED_TAG, PUBLISHED_TAG, Path, PublicationDescriptor, SelectedHead, StoreError, io_error,
    journal_length, read_byte, read_hash, read_u64, refresh_journal_tail, sync_directory,
};
use crate::digest;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};

impl FileStore {
    pub(in crate::durable) fn append_record(
        &self,
        tag: u8,
        descriptor: &PublicationDescriptor,
        paired_sequence: u64,
    ) -> Result<u64, StoreError> {
        if !matches!(tag, PREPARED_TAG | PUBLISHED_TAG) {
            return Err(StoreError::Corrupt);
        }
        if (tag == PREPARED_TAG && paired_sequence != 0)
            || (tag == PUBLISHED_TAG && paired_sequence == 0)
        {
            return Err(StoreError::Corrupt);
        }

        let path = self.root.join("journal");
        let mut tail_guard = self.journal_tail.lock().map_err(|_| StoreError::Corrupt)?;
        let tail = refresh_journal_tail(&path, &mut tail_guard)?;
        if let Some(sequence) =
            existing_tail_record(&path, &tail, tag, paired_sequence, descriptor)?
        {
            return Ok(sequence);
        }
        validate_append_pair(&path, tag, paired_sequence, descriptor)?;
        let sequence = tail.sequence.checked_add(1).ok_or(StoreError::Bounds)?;
        let record = JournalRecord {
            tag,
            sequence,
            paired_sequence,
            previous: tail.checksum,
            payload: *descriptor,
            checksum: [0; 32],
        };
        let encoded = encode_record(record);
        append_frame(&self.root, &path, &mut tail_guard, &tail, &record, &encoded)
    }
}

fn append_optional_hash(output: &mut Vec<u8>, value: Option<Hash>) {
    output.push(u8::from(value.is_some()));
    output.extend_from_slice(&value.unwrap_or([0; 32]));
}

fn append_workspace(output: &mut Vec<u8>, workspace: Option<WorkspaceBinding>) {
    output.push(u8::from(workspace.is_some()));
    if let Some(workspace) = workspace {
        output.extend_from_slice(workspace.root());
        output.extend_from_slice(workspace.closure().as_bytes());
        output.extend_from_slice(workspace.proof());
    } else {
        output.extend_from_slice(&[0; 96]);
    }
}

pub(super) fn encode_record(mut record: JournalRecord) -> Vec<u8> {
    let mut body = Vec::with_capacity(JOURNAL_RECORD_BYTES - 32);
    body.extend_from_slice(JOURNAL_MAGIC);
    body.push(JOURNAL_VERSION);
    body.push(record.tag);
    body.extend_from_slice(&record.sequence.to_le_bytes());
    body.extend_from_slice(&record.paired_sequence.to_le_bytes());
    body.extend_from_slice(&record.previous);
    body.extend_from_slice(record.payload.transaction.as_bytes());
    append_optional_hash(&mut body, record.payload.base);
    body.extend_from_slice(&record.payload.target);
    body.extend_from_slice(record.payload.layout.as_bytes());
    body.extend_from_slice(record.payload.pack.as_bytes());
    body.extend_from_slice(record.payload.closure.as_bytes());
    append_workspace(&mut body, record.payload.workspace);
    body.extend_from_slice(&record.payload.base_generation.to_le_bytes());
    body.extend_from_slice(&record.payload.target_generation.to_le_bytes());
    record.checksum = digest(b"store.journal.checksum.v2\0", &body);
    body.extend_from_slice(&record.checksum);
    body
}

pub(super) fn decode_record(bytes: &[u8]) -> Result<JournalRecord, StoreError> {
    if bytes.len() != JOURNAL_RECORD_BYTES || !bytes.starts_with(JOURNAL_MAGIC) {
        return Err(StoreError::Corrupt);
    }
    let mut at = JOURNAL_MAGIC.len();
    let version = read_byte(bytes, &mut at)?;
    if version != JOURNAL_VERSION {
        return Err(StoreError::Corrupt);
    }
    let tag = read_byte(bytes, &mut at)?;
    if !matches!(tag, PREPARED_TAG | PUBLISHED_TAG) {
        return Err(StoreError::Corrupt);
    }
    let sequence = read_u64(bytes, &mut at)?;
    let paired_sequence = read_u64(bytes, &mut at)?;
    let previous = read_hash(bytes, &mut at)?;
    let transaction = TransactionId(read_hash(bytes, &mut at)?);
    let base_hash = read_optional_hash(bytes, &mut at)?;
    let target = read_hash(bytes, &mut at)?;
    let layout = LayoutId::from_bytes(read_hash(bytes, &mut at)?);
    let pack = PackId::from_wire(read_hash(bytes, &mut at)?);
    let closure = ClosureId::from_bytes(read_hash(bytes, &mut at)?);
    let workspace = read_workspace(bytes, &mut at)?;
    let base_generation = read_u64(bytes, &mut at)?;
    let target_generation = read_u64(bytes, &mut at)?;
    let checksum = read_hash(bytes, &mut at)?;
    if at != bytes.len()
        || digest(b"store.journal.checksum.v2\0", &bytes[..bytes.len() - 32]) != checksum
    {
        return Err(StoreError::Corrupt);
    }
    let payload = PublicationDescriptor {
        transaction,
        base: base_hash,
        target,
        layout,
        pack,
        closure,
        workspace,
        base_generation,
        target_generation,
    };
    if transaction.0 != payload.derived_transaction() {
        return Err(StoreError::Corrupt);
    }
    if tag == PREPARED_TAG && paired_sequence != 0 {
        return Err(StoreError::Corrupt);
    }
    Ok(JournalRecord {
        tag,
        sequence,
        paired_sequence,
        previous,
        payload,
        checksum,
    })
}

fn existing_tail_record(
    path: &Path,
    tail: &JournalTail,
    tag: u8,
    paired_sequence: u64,
    descriptor: &PublicationDescriptor,
) -> Result<Option<u64>, StoreError> {
    let record_bytes = u64::try_from(JOURNAL_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
    if tail.offset < record_bytes {
        return Ok(None);
    }
    let last = read_record_at_offset(path, tail.offset - record_bytes);
    Ok(last.ok().and_then(|record| {
        (record.tag == tag
            && record.paired_sequence == paired_sequence
            && record.payload == *descriptor)
            .then_some(record.sequence)
    }))
}

fn validate_append_pair(
    path: &Path,
    tag: u8,
    paired_sequence: u64,
    descriptor: &PublicationDescriptor,
) -> Result<(), StoreError> {
    if tag != PUBLISHED_TAG {
        return Ok(());
    }
    let prepared = read_record_at(path, paired_sequence)?;
    if prepared.tag != PREPARED_TAG
        || prepared.sequence != paired_sequence
        || prepared.paired_sequence != 0
        || prepared.payload != *descriptor
    {
        return Err(StoreError::Corrupt);
    }
    Ok(())
}

fn append_frame(
    root: &Path,
    path: &Path,
    tail_guard: &mut JournalTail,
    prior: &JournalTail,
    record: &JournalRecord,
    encoded: &[u8],
) -> Result<u64, StoreError> {
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| io_error(&error))?;
    let observed_offset = file.metadata().map_err(|error| io_error(&error))?.len();
    if observed_offset != prior.offset {
        return Err(StoreError::Corrupt);
    }
    if let Err(error) = file.write_all(encoded) {
        return recover_completed_append(path, tail_guard, prior, record, io_error(&error));
    }
    #[cfg(test)]
    if take_test_fault(1) {
        return recover_completed_append(
            path,
            tail_guard,
            prior,
            record,
            StoreError::Io("injected post-journal-write failure".to_owned()),
        );
    }
    if let Err(error) = file.sync_all() {
        return recover_completed_append(path, tail_guard, prior, record, io_error(&error));
    }
    #[cfg(test)]
    if take_test_fault(2) {
        return recover_completed_append(
            path,
            tail_guard,
            prior,
            record,
            StoreError::Io("injected post-journal-sync failure".to_owned()),
        );
    }
    if let Err(error) = sync_directory(root) {
        return recover_completed_append(path, tail_guard, prior, record, error);
    }
    #[cfg(test)]
    if take_test_fault(3) {
        return recover_completed_append(
            path,
            tail_guard,
            prior,
            record,
            StoreError::Io("injected post-directory-sync failure".to_owned()),
        );
    }
    let checksum = record_checksum(encoded)?;
    let selected = if record.tag == PUBLISHED_TAG {
        Some(SelectedHead {
            journal_sequence: record.sequence,
            descriptor: record.payload,
        })
    } else {
        prior.selected
    };
    let record_bytes = u64::try_from(JOURNAL_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
    *tail_guard = JournalTail {
        sequence: record.sequence,
        checksum,
        offset: prior
            .offset
            .checked_add(record_bytes)
            .ok_or(StoreError::Bounds)?,
        selected,
    };
    Ok(record.sequence)
}

fn record_checksum(encoded: &[u8]) -> Result<Hash, StoreError> {
    encoded
        .get(encoded.len().checked_sub(32).ok_or(StoreError::Corrupt)?..)
        .ok_or(StoreError::Corrupt)?
        .try_into()
        .map_err(|_| StoreError::Corrupt)
}

fn recover_completed_append(
    path: &Path,
    tail_guard: &mut JournalTail,
    prior: &JournalTail,
    expected: &JournalRecord,
    error: StoreError,
) -> Result<u64, StoreError> {
    let Ok(length) = journal_length(path) else {
        return Err(error);
    };
    let Ok(record_bytes) = u64::try_from(JOURNAL_RECORD_BYTES) else {
        return Err(error);
    };
    let Some(expected_end) = expected.sequence.checked_mul(record_bytes) else {
        return Err(error);
    };
    if length < expected_end {
        return Err(error);
    }
    let Ok(frame) = read_record_at(path, expected.sequence) else {
        return Err(error);
    };
    if frame.tag != expected.tag
        || frame.sequence != expected.sequence
        || frame.paired_sequence != expected.paired_sequence
        || frame.previous != prior.checksum
        || frame.payload != expected.payload
    {
        return Err(error);
    }
    let selected = if expected.tag == PUBLISHED_TAG {
        Some(SelectedHead {
            journal_sequence: expected.sequence,
            descriptor: expected.payload,
        })
    } else {
        prior.selected
    };
    *tail_guard = JournalTail {
        sequence: expected.sequence,
        checksum: frame.checksum,
        offset: expected_end,
        selected,
    };
    let pending = if expected.tag == PUBLISHED_TAG {
        StoreError::PublishedWithSyncPending(Box::new(SelectedHead {
            journal_sequence: expected.sequence,
            descriptor: expected.payload,
        }))
    } else {
        StoreError::PreparedWithSyncPending {
            sequence: expected.sequence,
            transaction: expected.payload.transaction,
        }
    };
    Err(pending)
}

pub(super) fn read_record(file: &mut File) -> Result<JournalRecord, StoreError> {
    let mut bytes = vec![0; JOURNAL_RECORD_BYTES];
    file.read_exact(&mut bytes)
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::UnexpectedEof => StoreError::Corrupt,
            _ => io_error(&error),
        })?;
    decode_record(&bytes)
}

pub(super) fn read_record_at(path: &Path, sequence: u64) -> Result<JournalRecord, StoreError> {
    if sequence == 0 {
        return Err(StoreError::Corrupt);
    }
    let record_bytes = u64::try_from(JOURNAL_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
    let offset = sequence
        .checked_sub(1)
        .and_then(|value| value.checked_mul(record_bytes))
        .ok_or(StoreError::Bounds)?;
    read_record_at_offset(path, offset)
}

pub(super) fn read_record_at_offset(path: &Path, offset: u64) -> Result<JournalRecord, StoreError> {
    let mut file = File::open(path).map_err(|error| io_error(&error))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| io_error(&error))?;
    read_record(&mut file)
}

fn read_workspace(bytes: &[u8], at: &mut usize) -> Result<Option<WorkspaceBinding>, StoreError> {
    let present = read_byte(bytes, at)?;
    let root = read_hash(bytes, at)?;
    let closure = ClosureId::from_bytes(read_hash(bytes, at)?);
    let proof = read_hash(bytes, at)?;
    match present {
        0 => {
            if root != [0; 32] || closure.as_bytes() != &[0; 32] || proof != [0; 32] {
                return Err(StoreError::Corrupt);
            }
            Ok(None)
        }
        1 => {
            let binding = WorkspaceBinding::from_parts(root, closure);
            if binding.proof() != &proof {
                return Err(StoreError::Corrupt);
            }
            Ok(Some(binding))
        }
        _ => Err(StoreError::Corrupt),
    }
}

fn read_optional_hash(bytes: &[u8], at: &mut usize) -> Result<Option<Hash>, StoreError> {
    let present = read_byte(bytes, at)?;
    let hash = read_hash(bytes, at)?;
    match present {
        0 if hash == [0; 32] => Ok(None),
        1 => Ok(Some(hash)),
        _ => Err(StoreError::Corrupt),
    }
}
