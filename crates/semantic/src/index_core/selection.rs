//! Shared admission for exact and lexical manifest selections.
//!
//! Both manifests report the same positions and identities. This function is
//! the single scan; each manifest maps [`SelectionFault`] onto its public error.

pub(super) enum SelectionFault<Id> {
    SelectedSegmentLimit {
        limit: usize,
        observed: usize,
    },
    MissingSegmentLimit {
        limit: usize,
        observed: usize,
    },
    SnapshotSelectionWidth {
        selected: usize,
        observed: usize,
    },
    PresentNotSelected {
        present_position: usize,
        id: Id,
    },
    PresentOrderMismatch {
        present_position: usize,
        preceding_selected_position: usize,
        selected_position: usize,
        id: Id,
    },
    MissingNotSelected {
        missing_position: usize,
        id: Id,
    },
    DuplicatePresentSegment {
        left_position: usize,
        right_position: usize,
        id: Id,
    },
    PresentAndMissing {
        present_position: usize,
        missing_position: usize,
        id: Id,
    },
    DuplicateMissingSegment {
        left_position: usize,
        right_position: usize,
        id: Id,
    },
}

pub(super) fn admit_selection<Id, Present>(
    selected: &[Id],
    present: &[Present],
    missing: &[Id],
    limit: usize,
    present_id: impl Fn(&Present) -> Id,
) -> Result<(), SelectionFault<Id>>
where
    Id: Copy + PartialEq,
{
    if present.len() > limit {
        return Err(SelectionFault::SelectedSegmentLimit {
            limit,
            observed: present.len(),
        });
    }
    if missing.len() > limit {
        return Err(SelectionFault::MissingSegmentLimit {
            limit,
            observed: missing.len(),
        });
    }
    let observed_selection = present.len() + missing.len();
    if selected.len() != observed_selection {
        return Err(SelectionFault::SnapshotSelectionWidth {
            selected: selected.len(),
            observed: observed_selection,
        });
    }
    for (left_position, left) in present.iter().enumerate() {
        let left_id = present_id(left);
        for (right_position, right) in present.iter().enumerate().skip(left_position + 1) {
            if left_id == present_id(right) {
                return Err(SelectionFault::DuplicatePresentSegment {
                    left_position,
                    right_position,
                    id: left_id,
                });
            }
        }
        for (missing_position, missing_id) in missing.iter().enumerate() {
            if left_id == *missing_id {
                return Err(SelectionFault::PresentAndMissing {
                    present_position: left_position,
                    missing_position,
                    id: left_id,
                });
            }
        }
    }
    for (left_position, left) in missing.iter().enumerate() {
        for (right_position, right) in missing.iter().enumerate().skip(left_position + 1) {
            if *left == *right {
                return Err(SelectionFault::DuplicateMissingSegment {
                    left_position,
                    right_position,
                    id: *left,
                });
            }
        }
    }
    let mut preceding_selected_position = None;
    for (present_position, segment) in present.iter().enumerate() {
        let id = present_id(segment);
        let Some(selected_position) = selected.iter().position(|selected_id| *selected_id == id)
        else {
            return Err(SelectionFault::PresentNotSelected {
                present_position,
                id,
            });
        };
        if let Some(preceding) = preceding_selected_position
            && selected_position <= preceding
        {
            return Err(SelectionFault::PresentOrderMismatch {
                present_position,
                preceding_selected_position: preceding,
                selected_position,
                id,
            });
        }
        preceding_selected_position = Some(selected_position);
    }
    for (missing_position, id) in missing.iter().enumerate() {
        if !selected.contains(id) {
            return Err(SelectionFault::MissingNotSelected {
                missing_position,
                id: *id,
            });
        }
    }
    Ok(())
}
