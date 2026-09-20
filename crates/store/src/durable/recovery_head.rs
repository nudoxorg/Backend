//! Selected-head checkpoint encoding and repair.

use super::super::objects::write_atomic;
use super::journal;
#[cfg(test)]
use super::take_test_fault;
use super::{
    FileStore, HEAD_BYTES, HEAD_MAGIC, JOURNAL_RECORD_BYTES, JournalCheckpoint, JournalRecord,
    JournalTail, PUBLISHED_TAG, Path, SelectedHead, StoreError, io_error, read_hash, read_u64,
};
use crate::digest;
use std::fs;

impl FileStore {
    pub(in crate::durable) fn write_head(&self, head: &SelectedHead) -> Result<(), StoreError> {
        let mut bytes = Vec::with_capacity(HEAD_BYTES);
        bytes.extend_from_slice(HEAD_MAGIC);
        bytes.extend_from_slice(&journal::encode_record(JournalRecord {
            tag: PUBLISHED_TAG,
            sequence: head.journal_sequence,
            paired_sequence: 0,
            previous: [0; 32],
            payload: head.descriptor,
            checksum: [0; 32],
        }));
        let tail = self.journal_tail.lock().map_err(|_| StoreError::Corrupt)?;
        bytes.extend_from_slice(&tail.sequence.to_le_bytes());
        bytes.extend_from_slice(&tail.checksum);
        bytes.extend_from_slice(&tail.offset.to_le_bytes());
        let receipt_checksum = digest(b"store.head.receipt.v2\0", &bytes);
        bytes.extend_from_slice(&receipt_checksum);
        let result = write_atomic(&self.root.join("HEAD"), &bytes, &self.root);
        #[cfg(test)]
        if result.is_ok() && take_test_fault(4) {
            return Err(StoreError::Io("injected post-HEAD-sync failure".to_owned()));
        }
        result
    }
}

pub(super) fn validate_head_record(
    path: &Path,
    tail: &JournalTail,
    head: &SelectedHead,
) -> Result<(), StoreError> {
    if head.journal_sequence == 0 || head.journal_sequence > tail.sequence {
        return Err(StoreError::Corrupt);
    }
    let frame = journal::read_record_at(path, head.journal_sequence)?;
    if frame.tag != PUBLISHED_TAG || frame.payload != head.descriptor {
        return Err(StoreError::Corrupt);
    }
    Ok(())
}

pub(super) fn read_head(path: &Path) -> Result<Option<JournalCheckpoint>, StoreError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(&error)),
    };
    if bytes.len() < HEAD_BYTES {
        // A head rename can be interrupted before its final bytes become
        // visible. The journal remains authoritative and will rewrite it.
        return Ok(None);
    }
    if bytes.len() > HEAD_BYTES || !bytes.starts_with(HEAD_MAGIC) {
        return Ok(None);
    }
    match decode_head(&bytes) {
        Ok(checkpoint) => Ok(Some(checkpoint)),
        Err(StoreError::Io(message)) => Err(StoreError::Io(message)),
        Err(_) => Ok(None),
    }
}

fn decode_head(bytes: &[u8]) -> Result<JournalCheckpoint, StoreError> {
    let record_end = HEAD_MAGIC
        .len()
        .checked_add(JOURNAL_RECORD_BYTES)
        .ok_or(StoreError::Bounds)?;
    let record = journal::decode_record(
        bytes
            .get(HEAD_MAGIC.len()..record_end)
            .ok_or(StoreError::Corrupt)?,
    )?;
    if record.tag != PUBLISHED_TAG || record.paired_sequence != 0 || record.previous != [0; 32] {
        return Err(StoreError::Corrupt);
    }
    let mut at = record_end;
    let sequence = read_u64(bytes, &mut at)?;
    let checksum = read_hash(bytes, &mut at)?;
    let offset = read_u64(bytes, &mut at)?;
    let receipt_checksum = read_hash(bytes, &mut at)?;
    if at != bytes.len()
        || digest(b"store.head.receipt.v2\0", &bytes[..bytes.len() - 32]) != receipt_checksum
    {
        return Err(StoreError::Corrupt);
    }
    let record_bytes = u64::try_from(JOURNAL_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
    if offset
        != sequence
            .checked_mul(record_bytes)
            .ok_or(StoreError::Bounds)?
    {
        return Err(StoreError::Corrupt);
    }
    let head = SelectedHead {
        journal_sequence: record.sequence,
        descriptor: record.payload,
    };
    Ok(JournalCheckpoint {
        head,
        tail: JournalTail {
            sequence,
            checksum,
            offset,
            selected: Some(head),
        },
    })
}
