# Fresh Terra reviewer calibration — raw return

Task identity: `/root/p5_c0_ir_manager_fresh/p5_c0_ir_reviewer_calibration`
Requested model: `gpt-5.6-terra`
Fork mode: `none`
Role: separate read-only reviewer calibration

Findings — calibration-only; no implementation artifacts were inspected.

Severity: BLOCKER
Location: `P5_C0_MANAGER_CARD.md:47-48`; planned `domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs`
Evidence: A shadow/local type fixture could compile while proving nothing about the exported `nudox_ir_vocab` crate.
Violated law: The fixture must import the actual public artifact; local/shadow types are a hard stop.
Consequence: Static kind impossibility is unproved.
Smallest correction: Reject the fixture; retain only an actual-public-artifact compiler-process proof.
Falsifier: The fixture imports the resolved public rlib, reports exactly one `E0308` naming both IDs, and the legal `TypeId` mutant defeats that diagnostic predicate.

Severity: BLOCKER
Location: `P5_C0_MANAGER_CARD.md:47, 50-53, 78-83`; planned `domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs`
Evidence: A doctest-only, runtime-only, or noncausal compiler invocation could look green without proving the forbidden expression caused rejection.
Violated law: The public consumer must be an ordinary integration test; compiler failure remains causal and raw stderr is retained.
Consequence: The negative API claim is substituted with weaker evidence.
Smallest correction: Reject the proof and use the single-error compiler-process fixture plus retained stderr and legal-mutant result.
Falsifier: Removing/replacing the exact mismatch with legal `TypeId` causes the expected-diagnostic predicate to fail.

Severity: BLOCKER
Location: `P5_C0_MANAGER_CARD.md:38-42, 48`; planned `domains/ir/crates/nudox-ir-vocab/src/lib.rs`
Evidence: Any `From`/`TryFrom`, raw-byte constructor, public rebrand, or authority conversion is expressly forbidden.
Violated law: `DenseId<Owner>::new(u32)` is the sole local construction and is not an identity rebrand or raw decode.
Consequence: Private-brand separation can be bypassed through a public conversion surface.
Smallest correction: Delete the conversion/rebrand; preserve the existing private branded representation.
Falsifier: A downstream consumer cannot construct or convert `EntityId` into `TypeId`; the actual-crate mismatch fixture still yields the specified `E0308`.

Severity: BLOCKER
Location: `P5_C0_MANAGER_CARD.md:50-52`; planned `domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs`
Evidence: `panic!`, `unwrap`, `expect`, `unreachable!`, source-dropping conversion, or discarded cleanup is prohibited in the terminal fixture.
Violated law: Compiler-process failure must remain causal, not be converted into green success.
Consequence: A harness failure can masquerade as successful negative evidence.
Smallest correction: Reject the terminal and return structured causal failure handling without panic terminals.
Falsifier: Deliberately breaking the compiler invocation makes the test fail causally rather than pass.

Severity: BLOCKER
Location: `P5_C0_MANAGER_CARD.md:7-10, 15-17, 27-29, 48`; any compiler path
Evidence: Compiler edits are outside both the terminal and the two exact writable paths.
Violated law: C0-COMPILER is separately calibrated; compiler/API edits are a hard stop.
Consequence: The builder broadens C0-IR and invalidates the frozen calibration.
Smallest correction: Reject the edit and restore/re-card before any compiler work.
Falsifier: Changed-path inventory contains only the two authorized IR paths; any compiler path hit stops the checkpoint.

Severity: BLOCKER
Location: `P5_C0_MANAGER_CARD.md:59-67`; planned `domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs`
Evidence: The test forecast is 53 added formatted LOC, hard-capped at 75 with 22 lines required to remain unused; consuming reserve is an explicit stop-and-re-card trigger.
Violated law: Reserve is unused capacity, not spendable budget.
Consequence: The skeleton no longer fits the approved capability boundary.
Smallest correction: Stop, delete/split excess work, and issue a new card/calibration if the terminal truly requires more scope.
Falsifier: Formatted test delta remains 53 and the resulting test file remains 67 lines, preserving all 22 reserve lines.

