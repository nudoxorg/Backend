# Card clang-B-ir4 — render C record bodies from the Ir's member structure, with committed goldens

- Registered role: `nudox_luna_implementer` (`luna` subagent, effort max)
- Baseline: branch `luna/clang-lifecycle` @ `895deb147` or later. build_ir now populates
  members/parents for clang fragments (be31d5ef); `compiler/ir/render.rs` is the renderer.

## Owned paths

`compiler/ir/render.rs`, `compiler/ir/tests/`, and (only if a public export is genuinely
missing) `compiler/ir/lib.rs` re-export lines. Cargo.toml read-only.

## Forbidden surface

`compiler/driver/**`, `compiler/languages/**`, other lanes' files. Explicit paths only.

## Public terminal

1. When a rendered record item carries members (the Ir's member list is non-empty), the render
   output includes the C-honest body: `struct Name {` newline, one line per member with its
   declared name and its rendered type spelling, closing `};` — driven ENTIRELY by the Ir's
   type DAG and member lists (no source access; the renderer never sees bytes).
2. Type spellings through the DAG render C-honestly: `const struct Node *next` (const pointer
   to record), `int (*callback)(int, void *)`-shaped function-pointer members, array members
   with their measured lengths, and nested records by name. The existing type-display paths are
   extended, not duplicated.
3. Backwards compatibility: a record with NO members renders exactly as before — the existing
   renderer tests and every other lane's render goldens must not move one byte (members are
   only populated by lanes that populate them today: none besides clang's new capability).
4. Golden tests: committed golden files under `compiler/ir/tests/` covering at minimum:
   empty record, two-field record (int, const-pointer-to-self), array member, function-pointer
   member, nested record reference, enum with enumerators. Goldens are byte-exact with a
   provenance comment naming the constructing Ir.

## Constraints

- The renderer receives one already-built Ir; no new allocation owners beyond the existing
  display buffers; no new dependency; no unsafe; no macro.
- Formatting suppression and statement packing stay forbidden; goldens are exempt as visual
  evidence (their whitespace IS the contract).
- If the existing `write_object`/`ObjectMember` machinery already renders bodies for another
  shape, extend the SAME path rather than adding a parallel one.

## Evidence (inside the worktree, `CARGO_TARGET_DIR=$PWD/.local/target`)

1. `cargo test -p compiler-ir --offline` → all pass including the new golden tests.
2. `cargo test -p compiler-driver --offline --test clang_lane` → 16/16 (unchanged).
3. `cargo fmt --check` on owned paths; zero new warnings. (Ignore the pre-existing
   `compiler/ir/lib.rs` import-order drift if still present; report it.)

## Checkpoint

One commit, explicit paths, message `feat(compiler-ir): render C record bodies from member
structure`. Report: commit sha, the golden list with one-line descriptions, command tails.

## Plan closure

Next decision after return: the lifecycle packet (PURL grammar, build-system discovery,
publish→reopen→index), then the real-world corpus.
