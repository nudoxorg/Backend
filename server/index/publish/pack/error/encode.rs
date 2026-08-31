//! Defines pack error encode behavior for `server-index-publish`, whose purpose is to seal index segments into durable, reopenable snapshot packs.
//! This module owns the pack error encode invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Preflight and caller-output failures for canonical pack encoding.

use server_index_vocabulary::{ExactSegmentId, LexicalSegmentId};

use super::IndexPackLane;

/// Failure while preparing or encoding one canonical index pack.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum IndexPackEncodeError {
    /// One selected snapshot exact identity had no unique prepared-index source.
    #[error("selected exact index segment has no unique prepared source")]
    ExactSelection {
        /// Selected immutable exact segment.
        id: ExactSegmentId,
    },
    /// One selected snapshot lexical identity had no unique prepared-index source.
    #[error("selected lexical index segment has no unique prepared source")]
    LexicalSelection {
        /// Selected immutable lexical segment.
        id: LexicalSegmentId,
    },
    /// One row body or directory width exceeded the fixed pack address space.
    #[error("index pack size does not fit its fixed address space")]
    AddressSpace {
        /// Complete byte count that could not be represented by the grammar.
        observed: usize,
    },
    /// Caller-owned output cannot hold the complete preflighted canonical pack.
    #[error("index pack output is too small")]
    OutputTooSmall {
        /// Exact complete pack byte count.
        required: usize,
        /// Supplied caller-owned output capacity.
        available: usize,
    },
    /// Fixed compiler-derived selected capacity could not store one declared lane position.
    #[error("index pack selected lane exceeded fixed planning capacity")]
    PlanSlot {
        /// Selected lane whose slot was unavailable.
        lane: IndexPackLane,
        /// Declared selected segment position.
        ordinal: usize,
        /// Inline plan capacity.
        capacity: usize,
    },
}
