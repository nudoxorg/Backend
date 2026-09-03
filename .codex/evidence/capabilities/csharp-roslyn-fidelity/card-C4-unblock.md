# Card C4 — shared test-target unblock + lane-truth falsifier (registered role: nudox_luna_implementer)

## Baseline & owned paths
Baseline commit: c4b6ed32e. You own EXACTLY:
- compiler/driver/lower/rust.rs — ONLY its `#[cfg(test)] mod tests`
- compiler/driver/lower.rs — ONLY the minimum mechanical change(s) to its
  test-support types required to compile (e.g. a missing `#[derive]` on
  `AdmissionFault`), each with a one-line comment naming this card
- compiler/driver/lower/csharp.rs — ONLY an added `#[cfg(test)]` falsifier
No other path may be written. No semantic redesign anywhere.

## One public terminal
`cargo test -p compiler-driver --offline --lib` compiles and its
`lower::csharp` module passes, and the C# lane's wire-saturation truth is
pinned by a failing-if-changed falsifier.

## Task 1 — mechanical unblock of lower/rust.rs test module
At baseline this module does not compile (cold-build evidence):
- missing imports: `RustToolchain`, `RustProject`, `RustAuthorityError`
  (compiler_languages_rust), `RustEdition` (compiler_vocabulary),
  `SourceByteLimit` (compiler_languages_rust)
- `collect(...)` now takes 6 arguments — read the real call site in
  compiler/driver/types/compile.rs and adapt the fixture call exactly
  (one additional argument; derive its value from the existing fixture
  inputs, never invent behavior)
- `enum TestError` derives thiserror `Error` but `AdmissionFault` (in
  compiler/driver/lower.rs) does not implement `std::error::Error`: add the
  missing derive next to its existing `Debug` derive with the comment
  `// card C4: test-support boundary needs Error for the typed #[from]`
  and nothing else
- two lifetime elision errors in `rows(...)`/`occurrences(...)` helpers —
  apply the exact signatures rustc suggests
- any remaining compile errors in that module: fix mechanically to current
  signatures; if a fix would change semantics (not signatures), STOP and
  report it instead.
Then run the rust lane's tests; report failures but DO NOT fix semantic
assertion failures beyond compiling — report them as rows.

## Task 2 — C# wire-saturation falsifier
Add to compiler/driver/lower/csharp.rs test module one test
`wire_saturation_gaps_stay_image_retained_pending_trunk_cells`:
- build a fixture image whose type declaration carries one generic
  parameter with variance=1 (out) and one method with a `params` parameter
  (param flags 0x1) — the module's existing Fixture type already models
  these cells (variance byte, param flags byte)
- run collect + admit into a fragment; reopen it with the FragmentView /
  reopened-extension-pools APIs the module already uses
- assert EXACTLY: the reopened type parameter's variance cell is
  `Variance::Invariant` and no lane cell anywhere records the params
  modifier, with a doc comment citing
  `.codex/evidence/capabilities/csharp-roslyn-fidelity/escalation-wire-saturation.md`
  and compiler/driver/lower.rs lines 985/1005 plus
  compiler/ir/extension_pools.rs line 17 as the exact drop sites.
- the test's name and comment must state: when trunk lands the cells, this
  test MUST be flipped to assert the saturated values (Covariant; a params
  cell), never deleted.

## Gates
`CARGO_TARGET_DIR=/private/tmp/nudox-fidelity-csharp/.local/target-csharp
cargo test -p compiler-driver --offline --lib lower::csharp` green;
`--lib lower::rust` compiles (report its pass/fail rows);
`cargo test -p compiler-driver --offline --test csharp_image --test
native_compile` stays green.

## Bounds
No new dependencies, no unsafe, no formatting churn outside touched tests,
no public API changes. Mechanical only.

## Checkpoint & return
One commit, prefix `fix(driver-tests):`. Return: commit hash; exact
commands + tails; the rust-lane row list that remains semantically red
(if any); confirmation of the falsifier's exact assert lines.
