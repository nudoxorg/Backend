# Card J10-C — pooled reference-list width raise 16 -> 64

registered role: nudox_luna_implementer
baseline: commit d97b8359f (branch codex/fidelity-java) — create your own detached
worktree from it; do not write anywhere else.
expected config: max effort; house style (`deliver-reviewed-rust-slice`) applies.

## Baseline facts (Terra-scouted, 2026-09-03)

`compiler/driver/lower.rs` line 58: `pub(super) const MAX_REF_LIST_ELEMENTS: usize = 16;`
defines (a) the shared FactSet pooled-list scratch geometry
(`Box<[[u32; MAX_REF_LIST_ELEMENTS]]; MAX_REF_LISTS>` for atom/type/entity
lists, `MAX_REF_LISTS = 512`) and (b) the guard in `intern_ref_list`
(`FactFault::RefListElements`). It is an emission-side scratch bound only:
`compiler/ir`'s `ListInterner::intern` accepts any slice length (bounded by
u32 wire coordinates) and `validate_entity_list` is length-agnostic — the
wire grammar does NOT cap list width.

Real-corpus evidence that 16 blocks lawful input: the commons-lang3 3.14.0
whole-artifact measurement (246 source files, live javac, Terra probe) fails
8 files with `ProjectionFault::SiblingCapacity` because their same-name
overload groups exceed 16 prior executables. Measured per-file maximum
same-(owner,name) executable group sizes (decoded from live javac images):
ToStringBuilder.append = 46, StringUtils = 27, CompareToBuilder = 20,
StrBuilder.asString-ish group = 22, several 17-19. 46 is the observed
maximum across the whole artifact.

Trunk precedent: lane-owned commits raise shared lower.rs geometry constants
when a real corpus demands it (python 19e2196d4 raised MAX_FACT_CHILDREN and
MAX_TYPE_CHILDREN 8 -> 16; a later card raised them to 32; the trunk raised
the fact lane to 1024).

## Public terminal

`MAX_REF_LIST_ELEMENTS` becomes 64. Every lane may emit pooled reference
lists of up to 64 elements; the wire, validators, and reopen paths already
accept them. Behavior for lists of <= 16 elements is byte-identical (same
interning, same pool rows, same ordinals — only the row stride grows).

## Owned paths (no overlapping writer; nothing else may change)

- `compiler/driver/lower.rs` — the constant, its doc comment (state the
  measured 46 maximum and the 64 bound rationale in one line each), and the
  pool geometry that derives from it.
- Any lane test that pins the 16 boundary (grep first:
  `rg -n "MAX_REF_LIST_ELEMENTS|RefListElements|17" compiler/driver/lower/*.rs compiler/driver/tests/*.rs compiler/driver/tests/*/*.rs`).
  Known: `compiler/driver/lower/java.rs`'s sibling-capacity falsifier scales
  with the constant (`MAX_REF_LIST_ELEMENTS + 1`) and must stay green
  unchanged; go's `entity_list` pre-check and csharp's attribute pre-check
  use the constant and any of their tests pinning a 17-element rejection
  move to the exact new boundary with a why-comment quoting the measured
  corpus maximum. No production line outside lower.rs may change.

Forbidden: `MAX_REF_LISTS` (512), `MAX_EMISSION_FACTS` (1024), every wire
layout, `compiler/ir`, doclet, and vocabulary files.

## Proof matrix rows bound to this card

- P24 (new): "Overload groups up to the measured real-artifact maximum (46)
  lower completely." Weakened implementation: the 16 bound. Falsifier: the
  java sibling falsifier at the new boundary stays green, AND a new java
  lane falsifier builds 46 same-key executables + 1 and lowers them to a
  validated fragment whose sibling list decodes with 46 ordinals in order.
- P25 (new): "The pooled scratch bound stays honest": the pooled-list
  capacity law (lists-per-lane, not width) still folds typed when
  MAX_REF_LISTS is exhausted — existing capacity falsifiers must stay green.

## Environment

- `CARGO_TARGET_DIR=<your-worktree>/target-wc` (dedicated; never share).
- `NUDOX_JDK=/var/folders/vf/qpw72bpn65g0y01bnbwf90n80000gn/T/opencode/jdk/jdk-21.0.12.1+1/Contents/Home`
  for the java/live legs.

## Exact commands (all must be green before commit)

1. `cargo test -p compiler-driver --lib lower::java`
2. `cargo test -p compiler-driver --lib lower` (every lane's in-file tests)
3. `cargo test -p compiler-languages-java`
4. `cargo test -p compiler-driver --test java_lifecycle -- --test-threads=1`
5. `cargo check -p compiler-driver --tests`
6. `cargo fmt -p compiler-driver && git diff --check`

## Budgets and house laws

- Production delta <= 12 LOC; test delta <= 80 LOC.
- No behavior change for lists <= 16 elements: if any golden or fixture bytes
  change, stop and report — that would falsify the "byte-identical below the
  old bound" law.

## Checkpoint and return

Commit once, coherent, on a branch `j10c-list-width` in your worktree.
Return exactly:
- commit hash + branch,
- files changed with net LOC,
- the P24/P25 falsifier outputs (test names + pass lines),
- whether any golden/fixture bytes changed (must be "no" or a full
  explanation),
- smallest remaining red (or "none"),
- any deviation with one-line justification.
