//! Durable GC state records and bounded mark/queue membership.

use super::super::super::digest;
use super::super::{Hash, StoreError};
use super::{
    GC_MARK_RECORD_BYTES, GC_STATE_DIGEST_DOMAIN, GC_STATE_FIXED_BYTES, GC_STATE_MAGIC,
    GC_STATE_MAX_CURSOR, MAX_MARK_ITEMS, MAX_MARK_PAGE_BYTES, MAX_PAGE_ITEMS, MAX_QUEUE_ITEMS,
    MAX_ROOTS,
};

/// Per-call limits for bounded mark and sweep pages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GcLimits {
    /// Maximum roots admitted into one collection.
    pub max_roots: usize,
    /// Queue items processed by one mark page.
    pub mark_page_items: usize,
    /// Immutable object-envelope bytes loaded by one manifest page.
    pub mark_page_bytes: usize,
    /// Candidate records examined by one sweep page.  The candidate index is
    /// frozen once per sweep phase, so this bounds work even when every file
    /// is live and no file is quarantined.
    pub sweep_page_items: usize,
    /// Maximum durable mark records.
    pub max_mark_items: u64,
    /// Maximum durable queue records.
    pub max_queue_items: u64,
}

impl Default for GcLimits {
    fn default() -> Self {
        Self {
            max_roots: 4_096,
            mark_page_items: 256,
            mark_page_bytes: 4 * 1024 * 1024,
            sweep_page_items: 256,
            max_mark_items: MAX_MARK_ITEMS,
            max_queue_items: MAX_QUEUE_ITEMS,
        }
    }
}

/// Authenticated result of one completed collection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GcReport {
    /// Monotonic collection epoch.
    pub epoch: u64,
    /// Number of distinct physical roots marked.
    pub marked_items: u64,
    /// Number of immutable files quarantined and reclaimed.
    pub swept_items: u64,
    /// Bytes in quarantined files after metadata accounting.
    pub reclaimed_bytes: u64,
    /// Candidate records examined across all sweep phases.
    pub examined_items: u64,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum Phase {
    Mark = 1,
    SweepPacks = 2,
    SweepClosures = 3,
    SweepObjects = 4,
    Finished = 5,
}

