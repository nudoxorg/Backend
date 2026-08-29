# Packed object plane manager card

## Frozen trial

- Capability: complete-pack consumer path — look up one `ContentId` in the validated strict directory,
  lend its exact body from a complete backing pack, and optionally verify that selected body identity.
- Observable consumer: public integration tests write a pack, open the complete view, find
  first/middle/last or absence, lend the selected bytes, then observe typed verification success or
  a claimed/actual mismatch.
- Baseline: `699b7cca71c78de8afebc1cee9e2a26a3ca46fdc`; formatting was clean before this card.
- Allowed paths: `workspace2/crates/nudox-object-pack/**` and this document only. No workspace
  manifest, other crate, root plan, skill, or root ignore edit is authorized.
- Preserved: writer/header/directory bytes and validation priority; `no_std`; safe Rust; borrowed
  primary bytes; identity independent of placement; validation once; scalar BLAKE3 authority; exact
  rejection operands and sources; no store/hydration, partial range, transport, mmap, lease, SIMD,
  async, or artifact authentication surface.
- Dependency decision: no manifest change. Existing workspace BLAKE3 is sufficient if direct use is
  required; a direct dependency is an unbriefed manifest change and is rejected unless the scout
  demonstrates it is already transitively unavailable to the crate and the manager records a new
  decision.

## Resource and proof budget

| Resource | Limit / evidence |
| --- | --- |
| Retained/live bytes | borrowed views only; no new retained allocation |
| Representative hot path | zero allocations and zero payload copies after fixture setup |
| Work | raw `[u8; 32]` binary comparisons, logarithmic test-only bound |
| Validation | one complete index validation; trusted lookup reuses typed directory/layout |
| Production delta | forecast <= 230 formatted lines, with >= 25-line / 10% reserve |
| Test delta | forecast <= 260 formatted lines, with >= 26-line / 10% reserve |
| Checkpoint stop | at 60% of either forecast if remaining public contract does not fit |

Required falsifiers: empty/one/many; first/middle/last/below/above/missing; index/full-body
truncation and extra byte; cumulative/address-space exact errors unchanged; pointer containment;
non-bleeding neighboring bodies; correct and same-length-wrong BLAKE3 bodies; public integration;
isolated allocation evidence; search bound; exact view/witness size/alignment.

## Ownership diagram

```text
writer input --copy into caller pack--> complete caller bytes
complete caller bytes --validate once--> borrowed index/layout witness
borrowed typed directory --binary search--> selected typed body range
complete caller bytes + typed range --reborrow--> selected body bytes --scalar BLAKE3--> checked result
```

No owner is extended, allocated, or copied after the caller owns the complete bytes. A future store
would need an owner redesign or a copy and is explicitly not represented here.

## Baseline ledger

All digests are SHA-256 of frozen baseline file contents; LOC is normally formatted physical Rust
source lines. `MANAGER_EVIDENCE.md` was absent at the baseline.

| Path | SHA-256 | LOC |
| --- | --- | ---: |
| `Cargo.toml` | `b473fc7ee94c93a4583df4a2ce3e2b9a9d254002b2cc6dd6a7525d5cb3dea1ac` | — |
| `src/error.rs` | `ebc59e3691fd45f4f34c85bfdf5e2604c300716f5889770e1be779d8cbf4e0b0` | 172 |
| `src/format.rs` | `1d41a276c85e74f1e21467e5560dc8ed71999c931bff281ab2d0032429abaa69` | 113 |
| `src/header.rs` | `c412aa0252324fdc041eb8e981dd2bfebe0d8ac8f4dc885606b492b6acd0cc4c` | 71 |
| `src/index.rs` | `a9b8807e68a0c2ee96497f786de760b66c7993e2eb50c4a05f3f6a64e20bc4a7` | 132 |
| `src/lib.rs` | `26327e8dec5c60f881798783b6f015e90569aca3680e883c35c413aa2efc5990` | 18 |
| `src/write.rs` | `f14747bf6351b751ede57446bb147c676cbcd00f7ddee684f37ec9a60f9ece49` | 153 |
| `tests/allocation.rs` | `f5c4015d261af9648393318670a58d4ee1e3ad8b0d7e9e34e180707daa4e8786` | 75 |
| `tests/header.rs` | `bb331ff3acaa0947efab5b15d9e0f332d7abc467f887847c937c632d7af62ab2` | 112 |
| `tests/header_allocation.rs` | `b9fa0202588f79bcebeeef6b36de0517c860738995b929bd0d8a3037950173e9` | 42 |
| `tests/index.rs` | `7db17bace745dcf1af8ae3f005e3748acfe4438626196bfa578e74d23cae4077` | 242 |
| `tests/index_allocation.rs` | `9a4c26425f74b8fecdf103ffe6a3251f447d78157037a30a3379e5481ebc11de` | 54 |
| `tests/object_pack.rs` | `90336b3680e1eed422117b7457d3b16e91da587af779fcb632ec7e94f2a01fd8` | 211 |
| `tests/support/object_pack_index.rs` | `e59a473c5c0d9e06e5b40f720407689ae59d6662810be66979aae07a7e21168d` | 42 |
| `MANAGER_EVIDENCE.md` | absent | — |

Baseline Rust LOC: production 659; tests 778; total 1,437.

## Decision log and closure ledger

Pending scout, builder, and independent breaker evidence. The final section records accepted and
rejected commits, candidate digests/LOC, gates, counterexample, and remaining limitation.
