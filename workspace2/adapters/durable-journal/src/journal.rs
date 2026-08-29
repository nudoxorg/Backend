use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
};

use nudox_workflow::{
    Recovery, ReplayError, WorkflowEvent, WorkflowRecord, WorkflowState, reduce, replay_stream,
};
use zerocopy::IntoBytes;

use crate::{
    CommitError, CommitIoStep, FrameSequence, HeaderError, JournalError, JournalIoStep,
    JournalOffset, StableReceipt,
    format::{
        FrameRecord, HeaderRecord, JOURNAL_FRAME_BYTES, JOURNAL_HEADER_BYTES, frame_offset,
        receipt_end,
    },
};

#[derive(Debug)]
pub struct FileJournal {
    file: File,
    state: WorkflowState,
    next: FrameSequence,
    poisoned: bool,
}

#[derive(Debug)]
pub(crate) struct PersistFailure {
    pub(crate) step: CommitIoStep,
    pub(crate) source: io::Error,
}

impl FileJournal {
    pub fn create(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        Self::create_using(path.as_ref(), persist_header)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| JournalError::io(JournalIoStep::Open, source))?;
        let file_bytes = file_length(&file)?;
        validate_header(&mut file, file_bytes)?;
        let (recovery, next, repaired_end) = scan_committed_prefix(&mut file, file_bytes)?;
        repair_tail(&mut file, file_bytes, repaired_end)?;
        Ok(Self {
            file,
            state: recovery.state,
            next,
            poisoned: false,
        })
    }

    pub fn append(&mut self, event: WorkflowEvent) -> Result<StableReceipt, CommitError> {
        self.append_using(event, persist_frame)
    }

    pub fn replay(&mut self) -> Result<Recovery, JournalError> {
        if self.poisoned {
            return Err(JournalError::Poisoned);
        }
        let file_bytes = file_length(&self.file)?;
        let (recovery, next, repaired_end) = scan_committed_prefix(&mut self.file, file_bytes)?;
        repair_tail(&mut self.file, file_bytes, repaired_end)?;
        self.state = recovery.state;
        self.next = next;
        Ok(recovery)
    }

    fn create_using(
        path: &Path,
        persist: impl FnOnce(&mut File, &HeaderRecord) -> Result<(), JournalError>,
    ) -> Result<Self, JournalError> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|source| JournalError::io(JournalIoStep::Create, source))?;
        persist(&mut file, &HeaderRecord::canonical())?;
        Ok(Self {
            file,
            state: WorkflowState::empty(),
            next: FrameSequence::FIRST,
            poisoned: false,
        })
    }

    fn append_using(
        &mut self,
        event: WorkflowEvent,
        persist: impl FnOnce(&mut File, &FrameRecord) -> Result<(), PersistFailure>,
    ) -> Result<StableReceipt, CommitError> {
        if self.poisoned {
            return Err(CommitError::Poisoned);
        }
        let reduction = reduce(self.state, event).map_err(CommitError::Reduction)?;
        let sequence = self.next;
        let durable_end = receipt_end(sequence).ok_or(CommitError::ReceiptOverflow { sequence })?;
        let next = sequence
            .successor()
            .ok_or(CommitError::ReceiptOverflow { sequence })?;
        let attempted = WorkflowRecord::from(event);
        let frame = FrameRecord::encode(sequence, attempted);
        if let Err(failure) = persist(&mut self.file, &frame) {
            self.poisoned = true;
            return Err(CommitError::OutcomeUnknown {
                attempted,
                sequence,
                step: failure.step,
                source: failure.source,
            });
        }
        self.state = reduction.state;
        self.next = next;
        Ok(StableReceipt::committed(sequence, durable_end))
    }
}

fn file_length(file: &File) -> Result<JournalOffset, JournalError> {
    file.metadata()
        .map(|metadata| JournalOffset(metadata.len()))
        .map_err(|source| JournalError::io(JournalIoStep::Inspect, source))
}

fn persist_header(file: &mut File, header: &HeaderRecord) -> Result<(), JournalError> {
    file.write_all(header.as_bytes())
        .map_err(|source| JournalError::io(JournalIoStep::WriteHeader, source))?;
    file.sync_all()
        .map_err(|source| JournalError::io(JournalIoStep::SyncHeader, source))
}

