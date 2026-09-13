//! Schema and hashing primitives owned by the composition layer.
//!
//! Lower crates own the identities of workspace values and execution outputs.
//! This module owns identities that describe durability events. Keeping the
//! journal domain in the type system prevents a workspace frame from being
//! replayed as an effect or layout frame by accident.

use crate::journal::JournalDomain;
use std::marker::PhantomData;

/// Current on-disk journal format.
pub const JOURNAL_FORMAT: u16 = 2;
/// Fixed frame magic. The trailing nul keeps this distinct from text files.
pub const JOURNAL_MAGIC: [u8; 8] = *b"BEJNL2\0\0";
/// Domain separator for the engine hash-chain journal.
pub const JOURNAL_HASH_DOMAIN: &[u8] = b"backend.engine.journal.v2\0";

/// Workspace authority journal namespace.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkspaceLog;
/// External effect journal namespace.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct EffectLog;
/// Physical layout journal namespace.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LayoutLog;
/// Daemon control journal namespace.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DaemonLog;

/// A typed journal record schema. It is useful when a record identity needs
/// to be persisted alongside its frame chain.
pub struct JournalRecordSchema<D>(PhantomData<fn() -> D>);

/// Stable identifier for one canonical journal payload.
pub struct RecordId<D> {
    bytes: [u8; 32],
    _domain: PhantomData<fn() -> D>,
}

impl<D> Copy for RecordId<D> {}
impl<D> Clone for RecordId<D> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<D> PartialEq for RecordId<D> {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
}
impl<D> Eq for RecordId<D> {}
impl<D> std::hash::Hash for RecordId<D> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.bytes.hash(state);
    }
}
impl<D> PartialOrd for RecordId<D> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<D> Ord for RecordId<D> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.bytes.cmp(&other.bytes)
    }
}
impl<D> std::fmt::Debug for RecordId<D> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("RecordId").field(&self.bytes).finish()
    }
}

impl<D> RecordId<D> {
    /// Rehydrates an identity read from an engine-owned pointer. This is
    /// crate-private so external callers cannot mint an admitted record link.
    #[must_use]
    pub(crate) const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self {
            bytes,
            _domain: PhantomData,
        }
    }

    /// Computes a content identity for one canonical payload.
    #[must_use]
    pub fn from_payload(payload: &[u8]) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(JOURNAL_HASH_DOMAIN);
        hasher.update(b"record\0");
        hasher.update(payload);
        Self {
            bytes: *hasher.finalize().as_bytes(),
            _domain: PhantomData,
        }
    }

    /// Returns the fixed-width identity bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.bytes
    }
}

impl JournalDomain for WorkspaceLog {
    const DOMAIN: u8 = 0x81;
    const TYPE: u16 = 0x0001;
    const VERSION: u8 = 1;
}

impl JournalDomain for EffectLog {
    const DOMAIN: u8 = 0x81;
    const TYPE: u16 = 0x0002;
    const VERSION: u8 = 1;
}

impl JournalDomain for LayoutLog {
    const DOMAIN: u8 = 0x81;
    const TYPE: u16 = 0x0003;
    const VERSION: u8 = 1;
}

impl JournalDomain for DaemonLog {
    const DOMAIN: u8 = 0x81;
    const TYPE: u16 = 0x0004;
    const VERSION: u8 = 1;
}

/// Computes a domain, version, sequence, predecessor, and payload hash.
#[must_use]
pub fn chain_digest<D: JournalDomain>(
    sequence: u64,
    previous: &[u8; 32],
    payload: &[u8],
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(JOURNAL_HASH_DOMAIN);
    hasher.update(&[D::DOMAIN]);
    hasher.update(&D::TYPE.to_be_bytes());
    hasher.update(&[D::VERSION]);
    hasher.update(&sequence.to_be_bytes());
    hasher.update(previous);
    hasher.update(&(payload.len() as u64).to_be_bytes());
    hasher.update(payload);
    *hasher.finalize().as_bytes()
}
