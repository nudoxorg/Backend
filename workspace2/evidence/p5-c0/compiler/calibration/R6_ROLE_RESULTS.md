# P5 C0-COMPILER R6 calibration role results

All four roles were spawned with `fork_turns="none"` against card SHA-256
`26501daa96a0cbcd065d55a617ed1cbd396ad081a0a27bb139ae3cc6c662e145`.  This file preserves the
task/model/role custody required before a distinct hostile pre-edit review.  No role had implementation
authority.

| role | canonical spawned task | explicit model | allowed inputs | result |
| --- | --- | --- | --- | --- |
| cold reader one | `/root/p5_c0_compiler_manager/c0_compiler_r6_cold_reader_one` | `gpt-5.6-luna` | card and ten governing skill instructions only | CLEAR |
| cold reader two | `/root/p5_c0_compiler_manager/c0_compiler_r6_cold_reader_two` | `gpt-5.6-luna` | card and ten governing skill instructions only | CLEAR |
| plausible misreader | `/root/p5_c0_compiler_manager/c0_compiler_r6_plausible_misreader` | `gpt-5.6-luna` | card and ten governing skill instructions only | CLEAR |
| reviewer calibration | `/root/p5_c0_compiler_manager/c0_compiler_r6_reviewer_calibration` | `gpt-5.6-terra` | card, skills, four skeletons, six seeds, R6 mutation receipts, rejected commits | CLEAR |

## Independent cold reads

Both Luna readers verified the same card digest and independently restated the closed manual terminal:
Rust supports Parse and LowerIr; TypeScript supports Parse, exact typed full-registry LowerIr rejection,
and a consumed parse-only static subset.  They found the four writable paths, no manifest/dependency
authority, the actual-rlib sole-`E0599` proof, pointer-and-length identity for every valid cell, and the
same-source per-callable release-text conditions.  Each confirmed the per-file 25 LOC reserve and that
no production edit is allowed until this deck and a separate hostile Terra review clear.  Neither found a
blocker or major ambiguity.

## Plausible-misreader result

The third Luna tried the cheapest apparent evasions: a shadow subset type, stale rlib discovery, a second
stage decision, bool/index/tag selection metadata, direct frontend consumer calls, input-erasure theater,
weakened rejected-cell errors, multi-error diagnostics, consumer mutation, and whole-binary text claims.
It found each literally blocked by the frozen card: actual `--extern` rlib linkage and clean cardinality;
`drive` as sole stage decision; private capability rows/no selection metadata; byte-identical registry
consumer plus dispatch/drive inspection; named pointer-and-length mutants; literal typed error; sole E0599;
and per-callable-only text evidence.  It found no blocker or major ambiguity.

## Terra calibration result

The non-inheriting Terra reviewer verified all card, skeleton, seed, raw-log, and status hashes.  It
freshly reproduced the four seed runners from `f0fe44e12bb0e20bc3be2f99e37b38e2664b03f3`: every runner
exited 0; Rust Parse, Rust LowerIr, TypeScript Parse, and the typed TypeScript LowerIr tests each exited
101 red; every valid release-example LLVM command exited 0.  It confirmed the three valid cells fail on
their own pointer identity assertion and the rejected cell fails with `Ok(...)` versus the literal typed
`Err(UnsupportedStage { language: TypeScriptSubset, stage: LowerIr })`.

Its full `review-rust-gem` tripwire result cleared all prohibited classes.  It classified only the
prescribed consumed `FullRegistry` and `TypeScriptSubset` ZST values, the edition and exact-one-error test
numbers, and the rlib slot as required evidence rather than production selection metadata.  It confirmed
the three rejected commits demonstrate exactly the forbidden bool/index matrix, shadow proof, and broad
behavior-mismatched consumer.  It also explicitly confirmed the LLVM limitation: these seed runs prove
forwarding-test causality only, not input-removal, erasure, or zero-cost codegen.  Verdict: CLEAR.
