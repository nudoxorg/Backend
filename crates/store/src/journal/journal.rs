//! Defines journal behavior for `backend-store`, whose purpose is to persist and recover generation publication with bounded ownership.
//! This module owns the journal invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    num::TryFromIntError,
    path::Path,
    sync::Arc,
};

use crate::workflow::{
    Recovery, ReductionError, ReplayError, WorkflowEvent, WorkflowRecord, WorkflowState, reduce,
    reduce_chained, replay_stream,
};
use thiserror::Error;
use zerocopy::IntoBytes;

use crate::journal::{
    CommitError, CommitIoStep, FrameSequence, HeaderError, JournalError, JournalIoStep,
    JournalOffset, ReceiptFacts, StableReceipt,
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

/// The exact failure fan-out for one physical group append.
#[derive(Debug, Error)]
pub(crate) enum GroupCommitError {
    #[error("workflow reduction rejected grouped event")]
    Reduction {
        attempted: Arc<WorkflowRecord>,
        #[source]
        source: ReductionError,
    },
    #[error("journal is poisoned; reopen it before reconciling")]
    Poisoned,
    #[error("journal receipt cannot represent grouped sequence {sequence:?}")]
    ReceiptOverflow { sequence: FrameSequence },
    #[error("journal grouped receipt conversion failed for {sequence:?}")]
    ReceiptConversion {
        sequence: FrameSequence,
        #[source]
        source: TryFromIntError,
    },
    #[error("group storage requires {required} bytes but has {available}")]
    StorageTooSmall { required: usize, available: usize },
    #[error("journal group append outcome is unknown for {first_sequence:?} during {step:?}")]
    OutcomeUnknown {
        attempted: Arc<WorkflowRecord>,
        first_sequence: FrameSequence,
        count: usize,
        step: CommitIoStep,
        #[source]
        source: io::Error,
    },
}

/// Stable coordinates for every record in one successful physical group.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GroupReceipt {
    first_sequence: FrameSequence,
    count: usize,
}

impl GroupReceipt {
    pub(crate) fn receipt_at(self, index: usize) -> Option<StableReceipt> {
        if index >= self.count {
            return None;
        }
        let index = u64::try_from(index).ok()?;
        let sequence = FrameSequence::from(self.first_sequence.value.checked_add(index)?);
        let next = sequence.successor()?;
        let durable_end = frame_offset(next)?;
        Some(StableReceipt::committed(sequence, durable_end))
    }
}

impl FileJournal {
    pub fn create(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        Self::create_using(path.as_ref(), persist_header, sync_parent_directory)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| JournalError::io(JournalIoStep::Open, source))?;
        file.try_lock().map_err(JournalError::ExclusiveOwnership)?;
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

    /// Appends a prevalidated group through one physical write and one file sync.
    pub(crate) fn append_group(
        &mut self,
        events: &[WorkflowEvent],
        frames: &mut [u8],
    ) -> Result<GroupReceipt, GroupCommitError> {
        self.append_group_using(events, frames, persist_group, false)
    }

    pub(crate) fn append_publication_group(
        &mut self,
        events: &[WorkflowEvent],
        frames: &mut [u8],
    ) -> Result<GroupReceipt, GroupCommitError> {
        self.append_group_using(events, frames, persist_group, true)
    }

