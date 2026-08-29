use blake3::Hasher;
use nudox_workflow::{
    Recovery, ReductionError, ReplayError, WorkflowEvent, WorkflowRecord, WorkflowRecordError,
    WorkflowState, reduce, replay_stream,
};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
};
use thiserror::Error;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};
const H: usize = 32;
const R: usize = 68;
const F: usize = 92;
const P: u16 = 1;
const MAGIC: [u8; 8] = *b"NUDXJNL\0";
#[repr(C)]
#[derive(Clone, Copy, FromBytes, IntoBytes, KnownLayout, Immutable)]
struct Header {
    magic: [u8; 8],
    physical: u16,
    header_bytes: u16,
    record_bytes: u16,
    reserved: u16,
    checksum: [u8; 16],
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StableReceipt {
    pub sequence: u64,
    pub durable_end: u64,
}
#[derive(Debug, Error)]
pub enum CommitError {
    #[error("workflow reduction rejected the event")]
    Reduction(#[source] ReductionError),
    #[error("journal is poisoned")]
    Poisoned,
    #[error("journal write outcome is unknown for sequence {sequence}")]
    CommitOutcomeUnknown {
        attempted: WorkflowRecord,
        sequence: u64,
        #[source]
        source: io::Error,
    },
}
#[derive(Debug, Error)]
pub enum JournalError {
    #[error("journal I/O failed")]
    Io(#[source] io::Error),
    #[error("invalid journal header")]
    HeaderInvalid,
    #[error("journal frame checksum invalid at offset {offset} for sequence {sequence}")]
    FrameChecksum { sequence: u64, offset: u64 },
    #[error("journal sequence expected {expected}, observed {observed}")]
    Sequence { expected: u64, observed: u64 },
    #[error("workflow record decode failed")]
    Decode(#[source] WorkflowRecordError),
    #[error("workflow replay reduction failed")]
    Reduction(#[source] ReductionError),
}
pub struct FileJournal {
    file: File,
    state: WorkflowState,
    next: u64,
    poisoned: bool,
}
fn hash(p: &[&[u8]]) -> [u8; 16] {
    let mut h = Hasher::new();
    for x in p {
        h.update(x);
    }
    let mut o = [0; 16];
    o.copy_from_slice(&h.finalize().as_bytes()[..16]);
    o
}
fn header() -> Header {
    let mut x = Header {
        magic: MAGIC,
        physical: P,
        header_bytes: H as u16,
        record_bytes: R as u16,
        reserved: 0,
        checksum: [0; 16],
    };
    x.checksum = hash(&[b"nudox-journal-header-v1\0", &x.as_bytes()[..16]]);
    x
}
fn valid(x: &Header) -> bool {
    x.magic == MAGIC
        && x.physical == P
        && x.header_bytes as usize == H
        && x.record_bytes as usize == R
        && x.reserved == 0
        && x.checksum == hash(&[b"nudox-journal-header-v1\0", &x.as_bytes()[..16]])
}
fn frame(seq: u64, r: WorkflowRecord) -> [u8; F] {
    let mut x = [0; F];
    x[..8].copy_from_slice(&seq.to_le_bytes());
    x[8..76].copy_from_slice(r.as_bytes());
    let d = hash(&[
        b"nudox-journal-frame-v1\0",
        &P.to_le_bytes(),
        &seq.to_le_bytes(),
        &x[8..76],
    ]);
    x[76..].copy_from_slice(&d);
    x
}
struct Records<'a> {
    file: &'a mut File,
    expected: u64,
}
impl Iterator for Records<'_> {
    type Item = Result<WorkflowRecord, JournalError>;
    fn next(&mut self) -> Option<Self::Item> {
        let mut x = [0; F];
        match self.file.read_exact(&mut x) {
            Ok(()) => {
                let seq = u64::from_le_bytes(x[..8].try_into().unwrap_or([0; 8]));
                if seq != self.expected {
                    return Some(Err(JournalError::Sequence {
                        expected: self.expected,
                        observed: seq,
                    }));
                }
                if x[76..]
                    != hash(&[
                        b"nudox-journal-frame-v1\0",
                        &P.to_le_bytes(),
                        &seq.to_le_bytes(),
                        &x[8..76],
                    ])
                {
                    return Some(Err(JournalError::FrameChecksum {
                        sequence: seq,
                        offset: H as u64 + seq * F as u64,
                    }));
                }
                self.expected += 1;
                Some(
                    WorkflowRecord::try_from(&x[8..76])
                        .map_err(|_| JournalError::HeaderInvalid)
                        .and_then(|r| {
                            WorkflowEvent::try_from(&r)
                                .map(|_| r)
                                .map_err(JournalError::Decode)
                        }),
                )
            }
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => None,
            Err(e) => Some(Err(JournalError::Io(e))),
        }
    }
}
impl FileJournal {
    pub fn create(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(JournalError::Io)?;
        f.write_all(header().as_bytes()).map_err(JournalError::Io)?;
        f.sync_all().map_err(JournalError::Io)?;
        Ok(Self {
            file: f,
            state: WorkflowState::empty(),
            next: 0,
            poisoned: false,
        })
    }
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(JournalError::Io)?;
        let len = f.metadata().map_err(JournalError::Io)?.len();
        if len < H as u64 {
            return Err(JournalError::HeaderInvalid);
        }
        let mut b = [0; H];
        f.read_exact(&mut b).map_err(JournalError::Io)?;
        let h = Header::read_from_bytes(&b).map_err(|_| JournalError::HeaderInvalid)?;
        if !valid(&h) {
            return Err(JournalError::HeaderInvalid);
        }
        let tail = (len - H as u64) % F as u64;
        if tail != 0 {
            f.set_len(len - tail).map_err(JournalError::Io)?;
            f.sync_all().map_err(JournalError::Io)?
        }
        let mut j = Self {
            file: f,
            state: WorkflowState::empty(),
            next: 0,
            poisoned: false,
        };
        j.replay()?;
        Ok(j)
    }
    pub fn append(&mut self, e: WorkflowEvent) -> Result<StableReceipt, CommitError> {
        if self.poisoned {
            return Err(CommitError::Poisoned);
        }
        let red = reduce(self.state, e).map_err(CommitError::Reduction)?;
        let r = WorkflowRecord::from(e);
        let seq = self.next;
        let bytes = frame(seq, r);
        let out = self
            .file
            .seek(SeekFrom::End(0))
            .and_then(|_| self.file.write_all(&bytes))
            .and_then(|_| self.file.sync_all());
        if let Err(source) = out {
            self.poisoned = true;
            return Err(CommitError::CommitOutcomeUnknown {
                attempted: r,
                sequence: seq,
                source,
            });
        }
        self.state = red.state;
        self.next += 1;
        Ok(StableReceipt {
            sequence: seq,
            durable_end: H as u64 + self.next * F as u64,
        })
    }
    pub fn replay(&mut self) -> Result<Recovery, JournalError> {
        self.file
            .seek(SeekFrom::Start(H as u64))
            .map_err(JournalError::Io)?;
        let rec = Records {
            file: &mut self.file,
            expected: 0,
        };
        let recovery = replay_stream(rec).map_err(|e| match e {
            ReplayError::Source(x) => x,
            ReplayError::Decode(x) => JournalError::Decode(x),
            ReplayError::Reduction(x) => JournalError::Reduction(x),
        })?;
        self.next = self
            .file
            .metadata()
            .map_err(JournalError::Io)?
            .len()
            .saturating_sub(H as u64)
            / F as u64;
        self.state = recovery.state;
        Ok(recovery)
    }
}

#[cfg(test)]
#[path = "../tests/support.rs"]
mod private_tests;
