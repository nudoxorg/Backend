# Terra reviewer calibration raw return

Task: `/root/p5_c0_ir_manager_fresh/p5_c0_ir_variance_reviewer_calibration`.
Model: `gpt-5.6-terra`; `fork_turns: none`; card, skills, and frozen skeleton only.

Verified card commit `2e317c6329d9b73c576a8f9f2107cf79ff541e35`, SHA
`17143118e732379f7afff33569de5fe261accca0845b6d6046b7fcca397fd215`; skeleton SHA
`e214f0c821d2e209ae775cd153fa30422170b6654ede13b92958f68c6df04a0c`, 93 LOC. Pinned source
was deliberately unopened and remains **UNVERIFIED**.

| tripwire | count | locations | disposition |
|---|---:|---|---|
| panic/unwrap/expect/unreachable | 0 | skeleton 1–93 | clear |
| source-dropping conversion/map_err | 0 | skeleton 1–93 | I/O uses `?` |
| new conversion/raw bypass | 0 | skeleton; `new` fixture uses | baseline raw limitation preserved |
| arithmetic/dyn/Box/Vec/Arc/Rc | 0 | skeleton 1–93 | clear |
| public tuple/traits/unit namespace/generic abuse | 0 | skeleton 1–93 | clear |
| numeric sentinel state | 0 | layout/fixture literals | no semantic sentinel |
| Option/discarded/success-only terminal | 0 | discovery and legal assertions | bounded/causal |
| unsafe/SIMD/allocator/dependency/public item | 0 | skeleton 1–93 | std-only consumer |

Folded discovery at 27–40 preserves zero/one/multiple semantics: `replace` runs only for matching
names and a second match errors. Non-blocking hardening note: it does not explicitly check a matching
entry is a regular file; a directory/symlink would fail rustc rather than falsely pass. Literal layout,
target binding, mutant, and one-error predicate are structurally sound. Hidden cost: two PATH-sensitive
rustc processes and a directory scan. Verdict: **PASS FOR FROZEN-SKELETON CALIBRATION; not acceptance**.
