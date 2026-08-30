# P5 C0-COMPILER R8 calibration role results

All four roles were spawned with `fork_turns="none"` against card SHA-256
`0ff8bfa2f92bc7519bda85231f56f5d803e37e2e822a5ed54a1aed457656b6fd`; none had production authority.

| role | canonical spawned task | model | result |
| --- | --- | --- | --- |
| cold reader one | `/root/p5_c0_compiler_manager/c0_compiler_r8_cold_reader_one` | `gpt-5.6-luna` | CLEAR |
| cold reader two | `/root/p5_c0_compiler_manager/c0_compiler_r8_cold_reader_two` | `gpt-5.6-luna` | CLEAR |
| plausible misreader | `/root/p5_c0_compiler_manager/c0_compiler_r8_plausible_misreader` | `gpt-5.6-luna` | CLEAR |
| reviewer calibration | `/root/p5_c0_compiler_manager/c0_compiler_r8_reviewer_calibration` | `gpt-5.6-terra` | CLEAR |

The cold readers confirmed the paired fresh registry/compile-vocab rlib control, explicit paired externs,
same dependency path/profile/toolchain, no re-export and no Cargo fallback.  The misreader tried each
omission, stale/duplicate artifact, transitive resolution, control mismatch, direct-call, and codegen
theater route; the literal card blocks every route.

The separate Terra calibration reviewer verified the card/skeleton/seed/control digests, replayed all
four mutation runners (runner 0, targeted red 101, valid codegen 0), replayed the direct paired-artifact
control from fac5b709 (zero then exactly one of each rlib per side, paired direct externs, no Cargo
fallback), and reran frozen formatting, workspace/all-target tests, and warnings-denied Clippy.  Its full
tripwire table was clear.  It records residual direct dispatch in both sides and leaves only the already
stated mutant-IR input-removal/zero-cost limitation UNVERIFIED.
