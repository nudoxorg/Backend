//! Shared admission for exact and lexical manifest selections.
//!
//! Both manifests report the same positions and identities. This scan is the
//! single admission check; each manifest returns [`SelectionAdmissionFault`]
//! directly.

use super::MAX_SELECTED_SEGMENTS;

/// One rejected selection-admission fact for a manifest boundary.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SelectionAdmissionFault<Id: Copy + Eq> {
    /// The selected present segment count exceeded the bounded query contract.
    SelectedSegmentLimit {
        /// Largest legal selected segment count.
        limit: usize,
        /// Full untrusted selected segment count.
        observed: usize,
    },
    /// The selected missing segment count exceeded the bounded query contract.
    MissingSegmentLimit {
        /// Largest legal selected missing segment count.
        limit: usize,
        /// Full untrusted selected missing segment count.
        observed: usize,
    },
    /// Reachable and missing segments did not account for the snapshot selection.
    SnapshotSelectionWidth {
        /// Number of identities bound into the snapshot.
        selected: usize,
        /// Number supplied as reachable or missing.
        observed: usize,
    },
    /// One segment identity appeared in two present positions.
    DuplicatePresentSegment {
        /// Earlier duplicate position.
        left_position: usize,
        /// Later duplicate position.
        right_position: usize,
        /// Repeated segment identity.
        id: Id,
    },
    /// One segment identity appeared as both present and unavailable.
    PresentAndMissing {
        /// Present segment position.
        present_position: usize,
        /// Missing segment position.
        missing_position: usize,
        /// Conflicting segment identity.
        id: Id,
    },
    /// One missing segment identity appeared in two positions.
    DuplicateMissingSegment {
        /// Earlier duplicate position.
        left_position: usize,
        /// Later duplicate position.
        right_position: usize,
        /// Repeated segment identity.
        id: Id,
    },
    /// A reachable segment was not selected by the snapshot authority.
    PresentNotSelected {
        /// Reachable segment position.
        present_position: usize,
        /// Unselected segment identity.
        id: Id,
    },
    /// Reachable update order disagreed with the order bound into the snapshot.
    PresentOrderMismatch {
        /// Reachable segment position whose order was rejected.
        present_position: usize,
        /// Snapshot position of the preceding reachable segment.
        preceding_selected_position: usize,
        /// Snapshot position of the rejected reachable segment.
        selected_position: usize,
        /// Rejected segment identity.
        id: Id,
    },
    /// A missing identity was not selected by the snapshot authority.
    MissingNotSelected {
        /// Missing segment position.
        missing_position: usize,
        /// Unselected missing identity.
        id: Id,
    },
}

fn heap_sort_indexes(indexes: &mut [usize], compare: impl Fn(usize, usize) -> core::cmp::Ordering) {
    let len = indexes.len();
    if len < 2 {
        return;
    }
    for start in (0..len / 2).rev() {
        sift_down_indexes(indexes, start, len, &compare);
    }
    for end in (1..len).rev() {
        indexes.swap(0, end);
        sift_down_indexes(indexes, 0, end, &compare);
    }
}

fn sift_down_indexes(
    indexes: &mut [usize],
    start: usize,
    end: usize,
    compare: &impl Fn(usize, usize) -> core::cmp::Ordering,
) {
    let mut root = start;
    loop {
        let mut child = root * 2 + 1;
        if child >= end {
            break;
        }
        let right_child = child + 1;
        if right_child < end && compare(indexes[child], indexes[right_child]).is_lt() {
            child = right_child;
        }
        if compare(indexes[root], indexes[child]).is_ge() {
            break;
        }
        indexes.swap(root, child);
        root = child;
    }
}

