# Terra reviewer calibration — raw return

Task identity: `/root/p5_c0_ir_manager_fresh/p5_c0_ir_bound_reviewer_calibration`
Model: `gpt-5.6-terra`
Spawn contract: `fork_turns: none`; read-only; card, governing skills, and frozen skeleton only.

Card custody verified: worktree and branch match; committed card SHA is `ffce71ee4fd8d00f1ff5d1bd4d12397bc8938136a48c1ab0e6434bc48f3c86e8`. Skeleton is exactly 94 lines with SHA `e1138c221def6262d64f8ce168562394202ac9043bf70227743361b97f9d7b7d`.

| tripwire | count | exact locations | disposition | evidence or finding ID |
|---|---:|---|---|---|
| panic/unwrap/expect/unreachable | 0 | `evidence/p5-c0/ir/skeleton/coordinates.rs:1-94` scanned for literal markers | Clear; `assert!` is paired with causal compiler results, not a panic terminal | IR-CAL-01 |
| source-dropping conversion or map_err | 2 | `coordinates.rs:32-33` (`to_string_lossy`) | Cold filename-filter conversion only; no error/value source is dropped and no `map_err` exists. Cargo-produced ASCII rlib naming is additionally bounded by exact-one resolution. | IR-CAL-02 |
| lossy/ambiguous From/TryFrom or raw authority bypass | 0 | `coordinates.rs:1-94` | No new conversion or raw rebrand API. The card’s known baseline `raw` + public `new(u32)` reconstruction limitation is a preserved limitation, not evidence of a new bypass. Its source-pinned comparison path is deliberately **UNVERIFIED**. | IR-CAL-03 |
| checked-arithmetic sentinel/saturation or operand loss | 0 | `coordinates.rs:1-94` | Clear; no checked arithmetic. | IR-CAL-04 |
| dyn/Box/Vec/Arc/Rc | 0 | `coordinates.rs:1-94` | Clear; standard-library process/path/I/O handles only. | IR-CAL-05 |
| public tuple fields or positional semantic tuples | 0 | `coordinates.rs:1-94` | Clear. | IR-CAL-06 |
| unit/stateless namespace structs | 0 | `coordinates.rs:1-94` | Clear. | IR-CAL-07 |
| public local traits or one-implementation delegation | 0 | `coordinates.rs:1-94` | Clear. | IR-CAL-08 |
| one-letter generic parameters | 0 | `coordinates.rs:1-94` | Clear. | IR-CAL-09 |
| numeric discriminants/sentinels/offsets/capacities/loop bounds | 13 | `coordinates.rs:11,13-14,18-23,75-78` | Required literals: three exact raw-position `7` uses, six four-byte/four-alignment assertions across `DenseId<Entity>`, `EntityId`, `TypeId`, and exact diagnostic cardinality/presence checks. No sentinel state. | IR-CAL-10 |
| test-only Option/discarded results/success-only assertions | 1 | `coordinates.rs:28,35,40` | Allowed local zero-or-one rlib discovery; second rlib returns causal error and zero rlibs error. `drop(input)` at :61 closes stdin before wait, not discarded cleanup. Legal-success assertion is paired with `!mismatch`, so is not success-only. | IR-CAL-11 |
| unsafe/SIMD/allocator/dependency additions | 0 | `coordinates.rs:1-94` | Clear; standard library only. | IR-CAL-12 |
| public item without current consumer and falsifier | 0 | `coordinates.rs:1-94` | Clear; supplied artifact is an ordinary test consumer, with no shipping public item. | IR-CAL-13 |

Targeted attacks:

- Literal layout law is complete: `size_of` and `align_of` each equal literal `4` for all three required coordinate types at lines 18–23.
- The compiler fixture uses the running test executable’s parent (`:83-86`); the card explicitly binds Cargo gates to `CARGO_TARGET_DIR=domains/ir/target` at card lines 103–06 and 117–20. Under those gates this resolves the same `debug/deps` directory; no workspace-default assumption appears.
- Exact-one rlib is independently enforced at `:27-40`, then injected as the actual external crate at `:43-55`. This rejects zero or multiple candidates.
- Diagnostic proof is causal: exactly one `error[E` marker at `:75`, containing `E0308`, `EntityId`, and `TypeId` at `:76-78`.
- The source strings contain the exact legal mutant: only `EntityId::new(7)` becomes `TypeId::new(7)` (`:13-14`); `:89-92` requires the mutant compile and its diagnostic predicate be false. No aliases or local shadow types occur.
- No temporary source/output cleanup is hidden: stdin avoids fixture files, `--emit=metadata=-` prevents mutant output, and `wait_with_output` retains stderr. No compiler edits are present in the supplied skeleton; the card prohibits them.
- Reserve is intact: 94-line skeleton minus 14-line baseline = 80-line test delta, leaving the stated 16 lines below the 110 hard cap.

Strongest counterexample: a public caller may deliberately reconstruct a different brand from a copied raw coordinate because the pinned baseline exposes raw/public construction. The card explicitly limits this checkpoint to direct typed-use safety. The skeleton neither repairs nor adds that path; the actual source claim is **UNVERIFIED by design** because `domains/ir/crates/nudox-ir-vocab/src/lib.rs` was not opened.

Hidden-cost check: one directory scan plus two cold `rustc` processes occur only in the test; no production allocation, dependency, or dispatch cost is introduced. The simpler standard-library design is already used: direct `Command`, stdin, captured stderr, and `--extern`, with no compiletest crate, macro, or framework.

Verdict: **CALIBRATION-ONLY — no blocker or major in the frozen skeleton.** This does not accept an implementation or verify the deliberately unopened source-pinned comparison path.
