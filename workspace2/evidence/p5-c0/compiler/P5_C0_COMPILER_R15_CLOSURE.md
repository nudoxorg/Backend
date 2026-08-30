# P5 C0-COMPILER R15 closure receipt

## Finding

The reopened control is closed. The sole source repair contains rustc metadata stdout in the actual
compiler-process test and tightens its textual diagnostic cardinality. Rust supports Parse and LowerIr;
TypeScript supports Parse and returns the exact typed LowerIr error. The public TypeScript subset exposes
Parse only and is causally absent for LowerIr. `FullRegistry::dispatch` retains the sole language match;
`drive` retains the sole stage match. No C1, merge, or product-completion claim is made.

## Canonical custody

```text
baseline control      fac5b709d4596f889735a773a6dbd6a9c7c822f5
R15 card commit       0741bebef3479f1a5e701e5d2246a85f10c82931
R15 card SHA-256      933b72bcd4bce8ccc2bbfb48817816225e9fbdfecff7464efad94652cb8b6dd3
source repair commit  8aa30cdd57b32209ad185a2bb23f83307292d96c
closure review commit 30a69579a88bd407fbf164017d6661683541320f
```

R15 role/model custody is at `calibration/R15_ROLE_RESULTS.md`; hostile pre-edit is at
`calibration/R15_HOSTILE_PREEDIT.md`; the builder, postbuild, closure, and final gates are in the
`postbuild/` records. R9 and all R10-R14 decks are superseded history, not authority.

## Accepted and rejected history

Accepted source work remains `58949077` (closed dispatch), `3d3d099d` (forwarding tests), `489f2d39`
(actual-rlib subset proof), `5017ca39` (typed release consumer), and `8aa30cdd` (R15 one-path output
repair). Rejected counterexamples remain `916789c6` (bool/index metadata), `328f890a` (shadow/unproven
type-row fixture), and `788499a5` (broad behavior-mismatched panic/process consumer). The prior closure
at `c266d5c9` is superseded because it allowed binary rustc metadata to escape the test terminal.

## Changed paths and budget ledger

| Path | Frozen baseline LOC | Resulting LOC | Net delta | Resulting cap | Reserve |
| --- | ---: | ---: | ---: | ---: | ---: |
| `planes/compiler/crates/nudox-compile-registry/src/lib.rs` | 67 | 80 | 13 | 108 | 28 |
| `planes/compiler/crates/nudox-compile-registry/tests/dispatch.rs` | 30 | 48 | 18 | 73 | 25 |
| `planes/compiler/crates/nudox-compile-registry/tests/subset.rs` | 8 | 111 | 103 | 118 | 7 |
| `planes/compiler/crates/nudox-compile-registry/examples/release_consumer.rs` | 0 | 70 | 70 | 80 | 10 |

Production is 150 LOC from a 67 LOC baseline, a net 83 LOC delta with 50 aggregate lines of
remaining reserve. Tests are 159 LOC from a 38 LOC baseline, a net 121 LOC delta with 32 aggregate
lines of remaining reserve. No manifest or lockfile changed and no dependency was added.

The production representation contains the closed `Language` and `Stage` matches only: `dispatch`
owns the language decision and generic `drive` owns the sole stage decision. There is no capability
bool/index table, runtime tag, `dyn`, allocation, copy-owning container, macro-generated dispatch, or
duplicate runtime matrix lookup. All work is borrowed input forwarding; measured allocations and
copies are zero. The release consumer is typed and non-panicking.

## Capability, negative-API, and stdout evidence

The four ordinary dispatch tests name the exact properties:

| Valid or rejected cell | Evidence |
| --- | --- |
| Rust Parse | `rust_parse_forwards_its_own_pointer_and_length` proves supplied pointer and length are forwarded. |
| Rust LowerIr | `rust_lower_forwards_its_own_pointer_and_length` proves supplied pointer and length are forwarded. |
| TypeScript Parse | `typescript_parse_forwards_its_own_pointer_and_length` proves supplied pointer and length are forwarded. |
| TypeScript LowerIr | `typescript_lower_has_exact_typed_operands` proves the exact `FrontendError::Unsupported { language: Language::TypeScript, stage: Stage::LowerIr }` result. |

`typescript_lower_is_a_causal_absent_member` invokes rustc against the actual exported registry and
vocab rlibs passed by explicit `--extern` flags. Its forbidden `TypeScriptSubset::lower` source fails
with one coded error, sole `E0599`, exact operands, and the sole allowed abort summary; its legal
`TypeScriptSubset::parse` source succeeds. The child compiler uses `--emit=metadata=-` but explicitly
sets `stdout(Stdio::null())`, so the normal test terminal remains textual and no metadata filesystem
artifact is created. Raw commands, stderr, paths, hashes, and cardinalities are retained in
`postbuild/R15_SUBSET_PROCESS_CUSTODY.md`.

