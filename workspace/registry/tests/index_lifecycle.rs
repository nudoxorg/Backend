//! Pipeline part: **index lifecycle** (`registry::index`).
//!
//! TDD specs for a package's resolution state machine
//! (`Unindexed → Progressing(Phase) → Stored`) and the generic `Progressive`
//! progress reporting it drives.

/// A freshly tracked package starts `Unindexed`.
///
/// Assert: a new index entry is `ResolutionState::Unindexed { needed: false }`.
#[test]
fn new_package_starts_unindexed() {
    todo!("assert the initial Unindexed state");
}

/// `needed` flips when a related package was indexed but this one was skipped.
///
/// Assert: indexing a dependent leaves this package `Unindexed { needed: true }`.
#[test]
fn unindexed_marks_needed_when_a_relation_was_indexed() {
    todo!("assert `needed` flips for skipped-but-required packages");
}

/// Indexing advances through phases to `Stored`.
///
/// Assert: state moves `Unindexed → Progressing(Phase::Compiling) →
///   Progressing(Phase::Treesat) → Stored` as the compiler/treesitter run.
#[test]
fn indexing_advances_through_phases_to_stored() {
    todo!("assert the phase progression to Stored");
}

/// `Progressive` reports monotonic progress and a terminal completion.
///
/// Assert: `get_progress()` is non-decreasing across updates and `is_complete()`
///   becomes true at `Stored` (>= 100), firing `on_complete`.
#[test]
fn progress_is_monotonic_and_completes() {
    todo!("assert Progressive monotonicity + on_complete");
}

/// Re-indexing mutates state in place rather than minting a new "outcome".
///
/// Assert: requesting a re-index of a `Stored` package transitions it back to
///   `Progressing` on the same record (no separate outcome object).
#[test]
fn reindex_mutates_state_in_place() {
    todo!("assert re-index mutates the existing state");
}
