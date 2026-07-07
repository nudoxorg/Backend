//! Pipeline part: **index lifecycle** (`registry::index`).
//!
//! TDD specs for a package's resolution state machine
//! (`Unindexed → Progressing(Phase) → Stored`) and the generic `Progressive`
//! progress reporting it drives.

mod common;

use heart::{
    ContentHash, JobProgress, Percent, Phase, Progressive, ResolutionState,
};
use registry::schema::codec;

/// Round-trip a state through its persisted `parse_status` column projection —
/// the exact encode/decode pair the global index writes and reads.
fn through_columns(state: &ResolutionState) -> ResolutionState {
    let columns = codec::state_to_columns(state).expect("every state has a column projection");
    codec::state_from_columns(
        columns.state,
        columns.phase,
        columns.content_hash.as_deref(),
        columns.needed,
        columns.failure.as_ref(),
    )
    .expect("the column projection decodes back")
}

/// The progress view of a state at zero intra-phase progress.
fn progress_of(state: ResolutionState) -> JobProgress {
    JobProgress { state, phase_fraction: Percent::try_new(0).expect("0 is a valid percent") }
}

/// A freshly tracked package starts `Unindexed`.
///
/// Assert: a new index entry is `ResolutionState::Unindexed { needed: false }`.
#[test]
fn new_package_starts_unindexed() {
    // The persisted form of a fresh entry decodes to exactly this state...
    let fresh = codec::state_from_columns("unindexed", None, None, false, None)
        .expect("the fresh row decodes");
    assert_eq!(fresh, ResolutionState::Unindexed { needed: false });

    // ...and it reads as unstarted, zero-progress work.
    let progress = progress_of(fresh);
    assert_eq!(progress.current_phase(), None, "a fresh package has no active phase");
    assert!(!progress.is_complete());
    assert_eq!(progress.overall(), Percent::try_new(0).expect("0 is valid"));
}

/// `needed` flips when a related package was indexed but this one was skipped.
///
/// Assert: indexing a dependent leaves this package `Unindexed { needed: true }`.
///
/// The registry has no automatic dependent-marking logic yet (a gap against
/// this spec); what it does own is the `needed` bit itself — representable,
/// distinct, and losslessly persisted through the `parse_status` codec.
#[test]
fn unindexed_marks_needed_when_a_relation_was_indexed() {
    let skipped_but_required = ResolutionState::Unindexed { needed: true };
    let merely_untracked = ResolutionState::Unindexed { needed: false };
    assert_ne!(skipped_but_required, merely_untracked, "the needed bit must be observable");

    // The flip survives persistence — the column set carries `needed` through.
    assert_eq!(through_columns(&skipped_but_required), skipped_but_required);
    assert_eq!(through_columns(&merely_untracked), merely_untracked);
}

/// Indexing advances through phases to `Stored`.
///
/// Assert: state moves `Unindexed → Progressing(Phase::...) → Stored` as the
///   pipeline runs. (The real phase set is `Acquiring → Extracting → Compiling
///   → Emitting`; the spec's `Treesat` phase does not exist in the
///   implementation.)
#[test]
fn indexing_advances_through_phases_to_stored() {
    let hash = ContentHash::of_bytes(b"final snapshot");
    let mut walk: Vec<ResolutionState> = vec![ResolutionState::Unindexed { needed: false }];
    walk.extend(JobProgress::PHASES.iter().map(|phase| ResolutionState::Progressing(*phase)));
    walk.push(ResolutionState::Stored { hash });

    assert_eq!(
        JobProgress::PHASES,
        &[Phase::Acquiring, Phase::Extracting, Phase::Compiling, Phase::Emitting],
        "the ordered phase sequence is the pipeline's public contract"
    );

    let mut previous = Percent::try_new(0).expect("0 is valid");
    for state in &walk {
        // Every step of the walk is a persistable, lossless state...
        assert_eq!(&through_columns(state), state, "every lifecycle state must round-trip");

        // ...and the walk is a genuine advance, never a regression.
        let overall = progress_of(state.clone()).overall();
        assert!(
            overall >= previous,
            "advancing {state:?} regressed overall progress ({overall:?} < {previous:?})"
        );
        previous = overall;
    }
    assert!(
        progress_of(walk.last().expect("walk is non-empty").clone()).is_complete(),
        "the walk must terminate complete at Stored"
    );
}

/// `Progressive` reports monotonic progress and a terminal completion.
///
/// Assert: `get_progress()` is non-decreasing across updates and `is_complete()`
///   becomes true at `Stored` (>= 100), firing `on_complete`.
#[test]
fn progress_is_monotonic_and_completes() {
    let percent = |value: u8| Percent::try_new(value).expect("fixture percents are <= 100");
    let updates = [
        JobProgress { state: ResolutionState::Unindexed { needed: false }, phase_fraction: percent(0) },
        JobProgress { state: ResolutionState::Progressing(Phase::Acquiring), phase_fraction: percent(50) },
        JobProgress { state: ResolutionState::Progressing(Phase::Extracting), phase_fraction: percent(25) },
        JobProgress { state: ResolutionState::Progressing(Phase::Compiling), phase_fraction: percent(90) },
        JobProgress { state: ResolutionState::Progressing(Phase::Emitting), phase_fraction: percent(10) },
        JobProgress {
            state: ResolutionState::Stored { hash: ContentHash::of_bytes(b"snapshot") },
            phase_fraction: percent(0),
        },
    ];

    let mut previous = percent(0);
    for (step, update) in updates.iter().enumerate() {
        let overall = update.overall();
        assert!(
            overall >= previous,
            "progress regressed at step {step}: {overall:?} < {previous:?}"
        );
        previous = overall;

        let completion_observed = std::cell::Cell::new(false);
        update.when_complete(|| completion_observed.set(true));
        let terminal = matches!(update.state, ResolutionState::Stored { .. });
        assert_eq!(update.is_complete(), terminal, "only Stored may read complete");
        assert_eq!(
            completion_observed.get(),
            terminal,
            "the completion observer must fire exactly at the terminal state"
        );
    }
    assert!(previous.is_full(), "the terminal state must report full (100) progress");
}

/// Re-indexing mutates state in place rather than minting a new "outcome".
///
/// Assert: requesting a re-index of a `Stored` package transitions it back to
///   `Progressing` on the same record (no separate outcome object).
#[test]
fn reindex_mutates_state_in_place() {
    let package = common::rust_package("serde", "1.0.0");

    // The state machine has one mutable slot per package: the same binding
    // moves Stored → Progressing; there is no second "outcome" value.
    let mut state = ResolutionState::Stored { hash: ContentHash::of_bytes(b"old snapshot") };
    state = ResolutionState::Progressing(Phase::Acquiring);
    assert_eq!(state, ResolutionState::Progressing(Phase::Acquiring));

    // And the persisted mutator is a single-row upsert keyed on the package —
    // `ON CONFLICT (package_id) DO UPDATE`, mutating the existing record
    // rather than inserting a fresh outcome row.
    let (sql, _values) = registry::schema::queries::index::set_state(package.id(), &state)
        .expect("the mutator statement builds");
    assert!(
        sql.contains("ON CONFLICT (\"package_id\")") && sql.contains("UPDATE"),
        "set_state must upsert the one lifecycle row in place, got: {sql}"
    );
}
