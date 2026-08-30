# P1 C1a12B4 test LOC compaction card

Base: `767fe96f` on the isolated C1a repair branch. C1a12B3's semantic assertions passed review,
but its root test is 414 added formatted lines and the combined new test/UI surface is 448. The
frozen rule stops at any delta more than 20% or 25 lines beyond forecast; root test therefore
must be at most 384 added lines (`320 * 1.20`) and combined test/UI at most 420. No re-budget is
authorized because this is a readable table/helper consolidation opportunity, not new evidence.

Edit only `crates/nudox-root/tests/canonical_root_view.rs`. Preserve every accepted B3 assertion
and every exact mutation operand. Compress repeated setup and assertions into small named
helpers/tables: canonical fixture/byte creation, outcome predicates, malformed matrix rows,
entry comparisons, and paired locality setup are expected candidates. Do not turn meaningful
distinct proofs into opaque N-derived counters, delete a card case, use broad lint allowances, or
change UI/production/lab code.

Terminal: the root test is <=384 added formatted LOC; with its unchanged 19+15 UI source files,
combined is <=418 (and thus <=420); `cargo fmt --all -- --check`, root all-target strict clippy,
and exact integration test pass. Fresh Terra must verify preservation and recount before C1a12C.
