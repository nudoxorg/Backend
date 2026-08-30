# P5 C0-COMPILER R6 seeded-mutant execution

This is executable calibration evidence, not candidate implementation evidence.  The detached worktree
was created at `f0fe44e12bb0e20bc3be2f99e37b38e2664b03f3`; it copied the four frozen skeleton sources
before applying exactly one digest-pinned seed per run.  Its toolchain was `rustc 1.97.1
(8bab26f4f 2026-07-14)`, LLVM 22.1.6, host `aarch64-apple-darwin`, and `cargo 1.97.1
(c980f4866 2026-06-30)`.

## Commands and exact exits

The valid-cell command was, once for each `rust-parse`, `rust-lower`, and `typescript-parse`:

```text
sh workspace2/evidence/p5-c0/compiler/seeded/run-valid-cell-mutant.sh <cell> <detached-worktree> <evidence-dir>
```

Each runner exited `0` only after its inner targeted `cargo test --locked ... --test dispatch <named-test>`
exited `101` and its `cargo rustc --locked ... --example release_consumer --release -- --emit=llvm-ir`
exited `0`.  The latter command's complete stdout/stderr is retained below, and the runner emitted an
LLVM file during the detached run.

The rejected-cell command was:

```text
sh workspace2/evidence/p5-c0/compiler/seeded/run-typescript-lower-mutant.sh <detached-worktree> <evidence-dir>
```

It exited `0` only after its inner targeted `cargo test` exited `101`.

| mutation | raw red test | test status | test SHA-256 | raw release-codegen log | codegen status | codegen SHA-256 |
| --- | --- | --- | --- | --- | --- | --- |
| Rust Parse constant body | `r6-final-rust-parse-test.txt` | `101` | `46a0503fba026b58561edc06894057482cdc03a15c49870985dcf0dd912cbe0a` | `r6-final-rust-parse-codegen.txt` | `0` | `147e605e47f20aa9acaccfb04929f512b022be03a91e74774f67a97e058d1ab6` |
| Rust LowerIr constant body | `r6-final-rust-lower-test.txt` | `101` | `fa47af4eb339820be226272ed0265219286f4d7b89a274d3052b92f6f534095a` | `r6-final-rust-lower-codegen.txt` | `0` | `0bff5b02de90e89e2b103808a3ab17402c1b55b3dfd982b375baca287b7d843c` |
| TypeScript Parse constant body | `r6-final-typescript-parse-test.txt` | `101` | `a1f4931b735736fae8b7f7786d0e0ff613ae071c5b44b1ea7f0d7681b4fffba4` | `r6-final-typescript-parse-codegen.txt` | `0` | `8569c168cc4d0afd8d2363eaddada27a1974d7660d0722edddca9fd8896a0bac` |
| TypeScript LowerIr `Err` to `Ok(source)` | `r6-final-typescript-lower-test.txt` | `101` | `3e87c96b08845d7f0ab3f63b53c17efab16aa4b0a14b80dd0d4c93b17c99df25` | not run | not applicable | not applicable |

The status artifacts are retained separately: each `*-test.status` has SHA-256
`39b8dc3fc8b44765c8e6f1adee04c5b465e555ab791cc42d0d9e810d5b64297c` and contains `101`; each
valid `*-codegen.status` has SHA-256
`9a271f2a916b0b6ee6cecb2426f0b3206ef074578be55d9bc94f6f3fe3ab86aa` and contains `0`.

## What this does and does not establish

Each valid-cell red output names its own pointer-and-length test and fails at
`assertion failed: core::ptr::eq(output, source)`.  It therefore causally establishes source dependence
of that frozen forwarding cell.  The rejected-cell output instead shows the exact typed-error test
changed from the expected `Err(UnsupportedStage { language: TypeScriptSubset, stage: LowerIr })` to
`Ok(source)` and fails.

The release-consumer LLVM emission succeeded, but this seed runner does **not** perform a before/mutant
per-callable IR comparison.  Its deliberately nonpanic pointer-and-length verifier itself retains input
uses after a constant mutation.  Consequently the emitted LLVM proves only that the altered consumer
compiled; input-removal codegen and every zero-cost/erasure assertion are **UNVERIFIED** here.  No
codegen claim relies on this calibration artifact.