The retained R7 source-independent forwarding mutants each produced the named pointer-and-length test
failure; they establish the tests are causal. They do not mechanically prove that optimized codegen
would remove input dependence, so no input-erasure claim is made.

## Codegen and per-run artifact custody

The same named release-consumer source, source hashes, toolchain, profile, flags, callable set, and
input behavior were compiled directly against detached `fac5b709` control and the R15 candidate.
Separate owner-body builds compiled each registry source with its exact fresh vocab rlib. The retained
R15 raw LLVM and assembly artifacts show the three wrappers pass their pointer and length to a direct
`FullRegistry::dispatch` call. `drive` is fully inlined at opt-level 3; the owner `dispatch` body
therefore contains the realized stage branch/calls. There is no residual indirect call or panic path
in the inspected owner body.

There is a residual direct wrapper-to-`dispatch` call in both control and candidate. That is reported
as baseline-equivalent cost, not erased selection and not zero cost. Per-callable symbol text size is
UNVERIFIED on this aarch64 Apple platform; broad executable or rlib totals were not substituted.

The actual subset compiler process and its independent rerun each retain their own exact rlib path and
SHA-256. The first registry/vocab pair is `8445b7ce05b00c9bf482fc4a2ac2b64abbe6aa140e8485e4b55c32530515946e` /
`35b5f3c9d9606248824aa8d650c11223caa202e654eebb00bab215b0a1197864`; the independently rebuilt pair is
`5f8b9ecb1df84661fefd0cfb3c17c53ecbfeedeb7649ee3e1b1cb3a23b9ac25e` /
`b33d615ed074092986834bccbec22ee41fcaa1dbe88c49f5fd53ba7817bb70f4`. Equality across isolated Rust
builds is neither required nor claimed. The causal invariant is pinned source hash plus toolchain and
flags, zero/one fresh cardinality, and the exact path/hash bound to each run's command and output.

## R15 tripwire and task/model proof

| Tripwire | Count | Exact locations | Disposition | Evidence or finding ID |
| --- | ---: | --- | --- | --- |
| Cold readers | 2 | `c0_compiler_r15_cold_one`, `c0_compiler_r15_cold_two` | CLEAR | `calibration/R15_ROLE_RESULTS.md` |
| Plausible misreader | 1 | `c0_compiler_r15_misreader` | CLEAR after literal-card follow-up | `calibration/R15_ROLE_RESULTS.md` |
| Terra calibration | 1 | `c0_compiler_r15_terra_calibration` | CLEAR | `calibration/R15_ROLE_RESULTS.md` |
| Hostile pre-edit | 1 | `c0_compiler_r15_hostile_preedit` | CLEAR | `calibration/R15_HOSTILE_PREEDIT.md` |
| Luna exact-path builder | 1 | `c0_compiler_r15_builder_subset` | one authorized path only | `postbuild/R15_BUILDER_CHECKPOINT.md` |
| Terra postbuild | 2 | `c0_compiler_r15_postbuild_terra`, `c0_compiler_r15_postbuild_repair_terra` | CLEAR after custody repair | `postbuild/R15_POSTBUILD_TERRA.md` |
| Terra closure | 1 | `c0_compiler_r15_closure_terra` | CLEAR | `postbuild/R15_CLOSURE_TERRA.md` |
| Clean final gates | 2 | dedicated compiler target, sequential independent runs | PASS | `postbuild/R15_FINAL_GATES.md` |

The required cold and builder roles used explicit `gpt-5.6-luna`; the calibration, hostile, postbuild,
and closure reviewers used explicit `gpt-5.6-terra`; all were spawned with `fork_turns="none"`.

## Strongest counterexamples and limits

The rejected bool/index metadata control demonstrates why capability rows must be encoded by the closed
type representation. The shadow-type fixture demonstrates why the compiler process must use the actual
exported rlib. The broad panic/process consumer demonstrates why like-for-like typed callables are
required. The superseded closure demonstrates why `--emit=metadata=-` must contain stdout. The fresh
per-run rlib binding defeats stale/duplicate artifact substitution without claiming nondeterministic
rlib bytes reproduce across builds.

UNVERIFIED: stable per-callable text size on the current platform, optimized input-erasure under a
constant-body mutant, and any cross-platform codegen claim. No zero-cost claim is made.

## Terminal disposition

**PROMOTE FOR FUTURE INTEGRATION REVIEW**
