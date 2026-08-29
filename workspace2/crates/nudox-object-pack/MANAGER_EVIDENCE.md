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

### Checkpoint 1 — approved smallest proof

The read-only scout (`packed_scout`, no edits) established that retaining the zerocopy borrowed
directory is the only accepted lookup representation: it avoids lookup reparsing and the resulting
impossible-error leak. On 64-bit, a directory borrow replaces two derived scalar facts without
growing the current 40-byte/8-aligned index; on 32-bit it is 20-byte/4-aligned rather than 24/4.
Public plain facts may remain direct only where they do not duplicate the retained directory.

Approved result shape: a complete borrowed pack view, `Option` for genuine absent identity only, and
a selected borrowed object containing typed descriptor/range/body facts. Verification returns a
plain `Result` whose mismatch preserves expected and actual `ContentId<ObjectDomain>`; it is one
consumer/effect and does not warrant typestate. Lookup compares the query against each raw directory
`[u8; 32]` content cell; body endpoints derive from the typed cumulative end, declared length, and
typed index extent. No direct BLAKE3 dependency is permitted: `ContentId::from_canonical_bytes` is
the already-authoritative scalar path.

Builder ownership is limited to `src/error.rs`, `src/format.rs`, `src/index.rs`, `src/view.rs`,
`src/lib.rs`, and top-level crate tests/support. Skeleton forecast: +179 production and +210 tests,
leaving 51/50 lines before the card's 230/260 ceilings. Stop if any new future-phase API, manifest
change, unsafe/SIMD, allocation, or failed 60%-remaining-contract forecast appears.

Known environment constraint: baseline executable cargo gates cannot link because `clang` is absent;
workers must still run and report format/diff checks plus the exact linker failure rather than claim a
green test. The final closure distinguishes this external gate blocker from code findings.

Pending builder and independent breaker evidence. The final section records accepted and rejected
commits, candidate digests/LOC, gates, counterexample, and remaining limitation.

### Checkpoint 2 — build, break, and closure

The explicit worker-model spawn returned a live child under the requested `gpt-5.6-luna` override
with `fork_turns: none`; the successful model-override result, rather than the worker name, is the
model proof. Its first authorized turn produced the owned-path commit
`3f42b8f235f6cd89ba574c42c7752bfaa9eab96f`. The manager imported it as
`951f85b0`, then rejected its unchecked form: lookup converted validated coordinate/schema facts
into `None`, the complete view parsed the header twice, and it supplied no consumer falsifiers.
That rejected form remains inspectable at `3f42b8f`; 119 written production lines count as worker
churn. A service thread-limit denied a fresh repair/breaker spawn, so the manager performed the
narrow repair and independent hostile review without changing the frozen surface.

Accepted repair commit: `0ebe9b06e4eb9a6dc3cb209093670821de0e5bd7` on top of `951f85b0`.
`ObjectPackIndex::complete` validates the count/header and directory exactly once, retains the typed
directory witness, and compares the complete input extent before a view exists. `ObjectPackView` uses
raw 32-byte binary comparisons and returns `None` only after an exhausted search. Its selected range
uses only construction-proven native coordinates and its selected descriptor recreates a checked
schema fact; selected verification reuses `ContentId::from_canonical_bytes` and preserves both IDs.

Independent breaker counterexamples were: header-minus-one, index-minus-one, index-only body
truncation, trailing byte, below/above/missing identities, all three positions, neighbor boundaries,
and a same-length modified body. The public `tests/view.rs` attacks each and passed. The strongest
counterexample was the same-length modified first body: the view opened (body hashing is optional)
and `verify` returned `ObjectContent { expected, actual }` exactly. The former `ok()?` path was also
eliminated; a binary-search miss is now the only public absence.

Gates (from `workspace2`, with `RUSTC_WRAPPER=` because inherited `sccache` fails with EPERM):

- `cargo fmt -p nudox-object-pack --check` — pass.
- `cargo clippy -p nudox-object-pack --all-targets -- -D warnings` — pass.
- `cargo test -p nudox-object-pack --no-fail-fast` — pass: 19 unit/integration tests plus doctests.
- `git diff --check` — pass.

Candidate production LOC is 816 from frozen 659: +157, leaving 73 lines of the 230 ceiling.
Candidate test LOC is 1,006 from frozen 778: +228, leaving 32 lines of the 260 ceiling. No manifests,
dependencies, unsafe, SIMD, async, store, transport, or range-authentication code changed. Candidate
digests are `src/error.rs` `712bf70b3cb380d71d9e2b330d2f5f9778306663ee885ea1b3f0e83118cbe1d0`,
`src/index.rs` `5b217c3781ae44d1c569f099bc3cff5e8d337096c92cdf230ccb3c6fb56e8303`,
`src/lib.rs` `b8f7450fd26449f4e83eea852381032df6df00520fc508e9302a58f686f407a7`,
`src/view.rs` `509d921765a1fe32ee197acda7c3eec0a17abb57dcff2a1ef36c43721a78febf`, and
`tests/view.rs` `00a5693dfe9aa35ce2435653d37ba2a7a6c1c1023b708cfb4a6587c00c88943b`.

Rubric result: 8/8 plan-complete. Required semantic, fault, ownership, allocation, and gate rows are
reproducible; no 9–10 stretch is claimed. The test-only logarithmic bound is established by the
audited binary-search loop, not a counter-instrumented test; this is an honest residual measurement
gap, along with untested 32-bit execution. The next smallest decision is whether a future manager
needs to add an isolated comparison counter before beginning the separate authenticated partial-range
plane; it does not authorize that plane here.
