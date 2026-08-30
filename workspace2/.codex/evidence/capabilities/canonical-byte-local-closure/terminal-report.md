# Terminal implementation, performance, and custody report

## Outcome

Implementation is complete and reproducible at material candidate
`a2fbc7c9a40f535cd683a24c9a7ba06b8d4057a2`, tree
`f77c157799dc53b2d000d042df32050e3a43a197`. Capability state is
`evidence-blocked`, not `closed`, solely because the corrected source-isolated hostile-review route
returned no reviewer runtime identity. The source, integration journey, resource controls, and
repository gates are green.

## Raw gates

All commands ran from the isolated controller worktree with Nix-provided tools and
`PROMPT_MULTILINE_INDICATOR` removed where the host environment required it.

```text
cd workspace2
env -u PROMPT_MULTILINE_INDICATOR ./tools/quality.sh
```

Passed: semantic-lint UI suite; Dylint across every shipping workspace; stable format; debug tests;
normal/all-feature Clippy with `-D warnings -D clippy::undocumented_unsafe_blocks`; all-feature docs;
9 Loom tests; unsafe proof-boundary scan; resolved normal-dependency exclusions.

```text
cd workspace2
env -u PROMPT_MULTILINE_INDICATOR nix develop .#quality -c bash -c '
  project_dir=$PWD; source tools/pinned-toolchains.sh
  RUSTC_WRAPPER= stable_cargo test --workspace --all-targets --release --locked --offline
  RUSTC_WRAPPER= RUSTFLAGS="--cfg canonical_byte_local_closure_red --check-cfg=cfg(canonical_byte_local_closure_red)" \
    stable_cargo test -p nudox-operation --test canonical_byte_local_closure --locked --offline
'
```

Passed: release root workspace and the frozen public chief journey 1/1.

```text
cd workspace2
env -u PROMPT_MULTILINE_INDICATOR nix develop --impure --expr \
  'let nixpkgs=builtins.getFlake "nixpkgs";
       rustOverlay=import (builtins.getFlake "github:oxalica/rust-overlay");
       pkgs=import nixpkgs { system=builtins.currentSystem; overlays=[rustOverlay]; };
       nightly=pkgs.rust-bin.selectLatestNightlyWith
         (toolchain: toolchain.minimal.override { extensions=["rust-src" "miri"]; });
   in pkgs.mkShell { packages=[nightly pkgs.clang]; }' \
  -c sh -c '
    MIRIFLAGS=-Zmiri-disable-isolation cargo miri test -p nudox-frame &&
    MIRIFLAGS=-Zmiri-disable-isolation cargo miri test -p nudox-view &&
    MIRIFLAGS=-Zmiri-disable-isolation cargo miri test -p nudox-runtime local_payload_prefix_guard_drops_partial_initialization_during_unwind &&
    MIRIFLAGS=-Zmiri-disable-isolation cargo miri test -p nudox-runtime terminal_retention_is_in_place_and_inline_or_local_storage_needs_no_heap_owner &&
    MIRIFLAGS=-Zmiri-disable-isolation cargo miri test -p nudox-runtime payload_cell_lifecycle_covers_cancel_terminal_reuse_and_drop &&
    cargo test --doc -p nudox-runtime'
```

Passed: 12 frame Miri tests, 34 view Miri tests, the three named runtime Miri tests, and one runtime
compile-fail doc test. The repository's default-nightly script was first attempted but spent the
bounded window building unused documentation components; the minimal-nightly command above is the
successful raw gate.

```text
workspace2/layout-lab/run-shipping-atlas.sh
workspace2/layout-lab/run-canonical-closure-resources.sh
cd workspace2
env -u PROMPT_MULTILINE_INDICATOR nix develop .#quality -c bash -c '
  project_dir=$PWD; source tools/pinned-toolchains.sh
  RUSTC_WRAPPER= stable_cargo fmt --manifest-path layout-lab/Cargo.toml --all -- --check
  RUSTC_WRAPPER= stable_cargo check --manifest-path layout-lab/Cargo.toml --all-targets --locked --offline
  RUSTC_WRAPPER= stable_cargo clippy --manifest-path layout-lab/Cargo.toml --all-targets --locked --offline -- -D warnings
'
env -u PROMPT_MULTILINE_INDICATOR git diff --check
```