```text
tripwire | count | exact locations | disposition | evidence or finding ID
panic/unwrap/expect/unreachable | UNVERIFIED / UNVERIFIED | domains/ir/crates/nudox-ir-vocab/src/lib.rs; domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs (candidate lines intentionally not inspected) | Not cleared; prohibited for the terminal | Card:50-52; finding: panic-terminal
source-dropping conversion or map_err | UNVERIFIED / UNVERIFIED | domains/ir/crates/nudox-ir-vocab/src/lib.rs; domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs (candidate lines intentionally not inspected) | Not cleared; prohibited in test and conversion surface | Card:40-42,50-52; finding: raw-conversion
lossy/ambiguous From/TryFrom or raw authority bypass | UNVERIFIED / UNVERIFIED | domains/ir/crates/nudox-ir-vocab/src/lib.rs; domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs (candidate lines intentionally not inspected) | Not cleared; forbidden | Card:38-42; finding: raw-conversion
checked-arithmetic sentinel/saturation or operand loss | UNVERIFIED / UNVERIFIED | domains/ir/crates/nudox-ir-vocab/src/lib.rs; domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs (candidate lines intentionally not inspected) | Not cleared; no candidate inventory supplied | Card-only calibration; source inspection excluded
dyn/Box/Vec/Arc/Rc | UNVERIFIED / UNVERIFIED | domains/ir/crates/nudox-ir-vocab/src/lib.rs; domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs (candidate lines intentionally not inspected) | Not cleared; forbidden | Card:40-42
public tuple fields or positional semantic tuples | UNVERIFIED / UNVERIFIED | domains/ir/crates/nudox-ir-vocab/src/lib.rs; domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs (candidate lines intentionally not inspected) | Not cleared; public tuple fields forbidden | Card:40-42
unit/stateless namespace structs | UNVERIFIED / UNVERIFIED | domains/ir/crates/nudox-ir-vocab/src/lib.rs; domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs (candidate lines intentionally not inspected) | Not cleared; no candidate inventory supplied | Card-only calibration; source inspection excluded
public local traits or one-implementation delegation | UNVERIFIED / UNVERIFIED | domains/ir/crates/nudox-ir-vocab/src/lib.rs; domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs (candidate lines intentionally not inspected) | Not cleared; public traits forbidden | Card:40-42
one-letter generic parameters | UNVERIFIED / UNVERIFIED | domains/ir/crates/nudox-ir-vocab/src/lib.rs; domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs (candidate lines intentionally not inspected) | Not cleared; no candidate inventory supplied | Card-only calibration; source inspection excluded
numeric discriminants/sentinels/offsets/capacities/loop bounds | UNVERIFIED / UNVERIFIED | domains/ir/crates/nudox-ir-vocab/src/lib.rs; domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs (candidate lines intentionally not inspected) | Not cleared; no candidate inventory supplied | Card-only calibration; source inspection excluded
test-only Option/discarded results/success-only assertions | UNVERIFIED / UNVERIFIED | domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs; domains/ir/crates/nudox-ir-vocab/src/lib.rs (candidate lines intentionally not inspected) | Not cleared; discarded cleanup and weak runtime substitution prohibited | Card:47,50-53; finding: noncausal-proof
unsafe/SIMD/allocator/dependency additions | UNVERIFIED / UNVERIFIED | domains/ir/crates/nudox-ir-vocab/src/lib.rs; domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs (candidate lines intentionally not inspected) | Not cleared; all are forbidden/zero-budget | Card:27-28,40-42,61
public item without current consumer and falsifier | UNVERIFIED / UNVERIFIED | domains/ir/crates/nudox-ir-vocab/src/lib.rs; domains/ir/crates/nudox-ir-vocab/tests/coordinates.rs (candidate lines intentionally not inspected) | Not cleared; public-item appearance is a stop trigger | Card:64-67
```

Strongest attempted counterexample: a fixture with a locally recreated `EntityId`/`TypeId`, or a doctest/runtime assertion, which would appear to prove separation while never exercising the exported crate. The card rejects it explicitly.

Most likely hidden defect after implementation: an allocation/branch/lifetime issue is unlikely in this tiny vocabulary slice; the credible risk is instead a hidden test-harness branch that converts compiler invocation failure into green success.

Cleared suspicion: the card does not authorize compiler dispatch work or a compiler edit; it names only the two IR paths and repeatedly excludes C0-COMPILER.

A simpler standard-library design was considered: yes—the frozen candidate is the existing private phantom-brand `DenseId` representation, with no new dependency, macro, trait, conversion API, or allocation.

Confidence: high that the frozen card rejects all six seeded defect classes at hard-stop severity. Unverified gaps: candidate source, actual tripwire counts/lines, command output, rlib resolution, stderr retention, legal-mutant result, formatted LOC, and changed-path inventory were intentionally unavailable.

Calibration approval: **APPROVED for the frozen contract only.** This is not implementation approval; an actual candidate still requires the literal tripwire inventory with source-backed counts and evidence.
