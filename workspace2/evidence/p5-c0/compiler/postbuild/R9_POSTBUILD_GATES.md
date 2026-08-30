# R9 real-candidate gates and paired artifacts

Candidate source checkpoint: `5017ca393b9662b8e7fa2e32e0c88caaeabf8e81`.
Control: detached `fac5b709d4596f889735a773a6dbd6a9c7c822f5`.
The four candidate paths are byte-identical to the frozen artifacts pinned by
`P5_C0_COMPILER_MANAGER_CARD.md`.

## Normal compiler workspace gate

On `aarch64-apple-darwin`, rustc `1.97.1 (8bab26f4f 2026-07-14)` and cargo
`1.97.1 (c980f4866 2026-06-30)`, the dedicated target was package-cleaned. It contained zero registry
and zero vocab rlibs before `cargo test --locked --workspace --all-targets`, then exactly one of each
afterward. The four named dispatch pointer-and-length/exact-error tests passed. The actual-rlib subset
compiler-process test passed its sole `E0599` rejection and legal parse proof. The release example built
as an all-target. `cargo fmt --check`, warnings-denied `cargo clippy --workspace --all-targets`, and
`git diff --check` passed.

```text
registry source  bbe0152ce639ee7f92a9b72e26dd6b040133a28f3700f04aee302abd1c4fe867
vocab source     a26c48dcc3cbe71b35b83ddf324cc4c961b4a7ffd42e11abcc390e9e60233d55
registry rlib    ebded3c28deaa5efe7cd5ffece6e1e5b15b94debe4aface99ac32bd632473991
vocab rlib       91dd5acb523a7a6b5471d49c0ed01c94a9035074c89fa8f0a6304090f7a9b0d0
```

The subset test intentionally emits metadata bytes on standard output through `--emit=metadata=-`.

## Same-source release control

`seeded/run-same-source-control.sh` (digest
`6cc31be5688e9381bc3138babba6da6d06394ab763e6f4bc9fa152218eb12715`) replayed the control and actual
candidate. It package-cleaned separate release targets, asserted zero then one registry rlib and zero
then one vocab rlib on both sides, and directly compiled the same named consumer with the same edition,
target, opt level, dependency path form, and two explicit `--extern` flags. It also directly compiled
both registry sources with their fresh vocab rlib. `custody.txt` digest is
`6a22994c7fab86f04eda4164ac47f60a0e2648d1dd5dc01302b4aaff3c5fe748`.

| artifact | control SHA-256 | candidate SHA-256 |
| --- | --- | --- |
| `release_consumer.ll` | `24c0afcfcd4a53d3ef26211e355fa954368e626b4ab0b3ba46595a62ce88a4bc` | `688a939a4feeda779422184d3a91233a0524efb4bade04e3561e8dd41c0d12df` |
| `registry/nudox_compile_registry.ll` | `112269370f2063e60115324fc9c08ce1ce17f0feb3f173ba7a6ba84a2e954016` | `6d0153bc22fbaf76fc864d92c304cd6f2869f1c17444003610214ef65a086dab` |

Each candidate named callable (`rust_parse`, `rust_lower`, `typescript_parse`) passes its supplied
pointer and length to a direct `FullRegistry::dispatch` call. Both registry owner bodies define
`FullRegistry::dispatch`; their semantic LLVM differs only in private trait metadata/name. `drive` has
no standalone optimized definition: realized language/stage branches are in `dispatch`, with no owner
call. The consumer retains one direct dispatch call per wrapper. That residual is baseline-equivalent,
not erased; no zero-cost or selection-erasure claim is made.

Stable per-symbol callable text sizes are unavailable on this host. Release text is **UNVERIFIED**;
no executable, assembly-file, or rlib total is substituted.