Passed. Shipping atlas: 1,114 lines / 253,162 B / SHA-256
`f45d8209830a08341af4dc052d5555b6d26c0c5575dbfbeb8522e9190145e5c0`. Closure resources: 6
lines / 810 B / SHA-256 `a1b6aad0f843b262a3a497e789f679d24ec379af8171d940e9fef411e43d2d72`.

## Closure resource result

| Rows | canonical/locality caller bytes | validation allocations / peak / retained | closure scratch | plan scratch | warm plan / verify / bind-run allocations | plan / verify calls |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 71 / 49 | 1 / 4 B / 0 | 2 / 8 B | 1 / 4 B | 0 / 0 / 0 | 1 / 1 |
| 100,000 | 6,300,008 / 49 | 1 / 400,000 B / 0 | 2 / 800,000 B | 1 / 400,000 B | 0 / 0 / 0 | 100,000 / 100,000 |

The 100,000-row forward-parent owning test additionally records `N` authority rows, `N-1` order
rows, `N` parent rows, exact 400,000-byte hierarchy scratch, and at most `4*N` hierarchy steps. The
constant-memory control would perform exactly 5,000,050,000 reads. Present-parent resolution remains
`O(N log N)` because canonical binary search is used per present parent. No journey stage uses an
atomic. Exact dynamic branches and compiler-level copies are not measured; the atlas says `unknown`.

Public target layouts and all owner/lifetime classifications are in `resource-controls.md`; the key
nontrivial sizes are `ValidatedRoot` 80 B/2 cache lines, `BorrowedGenerationView` 88 B/2,
`BorrowedHydrationPlanView` 176 B/3, `VerifiedGeneration` 64 B/1, and
`BoundLocalObjectProvider` 88 B/2. Whole-binary code size is 587,776 B/434,608 B `__text` for the
all-crate atlas and 439,808 B/291,144 B `__text` for the closure control.

## Independently replayed performance controls

These are host samples on the M3 Pro, not shipping promises.

### Root owners and phase reuse

Current production raw authority at 100,000 rows:

| path | construction peak | retained | median ns | identity |
| --- | ---: | ---: | ---: | --- |
| owned `Vec` | 13,600,000 | 6,400,000 | 18,614,542 | `023486f9…302a` |
| historical-labelled streaming path | 13,600,000 | 6,400,000 | 18,387,750 | same |

The separately replayed phase-reuse candidate is not shipping:

| root density | current bytes / ns | phase-reused bytes / ns |
| ---: | ---: | ---: |
| 1% | 13,600,000 / 8,524,875 | 6,408,000 / 6,114,708 |
| 10% | 13,600,000 / 10,127,875 | 6,480,000 / 6,881,375 |
| 50% | 13,600,000 / 7,590,167 | 6,800,000 / 6,033,291 |
| 100% | 13,600,000 / 6,922,583 | 7,200,000 / 6,055,583 |

Checksums matched in every cell. The shipping 6.4 MB phase-niche claim is rejected.

### AoS, SoA, AoSoA, and in-place conversion

| rows | layout bytes | lookup / key / full scan ns |
| ---: | ---: | ---: |
| 100k AoS | 6,400,000 | 222,333 / 60,583 / 61,208 |
| 100k SoA | 6,400,000 | 119,125 / 9,583 / 61,875 |
| 100k AoSoA | 6,500,000 | 261,958 / 35,583 / 79,500 |
| 1m AoS | 64,000,000 | 2,454,542 / 1,391,750 / 1,765,250 |
| 1m SoA | 64,000,000 | 1,369,250 / 131,875 / 1,577,792 |
| 1m AoSoA | 65,000,000 | 2,734,750 / 2,544,958 / 1,477,833 |

