# P5 C0-IR closure ledger

Repair checkpoint: `40786c9cf5f4c928bf0dbeae26afc53b9461efaa`.

| phase | task identity | model | result |
| --- | --- | --- |
| pre-edit repair review | `/root/p5_c0_ir_manager_fresh/p5_c0_ir_variance_preedit_hostile_review` | `gpt-5.6-terra` | approved only verbatim test repair |
| repair builder | `/root/p5_c0_ir_manager_fresh/p5_c0_ir_variance_luna_repair` | `gpt-5.6-luna` | test-only checkpoint `40786c9` |
| post-repair hostile review | `/root/p5_c0_ir_manager_fresh/p5_c0_ir_final_postrepair_hostile_review` | `gpt-5.6-terra` | material/custody pass |
| closure hostile review | `/root/p5_c0_ir_manager_fresh/p5_c0_ir_closure_hostile_review` | `gpt-5.6-terra` | close, zero findings |

All role spawns used `fork_turns: none`; reviewers were read-only.

## Closure reviewer raw result

No BLOCKER, MAJOR, or MINOR findings.

| tripwire | count | disposition |
|---|---:|---|
| panic/unwrap/expect/unreachable | 0 | absent |
| source-dropping conversion or map_err | 0 | absent |
| raw authority bypass | 2 baseline surfaces | `src/lib.rs:20` raw and `:25` new are preserved limitation; no new bypass |
| arithmetic/dyn/Box/Vec/Arc/Rc | 0 | absent |
| tuple/unit namespace/trait/generic abuse | 0 | absent |
| numeric sentinel state | 0 | required test/layout/diagnostic literals only |
| test-only Option/discarded/success-only terminal | 0 | bounded discovery; stderr retained; legal success paired with false predicate |
| unsafe/SIMD/allocator/dependency/new public item | 0 | absent |

Verified: source SHA-256 `2470230102424a34892369204ce20c5a164cec25d894ce8eee45331e636e5a78`,
34 LOC; test/skeleton SHA-256 `e214f0c821d2e209ae775cd153fa30422170b6654ede13b92958f68c6df04a0c`,
93 LOC; net test delta `+79`, cap `110`, reserve `17`; one fresh rlib at
`domains/ir/target/debug/deps/libnudox_ir_vocab-85e88fc5cb4d75d9.rlib`; rejected process status 1,
sole `E0308`, both alias names; legal mutant status 0 and false diagnostic predicate; Rust 1.97.1 on
`aarch64-apple-darwin`; fmt, all-target tests, Clippy `-D warnings`, diff check, and status clean.

Strongest counterexample/control: changing only `EntityId::new(7)` to `TypeId::new(7)` succeeds and
makes the rejection predicate false. Preserved limitation: deliberate raw reconstruction remains
possible through existing public baseline API and is not claimed repaired.

Remaining UNVERIFIED: layout/diagnostic-format evidence is host/toolchain-specific; physical rlib-byte
reproducibility across clean builds is outside this terminal. No compiler, dispatch, C1, or product
claim follows.
