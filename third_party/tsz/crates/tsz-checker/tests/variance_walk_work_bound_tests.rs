//! Regression: the variance walk of a generic interface used to run unmetered.
//!
//! `interface Box<T> { m<U>(arg: Ladder): void; value: T }` makes the walk for `T`
//! enter the generic method, whose own type parameter `U` is bound. Inside such a
//! signature the walk used to remember nothing it had finished, so a type reached
//! through two paths per level (a ladder of interfaces that each mention the
//! previous one twice) was re-entered through `2^depth` paths. Only a per-walk
//! iteration cap ended it, and nothing observed the project's checkpoint, so a
//! file's work slice and the package deadline never saw those hundreds of
//! thousands of visits. Real projects hit the same shape through generic methods of
//! large library classes (react-query's `QueryClient`, zod's `ZodType`).
//!
//! The walk now measures each shared subtree once per visit context, and every
//! guarded step observes the ambient execution checkpoint.

use std::sync::atomic::{AtomicU64, Ordering};
use tsz_binder::BinderState;
use tsz_checker::context::CheckerOptions;
use tsz_checker::state::CheckerState;
use tsz_common::{ExecutionCheckpoint, ProjectExecutionStop, enter_ambient_checkpoint};
use tsz_parser::parser::ParserState;
use tsz_solver::construction::{QueryCache, TypeInterner};

/// Charges are only counted; nothing ever stops.
struct Counting(AtomicU64);

impl ExecutionCheckpoint for Counting {
    fn checkpoint(&self, work_units: u64) -> Result<(), ProjectExecutionStop> {
        self.0.fetch_add(work_units, Ordering::Relaxed);
        Ok(())
    }
}

/// Stops once more than `limit` units were charged, like a file's work slice.
struct Slice {
    spent: AtomicU64,
    limit: u64,
}

impl ExecutionCheckpoint for Slice {
    fn checkpoint(&self, work_units: u64) -> Result<(), ProjectExecutionStop> {
        let spent = self.spent.fetch_add(work_units, Ordering::Relaxed) + work_units;
        if spent > self.limit {
            Err(ProjectExecutionStop::WorkBudgetExhausted)
        } else {
            Ok(())
        }
    }
}

/// `interface L0 { leaf: string }`, then `L<k+1>` mentions `L<k>` twice.
fn ladder_source(depth: usize) -> String {
    let mut source = String::from("interface L0 { leaf: string }\n");
    for level in 1..=depth {
        source.push_str(&format!(
            "interface L{level} {{ left: L{}; right: L{} }}\n",
            level - 1,
            level - 1
        ));
    }
    source.push_str(&format!(
        "interface Box<T> {{ m<U>(arg: L{depth}): void; value: T }}\n\
         declare const num: Box<number>;\n\
         const str: Box<string> = num;\n"
    ));
    source
}

/// Checks `source` with `checkpoint` observed both explicitly (through the query
/// cache) and ambiently, as the project checker installs it for a file.
fn check_under(source: &str, checkpoint: &dyn ExecutionCheckpoint) {
    let mut parser = ParserState::new("test.ts".to_string(), source.to_string());
    let root = parser.parse_source_file();
    let mut binder = BinderState::new();
    binder.bind_source_file(parser.get_arena(), root);
    let interner = TypeInterner::new();
    let queries = QueryCache::new(&interner).with_execution_checkpoint(checkpoint);
    let mut checker = CheckerState::new(
        parser.get_arena(),
        &binder,
        &queries,
        "test.ts".to_string(),
        CheckerOptions {
            strict: true,
            ..Default::default()
        },
    );
    checker.ctx.set_lib_contexts(Vec::new());
    let _ambient = enter_ambient_checkpoint(checkpoint);
    checker.check_source_file(root);
}

#[test]
fn a_shared_ladder_inside_a_generic_method_costs_work_linear_in_its_depth() {
    let counting = Counting(AtomicU64::new(0));
    check_under(&ladder_source(30), &counting);
    let charged = counting.0.load(Ordering::Relaxed);
    // Thirty levels measured once each plus the file's ordinary work stay far
    // below one walk's iteration cap (100k visits, 25k units at one unit per four
    // polls). Re-entering the ladder through its 2^30 paths cannot fit under it.
    assert!(
        charged < 5_000,
        "checking the ladder charged {charged} work units"
    );
}

#[test]
fn a_file_slice_that_covers_the_memoized_walk_lets_the_check_complete() {
    // A slice far below what an unmemoized walk costs (25k units for one capped walk),
    // but above what the memoized check needs: the check completes inside it.
    let slice = Slice {
        spent: AtomicU64::new(0),
        limit: 5_000,
    };
    check_under(&ladder_source(30), &slice);
    assert!(
        slice.spent.load(Ordering::Relaxed) <= 5_000,
        "the check needed more than its slice"
    );
}
