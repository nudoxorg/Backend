# R9 separate Terra postbuild review

| task | model | candidate commit | verdict |
| --- | --- | --- | --- |
| `c0_compiler_postbuild_terra` | `gpt-5.6-terra` | `0cc5a2d53fb75858cba46f72c2ca067feaef8088` | CLEAR |

The reviewer verified each actual candidate source is byte-identical to its frozen skeleton, replayed
the normal compiler gates, and inspected the fresh paired direct-release raw artifacts. The candidate
has exactly the intended public values: `FullRegistry::dispatch` and `TypeScriptSubset::parse`; no
additional public error/tag/trait surface was found. The three named release callables forward pointer
and length to direct dispatch calls. The owner body has realized language/stage branches with no call,
indirect call, or panic route; `drive` is inlined. The retained direct wrapper-to-dispatch call is
baseline-equivalent and is not characterized as erased.

The actual-rlib subset test remains causal: its forbidden source is the sole `TypeScriptSubset.lower`
use, it requires exactly one `E0599` mentioning both names, and its parse-only legal mutant clears the
predicate. The exact-one rlib guard and explicit `--extern` prevent a shadow or stale rlib substitute.
All literal tripwires cleared. Stable per-symbol release text is unavailable, and the old valid-cell
mutant LLVM is not a focused before/mutant callable comparison, so release text and input-removal
codegen remain UNVERIFIED. No repair authority was issued.
