# P5 C0-COMPILER R7 seeded-mutant execution

This supersedes R6's seed execution after the frozen consumer and formatting repair.  A detached worktree
at `9225fa3bd192ea1f0c6a69f1a57a8afe66cdaf36` copied all four R7 skeleton sources before exactly one
digest-pinned mutation per run.  The valid command was
`sh .../run-valid-cell-mutant.sh <rust-parse|rust-lower|typescript-parse> <worktree> <evidence-dir>`;
the rejected command was `sh .../run-typescript-lower-mutant.sh <worktree> <evidence-dir>`.

| mutation | raw red test | test exit | test SHA-256 | raw release-codegen log | codegen exit | codegen SHA-256 |
| --- | --- | ---: | --- | --- | ---: | --- |
| Rust Parse constant body | `r7-rust-parse-test.txt` | 101 | `1b3e3398352c5624769e3841fa5e6c8131370233800285bae86a59a15dfe5faa` | `r7-rust-parse-codegen.txt` | 0 | `ad596826e3c1b7c78ae52a0922fc3f322100e48f6bd6c916ddfcfd7f2674ff8b` |
| Rust LowerIr constant body | `r7-rust-lower-test.txt` | 101 | `895089d84dc070b7ad1f861277b5b48f2a7875ded64f96ffb59c00b2fe6cb1d0` | `r7-rust-lower-codegen.txt` | 0 | `77c05d629658018ed9db11bbf7a4be3ff0224c3072811004586cf5e06dcec5ce` |
| TypeScript Parse constant body | `r7-typescript-parse-test.txt` | 101 | `c361c72f2cc2c7a35d9e3d9ec59d5434ec102e8133dc1fcb4defcffc72e59a13` | `r7-typescript-parse-codegen.txt` | 0 | `0fd6063a4bf855ab8a1cc3541b4094d39fe2d9641364827aef5cec9a75bf0817` |
| TypeScript LowerIr `Err` to `Ok(source)` | `r7-typescript-lower-test.txt` | 101 | `c0abab94dbbc2b386486c7364087bbf8c7e61cb1278f26491482f07f6b7cd650` | not run | n/a | n/a |

All three valid red outputs identify their own named pointer-and-length test and fail on
`assertion failed: core::ptr::eq(output, source)`.  The rejected-cell output identifies
`typescript_lower_has_exact_typed_operands` and fails `Ok(...)` against literal
`Err(UnsupportedStage { language: TypeScriptSubset, stage: LowerIr })`.  Each runner exits 0 only after
capturing that expected test exit; every `*-test.status` contains 101 and has SHA-256
`39b8dc3fc8b44765c8e6f1adee04c5b465e555ab791cc42d0d9e810d5b64297c`.  Each valid
`*-codegen.status` contains 0 and has SHA-256
`9a271f2a916b0b6ee6cecb2426f0b3206ef074578be55d9bc94f6f3fe3ab86aa`.

The three raw codegen logs warn about an unused `source` only because their deliberate constant-body
mutant dropped it.  That is expected mutation evidence, not a candidate lint result.  The unmutated R7
frozen skeleton passed its warnings-denied Clippy gate separately in
`calibration/R7_SKELETON_GATE.md`.

The emitted mutated LLVM is not a before/mutant comparison for each named callable, and the intentional
pointer-and-length verifier continues to read input.  Thus this proves forwarding-test causality only;
input-removal, erasure, residual-call, text-size, and zero-cost claims remain **UNVERIFIED**.
