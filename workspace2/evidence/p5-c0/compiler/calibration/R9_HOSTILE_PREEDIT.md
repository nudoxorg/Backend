# R9 hostile pre-edit review

Card SHA-256: `c4fa4dd7df6644eff74e44821f2fbcd65dcc05a8f8539977d0b520396a86b74d`.

| task | model | isolation | verdict |
| --- | --- | --- | --- |
| `c0_compiler_r9_hostile` | `gpt-5.6-terra` | explicit non-inheriting hostile pre-edit review | CLEAR |

The reviewer checked the literal `review-rust-gem` tripwire against the digest-pinned skeleton,
replayed the paired R9 artifact control, and found no blocker or major.  The direct registry-source
owner artifacts define `FullRegistry::dispatch` for both control and candidate.  At opt-level 3
`drive` has no standalone definition because it is inlined; its realized stage branches appear inside
the dispatch definition.  The two owner assemblies have the same two conditional branches and no
indirect-call or panic path.  Consumer artifacts retain the explicit one-fresh-registry-rlib and
one-fresh-vocab-rlib custody with both `--extern` flags.

The static proof uses the resolved registry rlib and rejects `TypeScriptSubset.lower` with exactly one
`E0599`; the legal parse mutant clears that exact diagnostic.  Full-registry TypeScript LowerIr checks
the exact typed operands.  All scope/cap tripwires clear.  The reviewer retained the declared
limitation: the valid-cell mutant LLVM is not a focused before/mutant callable comparison, so it proves
no input-removal, erased-selection, or zero-cost conclusion.

This review authorizes only the next literal, one-path-at-a-time Luna build checkpoints.  It does not
authorize C1 work, a semantic skeleton/card change, or an unqualified codegen claim.
