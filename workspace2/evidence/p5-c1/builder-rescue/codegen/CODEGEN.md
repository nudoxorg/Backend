# C1 builder rescue codegen custody

Status: DIRECT SOL OBSERVATION; INDEPENDENT R22 REVIEW EVIDENCE_BLOCKED.

Toolchain: `rustc 1.97.1 (8bab26f4f 2026-07-14)`, LLVM 22.1.6,
`aarch64-apple-darwin`. Production source SHA-256 is
`7e31c1b17c52c0d077300d6820f85267fce1ba684ee22cbda2084403987ad5cc`; consumer source SHA-256 is
`36c694eafe94a29662688d1b385c3f23dca86f2094f069c4e9a4e0e117e29837`.

The card's consumer command fails because the manifest is virtual and no package is selected. The
literal error is `virtual manifest, but this command requires running against an actual package`.
The direct observation added `-p nudox-ir-format`, leaving every other flag unchanged. This correction
is recorded rather than silently attributed to the advertised command.

| artifact | raw bytes / SHA-256 | gzip bytes / SHA-256 |
| --- | --- | --- |
| consumer LLVM | 212,164 / `d1a3f72ad63646e5de4bfed4913bef8dc57c40befb1834de538fac98573c10da` | 26,417 / `6cf55aa11a85879284d269334db031f47fe930ab7cc13b768f71767c7e0ef711` |
| consumer AArch64 | 66,410 / `91936bd700715edf7c95f3a99d3da3f11fd20553a80ae3758a326a25ae38c872` | 12,212 / `f96ccb568fb52b3177d180983d7aed644b83cb47c42c9586afb8e292a55021c0` |
| owner LLVM | 16,058 / `84d96ab8586f7cba9188c0366ceb26e4621962de6c956692ff313fde4e732475` | 3,065 / `a3764c051f3afb5265ce4fa1a2f5945d23602aaa76e7f6c2e341495735a41366` |
| owner AArch64 | 5,378 / `05a1ac262a235935562881582841ce584ef6e03e8ca1028fb1505160a1e29009` | 1,459 / `e9de5c4d1c3a8183e94f22c770bded07635a096e25e224a8d5a1567a086aa1dd` |

Deterministic `gzip -n -9` custody totals 43,153 bytes, leaving 22,383 bytes below the 65,536-byte
cap. All four `gzip -n -d -c ... | shasum -a 256` replays reproduced the raw hashes above.

| callable and exact ranges | allocator | memcpy/stores and staging | panic/unwind | direct calls | indirect/`blr` |
| --- | --- | --- | --- | --- | --- |
| prepared consumer, LLVM 539–645 / asm 450–546 | none | 24-byte error-result memcpy at LLVM 617; 48-byte prepared state plus 24/48-byte result slots; no payload staging | personality only; no panic call in range | writer LLVM 587/asm 499; validate LLVM 610/asm 512 | none |
| manual control, LLVM 649–869 / asm 549–722 | none | caller-input `i32` loads/stores at LLVM 795–835 and asm 610–670; 24-byte error-result memcpy at LLVM 841; 48-byte validation result only | two residual `slice_index_fail` calls at LLVM 789/828 and asm 710/719 | validate LLVM 821/asm 678 | none |
| owner `write_into`, LLVM 16–149 / asm 5–140 | none | direct caller-input `i32` loads and unaligned output stores at LLVM 111–138 and asm 48–69; no memcpy or payload staging | four residual bounds-panics plus two `slice_index_fail` calls at LLVM 39–131 / asm 93–133 | only the six residual panic helpers | none |
| owner `validate`, LLVM 153–260 / asm 144–207 | none | scalar loads/stores only; no payload copy or staging | none | none | none |

Both consumers retain input dependence: the manual body loads supplied entity/type words into caller
output, while the prepared body preserves the supplied slice pointers/lengths and passes them to the
owner writer. The result does **not** support a zero-cost, erasure, or panic-free claim. The four
fixed-width payload transfers are direct stores, not hidden aggregate staging.
