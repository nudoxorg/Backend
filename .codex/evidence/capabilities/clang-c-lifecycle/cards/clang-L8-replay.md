# Card clang-L8-replay — heal chained-journal replay so a two-generation store reopens

- Registered role: `nudox_luna_implementer` (luna, effort max)
- Baseline: branch `codex/fidelity-clang` @ `74c3a41fb` (worktree /private/tmp/nudox-fidelity-clang).
- Owned paths (no overlapping writer; all other workers are stopped):
  - `server/workflow/reduce.rs`
  - `server/workflow/durable.rs`
  - `server/workflow/tests.rs`
  - `server/journal/journal.rs`
  - `server/journal/journal/tests.rs` (only if a journal-level falsifier is added)
  - `compiler/driver/tests/clang_lifecycle.rs` — ONLY the pinned test
    `reopening_a_chained_journal_is_red_until_trunk_heals_replay`
- FORBIDDEN: everything else. In particular `compiler/driver/tests/clang_lifecycle.rs`'s other
  tests, `compiler/**` product code, `heart/**`, `interface/**`.

## Defect (Terra-diagnosed; reproduce it first)

Live publication chaining was landed by `f9efccc58` (server/journal): for publication groups
`append_group_using(..., allow_chain = true)` reduces a `Requested` event whose `StageKey`
differs from the still-keyed previous generation by retrying once from `WorkflowState::New`
(see the inline `.or_else` in `server/journal/journal.rs::append_group_using`).

Replay never learned that rule: `server/workflow/durable.rs::replay_stream` applies strict
`reduce`, so the second generation's `Requested` frame replays as
`ReductionError::StageKeyMismatch`. Consequence: any store that legally published two chained
generations can never be reopened — `FileJournal::open`/`recover` maps the replay failure to
`JournalError::Reduction(_)`.

Reproduce before editing:
`CARGO_TARGET_DIR=$PWD/.local/target cargo test -p compiler-driver --offline --test clang_lifecycle reopening_a_chained 2>&1 | tail -5`
— it passes today only because it ASSERTS the broken behavior; its header comment declares the
flip to successful reopen as the intended signal.

## Public terminal

1. One shared chaining rule lives in the reducer, not in the journal: add
   `pub fn reduce_chained(state: WorkflowState, event: WorkflowEvent) -> Result<Reduction,
   ReductionError>` next to `reduce` in `server/workflow/reduce.rs`. Semantics mirror the live
   append fallback EXACTLY: strict `reduce` first; only when it fails with any error, the event
   kind is `EventKind::Requested`, and `state` is `WorkflowState::Keyed { .. }`, retry once from
   `WorkflowState::New`; if the retry also fails, return the ORIGINAL error (never the retry's —
   this matches `append_group_using`'s current `.or_else` mapping). `WorkflowState::New` input
   keeps strict behavior. Document the law in the doc comment: a `Requested` event on a still-keyed
   state opens the next generation chain.
2. `replay_stream` in `durable.rs` uses `reduce_chained` for every committed record and documents
   why unconditional chaining is the faithful inverse: publication groups were admitted under the
   chained rule, plain groups only under strict reduce, strict success implies chained success,
   and a stream that fails the chained reducer could never have been committed. Group boundaries
   are not recorded on the wire, so the reducer cannot distinguish them — one rule for committed
   history.
3. `server/journal/journal.rs::append_group_using` DELETES its inline `.or_else` copy and calls
   `reduce_chained` when `allow_chain`, strict `reduce` otherwise. One declaration of the rule;
   drift between live append and replay becomes unrepresentable.
4. Flip the pinned lifecycle test to
   `chained_journal_reopens_after_shutdown` (keep the same fixture: gen-1 `[main.c]`, gen-2
   `[util.c]`, different stage keys): `journal.shutdown()` then
   `DurablePublisher::reopen(...)` succeeds; `open_published` on the reopened publisher returns
   sequence 1 whose fragment validates; additionally capture gen-1's manifest facts before
   gen-2 (as `clang_database_whole_tu_generation_journey` does), reopen the gen-1 fragment from
   `ImmutableArtifactStore`, and assert `FragmentView::validate` accepts it — the old generation
   survives the chained reopen.

## Falsifiers (all must be red on a weakened implementation)

- F1: a workflow-level replay test (in `server/workflow/tests.rs`): reduce+record a full gen-1
  event group (Requested..Published, key K1) followed by a full gen-2 group (Requested..Published,
  key K2) into records; `replay_stream` over them must succeed and recover the gen-2 keyed state.
  Weakened impls: strict replay (fails, current trunk); replay that accepts ANY mismatched event
  (violates F2).
- F2: a replay test where a NON-`Requested` event (e.g. `Admitted` or `Published`) carries key K2
  while the state is keyed on K1: `replay_stream` must fail with
  `ReplayError::Reduction(ReductionError::StageKeyMismatch { expected: K1, observed: K2 })` — the
  fallback is scoped to `Requested`, never a general mismatch eraser.
- F3: a chained-retry-failure test: a `Requested` event with key K2 while keyed on K1 whose
  reduction from New is ILLEGAL is impossible for `Requested` from `New` (Requested is legal
  from New), so instead pin the error-preservation law at the journal level: with
  `append_group` (plain, `allow_chain = false`), a mismatched-key group still fails with the
  ORIGINAL `StageKeyMismatch` (expected/observed exact), proving the `else` branch did not
  start swallowing mismatches.
- F4: the flipped lifecycle test above reopens for real.
- F5: existing suites stay green: `cargo test -p server-workflow --offline`,
  `cargo test -p server-journal --offline`, and
  `cargo test -p compiler-driver --offline --test clang_lifecycle` (all 7 tests, with the flipped
  one green).

## Constraints

- No new dependency, no unsafe (`workspace lints deny unsafe_code`), no wire/format change: the
  chained rule is derived from existing committed bytes, so old journals replay identically.
  Do NOT add a group-boundary marker to frames (wire change — out of scope).
- Keep the reducer allocation-free: `reduce_chained` is a pure function like `reduce`.
- Deny set stays: no `unwrap`/`expect`/`panic` in shipped code; tests follow the existing style.
- `cargo fmt` on owned files.

## Evidence (`CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p server-workflow --offline` green, twice.
2. `cargo test -p server-journal --offline` green, twice.
3. `cargo test -p compiler-driver --offline --test clang_lifecycle` green (7 tests) twice.
4. One commit: `fix(workflow): replay chained journal generations with the publication chain rule`.
   Report: commit sha, F1–F5 command tails, the exact `reduce_chained` text, smallest remaining red.