fn first_later_present_duplicate<Id: Copy + Eq + Ord>(
    present_position: usize,
    sorted_indexes: &[usize],
    present_id: &impl Fn(usize) -> Id,
) -> Option<usize> {
    let id = present_id(present_position);
    let mut lo = 0;
    let mut hi = sorted_indexes.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if present_id(sorted_indexes[mid]) < id {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    let mut index = lo;
    while index < sorted_indexes.len() && present_id(sorted_indexes[index]) == id {
        let later_position = sorted_indexes[index];
        if later_position > present_position {
            return Some(later_position);
        }
        index += 1;
    }
    None
}

fn earliest_adjacent_duplicate<Id: Copy + Eq>(
    sorted_indexes: &[usize],
    id_at: impl Fn(usize) -> Id,
) -> Option<(usize, usize, Id)> {
    if sorted_indexes.len() < 2 {
        return None;
    }
    let mut earliest = None;
    for pair in 0..(sorted_indexes.len() - 1) {
        let left_position = sorted_indexes[pair];
        let right_position = sorted_indexes[pair + 1];
        let id = id_at(left_position);
        if id == id_at(right_position) {
            match earliest {
                None => earliest = Some((left_position, right_position, id)),
                Some((best_left, _, _)) if left_position < best_left => {
                    earliest = Some((left_position, right_position, id));
                }
                _ => {}
            }
        }
    }
    earliest
}

fn sorted_lower_bound_missing<Id: Copy + Eq + Ord>(
    sorted_indexes: &[usize],
    missing: &[Id],
    target: Id,
) -> Option<usize> {
    let mut lo = 0;
    let mut hi = sorted_indexes.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if missing[sorted_indexes[mid]] < target {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    if lo < sorted_indexes.len() && missing[sorted_indexes[lo]] == target {
        Some(sorted_indexes[lo])
    } else {
        None
    }
}

fn sorted_lower_bound_selected<Id: Copy + Eq + Ord>(
    sorted_indexes: &[usize],
    selected: &[Id],
    target: Id,
) -> Option<usize> {
    let mut lo = 0;
    let mut hi = sorted_indexes.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if selected[sorted_indexes[mid]] < target {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    if lo < sorted_indexes.len() && selected[sorted_indexes[lo]] == target {
        Some(sorted_indexes[lo])
    } else {
        None
    }
}

pub(super) fn validate_selection_admission<Id: Copy + Eq + Ord>(
    selected: &[Id],
    present_len: usize,
    present_id: impl Fn(usize) -> Id,
    missing: &[Id],
) -> Result<(), SelectionAdmissionFault<Id>> {
    if present_len > MAX_SELECTED_SEGMENTS {
        return Err(SelectionAdmissionFault::SelectedSegmentLimit {
            limit: MAX_SELECTED_SEGMENTS,
            observed: present_len,
        });
    }
    if missing.len() > MAX_SELECTED_SEGMENTS {
        return Err(SelectionAdmissionFault::MissingSegmentLimit {
            limit: MAX_SELECTED_SEGMENTS,
            observed: missing.len(),
        });
    }
    let observed_selection = present_len + missing.len();
    if selected.len() != observed_selection {
        return Err(SelectionAdmissionFault::SnapshotSelectionWidth {
            selected: selected.len(),
            observed: observed_selection,
        });
    }

    let mut present_indexes = [0_usize; MAX_SELECTED_SEGMENTS];
    for index in 0..present_len {
        present_indexes[index] = index;
    }
    heap_sort_indexes(&mut present_indexes[..present_len], |left, right| {
        present_id(left)
            .cmp(&present_id(right))
            .then(left.cmp(&right))
    });

    let missing_len = missing.len();
    let mut missing_indexes = [0_usize; MAX_SELECTED_SEGMENTS];
    for index in 0..missing_len {
        missing_indexes[index] = index;
    }
    heap_sort_indexes(&mut missing_indexes[..missing_len], |left, right| {
        missing[left].cmp(&missing[right]).then(left.cmp(&right))
    });
    for present_position in 0..present_len {
        let id = present_id(present_position);
        if let Some(right_position) = first_later_present_duplicate(
            present_position,
            &present_indexes[..present_len],
            &present_id,
        ) {
            return Err(SelectionAdmissionFault::DuplicatePresentSegment {
                left_position: present_position,
                right_position,
                id,
            });
        }
        if let Some(missing_position) =
            sorted_lower_bound_missing(&missing_indexes[..missing_len], missing, id)
        {
            return Err(SelectionAdmissionFault::PresentAndMissing {
                present_position,
                missing_position,
                id,
            });
        }
    }

    if let Some((left_position, right_position, id)) =
        earliest_adjacent_duplicate(&missing_indexes[..missing_len], |index| missing[index])
    {
        return Err(SelectionAdmissionFault::DuplicateMissingSegment {
            left_position,
            right_position,
            id,
        });
    }

    let selected_len = selected.len();
    let mut selected_indexes = [0_usize; MAX_SELECTED_SEGMENTS];
    for index in 0..selected_len {
        selected_indexes[index] = index;
    }
    heap_sort_indexes(&mut selected_indexes[..selected_len], |left, right| {
        selected[left].cmp(&selected[right]).then(left.cmp(&right))
    });

    let mut preceding_selected_position = None;
    for present_position in 0..present_len {
        let id = present_id(present_position);
        let Some(selected_position) =
            sorted_lower_bound_selected(&selected_indexes[..selected_len], selected, id)
        else {
            return Err(SelectionAdmissionFault::PresentNotSelected {
                present_position,
                id,
            });
        };
        if let Some(preceding) = preceding_selected_position
            && selected_position <= preceding
        {
            return Err(SelectionAdmissionFault::PresentOrderMismatch {
                present_position,
                preceding_selected_position: preceding,
                selected_position,
                id,
            });
        }
        preceding_selected_position = Some(selected_position);
    }
    for missing_position in 0..missing_len {
        let id = missing[missing_position];
        if sorted_lower_bound_selected(&selected_indexes[..selected_len], selected, id).is_none() {
            return Err(SelectionAdmissionFault::MissingNotSelected {
                missing_position,
                id,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod validate_selection_admission_tests {
    use super::*;

    #[test]
    fn present_and_missing_before_later_present_duplicate() {
        let present = [1_u16, 2, 2];
        let missing = [1_u16];
        let selected = [1_u16, 2, 3, 4];
        assert_eq!(
            validate_selection_admission(
                &selected,
                present.len(),
                |index| present[index],
                &missing,
            ),
            Err(SelectionAdmissionFault::PresentAndMissing {
                present_position: 0,
                missing_position: 0,
                id: 1,
            })
        );
    }

    #[test]
    fn duplicate_present_reports_leftmost_repeat_position() {
        let present = [2_u16, 1, 2, 1];
        let selected = [2_u16, 1, 2, 1];
        assert_eq!(
            validate_selection_admission(&selected, present.len(), |index| present[index], &[]),
            Err(SelectionAdmissionFault::DuplicatePresentSegment {
                left_position: 0,
                right_position: 2,
                id: 2,
            })
        );
    }

    #[test]
    fn missing_not_selected_reports_smallest_missing_position() {
        let present = [1_u16, 2];
        let missing = [9_u16];
        let selected = [1_u16, 2];
        assert_eq!(
            validate_selection_admission(&selected, 1, |index| present[index], &missing),
            Err(SelectionAdmissionFault::MissingNotSelected {
                missing_position: 0,
                id: 9,
            })
        );
    }

    #[test]
    fn snapshot_selection_width_reports_selected_and_observed_counts() {
        let present = [1_u16];
        let selected = [1_u16, 2];
        assert_eq!(
            validate_selection_admission(&selected, present.len(), |index| present[index], &[]),
            Err(SelectionAdmissionFault::SnapshotSelectionWidth {
                selected: 2,
                observed: 1,
            })
        );
    }
}
