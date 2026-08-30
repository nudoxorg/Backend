# R2 calibration restart

`f0bb2e9f` is superseded; its passing gates are not R2 evidence. The R2 card was frozen at
`2a4aa451afb5f89c1801b5c79db00cf8233d5874` before this restart.

| role | task | explicit model/fork | result |
|---|---|---|---|
| Terra manager | `/root/p5_c1_format_rescue_manager` | Terra manager | recarded the semantic evidence boundary |
| Luna calibration | `c1_r2_luna_calibration` | `gpt-5.6-luna`, `fork_turns=none` | found current unsafe/Vec/allocator/custody/fixture gaps; confirmed bounded feasibility |
| Terra hostile calibration | `c1_r2_terra_calibration` | `gpt-5.6-terra`, `fork_turns=none` | REPAIR REQUIRED for current candidate; card coherent; exact paths limited to fragment tests plus R2 evidence |

Both calibrators agree that the former allocator result is rejected rather than narrowed: whole-consumer
allocation/copy/call-path cost is UNVERIFIED. The repair must use fixed arrays, causal private-only
pointer containment, an all-fields E0451 child, and external `shasum -a 256` rlib custody. No new API,
crate, dependency, manifest, lockfile, wire byte, or C2 item is authorized.