SoA is a workload-specific candidate. Same-size in-place conversion retained 6.4/64 MB but cost
2,602,458 ns at 100k and 177,044,041 ns at 1m, so it remains rejected.

### Atomics and public runtime

The isolated credit control (1,000,000 operations per producer) reproduced AcqRel vs Relaxed median
ns: `1/1 6,592,875/4,543,500`; duplicate `1/1 7,285,750/4,494,500`; `2/1
56,967,917/21,968,916`; `2/2 48,922,250/16,952,958`; `4/1 214,297,292/82,663,334`; `4/4
168,395,125/58,395,375`; `8/1 680,482,666/236,382,042`; `8/8
328,955,666/268,062,625`. The checked-in raw table also records operations/second. Relaxed ordering
is an unshipped candidate without a correctness proof.

Every public-runtime cell completed 20,000 operations and drained checked-out/reserved/terminal
counts to zero. Default vs optional atomic-accounting median ns for producer counts 1/2/4/8 were:

```text
payload 0:    5,724,459/6,715,916  5,322,084/6,232,041  6,289,500/6,786,708  5,783,167/6,770,875
payload 64:   8,283,750/9,025,250  8,259,500/9,389,292  8,154,958/9,414,417  8,709,417/9,752,625
payload 4096: 10,300,000/12,266,042 8,851,750/10,291,583 8,464,291/9,583,959 9,354,667/10,404,500
```

Atomic accounting was 7.9–19.1% slower in all 12 matched cells.

### Scalar/SIMD and grouped index controls

Shipping locality scalar/SIMD medians (ns): `0 173,430,333/178,282,375`; `1
319,824,958/309,440,084`; `4 82,689,417/84,130,458`; `8 52,043,875/50,059,625`; `16
35,514,375/39,372,708`; `32 32,669,625/11,005,541`; `64 23,532,958/6,589,250`; `128
20,670,875/4,780,625`; `256 19,808,042/3,197,625`; `1024 20,398,542/3,111,917`; `4096
31,664,959/1,882,834`; `16384 22,999,333/1,767,333`. Scalar remains the oracle and dispatch stays
outside the hot loop at 32 rows.

Grouped NEON metadata was 655,360 B versus baseline 1,048,576 B. Hit median was
6,881,958 ns versus 7,719,625; miss was 19,912,791 versus 14,456,500; mixed was 14,957,375 versus
15,544,500. The miss regression rejects a universal shipping index. AoSoA, in-place conversion,
relaxed production atomics, a universal grouped-SIMD index, SIMD below the crossover, and gather
binary search remain rejected.

## Retained mechanisms

- One nested typed `RootWireRecord` grammar and checked `ContentAuthority` projection.
- One exact transient `u32` parent lane, released before `ValidatedRoot` returns.
- Borrowed canonical root/locality/pack views with caller-owned reusable closure/plan scratch.
- Sparse absent ordinals, exact demand/verification causes, and sealed verified-generation facts.
- Selected-body verification and borrowed store transfer with source-pointer proof.
- Checked `bind_verified` plus no-request bound `start()`.
- Static observability name for demand mismatch; typed lazy tracing remains unformatted/unallocated
  when filtered.
- Scalar oracle plus existing SIMD locality dispatch at the measured crossover.

## Rejected mechanisms

- Quadratic constant-memory hierarchy walking; retained native rows/descriptors; second arenas;
  `Box`/`Arc`/`dyn`; serde; caching; unsafe/self-reference crates; unearned allocator policies.
- Shipping phase-niche reuse, object-pack scatter/gather writer, or relaxed atomics: current source
  does not contain those historical claims.
- Mandatory SoA/AoSoA, in-place transposition, universal Swiss/NEON index, and short/gather SIMD.
- Any review/approval inference from the two failed corrected sidecars or historical direct attacks.

## Exact coherent commits

