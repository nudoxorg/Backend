# R15 fresh real-candidate gates and same-source control

Candidate source checkpoint: `8aa30cdd57b32209ad185a2bb23f83307292d96c`.
Control: detached `fac5b709d4596f889735a773a6dbd6a9c7c822f5`.
All four real candidate paths are byte-identical to their R15 frozen skeletons.

## Normal gate and observability

On aarch64 Apple, rustc `1.97.1 (8bab26f4f 2026-07-14)` and cargo
`1.97.1 (c980f4866 2026-06-30)`, the dedicated target was package-cleaned; registry and vocab rlibs
were zero before build and exactly one each afterward. All four dispatch tests, the static subset test,
and the release example passed. The subset test terminal was clean ordinary text only:

```text
running 1 test
test typescript_lower_is_a_causal_absent_member ... ok
test result: ok. 1 passed; 0 failed
```

No rustc metadata bytes appeared. Format, warnings-denied Clippy, and diff check passed.

```text
registry source  bbe0152ce639ee7f92a9b72e26dd6b040133a28f3700f04aee302abd1c4fe867
vocab source     a26c48dcc3cbe71b35b83ddf324cc4c961b4a7ffd42e11abcc390e9e60233d55
registry rlib    036e28dc17761b18dafd5bf6814177ae7a6e4a9f666e7bd8685eda58a03c67a6
vocab rlib       ad94f8e112ef98e8d87b81ca87577792fc4afa629d1cd31ee5aafbf881d333f1
```

## Fresh paired direct-rustc control

`seeded/run-same-source-control.sh` replayed the fresh detached control and the real candidate. It
cleaned isolated release targets, proved zero then one fresh registry and vocab rlib per side, and used
the same named consumer, edition, target, opt level, dependency-path form, and explicit two `--extern`
arguments. It also compiled both registry sources directly with explicit fresh vocab rlibs. Raw outputs
are `R15_SAME_SOURCE/`; custody digest is
`1102067316dcf65e19159fe7bf7d8cbf71496bbb63db53f9c77d84aca1347171`.

| artifact | control SHA-256 | candidate SHA-256 |
| --- | --- | --- |
| `release_consumer.ll` | `23337db7d09a5c9876e97f3826d13a2e83d0ba04cc624414ee078d19f738e54d` | `16fc70851895eab654e940dacd0a066d84469db2a204c87c7ff623a8915be511` |
| `registry/nudox_compile_registry.ll` | `112269370f2063e60115324fc9c08ce1ce17f0feb3f173ba7a6ba84a2e954016` | `6d0153bc22fbaf76fc864d92c304cd6f2869f1c17444003610214ef65a086dab` |

All three named wrappers pass their supplied pointer and length to direct dispatch calls. Both owner
bodies define dispatch; optimized drive is realized inside dispatch, with one language and one stage
branch and no owner indirect call or panic path. Direct wrapper-to-dispatch calls remain on both sides:
baseline-equivalent residual work, not erasure. Stable per-symbol callable text sizing is unavailable on
this host, so release text is **UNVERIFIED**; no aggregate artifact size is substituted.