impl Phase {
    pub(super) fn decode(value: u8) -> Result<Self, StoreError> {
        match value {
            1 => Ok(Self::Mark),
            2 => Ok(Self::SweepPacks),
            3 => Ok(Self::SweepClosures),
            4 => Ok(Self::SweepObjects),
            5 => Ok(Self::Finished),
            _ => Err(StoreError::Corrupt),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct GcState {
    pub(super) phase: Phase,
    pub(super) epoch: u64,
    pub(super) queue_offset: u64,
    pub(super) marked_items: u64,
    pub(super) swept_items: u64,
    pub(super) reclaimed_bytes: u64,
    pub(super) examined_items: u64,
    pub(super) candidate_length: u64,
    pub(super) cursor: Vec<u8>,
    pub(super) roots_digest: Hash,
    pub(super) queue_digest: Hash,
    pub(super) mark_digest: Hash,
    pub(super) candidate_digest: Hash,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) enum MarkKey {
    Pack(Hash),
    Closure(Hash),
    Object(Hash),
}

impl MarkKey {
    pub(super) fn encode(self) -> [u8; GC_MARK_RECORD_BYTES] {
        let mut bytes = [0; GC_MARK_RECORD_BYTES];
        match self {
            Self::Pack(id) => {
                bytes[0] = 1;
                bytes[1..].copy_from_slice(&id);
            }
            Self::Closure(id) => {
                bytes[0] = 2;
                bytes[1..].copy_from_slice(&id);
            }
            Self::Object(id) => {
                bytes[0] = 3;
                bytes[1..].copy_from_slice(&id);
            }
        }
        bytes
    }

    pub(super) fn decode(bytes: &[u8; GC_MARK_RECORD_BYTES]) -> Result<Self, StoreError> {
        let id: Hash = bytes[1..].try_into().map_err(|_| StoreError::Corrupt)?;
        match bytes[0] {
            1 => Ok(Self::Pack(id)),
            2 => Ok(Self::Closure(id)),
            3 => Ok(Self::Object(id)),
            _ => Err(StoreError::Corrupt),
        }
    }
}

impl GcState {
    pub(super) fn encode(&self) -> Result<Vec<u8>, StoreError> {
        if self.cursor.len() > GC_STATE_MAX_CURSOR {
            return Err(StoreError::Bounds);
        }
        let mut bytes = Vec::with_capacity(GC_STATE_FIXED_BYTES + self.cursor.len());
        bytes.extend_from_slice(GC_STATE_MAGIC);
        bytes.push(self.phase as u8);
        bytes.extend_from_slice(&self.epoch.to_be_bytes());
        bytes.extend_from_slice(&self.queue_offset.to_be_bytes());
        bytes.extend_from_slice(&self.marked_items.to_be_bytes());
        bytes.extend_from_slice(&self.swept_items.to_be_bytes());
        bytes.extend_from_slice(&self.reclaimed_bytes.to_be_bytes());
        bytes.extend_from_slice(&self.examined_items.to_be_bytes());
        bytes.extend_from_slice(&self.candidate_length.to_be_bytes());
        bytes.extend_from_slice(
            &u32::try_from(self.cursor.len())
                .map_err(|_| StoreError::Bounds)?
                .to_be_bytes(),
        );
        bytes.extend_from_slice(&self.roots_digest);
        bytes.extend_from_slice(&self.queue_digest);
        bytes.extend_from_slice(&self.mark_digest);
        bytes.extend_from_slice(&self.candidate_digest);
        bytes.extend_from_slice(&self.cursor);
        bytes.extend_from_slice(&digest(GC_STATE_DIGEST_DOMAIN, &bytes));
        Ok(bytes)
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<Self, StoreError> {
        if bytes.len() < GC_STATE_FIXED_BYTES || !bytes.starts_with(GC_STATE_MAGIC) {
            return Err(StoreError::Corrupt);
        }
        let mut at = GC_STATE_MAGIC.len();
        let phase = Phase::decode(*bytes.get(at).ok_or(StoreError::Corrupt)?)?;
        at = at.checked_add(1).ok_or(StoreError::Bounds)?;
        let epoch = read_u64_be(bytes, &mut at)?;
        let queue_offset = read_u64_be(bytes, &mut at)?;
        let marked_items = read_u64_be(bytes, &mut at)?;
        let swept_items = read_u64_be(bytes, &mut at)?;
        let reclaimed_bytes = read_u64_be(bytes, &mut at)?;
        let examined_items = read_u64_be(bytes, &mut at)?;
        let candidate_length = read_u64_be(bytes, &mut at)?;
        let cursor_len =
            usize::try_from(read_u32_be(bytes, &mut at)?).map_err(|_| StoreError::Bounds)?;
        if cursor_len > GC_STATE_MAX_CURSOR {
            return Err(StoreError::Bounds);
        }
        let roots_digest: Hash = bytes
            .get(at..at.checked_add(32).ok_or(StoreError::Bounds)?)
            .ok_or(StoreError::Corrupt)?
            .try_into()
            .map_err(|_| StoreError::Corrupt)?;
        at = at.checked_add(32).ok_or(StoreError::Bounds)?;
        let queue_digest: Hash = bytes
            .get(at..at.checked_add(32).ok_or(StoreError::Bounds)?)
            .ok_or(StoreError::Corrupt)?
            .try_into()
            .map_err(|_| StoreError::Corrupt)?;
        at = at.checked_add(32).ok_or(StoreError::Bounds)?;
        let mark_digest: Hash = bytes
            .get(at..at.checked_add(32).ok_or(StoreError::Bounds)?)
            .ok_or(StoreError::Corrupt)?
            .try_into()
            .map_err(|_| StoreError::Corrupt)?;
        at = at.checked_add(32).ok_or(StoreError::Bounds)?;
        let candidate_digest: Hash = bytes
            .get(at..at.checked_add(32).ok_or(StoreError::Bounds)?)
            .ok_or(StoreError::Corrupt)?
            .try_into()
            .map_err(|_| StoreError::Corrupt)?;
        at = at.checked_add(32).ok_or(StoreError::Bounds)?;
        let cursor = bytes
            .get(at..at.checked_add(cursor_len).ok_or(StoreError::Bounds)?)
            .ok_or(StoreError::Corrupt)?
            .to_vec();
        at = at.checked_add(cursor_len).ok_or(StoreError::Bounds)?;
        let checksum_at = at;
        let checksum: Hash = bytes
            .get(at..at.checked_add(32).ok_or(StoreError::Bounds)?)
            .ok_or(StoreError::Corrupt)?
            .try_into()
            .map_err(|_| StoreError::Corrupt)?;
        at = at.checked_add(32).ok_or(StoreError::Bounds)?;
        if at != bytes.len() || digest(GC_STATE_DIGEST_DOMAIN, &bytes[..checksum_at]) != checksum {
            return Err(StoreError::Corrupt);
        }
        Ok(Self {
            phase,
            epoch,
            queue_offset,
            marked_items,
            swept_items,
            reclaimed_bytes,
            examined_items,
            candidate_length,
            cursor,
            roots_digest,
            queue_digest,
            mark_digest,
            candidate_digest,
        })
    }
}

pub(super) fn validate_limits(limits: GcLimits) -> Result<(), StoreError> {
    if limits.max_roots == 0
        || limits.max_roots > MAX_ROOTS
        || limits.mark_page_items == 0
        || limits.mark_page_items > MAX_PAGE_ITEMS
        || limits.mark_page_bytes == 0
        || limits.mark_page_bytes > MAX_MARK_PAGE_BYTES
        || limits.sweep_page_items == 0
        || limits.sweep_page_items > MAX_PAGE_ITEMS
        || limits.max_mark_items == 0
        || limits.max_mark_items > MAX_MARK_ITEMS
        || limits.max_queue_items == 0
        || limits.max_queue_items > MAX_QUEUE_ITEMS
    {
        return Err(StoreError::Bounds);
    }
    Ok(())
}

pub(super) fn read_u32_be(bytes: &[u8], at: &mut usize) -> Result<u32, StoreError> {
    let end = at.checked_add(4).ok_or(StoreError::Bounds)?;
    let value = bytes
        .get(*at..end)
        .ok_or(StoreError::Corrupt)?
        .try_into()
        .map_err(|_| StoreError::Corrupt)?;
    *at = end;
    Ok(u32::from_be_bytes(value))
}

pub(super) fn read_u64_be(bytes: &[u8], at: &mut usize) -> Result<u64, StoreError> {
    let end = at.checked_add(8).ok_or(StoreError::Bounds)?;
    let value = bytes
        .get(*at..end)
        .ok_or(StoreError::Corrupt)?
        .try_into()
        .map_err(|_| StoreError::Corrupt)?;
    *at = end;
    Ok(u64::from_be_bytes(value))
}
