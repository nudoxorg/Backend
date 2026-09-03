# Card J9Q — capacity rejections at 1024 geometry + render goldens at trunk vocabulary

## Registered role

`nudox_luna_implementer`. You implement one frozen proof card. You do not choose
product architecture, do not widen paths, do not change any proof-matrix row.

## Baseline and workspace

- Worktree: `/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/java-lane`,
  detached HEAD at `568c7f40` (verify `git rev-parse HEAD` before the first
  edit; if it differs, STOP and report).
- Build environment: `CARGO_TARGET_DIR=/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/java-gate-target`
  and `NUDOX_JDK=/private/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/jdk/jdk-21.0.12.1+1/Contents/Home`.
- The worktree carries quarantine modifications to FOREIGN files
  (`compiler/driver/lower/rust.rs`, `compiler/driver/lower/csharp.rs`,
  `Cargo.lock`) and untracked foreign scratch. NEVER stage, commit, revert, or
  reformat them.

## Owned paths (exclusive write custody)

1. `compiler/driver/lower/java.rs` — ONLY the two capacity tests inside its
   `#[cfg(test)] mod tests` (see below). No production edits in this file.
2. `compiler/driver/tests/java_render.rs` — the four failing goldens.

## Law 1 — capacity rejections retain exact operands (post-escalation)

The shared lane's `push_fact` no longer erases rejection causes. Java collect
surfaces `JavaCollectError::Rejected(FactRejection)` where `FactRejection`
carries `fact` (the ordinal the fact would have occupied), `name_len` (the
rejected declaration name's exact byte length), and `cause: FactFault`. The
geometry is now `MAX_EMISSION_FACTS = 1024`.

Fix these two tests in `compiler/driver/lower/java.rs` (test module only):

1. `capacity_beyond_the_lane_rejects_exactly` currently expects the erased
   `Lowering(NoSupportedDeclaration)` fold. It must now expect
   `Err(JavaCollectError::Rejected(rejection))` and assert the exact operands:
   `rejection.fact == crate::lower::MAX_EMISSION_FACTS`, `rejection.cause ==
   FactFault::Capacity`, and `rejection.name_len == 5` (the first overflowing
   row is named `p1024`, five bytes).
2. `two_hundred_fifty_seven_programmatic_fields_fold_through_typed_capacity`:
   257 fields now ADMIT (they fit the 1024-fact lane). Restructure into two
   honest assertions inside one test: (a) 257 programmatic fields lower
   successfully (`Ok`) — the old fold expectation was a geometry artifact;
   (b) pushing fields until the lane overflows (`MAX_EMISSION_FACTS + 1`
   declarations like case 1) yields the typed `Rejected(FactRejection)`
   capacity rejection with exact `fact`/`cause` operands. Name the test
   accordingly (e.g. `...admit_and_overflow_folds_typed`).

Read `crate::types::FactRejection` (compiler/driver/types/lowering.rs) and the
`FactFault` variants before writing assertions. Keep the no-unwrap/expect
discipline of the module.

## Law 2 — render goldens pin the trunk rendering vocabulary

Trunk commit 4d1cceba9 changed the shared rendering vocabulary: unknown
visibility renders prefix-free, int/None and typed signatures render through
live type rows, zero-arity tails render, and documentation facts render via
`display_docs`. `compiler/driver/tests/python_render.rs` (current HEAD) is the
reference vocabulary.

Re-derive these four tests in `compiler/driver/tests/java_render.rs`:

1. `java_declarations_render_exactly` — `/* visibility unknown */` prefixes are
   gone; `run` and fields may now carry full typed signatures from the
   fixture's FunctionPointer/primitive rows. Run the test, read the actual
   strings, verify each against source truth (the fixture builder in the same
   file), then pin exact expected strings. Resolve or update the FINDING
   comments: if trunk resolved a finding, replace it with a one-line note
   naming the resolving commit; if a finding still stands, keep it exact.
2. `java_kind_prefixes_and_overloads_are_exact` — same treatment for
   trait/enum prefixes and the two `overloaded` renders.
3. `java_type_rows_render_exactly` — fields now expose semantic types. Assert
   the exact `display_type` string for `input`, `label`, and `Outer.Inner`
   against their declared fixture types (no more `is_none`), plus the
   unchanged `primitive` assertion if it still holds.
4. `java_docs_and_embedding_render_exactly` — `display_docs` now renders the
   Javadoc (inline `{@link}` becomes a markdown link). Pin the exact rendered
   docs string and keep the embedding assertion consistent with the new docs
   plane.

NEVER pin an actual blindly: for every golden you change, state in a
one-line comment WHY the new string is correct (source truth + vocabulary
law), and verify the string against the fixture's image rows.

## Exact gates (all green at your checkpoint)

1. `cargo test -p compiler-driver --lib lower::java`
2. `cargo test -p compiler-driver --test java_render`
3. `cargo test -p compiler-driver --test java_image --test java_lifecycle`
4. `cargo check -p compiler-driver -p compiler-languages-java --tests`
   (foreign rust/csharp/typescript test-target drift is recorded quarantine,
   NOT yours; report growth, do not fix it.)
5. `rustfmt --edition 2024 --check` on your two owned files.

## Commit and return

One commit on top of `568c7f40`, message:
`test(java): typed capacity rejections and trunk-vocabulary render goldens`
Stage ONLY your two owned paths. Return: commit hash; one-line output per
gate; diff LOC; the smallest remaining red row; any stop decision hit.
