# P5 C0-COMPILER closure receipt

## Finding

The manual closed static-dispatch control is complete as a narrow C0 experiment. Rust supports Parse and
LowerIr; TypeScript supports Parse and full-registry LowerIr returns the exact typed UnsupportedStage
error. The public TypeScriptSubset exposes only Parse and an external compiler process rejects lower as
an absent member. `drive` owns the sole stage decision; `FullRegistry::dispatch` owns the sole language
decision. This is not C1, a merge, or product completion.

## Canonical custody

```text
baseline control    fac5b709d4596f889735a773a6dbd6a9c7c822f5
card commit         414bb5ce76ed308095ae0c8860e7fb3f2b3ec7bf
card SHA-256        c4fa4dd7df6644eff74e44821f2fbcd65dcc05a8f8539977d0b520396a86b74d
source candidate    5017ca393b9662b8e7fa2e32e0c88caaeabf8e81
postbuild evidence  0cc5a2d53fb75858cba46f72c2ca067feaef8088
closure review      42548c67b0c846d4c092f7eb7d73cbcb31c3a0aa
```

R9 had two independent cold Luna reads, a Luna plausible-misreader, and Terra calibration, followed by
a separate hostile Terra pre-edit review. All were explicit model tasks with `fork_turns="none"` and
CLEAR; custody is in `calibration/R9_ROLE_RESULTS.md` and `calibration/R9_HOSTILE_PREEDIT.md`. Separate
Terra postbuild and closure reviews are CLEAR in `postbuild/R9_POSTBUILD_TERRA.md` and
`postbuild/R9_CLOSURE_TERRA.md`.

## Accepted and rejected history

Accepted source commits are `58949077` (library), `3d3d099d` (pointer/length tests), `489f2d39`
(actual-rlib static subset), and `5017ca39` (typed nonpanic release consumer). Exact Luna task/path
checkpoints are in `postbuild/R9_BUILDER_CHECKPOINTS.md`. Rejected `916789c6`, `328f890a`, and
`788499a5` remain history-only counterexamples: bool/index metadata, a shadow/unproven type-row fixture,
and a behavior-mismatched broad/panic consumer. Earlier R1-R8 decks are churn only.

## Changed source and resource ledger

| path | baseline LOC | candidate LOC | net LOC | role |
| --- | ---: | ---: | ---: | --- |
| `planes/compiler/crates/nudox-compile-registry/src/lib.rs` | 67 | 80 | 13 | closed rows and sole decisions |
| `planes/compiler/crates/nudox-compile-registry/tests/dispatch.rs` | 30 | 48 | 18 | three pointer+length and exact-error cells |
| `planes/compiler/crates/nudox-compile-registry/tests/subset.rs` | 8 | 101 | 93 | resolved-rlib E0599 absence proof |
| `planes/compiler/crates/nudox-compile-registry/examples/release_consumer.rs` | 0 | 70 | 70 | same-source direct release callables |

Production delta is 83 LOC (150 total; 50 unused against cap); test delta is 111 LOC (149 total; 42
unused). No manifest or lockfile changed. Production retains only the existing inward
`nudox-compile-vocab` dependency, uses borrowed slices, and adds zero allocations/copies. A full
registry request has one language match and one stage match; there is no runtime capability matrix,
boolean/index metadata, tag, dynamic dispatch, or second decision owner.

## Causal cells and static subset

`rust_parse_forwards_its_own_pointer_and_length`,
`rust_lower_forwards_its_own_pointer_and_length`, and
`typescript_parse_forwards_its_own_pointer_and_length` each require pointer identity and length identity
on different inputs. `typescript_lower_has_exact_typed_operands` requires literal
`Err(UnsupportedStage { language: TypeScriptSubset, stage: LowerIr })`. The static subset test resolves
the actual fresh registry rlib adjacent to its executable, rejects duplicate/missing rlibs, uses an
explicit `--extern`, and requires exactly one E0599 naming both TypeScriptSubset and lower; the legal
Parse-only mutant compiles.

R7's digest-pinned valid-cell constant-body mutations each produced its named red test with exit 101 and
pointer assertion failure; the separate TypeScript LowerIr Err-to-Ok mutation produced exit 101 in the
exact-error test. Raw outputs and SHA-256 values are in `mutants/R7_EXECUTION.md`. This proves the tests
are causal, not callable-IR input-removal, because the verifier itself still uses input.

## Codegen and text evidence

`postbuild/R9_SAME_SOURCE/` retains a fresh paired direct-rustc control using detached `fac5b709` and
the actual candidate. Both use the same named consumer, toolchain, aarch64 target, opt level 3,
dependency-path form, and explicit one-fresh registry plus one-fresh vocab rlib. `custody.txt` hashes
the source and rlib pairs. Consumer LLVM shows every named wrapper passes its supplied pointer and length
to direct `FullRegistry::dispatch`. Paired owner LLVM defines dispatch; optimized drive has no standalone
definition, and realized branches occur inside dispatch. There is no owner indirect call or panic path.
The wrapper-to-dispatch direct call remains on both sides, so it is baseline-equivalent residual work,
not erased selection.

Release text is **UNVERIFIED** because this host supplied no stable per-symbol callable size mechanism.
No rlib, assembly-file, or executable total was substituted. Input-removal codegen, dispatch erasure,
and zero-cost are also **UNVERIFIED** and never claimed.

## Final gates and conclusion

Two clean final dedicated-target gates each passed zero-before/one-after registry and vocab rlib custody,
`cargo test --locked --workspace --all-targets`, `cargo fmt --check`, warnings-denied
`cargo clippy --workspace --all-targets`, and `git diff --check`. The target cache was removed after each
run; the working tree is clean. Literal tripwire scans found one match-stage, one match-language, and
zero prohibited matrix/bool/index/macro/unsafe/heap/extra-public-surface findings.

Strongest counterexample: a stale/shadow rlib or a direct dispatch call misdescribed as erasure. The
first is blocked by zero/one cardinality, paths, hashes, and explicit externs; the second is visible in
both direct callable artifacts and reported as residual. The sole platform limitation is per-symbol text
evidence.

**PROMOTE FOR FUTURE INTEGRATION REVIEW**
