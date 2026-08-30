# P1 C1a12B5 final per-delta LOC compaction

Base: `0ac862ba` on the isolated repair branch. Final C1a closure review passes every executable,
source, scope, path, replay, and total-ceiling gate, but correctly applies the master card's
separate stop rule: the root integration test is 382 added formatted lines versus a 320 forecast,
a +62 delta exceeding both 20% and 25 lines. No re-budget is authorized.

Edit only `crates/nudox-root/tests/canonical_root_view.rs`. Compact the existing B3 evidence into
readable fixture and assertion tables/helpers until the new file is **at most 345 formatted
lines** (forecast +25). Preserve all final-review-verified semantic cases literally: input-relative
pointer bounds; canonical bytes/facts; global mutation collision priority; checked geometry;
0/1/100k shallow/deep; branch-tail/incoming-cycle; full public entry get/scan; different valid
locality stability/mismatch; and existing facts/sentinel proof. Do not delete a case, weaken an
assertion to a counter/checksum, change UI/production/lab/P2, or use broad lint allowance.

Run format, strict root all-target clippy, and root integration test. The final Terra recheck
recounts the entire candidate baseline-relative LOC before manager integration.
