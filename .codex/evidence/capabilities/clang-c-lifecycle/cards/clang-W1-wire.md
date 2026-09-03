# Card clang-W1-wire — migrate the clang lane's type-fact hand decoders to the schema-2 wire

- Registered role: `nudox_luna_implementer` (luna, effort max)
- Baseline: branch `luna/clang-lifecycle` @ `621b7d67c` (trunk consumed; 13/16 clang_lane green).
- Owned path: `compiler/driver/tests/clang_lane.rs` — NOTHING else. Zero production edits. If you
  conclude production code must change, STOP and report the exact need.

## The three failing tests (hand decoders predate the schema-2 wire)

- `anonymous_struct_names_its_field_child` (fails `Check("invalid name cell")`)
- `mutual_recursion_collapses_forwards_and_names_pointer_children`
- `recursive_pointer_rows_are_content_addressed_and_mutation_changes_shape`

They hand-decode the type-fact payload with `word`/`skip_name`/`child_target` helpers written for
the pre-merge layout. Trunk raised the geometry and moved type facts to the schema-2 segment
(`compiler/ir/type_facts.rs` is the authoritative encoder; trunk migrated its own falsifiers the
same way in commit `5f72f22bb` — see `compiler/driver/tests/rust_traits.rs` for the precedent
hand-decode shapes).

## Public terminal

All 16 `clang_lane` tests pass. The three migrated decoders must remain BYTE-PINNING hand
decoders — do not replace them with the crate's own `DecodedTypeFact` decoder and thereby stop
pinning exact wire bytes. Assert at least: exact row count, exact name-cell spellings, exact
payload cells for one fully-known row per test, and the unchanged mutation law (corrupting one
decoded byte in `recursive_pointer_rows...` still changes the decoded shape; the mutation test
must still exist and still bite).

## Constraints

- No new dependency, no unsafe, no production edits, no other files.
- Keep `#![forbid(unsafe_code)]` and the deny set at the top of the file.
- You may refactor the shared helpers if the schema-2 layout makes one typed record helper better
  than three per-test parsers; it must still hand-parse bytes, not call compiler-ir's decoder.

## Evidence (worktree `backend/.local/worktrees/clang-lifecycle`, `CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-driver --offline --test clang_lane` → 16/16, run twice.
2. `cargo fmt --check` on the owned file.
3. Report: commit sha, the wire layout you found (cell widths/names), the exact assertion list of
   the three tests, command tails, smallest remaining red.
