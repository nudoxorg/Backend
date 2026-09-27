//! Oracle for the structured flow evaluator.
//!
//! Bytes are a fixed-width record grammar, not a production decoder. The law
//! compares `reduce_rows`, `filter`, and `distinct` with an `i128` sum, which
//! is wider than the operators' `i64` support.

use std::collections::{BTreeMap, BTreeSet};

use backend_flow::{
    Delta, DistinctState, Epoch, FlowError, ObjectIdentity, RelationIdentity, RowKey, Time,
    reduce_rows,
};

use crate::{OracleFailure, Verdict};

/// Largest input this harness will mutate or replay.
///
/// The integer lives in the sibling `max_len` file so Nix and Rust share it.
pub(crate) const MAX_LEN: usize = crate::decimal_usize(include_str!("max_len"));

const HEADER: usize = 4;
const RECORD: usize = 16;
// Sixty records of i32 diffs cannot overflow i64, even as a prefix sum.
const MAX_RECORDS: usize = 64;

const _: () = assert!(MAX_LEN == HEADER + MAX_RECORDS * RECORD);
const _: () = assert!(MAX_RECORDS == 64);

/// One admitted update: key `1`, value `1`, epoch `0`, diff `+1`.
///
/// # Errors
///
/// Construction does not fail. The discovered target table still requires a `Result`.
#[allow(
    clippy::unnecessary_wraps,
    reason = "Target::canonical is fn() -> Result<Vec<u8>, String>"
)]
pub(crate) fn canonical() -> Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(HEADER + RECORD);
    bytes.extend_from_slice(b"FLW1");
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&1_i32.to_le_bytes());
    Ok(bytes)
}

/// Replays the structured evaluator.
///
/// # Errors
///
/// Returns when an operator disagrees with the independent sum, or when a
/// record window is not four bytes.
pub(crate) fn exercise(bytes: &[u8]) -> Result<(), OracleFailure> {
    judge(bytes).map(|_| ())
}

/// Classifies one buffer after the flow laws run.
///
/// # Errors
///
/// Returns when a law fails. A clean rejection is [`Verdict::Rejected`].
pub(crate) fn judge(bytes: &[u8]) -> Result<Verdict, OracleFailure> {
    let Some(deltas) = parse(bytes)? else {
        return Ok(Verdict::Rejected);
    };
    if deltas.is_empty() {
        return Ok(Verdict::Rejected);
    }
    evaluate(&deltas)?;
    Ok(Verdict::Accepted)
}

fn parse(bytes: &[u8]) -> Result<Option<Vec<Delta<u32>>>, OracleFailure> {
    let Some(body) = bytes.strip_prefix(b"FLW1") else {
        return Ok(None);
    };
    let (chunks, remainder) = body.as_chunks::<RECORD>();
    if !remainder.is_empty() || chunks.len() > MAX_RECORDS {
        return Ok(None);
    }
    let relation = RelationIdentity::from_bytes([0x11; 32]);
    let object = ObjectIdentity::from_bytes([0x22; 32]);
    let mut deltas = Vec::new();
    for chunk in chunks {
        let key = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        let value = u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
        let epoch = u32::from_le_bytes([chunk[8], chunk[9], chunk[10], chunk[11]]);
        let diff = i32::from_le_bytes([chunk[12], chunk[13], chunk[14], chunk[15]]);
        if diff == 0 {
            continue;
        }
        let delta = Delta::checked(
            RowKey {
                relation,
                object,
                key: u64::from(key),
            },
            value,
            Time::new(Epoch(u64::from(epoch)), 0),
            i64::from(diff),
        )
        .map_err(|error| OracleFailure::new(format!("nonzero diff was rejected: {error}")))?;
        deltas.push(delta);
    }
    Ok(Some(deltas))
}

fn evaluate(deltas: &[Delta<u32>]) -> Result<(), OracleFailure> {
    let once = reduce_rows(deltas.iter().cloned());
    let twice = reduce_rows(deltas.iter().cloned());
    if once != twice {
        return Err(OracleFailure::new("reduce_rows is not deterministic"));
    }
    let summed = independent_sums(deltas);
    match once {
        Err(FlowError::Overflow) => {
            if summed.overflowed {
                Ok(())
            } else {
                Err(OracleFailure::new(
                    "reduce_rows overflowed a sum that fits in i64",
                ))
            }
        }
        Err(error) => Err(OracleFailure::new(format!("reduce_rows failed: {error}"))),
        Ok(map) => {
            if summed.overflowed {
                return Err(OracleFailure::new("reduce_rows accepted a sum outside i64"));
            }
            if map != summed.totals {
                return Err(OracleFailure::new("reduce_rows disagrees with an i128 sum"));
            }
            check_filter(deltas, &map)?;
            check_distinct(deltas, &summed.positive)?;
            Ok(())
        }
    }
}

struct IndependentSums {
    overflowed: bool,
    totals: BTreeMap<(RowKey, u32), i64>,
    positive: BTreeSet<(RowKey, u32)>,
}

fn independent_sums(deltas: &[Delta<u32>]) -> IndependentSums {
    let mut wide: BTreeMap<(RowKey, u32), i128> = BTreeMap::new();
    for row in deltas {
        let key = (row.key, row.value);
        let addend = i128::from(row.diff.value());
        let next = match wide.get(&key) {
            Some(total) => total.saturating_add(addend),
            None => addend,
        };
        wide.insert(key, next);
    }
    let mut overflowed = false;
    let mut totals = BTreeMap::new();
    let mut positive = BTreeSet::new();
    for (key, total) in wide {
        match i64::try_from(total) {
            Ok(0) => {}
            Ok(value) => {
                totals.insert(key, value);
                if value > 0 {
                    positive.insert(key);
                }
            }
            Err(_) => overflowed = true,
        }
    }
    IndependentSums {
        overflowed,
        totals,
        positive,
    }
}

fn check_filter(
    deltas: &[Delta<u32>],
    expect: &BTreeMap<(RowKey, u32), i64>,
) -> Result<(), OracleFailure> {
    let filtered = backend_flow::filter(deltas.iter().cloned(), |_, _| true)
        .map_err(|error| OracleFailure::new(format!("filter failed: {error}")))?;
    let reduced = reduce_rows(filtered).map_err(|error| {
        OracleFailure::new(format!("filter output could not be reduced: {error}"))
    })?;
    if reduced != *expect {
        return Err(OracleFailure::new(
            "filter did not preserve the reduced support",
        ));
    }
    Ok(())
}

fn check_distinct(
    deltas: &[Delta<u32>],
    positive: &BTreeSet<(RowKey, u32)>,
) -> Result<(), OracleFailure> {
    let mut state = DistinctState::new();
    let emitted = state
        .apply(deltas.iter().cloned())
        .map_err(|error| OracleFailure::new(format!("distinct failed: {error}")))?;
    for row in &emitted {
        let weight = row.diff.value();
        if weight != 1 && weight != -1 {
            return Err(OracleFailure::new(
                "distinct emitted a weight other than a membership crossing",
            ));
        }
    }
    if state.members() != *positive {
        return Err(OracleFailure::new(
            "distinct membership disagrees with the positive i128 sums",
        ));
    }
    Ok(())
}
