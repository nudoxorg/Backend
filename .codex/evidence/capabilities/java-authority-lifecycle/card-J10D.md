# Card J10-D — documentation-lane width and doc-fault context

registered role: nudox_luna_implementer
baseline: commit a6b1e8110 (branch codex/fidelity-java) — create your own detached
worktree from it; do not write anywhere else.
expected config: max effort; house style (`deliver-reviewed-rust-slice`) applies.

## Baseline facts (Terra-scouted 2026-09-03, backtrace-verified)

Terra's whole-artifact census on commons-lang3 3.14.0 (246 files, live javac,
Temurin 21.0.12.1) reduced to exactly 2 failures after cards A–C:
`ArrayUtils.java` and `StringUtils.java`, both
`JavaProjection { class: IndexCapacity, declaration: "" , owner: Absent }`.

Captured backtraces both land in `push_docs`
(`compiler/driver/lower/java.rs:1691/1702`, called from `push_executable`):

- The documentation lane streams one fragment per Javadoc line plus one
  `SoftBreak` per line plus `Code`/`Link` fragments per inline tag.
- Measured demand on the corpus: `StringUtils.java` ≈ 13,529 fragments
  (6,219 doc lines, 1,091 inline tags), `ArrayUtils.java` ≈ 13,046
  (5,821 lines, 1,404 tags). Every other file in the artifact fits.
- `compiler/driver/lower.rs:50`: `MAX_EMISSION_DOC_FRAGMENTS: usize = 4096`
  and the lane scratch is `vec![DocInput::SoftBreak; MAX_EMISSION_DOC_FRAGMENTS]
  .into_boxed_slice()` (boxed, so the raise costs heap, not stack).
- The overflow maps through closures that DISCARD `FactFault::DocCapacity`
  (types/lowering.rs:81) into `ProjectionFault::IndexCapacity` and then fold
  with the BARE `terminal`, losing the declaration context even though
  `declared` is in scope — the residual cause-erasure class from J10-B.

## Public terminal

1. `MAX_EMISSION_DOC_FRAGMENTS` becomes 16384 — the next dense bound above
   the measured 13,529-fragment maximum, with the measurement quoted in the
   constant's doc comment. Files at the old bound behave byte-identically.
2. Every doc-lane fold in `push_docs`/`push_doc_line`/`push_text` threads the
   failing declaration's name and owner atoms through `terminal_for`, exactly
   like the J10-B sites. No `FactFault` variant text changes; the class cell
   stays `IndexCapacity` (it is a lane-capacity fault).

## Owned paths (no overlapping writer; nothing else may change)

- `compiler/driver/lower.rs` — the constant + its doc comment only.
- `compiler/driver/lower/java.rs` — the doc-lane closures and any OTHER bare
  `terminal(ProjectionFault::IndexCapacity)` reachable from `push_docs`;
  lane tests for the new falsifiers.

Forbidden: every other lane; the vocabulary (the closed class set is
sufficient); wire layouts; `MAX_EMISSION_FACTS`; the doc fragment grammar
(text/soft-break/code/link) and its rendering contract.

## Proof matrix rows bound to this card

- P26 (new): "A declaration whose Javadoc demand exceeds the old 4096-fragment
  bound lowers completely." Falsifier (RED at baseline): a programmatically
  generated fixture image whose single method carries a Javadoc block with
  >4096 lines (generate the source text in the test; do not commit a huge
  fixture file) must lower to a validated fragment whose documentation lane
  decodes with the exact expected fragment count.
- P27 (new): "Doc-lane overflow names the declaration": a fixture whose doc
  demand exceeds the NEW bound (generate >16384 fragments) must fail with a
  rendered cause naming `IndexCapacity` plus the generated method's name and
  owner — not an empty declaration. (Keep this fixture generation bounded:
  the test generates the text, asserts the typed failure, and does not
  allocate unbounded scratch.)

## Environment

- `CARGO_TARGET_DIR=<your-worktree>/target-wd` (dedicated; never share).
- `NUDOX_JDK=/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/jdk/jdk-21.0.12.1+1/Contents/Home`
  for the lifecycle leg.

## Exact commands (all must be green before commit)

1. `cargo test -p compiler-driver --lib lower::java`
2. `cargo test -p compiler-driver --test java_lifecycle -- --test-threads=1`
3. `cargo test -p compiler-driver --test java_render --test java_image`
4. `cargo check -p compiler-driver --tests`
5. `cargo fmt -p compiler-driver && git diff --check`

Note: `cargo test -p compiler-driver --lib lower` (the shared lower suite)
currently retains a pre-existing trunk-owned red
(`lower::tests::bounded_fact_and_child_lanes_reject_overflow_and_admit_the_exact_bound`,
reproduced by Terra at the baseline before this card); it is outside your
custody. Do not fix it, do not touch `compiler/driver/lower/tests.rs`.

## Budgets and house laws

- Production delta <= 40 LOC; test delta <= 160 LOC.
- Behavior for files under the old bound stays byte-identical; if any golden
  or fixture bytes change, stop and report.
- No unwrap/expect/panic; no ignored tests.

## Checkpoint and return

Commit once, coherent, on a branch `j10d-doc-lane` in your worktree. Return
exactly: commit hash + branch; files changed with net LOC; P26/P27 outputs
(test names + pass lines + the recorded baseline-red line for P26); whether
any golden/fixture bytes changed; smallest remaining red or "none"; any
deviation with one-line justification.
