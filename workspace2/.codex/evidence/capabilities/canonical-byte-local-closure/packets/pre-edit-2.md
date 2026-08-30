# Review packet: canonical-byte-local-closure / pre-edit-2

Snapshot baseline: commit `3b11fdbf57c858206a44203e97f00cbbacf6641b`, tree `0e716a614cb0f5a60933167b6cae89fec27f3213`. This packet supersedes `pre-edit-1.md` after Sol corrected its selected-body pointer oracle and accepted concrete fault/resource obligations.

## Terminal

Caller-owned canonical root bytes validate to a borrowed root witness; it pairs with canonical locality for complete-closure hydration; a selected verified pack body transfers as its `pack_bytes` borrow into the bounded immutable store; reusable sealed verified authority binds each pinned operation at one identity-check boundary, after which progress performs no generation/object equality recheck.

## Frozen law and negative space

Root owns grammar/read errors/pairing; hydration owns demand/dependency identity/full closure; existing pack/store are read-only prerequisites; operation owns the one bind boundary/fused progress. Malformed root/pack, mismatched locality, partial/missing closure, and stale root cannot bind. No retained native root-row terminal owner, post-validation raw reconstruction, panic/fallback/omission, body copy/per-object allocation, downstream recheck, `Box`/`Vec`/`Arc`/`dyn`, test crate, async/runtime/transport/publication/cache, unsafe, dependency, or SIMD addition is allowed.

## Required proof rows

CBC-01 borrowed canonical root and exact error priority; CBC-02 paired root/locality; CBC-03 complete borrowed plan without reparse; CBC-04 verified selected pack body transfers as the same pack backing borrow; CBC-05 sealed full store presence; CBC-06 reusable witness and sole bind boundary; CBC-07 public grammar/consumer controls; CBC-08 isolated one/100k owner/backing/peak/scratch/allocation/copy/scan/lookup/branch/operation-work facts; CBC-09 ordinary top-level and owning-crate fault journeys for malformed root, mismatched locality, malformed/substituted pack, partial/missing store, stale bind, and input-removal/constant-body mutants.

## Review request

Find concrete ownership/type/error/work holes, fact-mixing, unjustified surface, missed chief fault requirement, and a simpler control. Report ranked findings and cleared suspicions only. Do not edit source, propose repairs, or accept the capability.

## Frozen red command

```sh
RUSTFLAGS='--cfg canonical_byte_local_closure_red --check-cfg=cfg(canonical_byte_local_closure_red)' \
  cargo test -p nudox-operation --test canonical_byte_local_closure --locked --offline
```

At this baseline it exits 101 with the same missing public APIs: `plan_borrowed`; `BorrowedGenerationView`, `RootReadError`, `ValidatedRoot`; `Need::bind_borrowed`; `LocalObjectProvider::bind_verified`.
