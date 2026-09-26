//! Bounded, crash-safe reachability collection for immutable store files.
//!
//! Collection is driven by typed roots and explicit object edges. It never
//! interprets product payloads to guess reachability.

use super::super::StoreError;
use super::io_error;

pub(super) const GC_DIR: &str = "gc";
pub(super) const GC_STATE: &str = "state";
pub(super) const GC_ROOTS: &str = "roots";
pub(super) const GC_QUEUE: &str = "queue";
pub(super) const GC_MARK: &str = "mark";
pub(super) const GC_QUARANTINE: &str = "quarantine";
pub(super) const GC_STATE_MAGIC: &[u8] = b"LUNA_GC_STATE_V2\0";
pub(super) const GC_STATE_DIGEST_DOMAIN: &[u8] = b"store.gc.state.v2\0";
pub(super) const GC_ROOT_MAGIC: &[u8] = b"LUNA_GC_ROOTS_V1\0";
pub(super) const GC_QUEUE_DIGEST_DOMAIN: &[u8] = b"store.gc.queue.v1\0";
pub(super) const GC_MARK_DIGEST_DOMAIN: &[u8] = b"store.gc.mark.v1\0";
pub(super) const GC_CANDIDATE_DIGEST_DOMAIN: &[u8] = b"store.gc.candidate.v1\0";
pub(super) const GC_STATE_MAX_CURSOR: usize = 512;
pub(super) const GC_QUEUE_RECORD_BYTES: usize = 65;
pub(super) const GC_MARK_RECORD_BYTES: usize = 33;
pub(super) const GC_CANDIDATE_RECORD_BYTES: usize = 34;
pub(super) const GC_ROOT_RECORD_BYTES: usize = GC_QUEUE_RECORD_BYTES;
pub(super) const GC_STATE_FIXED_BYTES: usize =
    GC_STATE_MAGIC.len() + 1 + 8 + 8 + 8 + 8 + 8 + 8 + 8 + 4 + 32 + 32 + 32 + 32 + 32;
pub(super) const MAX_ROOTS: usize = 16_384;
pub(super) const MAX_PAGE_ITEMS: usize = 16_384;
pub(super) const MAX_MARK_PAGE_BYTES: usize = 64 * 1024 * 1024;
pub(super) const GC_STATE_MAX_BYTES: usize = GC_STATE_FIXED_BYTES + GC_STATE_MAX_CURSOR;
pub(super) const MAX_MARK_ITEMS: u64 = 8_000_000;
pub(super) const MAX_QUEUE_ITEMS: u64 = 8_000_000;

#[cfg(test)]
use std::sync::Mutex;

#[cfg(test)]
static TEST_FAULT: Mutex<Vec<(std::thread::ThreadId, u8)>> = Mutex::new(Vec::new());

mod index;
mod mark;
mod roots;
mod state;
mod state_io;

pub use roots::{GcRoot, GcRoots};
pub use state::{GcLimits, GcReport};
use state::Phase;

mod sweep;

fn sweep_kind(phase: Phase) -> Result<sweep::SweepKind, StoreError> {
    match phase {
        Phase::SweepPacks => Ok(sweep::SweepKind::Pack),
        Phase::SweepClosures => Ok(sweep::SweepKind::Closure),
        Phase::SweepObjects => Ok(sweep::SweepKind::Object),
        Phase::Mark | Phase::Finished => Err(StoreError::Corrupt),
    }
}

fn decode_sweep_offset(cursor: &[u8], index_len: u64) -> Result<u64, StoreError> {
    if !cursor.is_empty() && cursor.len() != 8 {
        return Err(StoreError::Corrupt);
    }
    let offset = if cursor.is_empty() {
        0
    } else {
        u64::from_be_bytes(cursor.try_into().map_err(|_| StoreError::Corrupt)?)
    };
    let record_bytes = u64::try_from(GC_CANDIDATE_RECORD_BYTES).map_err(|_| StoreError::Bounds)?;
    if offset > index_len || !offset.is_multiple_of(record_bytes) {
        return Err(StoreError::Corrupt);
    }
    Ok(offset)
}

#[cfg(test)]
pub(crate) fn set_test_fault(point: u8) {
    if let Ok(mut fault) = TEST_FAULT.lock() {
        let thread = std::thread::current().id();
        fault.retain(|(owner, _)| *owner != thread);
        fault.push((thread, point));
    }
}

#[cfg(test)]
fn take_test_fault(point: u8) -> bool {
    let Ok(mut fault) = TEST_FAULT.lock() else {
        return false;
    };
    let thread = std::thread::current().id();
    if let Some(index) = fault
        .iter()
        .position(|(owner, fault_point)| *owner == thread && *fault_point == point)
    {
        fault.remove(index);
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests;