- `ef72f7286b7014fb4cf0f89f6496586b021f8d97` — freeze public red journey
- `500f89cc858c7b5315b1299b7f71c2db117111c6` — freeze phase 0
- `4444bc4b068adb632c7f8ceb211f6fe421c6690d` — record initial review custody block
- `bfea0040c3a76f90c9d3abaef2f182a7b055457b` — correct pack pointer oracle
- `3b11fdbf57c858206a44203e97f00cbbacf6641b` — bind fault/resource proof
- `d9ce70271eb7ba07a82306996317ddf3cb46c7e0` — refresh phase-0 contract
- `98e4e56e399a0b473b0e37ec9122f39770427a6f` — authorize hydration bind edge
- `622553c9db62c6a52fcf619f63ad36cf0fceef01` — authorize vertical card
- `3e23c75002b8944151d06a2f95ef113d70050e24` — bind corrected review custody
- `b062d77e9805d8be1a4ae85f7efb4b28402e700d` — authorize linear root scratch
- `a4c454a18cc54dc095eb429b5609f62a615c6ff0` — authorize root card
- `31333fa1761d8c242c589886dbecb1b0680cde78` — record vertical Luna dispatch
- `7c5a2b0accf6644dfb9a081fda5e8225da23f7ba` — freeze root salvage card
- `7717e995ee6448d15ee9206bc1b5ef099abd790a` — record root Luna dispatch
- `9d9738a1ae7ded73f8e9a532deb4515f88eb4f9f` — validate borrowed canonical roots
- `4f000d0bca7d98ed3ce1ae6887a918783c2d4400` — freeze hydration card
- `181925d093633a5a6b725f6375f8090d893956b7` — record hydration Luna dispatch
- `fa24a3b4c1e620736b7a158f18571c8f4ce8eb65` — plan borrowed closures
- `4fdbe439b0d1f29ed5238d1a31cc505b47d5f327` — preserve borrowed demand cause
- `b0aec97d3c8f4a2375bd6e99619b4de6b2de7da2` — bind hydration checkpoint
- `a4eb376e71eef7bef0c8dd6a2a16fddae86eebd4` — freeze operation card
- `72d0403893cbef0cafd85a2781a1db3d88737ceb` — record operation Luna retry
- `c058fb25d0e03e6c6bf3f394085bfae14e654803` — bind verified borrowed provider
- `0484643bf8a7f23f6bd5bdf1b18703a740b89b5d` — preserve verified pack-body lifetime
- `52170327ca2967a9d4f13ed9747d43ec157c6a0b` — type journey byte capacity
- `83c9c2ba1b6570bbb1be1db1fea10e2fed3bae6b` — honor promised presence
- `acee01eabae2385337e777d1d2ab6faaec5fdbe0` — bind operation checkpoint
- `622cdbe53de81f9339e3901cd301eedc4db3c51b` — format journey
- `e4558004dfb71cc7133e5be661e503d45b6791a7` — preserve blocked candidate review
- `2dd846956e98a8d314841f2a40ea7991ae5a1645` — freeze atlas card
- `1bde72547c8dc06726e5803453d9816729249b04` — correct atlas Nix root
- `4b3bfea90e310a635f2d75cc90ce10b6dcd68567` — repair root production control
- `72c98433b33bd141db7240771aa21fbe93f41016` — name demand mismatch
- `b95626bbde45d2824e391af62fc5e4050f8243f4` — correct stale performance claims
- `910e56b27dc5ae1851bdb6ab4e0cf78222464547` — refresh production root raw control
- `1bb01041c935814b623ac92f6cf4e570201fdd74` — add executable shipping atlas
- `bc87fd8958ea0c4a21db34f9ad9b9580546862f9` — bind final atlas measurements
- `4b24257d51d70c9171fcaba5b6aca64943488dc8` — measure closure resources
- `4ef531c7c9242e5b160b00eae06e5abf799f631f` — retain missing-parent coordinate
- `a2fbc7c9a40f535cd683a24c9a7ba06b8d4057a2` — preserve resource-control causes

The containing terminal-compaction commit is intentionally recorded by Git rather than embedded in
its own contents. `changed-paths.md` is the exact path ledger.
