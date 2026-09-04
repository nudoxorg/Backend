# Card clang-C1d-journey — the generation journey test (pure composition, no production changes)

- Registered role: `nudox_luna_implementer` (`luna` subagent, effort max)
- Baseline: branch `luna/clang-lifecycle` @ `4921f60f` (real database TUs parse; the additive
  entry, cancellation, absence, and capacity terminals are green).
- Owned paths: `compiler/driver/tests/clang_lifecycle.rs` and, if a shared helper must move,
  `compiler/driver/tests/clang_lane.rs`. NOTHING ELSE — this card changes zero production code.
  If you conclude production code must change, STOP and report the exact need.

## Public terminal

The C1b journey, now actually proved end-to-end in `clang_lifecycle.rs`, mirroring the
composition pattern of `compiler/driver/tests/python_purl_lifecycle.rs` (publish_compiled →
DurablePublisher::reopen → open_published → plan/encode/seal_compilation_index):

1. Write a real on-disk directory: `base.h`, `src/main.c` (includes `base.h` via the `-I`
   entry), `src/util.c`, and a `compile_commands.json` whose entries use relative paths and a
   `-I include` flag plus one `--sysroot <dir>` cell echoed verbatim into the parse (assert the
   sysroot survives into the authority's parse arguments through the observable facts — e.g., a
   header only resolvable under the sysroot dir).
2. Discover TUs from the database, compile each through the additive public entry.
3. Publish generation 1 (both TUs). Record the generation.
4. Reopen the journal (DurablePublisher::reopen). Add `src/extra.c` + its database entry.
   Compile and publish generation 2 (three TUs) THROUGH THE REOPENED JOURNAL with
   `PublishControl::Continue` — exactly the python precedent at
   python_purl_lifecycle.rs:404-432 (a fresh publisher raises the typed root/dep-set
   `Conflict`; the reopened journal with Continue is the sanctioned second-generation path).
   Then shutdown, reopen again, and open the newest publication.
5. Reopen the durable journal, open the published store: BOTH generations' fragments validate
   (`FragmentView::validate`), and the index pack plans, encodes, and seals for both
   generations.
6. Falsifier: mutating one committed fragment's semantic bytes after generation 1 must make
   the reopened validation fail with the exact typed fault (the store really validates, it
   does not rubber-stamp).

## Constraints

- Zero production edits. Test code only. The test keeps the bounded-retry cleanup pattern and
  must be deterministic across repeated runs (run it three times locally before reporting).
- Exact typed assertions throughout: no `unwrap` into generic failures; every expected error
  names its variant.

## Evidence (inside the worktree, `CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-driver --offline --test clang_lifecycle` → all pass, run three times.
2. `cargo test -p compiler-driver --offline --test clang_lane` → 16/16 unchanged.
3. `cargo fmt --check` on the owned test files.

## Checkpoint

One commit, explicit paths, message `test(clang): prove the whole-TU generation journey through
publish, reopen, and index`. Report: commit sha, the journey assertion list, command tails.

## Plan closure

Next decisions: the C2 build-system drive adapters (CMake/Make/Meson/BUCK), then the
real-world corpus packet (R11).
