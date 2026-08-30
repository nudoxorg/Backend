# C1 format rescue R2 closure

This replaces neither history nor the original evidence files: `f0bb2e9f` and its `CLOSURE.md` remain
superseded because they accepted forbidden `unsafe`/allocator/`Vec` evidence, a non-causal payload
containment test, a generic private-view diagnostic, and unrecorded ephemeral rlib custody.

## Accepted R2 range and closure review

```text
semantic recard:       2a4aa451
R2 calibration record: 454990cf
bounded test repair:   77ef0289
unit-test cleanup:     718b0c00
LOC ledger repair:     f941c130
```

Separate Terra postbuild task `c1_r2_terra_postbuild` (explicit `gpt-5.6-terra`,
`fork_turns=none`) first required the LOC repair, then returned FINAL R2 CLOSURE CLEAR on `f941c130`.
It independently verified `src/lib.rs` 146/210 and `tests/fragment.rs` 258/300 (R2 forecast 260,
reserve 40), clean status/range/working-tree diffs, and no code reopen.

## R2 facts actually proved

- No live format source contains `unsafe`, `GlobalAlloc`, `global_allocator`, `Vec`, `to_vec`, or an
  allocator/copy measurement claim.
- Payload-cell tests use fixed stack arrays and prove only payload validity. The sole containment unit
  test checks the private envelope/entity/type borrow start/end ranges against the caller slice; it no
  longer uses `unwrap`.
- The downstream private-view source supplies `envelope`, `entity_lane`, and `type_lane`; its isolated
  child has exactly one `error[E0451]` and all three field symbols. The other seven isolated children
  retain their exact coded absence/conversion predicates. Legal named validation consumes both cursors.
- The resolver finds one format rlib beside `current_exe`, invokes external `shasum -a 256`, stores a
  `[u8; 64]` lower-hex digest, and emits path/cardinality/hash under `--nocapture`.

Allocation, copies, optimized call paths, and whole-consumer codegen are **UNVERIFIED**. `#![no_std]`
and the absence of an `alloc` dependency are structural facts only.

## Final fresh-target custody and gates

The two manager gates both passed `cargo fmt --check`, locked domain tests, locked warnings-denied
Clippy, `git diff --check`, and clean status. The second additionally ran the format integration test
alone with `--nocapture`. Test cardinalities per complete domain run: 1 format unit, 4 format
integration, 0 vocabulary unit, 2 vocabulary integration, 0 format doctest, 1 vocabulary doctest.

```text
gate one rlib custody:
path=/private/tmp/p5-c1-r2-gate-one.68I6q2/debug/deps/libnudox_ir_format-b61fc910457ad02a.rlib
cardinality=1
sha256=9e99e8a3b9cd4c9a1b4537f15e32a83a24a1615227331fc00ffbdde0b7d2bef2

gate two rlib custody:
path=/private/tmp/p5-c1-r2-gate-two.sFaI4C/debug/deps/libnudox_ir_format-b61fc910457ad02a.rlib
cardinality=1
sha256=fc5e9f10d0a756c6a453a3f94b438011521fe2798a96968dba419b3c55f81047
```

Ephemeral paths are retained above as per-run evidence; replay creates a fresh temporary target and
records its new exact custody line. Per-run external source custody (`shasum -a 256`) from gate two:

```text
afeb037e49f08d479c39a866e06ec6ca8ffbbaeace5a52978682615bfcc1cb9c  crates/nudox-ir-format/Cargo.toml
cc5506ad173201b6cd7a4015757a52bf8e4f41b2fa8393814eca6919c8ef26ca  crates/nudox-ir-format/src/lib.rs
f759107361d9705139d3a2fd679851b202f7af746ada01272df9694c4c037581  crates/nudox-ir-format/tests/fragment.rs
784e273c39b4e56de4c8f4eaffff7ea525abeadcd094e7cd4fceb0bac3c04bc7  Cargo.lock
```

No merge, product closure, builder/prepared implementation, or C2 claim follows from this prototype.