fn persist_frame(file: &mut File, frame: &FrameRecord) -> Result<(), PersistFailure> {
    file.seek(SeekFrom::End(0))
        .map_err(|source| PersistFailure {
            step: CommitIoStep::Position,
            source,
        })?;
    file.write_all(frame.as_bytes())
        .map_err(|source| PersistFailure {
            step: CommitIoStep::WriteFrame,
            source,
        })?;
    file.sync_all().map_err(|source| PersistFailure {
        step: CommitIoStep::SyncFrame,
        source,
    })
}

fn validate_header(file: &mut File, file_bytes: JournalOffset) -> Result<(), JournalError> {
    if file_bytes.0 < JOURNAL_HEADER_BYTES as u64 {
        return Err(HeaderError::Truncated {
            required: JournalOffset(JOURNAL_HEADER_BYTES as u64),
            actual: file_bytes,
        }
        .into());
    }
    let mut bytes = [0; JOURNAL_HEADER_BYTES];
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.read_exact(&mut bytes))
        .map_err(|source| JournalError::io(JournalIoStep::ReadHeader, source))?;
    HeaderRecord::decode(bytes).map(|_| ()).map_err(Into::into)
}

fn scan_committed_prefix(
    file: &mut File,
    file_bytes: JournalOffset,
) -> Result<(Recovery, FrameSequence, JournalOffset), JournalError> {
    let payload_bytes = file_bytes.0 - JOURNAL_HEADER_BYTES as u64;
    let complete_frames = payload_bytes / JOURNAL_FRAME_BYTES as u64;
    let repaired_end =
        JournalOffset(JOURNAL_HEADER_BYTES as u64 + complete_frames * JOURNAL_FRAME_BYTES as u64);
    file.seek(SeekFrom::Start(JOURNAL_HEADER_BYTES as u64))
        .map_err(|source| JournalError::io(JournalIoStep::ReadFrame, source))?;
    let mut records = Records {
        file,
        remaining: complete_frames,
        expected: FrameSequence::FIRST,
    };
    let recovery = replay_stream(&mut records).map_err(map_replay_error)?;
    Ok((recovery, records.expected, repaired_end))
}

fn repair_tail(
    file: &mut File,
    observed_end: JournalOffset,
    repaired_end: JournalOffset,
) -> Result<(), JournalError> {
    if observed_end == repaired_end {
        return Ok(());
    }
    file.set_len(repaired_end.0)
        .map_err(|source| JournalError::io(JournalIoStep::RepairTail, source))?;
    file.sync_all()
        .map_err(|source| JournalError::io(JournalIoStep::SyncTailRepair, source))
}

struct Records<'file> {
    file: &'file mut File,
    remaining: u64,
    expected: FrameSequence,
}

impl Iterator for Records<'_> {
    type Item = Result<WorkflowRecord, JournalError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        let result = read_record(self.file, self.expected);
        if result.is_ok() {
            let Some(next) = self.expected.successor() else {
                return Some(Err(JournalError::OffsetOverflow {
                    sequence: self.expected,
                }));
            };
            self.expected = next;
        }
        Some(result)
    }
}

fn read_record(file: &mut File, expected: FrameSequence) -> Result<WorkflowRecord, JournalError> {
    let mut bytes = [0; JOURNAL_FRAME_BYTES];
    file.read_exact(&mut bytes)
        .map_err(|source| JournalError::io(JournalIoStep::ReadFrame, source))?;
    let frame = FrameRecord::decode(bytes);
    let observed = frame.sequence();
    if observed != expected {
        return Err(JournalError::Sequence { expected, observed });
    }
    if !frame.checksum_is_valid() {
        return Err(JournalError::FrameChecksum {
            sequence: observed,
            offset: frame_offset(observed)
                .ok_or(JournalError::OffsetOverflow { sequence: observed })?,
        });
    }
    Ok(frame.record())
}

fn map_replay_error(error: ReplayError<JournalError>) -> JournalError {
    match error {
        ReplayError::Source(source) => source,
        ReplayError::Decode(source) => JournalError::Decode(source),
        ReplayError::Reduction(source) => JournalError::Reduction(source),
    }
}

#[cfg(test)]
mod tests;
