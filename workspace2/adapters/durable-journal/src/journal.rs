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
    format::{FrameRecord, HeaderRecord, JOURNAL_FRAME_BYTES, JOURNAL_HEADER_BYTES, frame_offset},
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
        let bytes = file
            .metadata()
            .map(|metadata| JournalOffset::from(metadata.len()))
            .map_err(|source| JournalError::io(JournalIoStep::Inspect, source))?;
        validate_header(&mut file, bytes)?;
        let (recovery, next) = recover(&mut file, bytes)?;
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
        let bytes = self
            .file
            .metadata()
            .map(|metadata| JournalOffset::from(metadata.len()))
            .map_err(|source| JournalError::io(JournalIoStep::Inspect, source))?;
        let (recovery, next) = recover(&mut self.file, bytes)?;
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
        let next = sequence
            .successor()
            .ok_or(CommitError::ReceiptOverflow { sequence })?;
        let durable_end = frame_offset(next).ok_or(CommitError::ReceiptOverflow { sequence })?;
        let attempted = WorkflowRecord::from(event);
        if let Err(failure) = persist(&mut self.file, &FrameRecord::encode(sequence, attempted)) {
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

fn validate_header(file: &mut File, bytes: JournalOffset) -> Result<(), JournalError> {
    if *bytes < JOURNAL_HEADER_BYTES as u64 {
        return Err(HeaderError::Truncated {
            required: JournalOffset::from(JOURNAL_HEADER_BYTES as u64),
            actual: bytes,
        }
        .into());
    }
    let mut header = [0; JOURNAL_HEADER_BYTES];
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.read_exact(&mut header))
        .map_err(|source| JournalError::io(JournalIoStep::ReadHeader, source))?;
    HeaderRecord::decode(header).map(|_| ()).map_err(Into::into)
}

fn recover(
    file: &mut File,
    bytes: JournalOffset,
) -> Result<(Recovery, FrameSequence), JournalError> {
    let payload = *bytes - JOURNAL_HEADER_BYTES as u64;
    let remaining = payload / JOURNAL_FRAME_BYTES as u64;
    let repaired_end = JournalOffset::from(*bytes - payload % JOURNAL_FRAME_BYTES as u64);
    file.seek(SeekFrom::Start(JOURNAL_HEADER_BYTES as u64))
        .map_err(|source| JournalError::io(JournalIoStep::ReadFrame, source))?;
    let (recovery, next) = {
        let mut records = Records {
            file,
            remaining,
            expected: FrameSequence::FIRST,
        };
        let recovery = replay_stream(&mut records).map_err(map_replay_error)?;
        (recovery, records.expected)
    };
    if bytes != repaired_end {
        file.set_len(*repaired_end)
            .map_err(|source| JournalError::io(JournalIoStep::RepairTail, source))?;
        file.sync_all()
            .map_err(|source| JournalError::io(JournalIoStep::SyncTailRepair, source))?;
    }
    Ok((recovery, next))
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
        ReplayError::Source(error) => error,
        ReplayError::Decode(error) => JournalError::Decode(error),
        ReplayError::Reduction(error) => JournalError::Reduction(error),
    }
}

#[cfg(test)]
mod tests;