    fn append_group_using(
        &mut self,
        events: &[WorkflowEvent],
        frames: &mut [u8],
        persist: impl FnOnce(&mut File, &[u8]) -> Result<(), PersistFailure>,
        allow_chain: bool,
    ) -> Result<GroupReceipt, GroupCommitError> {
        if self.poisoned {
            return Err(GroupCommitError::Poisoned);
        }
        if events.is_empty() {
            return Err(GroupCommitError::StorageTooSmall {
                required: JOURNAL_FRAME_BYTES,
                available: frames.len(),
            });
        }
        let required = events.len().checked_mul(JOURNAL_FRAME_BYTES).ok_or(
            GroupCommitError::StorageTooSmall {
                required: usize::MAX,
                available: frames.len(),
            },
        )?;
        if frames.len() < required {
            return Err(GroupCommitError::StorageTooSmall {
                required,
                available: frames.len(),
            });
        }

        let first_sequence = self.next;
        let mut reduced_state = self.state;
        for (index, event) in events.iter().copied().enumerate() {
            let attempted = WorkflowRecord::from(event);
            reduced_state = if allow_chain {
                reduce_chained(reduced_state, event)
            } else {
                reduce(reduced_state, event)
            }
            .map_err(|source| GroupCommitError::Reduction {
                attempted: Arc::new(attempted),
                source,
            })?
            .state;
            let sequence = match first_sequence
                .value
                .checked_add(u64::try_from(index).map_err(|source| {
                    GroupCommitError::ReceiptConversion {
                        sequence: first_sequence,
                        source,
                    }
                })?)
                .map(FrameSequence::from)
            {
                Some(sequence) => sequence,
                None => {
                    return Err(GroupCommitError::ReceiptOverflow {
                        sequence: first_sequence,
                    });
                }
            };
            let frame = FrameRecord::encode(sequence, attempted);
            let start = index * JOURNAL_FRAME_BYTES;
            frames[start..start + JOURNAL_FRAME_BYTES].copy_from_slice(frame.as_bytes());
        }
        let count = events.len();
        let last_sequence = match first_sequence
            .value
            .checked_add(u64::try_from(count - 1).map_err(|source| {
                GroupCommitError::ReceiptConversion {
                    sequence: first_sequence,
                    source,
                }
            })?)
            .map(FrameSequence::from)
        {
            Some(sequence) => sequence,
            None => {
                return Err(GroupCommitError::ReceiptOverflow {
                    sequence: first_sequence,
                });
            }
        };
        let next = last_sequence
            .successor()
            .ok_or(GroupCommitError::ReceiptOverflow {
                sequence: last_sequence,
            })?;
        let _durable_end = frame_offset(next).ok_or(GroupCommitError::ReceiptOverflow {
            sequence: last_sequence,
        })?;
        if let Err(failure) = persist(&mut self.file, &frames[..required]) {
            self.poisoned = true;
            return Err(GroupCommitError::OutcomeUnknown {
                attempted: Arc::new(WorkflowRecord::from(events[0])),
                first_sequence,
                count,
                step: failure.step,
                source: failure.source,
            });
        }
        self.state = reduced_state;
        self.next = next;
        Ok(GroupReceipt {
            first_sequence,
            count,
        })
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

    pub(crate) fn current_key(&self) -> Option<crate::workflow::StageKey> {
        match self.state {
            WorkflowState::New => None,
            WorkflowState::Keyed { key, .. } => Some(key),
        }
    }

    pub(crate) fn last_receipt(&self) -> Option<StableReceipt> {
        if self.next == FrameSequence::FIRST {
            return None;
        }
        let sequence = FrameSequence::from(self.next.value - 1);
        let durable_end = frame_offset(self.next)?;
        Some(StableReceipt::committed(sequence, durable_end))
    }

    pub(crate) fn receipt_is_current(&self, receipt: ReceiptFacts) -> bool {
        receipt.sequence.value.checked_add(1) == Some(self.next.value)
            && frame_offset(self.next) == Some(receipt.durable_end)
    }

    fn create_using(
        path: &Path,
        persist: impl FnOnce(&mut File, &HeaderRecord) -> Result<(), JournalError>,
        persist_parent: impl FnOnce(&Path) -> Result<(), JournalError>,
    ) -> Result<Self, JournalError> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|source| JournalError::io(JournalIoStep::Create, source))?;
        file.try_lock().map_err(JournalError::ExclusiveOwnership)?;
        persist(&mut file, &HeaderRecord::canonical())?;
        persist_parent(parent(path))?;
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

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn sync_parent_directory(path: &Path) -> Result<(), JournalError> {
    parent_directory(path)?
        .sync_all()
        .map_err(|source| JournalError::io(JournalIoStep::SyncParentDirectory, source))
}

fn parent_directory(path: &Path) -> Result<File, JournalError> {
    backend_platform::durability::open_directory(path)
        .map_err(|source| JournalError::io(JournalIoStep::OpenParentDirectory, source))
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

fn persist_group(file: &mut File, frames: &[u8]) -> Result<(), PersistFailure> {
    file.seek(SeekFrom::End(0))
        .map_err(|source| PersistFailure {
            step: CommitIoStep::Position,
            source,
        })?;
    file.write_all(frames).map_err(|source| PersistFailure {
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
    Ok(frame.record)
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
