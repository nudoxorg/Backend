//! Generic, filesystem-backed BLAKE3 hash-chain journal.
//!
//! A journal domain supplies a canonical record codec. This module supplies
//! framing, length checks, fsync, predecessor validation, and recovery of a
//! torn final write.

use crate::schema::{JOURNAL_FORMAT, JOURNAL_MAGIC, RecordId, chain_digest};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

mod admission;
mod append;
mod frame;
mod record;
mod recovery;
mod scan;

pub use admission::{JournalCodec, JournalDomain, JournalLimits};
pub use record::{
    ChainHash, JournalCheckpoint, JournalError, JournalFrame, JournalFrameRef, JournalReceipt,
    JournalRecovery, JournalScan,
};

use admission::{HEADER_BYTES, MAX_PAYLOAD, containing_directory, validate_limits};

pub(crate) use frame::read_frame_at_path;
pub use frame::sync_to_end;

/// A synchronized append-only journal.
pub struct HashChainJournal<D: JournalCodec> {
    path: PathBuf,
    state: Mutex<JournalState<D>>,
}

struct JournalState<D: JournalCodec> {
    file: File,
    next_sequence: u64,
    chain: ChainHash<D>,
    next_offset: u64,
    unusable: bool,
}

impl<D: JournalCodec> fmt::Debug for HashChainJournal<D> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HashChainJournal")
            .field("path", &self.path)
            .field("domain", &D::DOMAIN)
            .field("type", &D::TYPE)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests;
