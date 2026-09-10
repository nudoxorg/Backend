//! Defines terminal behavior for `interface-search`, whose purpose is to define one honest multi-lane search vocabulary and its ranking over every retrieval backend.
//! This module owns the terminal invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Merged hits, lane reports, and the deterministic rank that combines them.

use interface_documents::{Signature, Symbol, Text};

use crate::{Cursor, Lane, LaneReport, ResultLimit, SearchRequest};

/// Lane-defined deterministic score; higher ranks first within a lane.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Score(pub u32);

/// One result row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Hit {
    /// Identity header.
    pub symbol: Symbol,
    /// Signature when the lane could afford it.
    pub signature: Option<Signature>,
    /// First documentation line when retained.
    pub summary: Option<Text>,
    /// Lane that produced the row.
    pub lane: Lane,
    /// Lane score.
    pub score: Score,
}

/// Whether more rows exist past this page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Truncation {
    /// Every merged row fit.
    Complete,
    /// The page was cut; continue from the cursor.
    Truncated {
        /// Next page start.
        next: Cursor,
    },
}

/// One complete answer to a [`crate::SearchRequest`].
///
/// The terminal echoes its request so a renderer can spell the scope, the query, and the
/// continuation without holding the request separately.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchTerminal {
    /// The request this terminal answers.
    pub request: SearchRequest,
    /// Merged, deduplicated rows in rank order.
    pub hits: Box<[Hit]>,
    /// One report per lane, in [`Lane::ALL`] order.
    pub lanes: [LaneReport; 4],
    /// Page continuation.
    pub truncation: Truncation,
}

/// Merges per-lane rows into one ranked page.
///
/// Rows are grouped by exact key. A key seen by several lanes keeps the earliest lane's row and
/// the sum of a fixed lane weight plus each lane's normalized score, so a declaration that is both
/// an exact match and semantically close outranks one that is only either. Ties break on the
/// address spelling so paging is deterministic. The cursor skips already-served rows.
#[must_use]
pub fn merge_lanes(
    lanes: [(Lane, Vec<Hit>); 4],
    limit: ResultLimit,
    cursor: Option<Cursor>,
) -> (Box<[Hit]>, Truncation) {
    let mut merged: Vec<(u64, Hit)> = Vec::new();
    for (lane, rows) in lanes {
        let weight = lane_weight(lane);
        let max = rows.iter().map(|row| row.score.0).max().unwrap_or(1).max(1);
        for row in rows {
            let normalized = u64::from(row.score.0) * 1_000 / u64::from(max);
            let contribution = weight + normalized;
            match merged
                .iter_mut()
                .find(|(_, kept)| kept.symbol.key() == row.symbol.key())
            {
                Some((rank, _)) => *rank += contribution,
                None => merged.push((contribution, row)),
            }
        }
    }
    merged.sort_by(|(left_rank, left), (right_rank, right)| {
        right_rank
            .cmp(left_rank)
            .then_with(|| {
                left.symbol
                    .address
                    .to_string()
                    .cmp(&right.symbol.address.to_string())
            })
    });
    let start = cursor.map_or(0, |cursor| usize::try_from(cursor.0).unwrap_or(usize::MAX));
    let page = usize::from(limit.get());
    let total = merged.len();
    let rows: Vec<Hit> = merged
        .into_iter()
        .skip(start)
        .take(page)
        .map(|(_, hit)| hit)
        .collect();
    let end = start + rows.len();
    let truncation = if end < total {
        Truncation::Truncated {
            next: Cursor(u32::try_from(end).unwrap_or(u32::MAX)),
        }
    } else {
        Truncation::Complete
    };
    (rows.into_boxed_slice(), truncation)
}

const fn lane_weight(lane: Lane) -> u64 {
    match lane {
        Lane::Exact => 10_000,
        Lane::Lexical => 4_000,
        Lane::Graph => 3_000,
        Lane::Semantic => 2_000,
    }
}
