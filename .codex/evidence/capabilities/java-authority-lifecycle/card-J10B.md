# Card J10-B — java projection-fault cause retention

registered role: nudox_luna_implementer
baseline: commit ea0950773 (branch codex/fidelity-java) — create your own detached
worktree from it; do not write anywhere else.
expected config: max effort; house style (`deliver-reviewed-rust-slice`) applies.

## Baseline facts (Terra-scouted, 2026-09-03)

`compiler/driver/lower/java.rs` folds every projection fault through:

```rust
fn terminal(fault: ProjectionFault<'_>) -> JavaCollectError {
    let _ = fault;
    JavaCollectError::Lowering(LoweringUnsupported::NoSupportedDeclaration)
}
```

and identically in `lane_terminal(fault: FactFault)`. The `ProjectionFault`
variants retain operands (`OrphanOwner { owner: SymbolRef }`, `Utf16 { units,
utf16_len }`, `ForeignKey(ForeignKeyFault)`, `SiblingCapacity`,
`IndexCapacity`, `Malformed { kind }`, ...) purely as dead weight: the fold
erases the class, the failing declaration, and its owner atom. The public
terminal renders only "… has an unsupported LowerIr declaration recipe".

Terra measured the damage on the real corpus: 9 of 246 commons-lang3 3.14.0
files die with the erased terminal (8x `SiblingCapacity`, 1x `IndexCapacity`
— variant visible only through temporary Terra instrumentation, since
removed). The failing declaration and its owner are unrecoverable from the
public surface. That is a cause-erasing shortcut and the lane's law P17
("typed lowering failure naming purl + path + exact failure") is unmeetable
without it.

Established vocabulary precedent for operand-bearing causes:
`LoweringUnsupported::ExtensionAtomUnbound { row, provisional, atom_count }`
and the lane-specific `RustFunction`. `CompileFailure::LoweringUnsupported`
already carries `#[source] cause: LoweringUnsupported` through
`compiler/driver/types/compile.rs` (`java_terminal`), so enriching the cause
enriches the public terminal with zero terminal-arm changes.

## Public terminal

Every `JavaCollectError::Lowering` cause produced by the Java lane names:
1. the exact projection-fault class (closed set = today's `ProjectionFault`
   variants; `FactFault` classes stay on the admission `Rejected` arm and are
   already operand-bearing there — do not duplicate them),
2. the failing declaration's name atom text and, when the declaration has an
   owner, the owner atom text,
through `LoweringUnsupported`'s `Display` (and `Debug`). `NoSupportedDeclaration`
stays the vocabulary's generic form and remains used by other lanes; the Java
lane stops producing it for projection faults. The one legitimate remaining
producer in the Java lane is the shared seam's `facts.len() == 0` rejection in
`compiler/driver/types/compile.rs` (an honest empty-compilation terminal), and
the `DeclarationKind::Package => 0` unnamed-package path which must keep
folding to exactly today's behavior.

## Owned paths (no overlapping writer; nothing else may change)

- `compiler/vocabulary/lib.rs` — additive `LoweringUnsupported` variant(s)
  only; every other lane's variants untouched; no variant removed or renamed.
- `compiler/driver/lower/java.rs` — the two folds, the fault sites that gain
  operands, and the lane's own tests (the depth-limit test near line 3571
  currently matches `NoSupportedDeclaration`; re-pin it to the exact new
  variant).
- `compiler/driver/tests/java_lifecycle.rs`, `java_corpus.rs` — only if they
  match `LoweringUnsupported` variants of Java causes today (grep first; keep
  edits to exact re-pins with why-comments).

Forbidden: `ProjectionFault` variant deletion/renaming; the shared
`push_fact`/`FactRejection` admission path (Sol escalation already recorded
for it); `terminal.rs` terminal arms; other lanes.

## Design guidance (Luna may reshape within these laws)

- One variant with a closed class cell + bounded operand texts is preferred
  over eleven per-fault variants; if per-fault operand shapes genuinely
  differ, a small closed set is acceptable. Operands: fault class (typed),
  declaration name bytes, owner bytes. Bounded: the image already bounds atom
  lengths; do not add unbounded strings. `Display` must render class + name +
  owner without allocation tricks (thiserror formatting is the house tool).
- `OrphanOwner` must carry the owner symbol's atom text (the brief literally
  requires "the failing declaration's owner atom"); when the owner atom has no
  interned spelling, retain the exact absent fact honestly rather than a
  placeholder string.
- `push()`'s `u32::try_from(ordinal)` overflow arm (java.rs ~line 172) stays
  out of scope: it is structurally unreachable under MAX_EMISSION_FACTS and
  today's comment says so; leave one line of evidence in the report.

## Proof matrix rows bound to this card

- P22 (new): "Every Java lowering failure names its exact projection-fault
  class and the failing declaration's owner atom." Weakened implementation =
  the baseline `let _ = fault` fold. Falsifier (must be RED at baseline):
  hand-built fixture images in the `lower::java` test module that trigger
  `OrphanOwner` (executable whose owner name is not interned) and
  `SiblingCapacity` (17 same-key executables) compile through the lane and
  assert the failure's rendered cause names the class, the declaration name,
  and the owner text exactly. A generic `NoSupportedDeclaration` assertion
  fails the test — that is the mutant kill.
- P23 (new): "The whole-artifact failure surface is diagnosable": rerunning
  Terra's whole-artifact measurement (the Luna worker does NOT fetch the
  corpus for this; Terra owns it) must yield, for every failing file, a
  public error whose cause names the fault class. This row is marked PROVED
  BY WORKER only via the P22 falsifiers; Terra reproduces the corpus leg.

## Environment

- `CARGO_TARGET_DIR=<your-worktree>/target-wb` (dedicated; never share).
- `NUDOX_JDK=/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/jdk/jdk-21.0.12.1+1/Contents/Home`
  (only needed if you run the lifecycle suite; its T2/T3 legs fetch live from
  the canonical Central host).

## Exact commands (all must be green before commit)

1. `cargo test -p compiler-driver --lib lower::java`
2. `cargo test -p compiler-vocabulary`
3. `cargo check -p compiler-driver --tests`
4. `cargo test -p compiler-driver --test java_lifecycle -- --test-threads=1`
5. `cargo fmt -p compiler-vocabulary -p compiler-driver && git diff --check`

## Budgets and house laws

- Production delta <= 120 LOC across vocabulary + java.rs + compile.rs;
  test delta <= 260 LOC. Honest variance in the report if exceeded.
- No `unwrap`/`expect`/panics; no `map_err(|_| …)`; no stringly fault text.
- The closed `LoweringUnsupported` enum stays closed: no `#[non_exhaustive]`,
  no catch-all variant with a string payload.

## Checkpoint and return

Commit once, coherent, on a branch `j10b-cause-retention` in your worktree.
Return exactly:
- commit hash + branch,
- files changed with net LOC,
- the P22 falsifier outputs (exact assertion text + pass lines),
- smallest remaining red (or "none"),
- any deviation from this card with one-line justification.
