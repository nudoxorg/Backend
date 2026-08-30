# P1 C1a14 compile-fail diagnostic split and test-budget exception

Base: `0d37b9bf` on the isolated repair branch. A safe external compiler probe established that
private-field literal diagnostics (`E0451`) are suppressed when the same UI file also has the
three `DerefMut` reassignment errors. The master card requires evidence of both facts; one fixture
cannot show them together. This is a review-discovered compiler-diagnostic limitation, not a
production/API change.

This card supersedes the master card's two-C1a-UI-case detail only. It permits exactly:
`forged_validated_root.rs/.stderr` as privacy-only root/view literal forging, plus new
`immutable_validated_facts.rs/.stderr` for direct reassignment through mutable root, locality,
and borrowed-view witnesses. The existing `escaped_borrowed_root.rs/.stderr` remains the lifetime
case. The glob harness is unchanged. All expressions are safe; literal values use a diverging
`impossible<T>() -> T { std::process::abort() }` placeholder.

Budget exception: root behavioral test is currently <=345 (within its original +25 stop margin),
escape fixture adds 15 lines, privacy fixture must remain <=33, and immutable-facts fixture is
budgeted at <=15 after `rustfmt`. Thus maximum added Rust test source is 408, under the unchanged
420 ceiling. The final formatted fixture is 15 lines; this corrects the pre-format 14-line estimate
without expanding the permitted diagnostic scope.
This explicit exception replaces the earlier “exactly two fixtures” count; no other test or
production budget changes are authorized. Run workspace formatting, strict root clippy, and the
full trybuild compile-fail harness; commit only the permitted UI source/stderr files. Fresh Terra
closure review must verify separate E0451 and E0594 diagnostics and final baseline-relative LOC.
