//! Bounded exact-identity comparison over admitted retained material.
//! No spelling or ordinal joins are performed. One-sided absence is exact only
//! when the opposite captured set is complete for the compared relation.
use std::mem::size_of;

pub(super) type Key = [u8; 32];
#[derive(Clone, Copy)]
pub(super) struct Entry<'a> {
    pub id: Key,
    pub bytes: &'a [u8],
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Side {
    BaseOnly,
    TargetOnly,
    BothChanged,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Certainty {
    Exact,
    Candidate,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Change {
    pub id: Key,
    pub side: Side,
    pub certainty: Certainty,
}
#[derive(Clone, Copy)]
/// `output_bytes` bounds fixed-size logical changes, not a serialized reply;
/// the public adapter must enforce its own wire-byte limit.
pub(super) struct Limits {
    pub input_rows: usize,
    pub compared_bytes: usize,
    pub comparisons: usize,
    pub output_rows: usize,
    pub output_bytes: usize,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Stop {
    Complete,
    Input,
    Comparisons,
    OutputRows,
    OutputBytes,
    Cancelled,
    Invalid,
}
pub(super) struct Outcome {
    pub changes: Vec<Change>,
    pub stop: Stop,
    pub comparisons: usize,
    pub compared_bytes: usize,
}

/// Compare sorted unique exact IDs and their captured canonical bytes. For
/// source images IDs must be admitted host keys; for semantic facts IDs must
/// be native row keys. Matching IDs are not evidence of rename continuity.
pub(super) fn diff<F: FnMut() -> bool>(
    base: &[Entry<'_>],
    target: &[Entry<'_>],
    base_complete: bool,
    target_complete: bool,
    limits: Limits,
    mut cancelled: F,
) -> Outcome {
    let mut out = Outcome {
        changes: Vec::new(),
        stop: Stop::Complete,
        comparisons: 0,
        compared_bytes: 0,
    };
    if base
        .len()
        .checked_add(target.len())
        .is_none_or(|n| n > limits.input_rows)
    {
        out.stop = Stop::Input;
        return out;
    }
    for entries in [base, target] {
        for pair in entries.windows(2) {
            if cancelled() {
                out.stop = Stop::Cancelled;
                return out;
            }
            if pair[0].id >= pair[1].id {
                out.stop = Stop::Invalid;
                return out;
            }
        }
    }
    let (mut a, mut b) = (0, 0);
    while a < base.len() || b < target.len() {
        if cancelled() {
            out.stop = Stop::Cancelled;
            break;
        }
        if out.comparisons == limits.comparisons {
            out.stop = Stop::Comparisons;
            break;
        }
        out.comparisons += 1;
        let ordering = match (base.get(a), target.get(b)) {
            (Some(left), Some(right)) => left.id.cmp(&right.id),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => break,
        };
        let change = match ordering {
            std::cmp::Ordering::Less => {
                let id = base[a].id;
                a += 1;
                Some(Change {
                    id,
                    side: Side::BaseOnly,
                    certainty: if target_complete {
                        Certainty::Exact
                    } else {
                        Certainty::Candidate
                    },
                })
            }
            std::cmp::Ordering::Greater => {
                let id = target[b].id;
                b += 1;
                Some(Change {
                    id,
                    side: Side::TargetOnly,
                    certainty: if base_complete {
                        Certainty::Exact
                    } else {
                        Certainty::Candidate
                    },
                })
            }
            std::cmp::Ordering::Equal => {
                let Some(read) = base[a]
                    .bytes
                    .len()
                    .checked_add(target[b].bytes.len())
                    .and_then(|n| out.compared_bytes.checked_add(n))
                else {
                    out.stop = Stop::Input;
                    break;
                };
                if read > limits.compared_bytes {
                    out.stop = Stop::Input;
                    break;
                }
                out.compared_bytes = read;
                let change = (base[a].bytes != target[b].bytes).then_some(Change {
                    id: base[a].id,
                    side: Side::BothChanged,
                    certainty: Certainty::Exact,
                });
                a += 1;
                b += 1;
                change
            }
        };
        if let Some(change) = change {
            if out.changes.len() == limits.output_rows {
                out.stop = Stop::OutputRows;
                break;
            }
            if out
                .changes
                .len()
                .checked_add(1)
                .and_then(|n| n.checked_mul(size_of::<Change>()))
                .is_none_or(|n| n > limits.output_bytes)
            {
                out.stop = Stop::OutputBytes;
                break;
            }
            out.changes.push(change);
        }
    }
    out
}
#[cfg(test)]
mod tests {
    use super::*;
    fn e(id: u8, bytes: &'static [u8]) -> Entry<'static> {
        Entry {
            id: [id; 32],
            bytes,
        }
    }
    fn cap() -> Limits {
        Limits {
            input_rows: 20,
            compared_bytes: 100,
            comparisons: 20,
            output_rows: 20,
            output_bytes: 1000,
        }
    }
    #[test]
    fn deletion_needs_complete_target_and_never_joins_spellings() {
        let base = [e(1, b"--same")];
        let target = [e(2, b"--same")];
        let full = diff(&base, &target, true, true, cap(), || false);
        assert_eq!(
            full.changes,
            [
                Change {
                    id: [1; 32],
                    side: Side::BaseOnly,
                    certainty: Certainty::Exact
                },
                Change {
                    id: [2; 32],
                    side: Side::TargetOnly,
                    certainty: Certainty::Exact
                }
            ]
        );
        let partial = diff(&base, &[], true, false, cap(), || false);
        assert_eq!(partial.changes[0].certainty, Certainty::Candidate);
        let complete = diff(&base, &[], true, true, cap(), || false);
        assert_eq!(complete.changes[0].certainty, Certainty::Exact);
    }
    #[test]
    fn recomputation_preserves_page_order_and_never_upgrades_gaps() {
        let base = [e(1, b"one"), e(3, b"three"), e(5, b"five")];
        let target = [e(2, b"two"), e(4, b"four")];
        let mut first_cap = cap();
        first_cap.output_rows = 2;
        let first = diff(&base, &target, false, false, first_cap, || false);
        assert_eq!(first.stop, Stop::OutputRows);
        let all = diff(&base, &target, false, false, cap(), || false);
        assert_eq!(first.changes, all.changes[..2]);
        assert_eq!(all.changes.len(), 5);
        assert!(
            all.changes
                .iter()
                .all(|row| row.certainty == Certainty::Candidate)
        );
    }
    #[test]
    fn matched_identity_changes_and_limits_are_explicit() {
        let base = [e(1, b"old")];
        let target = [e(1, b"new")];
        let result = diff(&base, &target, false, false, cap(), || false);
        assert_eq!(
            result.changes[0],
            Change {
                id: [1; 32],
                side: Side::BothChanged,
                certainty: Certainty::Exact
            }
        );
        let mut no_output = cap();
        no_output.output_rows = 0;
        assert_eq!(
            diff(&base, &target, true, true, no_output, || false).stop,
            Stop::OutputRows
        );
        let mut no_bytes = cap();
        no_bytes.output_bytes = 0;
        assert_eq!(
            diff(&base, &target, true, true, no_bytes, || false).stop,
            Stop::OutputBytes
        );
        let mut limited = cap();
        limited.compared_bytes = 5;
        assert_eq!(
            diff(&base, &target, true, true, limited, || false).stop,
            Stop::Input
        );
        assert_eq!(
            diff(&base, &target, true, true, cap(), || true).stop,
            Stop::Cancelled
        );
        assert_eq!(
            diff(&[e(1, b"a"), e(1, b"b")], &[], true, true, cap(), || false).stop,
            Stop::Invalid
        );
    }
}
