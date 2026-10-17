//! Fallible ownership-preserving sorting for one bounded local semantic operation.

use super::QueryError;
use std::collections::TryReserveError;

/// Each merge move crosses a checkpoint; no full-run library sort is used.
/// Three pre-reserved descriptor vectors move payload ownership without cloning.
/// Equal keys retain input order. No incomplete run escapes after refusal.
pub(super) fn sort_checked<T, K: Ord, E: From<QueryError> + From<TryReserveError>>(
    mut items: Vec<T>,
    key: impl Fn(&T) -> K,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<Vec<T>, E> {
    checkpoint()?;
    // Option descriptors avoid a second payload owner during merge.
    let count = items.len();
    let mut source = Vec::new();
    let mut destination = Vec::new();
    let mut result = Vec::new();
    source.try_reserve_exact(count)?;
    destination.try_reserve_exact(count)?;
    result.try_reserve_exact(count)?;
    for item in items.drain(..) {
        checkpoint()?;
        source.push(Some(item));
    }
    drop(items);
    // One item is the initial sorted run. This also preserves equal-key stability
    // without untracked allocation in a library stable sorter.
    let mut width = 1usize;
    while width < count {
        checkpoint()?;
        let span = width.checked_mul(2).ok_or(QueryError::CorpusLimit)?;
        for start in (0..count).step_by(span) {
            let middle = start.saturating_add(width).min(count);
            let end = start.saturating_add(span).min(count);
            let (mut left, mut right) = (start, middle);
            while left < middle || right < end {
                checkpoint()?;
                let take_left = right == end
                    || (left < middle
                        && source[left].as_ref().map(&key) <= source[right].as_ref().map(&key));
                let index = if take_left {
                    let index = left;
                    left += 1;
                    index
                } else {
                    let index = right;
                    right += 1;
                    index
                };
                destination.push(source[index].take());
            }
        }
        std::mem::swap(&mut source, &mut destination);
        destination.clear();
        width = span;
    }
    for item in source {
        checkpoint()?;
        result.push(item.ok_or(QueryError::CorpusLimit)?);
    }
    checkpoint()?;
    Ok(result)
}

/// Initial lexical match IDs are unique. Their canonical ID ordering needs
/// no stable tie buffer, so sort descriptors in place with bounded checkpoints.
pub(in crate::builtin) fn sort_unstable_in_place_checked<T, K: Ord, E>(
    items: &mut [T],
    key: impl Fn(&T) -> K,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    sort_unstable_by_in_place_checked(items, |left, right| key(left).cmp(&key(right)), checkpoint)
}

/// Comparator form retains the selected lexical rank policy without a second allocation.
pub(in crate::builtin) fn sort_unstable_by_in_place_checked<T, E>(
    items: &mut [T],
    compare: impl Fn(&T, &T) -> std::cmp::Ordering,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), E> {
    fn sift<T, E>(
        items: &mut [T],
        mut root: usize,
        end: usize,
        compare: &impl Fn(&T, &T) -> std::cmp::Ordering,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<(), E> {
        while root < end / 2 {
            checkpoint()?;
            let mut child = root * 2 + 1;
            if child + 1 < end && compare(&items[child], &items[child + 1]).is_lt() {
                child += 1;
            }
            if !compare(&items[root], &items[child]).is_lt() {
                break;
            }
            items.swap(root, child);
            root = child;
        }
        Ok(())
    }
    checkpoint()?;
    let count = items.len();
    for root in (0..count / 2).rev() {
        sift(items, root, count, &compare, checkpoint)?;
    }
    for end in (1..count).rev() {
        checkpoint()?;
        items.swap(0, end);
        sift(items, 0, end, &compare, checkpoint)?;
    }
    checkpoint()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Debug)]
    enum Refusal {
        Allocation(TryReserveError),
        Contract(QueryError),
        Stop(usize),
    }
    impl From<TryReserveError> for Refusal {
        fn from(error: TryReserveError) -> Self {
            Self::Allocation(error)
        }
    }
    impl From<QueryError> for Refusal {
        fn from(error: QueryError) -> Self {
            Self::Contract(error)
        }
    }

    impl std::fmt::Display for Refusal {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Allocation(error) => error.fmt(f),
                Self::Contract(error) => error.fmt(f),
                Self::Stop(at) => write!(f, "fixture stopped at {at}"),
            }
        }
    }

    #[test]
    fn sorted_owned_runs_preserve_stable_ties_and_interrupt_each_merge() {
        let input = (0..4097)
            .rev()
            .map(|ordinal| (ordinal % 17, ordinal))
            .collect::<Vec<_>>();
        let mut expected = input.clone();
        expected.sort_by_key(|entry| entry.0);
        let mut full_visits = 0;
        let output = sort_checked(input.clone(), |entry| entry.0, &mut || {
            full_visits += 1;
            Ok::<(), super::super::LocalSemanticError>(())
        })
        .expect("sorted");
        assert_eq!(output, expected);
        for stop_at in [1, 300, input.len() + 257, full_visits - 1] {
            let mut visits = 0;
            let stopped = sort_checked(input.clone(), |entry| entry.0, &mut || {
                visits += 1;
                if visits == stop_at {
                    Err(Refusal::Stop(visits))
                } else {
                    Ok(())
                }
            });
            assert!(matches!(stopped, Err(Refusal::Stop(at)) if at == stop_at));
            assert_eq!(visits, stop_at);
        }
    }

    #[test]
    fn in_place_unique_id_order_preserves_all_entries_and_stops_without_scratch() {
        let input = (0..4097).rev().map(|id| (id, id * 7)).collect::<Vec<_>>();
        let mut expected = input.clone();
        expected.sort_unstable_by_key(|entry| entry.0);
        let mut checked = input.clone();
        let allocation = checked.as_ptr();
        let capacity = checked.capacity();
        let mut complete = 0;
        sort_unstable_in_place_checked(&mut checked, |entry| entry.0, &mut || {
            complete += 1;
            Ok::<(), Refusal>(())
        })
        .expect("in-place ordering");
        assert_eq!(checked, expected);
        assert_eq!(checked.as_ptr(), allocation);
        assert_eq!(checked.capacity(), capacity);
        for stop_at in [1, 300, input.len() + 257, complete - 1] {
            let mut checked = input.clone();
            let mut observed = 0;
            let refusal =
                sort_unstable_in_place_checked(&mut checked, |entry| entry.0, &mut || {
                    observed += 1;
                    if observed == stop_at {
                        Err(Refusal::Stop(observed))
                    } else {
                        Ok(())
                    }
                });
            assert!(matches!(refusal, Err(Refusal::Stop(at)) if at == stop_at));
            assert_eq!(observed, stop_at);
            // Rejected partial ordering never escapes as a successful answer.
            checked.sort_unstable_by_key(|entry| entry.0);
            assert_eq!(
                checked, expected,
                "interruption never loses descriptor ownership"
            );
        }
    }
}
