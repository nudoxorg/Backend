# Card clang-W2r2-drive — build-drive falsifiers 6-8 (owed completions)

- Registered role: `nudox_luna_implementer` (luna, effort max)
- Baseline: branch `luna/clang-lifecycle` @ `a1d30131e` plus the current working tree. Your
  predecessor landed findings 1-5 (commit `ad5fe6c9`); items 6-8 were punted and are OWED.
- Owned paths: `compiler/driver/build_drive.rs` (only if a test-driven gap forces it),
  `compiler/driver/tests/build_drive.rs`. `clang_lifecycle.rs` has another worker's uncommitted
  changes — leave it entirely alone.

## Terra-diagnosed fact that scopes item 6

Same-publisher generation chaining works, but REOPENING a chained journal fails today (trunk
replay defect — `server/workflow/durable.rs::replay_stream` lacks the chaining fallback that
`server/journal/journal.rs::append_group_using` has). Your item-6 composition must therefore
chain gen-1 → gen-2 through the SAME publisher and verify everything through the LIVE publisher
plus `ImmutableArtifactStore` — do NOT reopen a chained journal, and do not test around the
defect silently.

## Owed falsifiers

6. **Make e2e with publishing** (the mandate's repo → TU → IR packaging for Make): real repo with
   a `Makefile` (two `.c` TUs + one included header + one flag such as `--sysroot <dir>` or `-I`
   that only works verbatim) → `discover_and_drive` → compile every command through
   `compile_database_translation_unit` → `FragmentView::validate` → publish gen-1 (`publish_compiled`,
   `PublishControl::Continue`) → add a third TU + Makefile rule → drive again → publish gen-2
   through the SAME publisher → generation advanced + `pinned_root` changed → `open_published` on
   the live publisher → all fragments validate → index build/plan/encode/seal → gen-1 fragment
   from `ImmutableArtifactStore` hash-identical + validating. Assert the sysroot/`-I` flag reached
   libclang verbatim via a header only resolvable because of it.
7. **Drive-failure terminal**: a real Makefile whose dry-run fails (e.g. the rule invokes a
   missing command) → exact `DriveFailed { tool: "make", captured }` with non-empty retained
   capture.
8. **Cancellation observable**: cancelled flag set between detection and spawn → `Cancelled`; and
   one test where cancellation is set after the drive starts and the flag is observed between two
   child spawns (e.g. Meson setup → ninja step) or, for Make, before the spawn — assert no partial
   database was left (the scratch dir has no `compile_commands.json`).

## Constraints

Typed terminals only, no silent skips, no new dependency, no unsafe, no network, scratch dirs
under caller-provided paths, `#![forbid(unsafe_code)]` + deny set. If you must touch
`build_drive.rs`, keep the change minimal and name the test that forced it.

## Evidence (`CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-driver --offline --test build_drive` green three consecutive runs.
2. `cargo fmt` the two owned files. One commit: `test(clang): codify make-driven repo packaging through publication`.
   Report: commit sha, assertion list, command tails, smallest remaining red.
