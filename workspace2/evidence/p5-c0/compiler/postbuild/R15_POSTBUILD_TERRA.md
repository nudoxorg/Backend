# R15 separate Terra postbuild review and custody repair

Initial reviewer `c0_compiler_r15_postbuild_terra` (explicit `gpt-5.6-terra`) found correct source and
codegen but stopped for missing independently auditable compiler-process custody. Falsifier-bound Luna
task `c0_compiler_r15_subset_custody_repair` added no source change and retained the original run.
Fresh Terra verifier `c0_compiler_r15_postbuild_repair_terra` confirmed the original and independent
later rlib pairs are separate per-run identities, not a reproducibility assertion. It cleared the prior
major at `803aa9f673b6a409ec86e2425ddb6a02c4085f89`.

| tripwire | count | exact locations | disposition | evidence or finding ID |
| --- | ---: | --- | --- | --- |
| panic/unwrap/expect/unreachable | 0 | complete actual four-path set | CLEAR | R15-POST-TW |
| source-dropping conversion or map_err | 0 | complete actual four-path set | CLEAR | R15-POST-TW |
| lossy/ambiguous From/TryFrom or raw authority bypass | 0 | complete actual four-path set | CLEAR | R15-POST-TW |
| checked-arithmetic sentinel/saturation or operand loss | 0 | complete actual four-path set | CLEAR | R15-POST-TW |
| dyn/Box/Vec/Arc/Rc | 0 | complete actual four-path set | CLEAR | R15-POST-TW |
| public tuple fields or positional semantic tuples | 0 | complete actual four-path set | CLEAR | R15-POST-TW |
| unit/stateless namespace structs | 2 | `src/lib.rs:58,74` | allowed consumed public capability values | R15-POST-CAP |
| public local traits or one-implementation delegation | 0 | `src/lib.rs:10-18,20-48` | private traits, two concrete implementations | R15-POST-CAP |
| one-letter generic parameters | 0 | complete actual four-path set | sole generic `ConcreteFrontend` | R15-POST-TW |
| numeric discriminants/sentinels/offsets/capacities/loop bounds | 8 | `subset.rs:10,11,56,83,85`; `dispatch.rs:29,38`; `release_consumer.rs:64` | fixtures, edition, diagnostic contract only | R15-POST-E0599 |
| test-only Option/discarded results/success-only assertions | 2 | `tests/subset.rs:10,11` | paired forbidden/legal probes | R15-POST-E0599 |
| unsafe/SIMD/allocator/dependency additions | 0 | actual source/manifests/R15 diff | CLEAR | R15-POST-TW |
| public item without current consumer and falsifier | 0 | `src/lib.rs:58-79` | dispatch, consumer, external subset probe | R15-POST-CAP |
| stdout containment and exact diagnostic parser | 0 | `tests/subset.rs:55-92` | null stdout, piped exact stderr predicate | R15-POST-E0599 |
| actual-rlib absence proof / stale-artifact resistance | 0 | `R15_SUBSET_PROCESS_CUSTODY.md`; `tests/subset.rs:13-110` | original and later runs each bind source/toolchain/flags/zero-one/path/hash/output | R15-POST-RLIB-CLEAR |
| closed typed rows and sole decisions | 0 | `src/lib.rs:10-18,20-55,58-79`; `dispatch.rs:10-47` | one language and one stage decision | R15-POST-CAP |
| path/cap restrictions and frozen-object custody | 0 | actual four paths; builder record; source transaction | actual SHA/LOC match frozen; only subset changed | R15-POST-CUSTODY |

The retained release artifacts show three wrappers forwarding pointer plus length to direct dispatch. Both
owner bodies have the realized branches with no owner indirect call or panic path. Direct dispatch calls
are baseline-equivalent residual work. Stable per-symbol release text remains **UNVERIFIED**; no total
size proxy is used. The strongest counterexample was a fresh, same-source isolated run with differing
rlib bytes; it is retained as the second valid per-run identity, not treated as contradiction.
