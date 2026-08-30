# Review packet: canonical-byte-local-closure / pre-edit-1

Snapshot: baseline commit `ef72f7286b7014fb4cf0f89f6496586b021f8d97`, tree `2d4fedd242810a9c27f470be8ea33790d70553b1`.

## Terminal

Caller-owned canonical root bytes validate into a borrowed root witness; it pairs with canonical locality for complete-closure hydration; selected verified pack bodies transfer as borrowed owners to the bounded immutable store; a sealed verified generation binds one pinned object operation once.

## Contract

- Root owns grammar, borrowed validation, exact read errors, and root/locality identity pairing.
- Hydration owns demand binding, dependency identity, and complete closure verification.
- Existing object-pack owns selected body verification; existing memory store owns bounded admission.
- Operation owns one-time verified binding and fused progress.
- Complete closure only: partial projection, missing store object, malformed root/pack, or stale root must not reach a verified/bound operation state.
- Successful body transfer remains borrowed from pack backing; no body copy or per-object allocation.

## Negative space

No retained native root-row owner in this journey; no post-validation raw reconstruction, panic, fallback, or omission; no bind-time duplicate check after success; no `Box`/`Vec`/`Arc`/`dyn`, test crate, async/runtime/transport/publication/cache, unsafe, dependency, or SIMD addition. Object-pack and memory-store production source are prerequisites and read-only. Compiler/index/durability/controller paths are excluded.

## Matrix IDs

CBC-01 canonical borrowed root; CBC-02 root/locality pairing; CBC-03 borrowed complete plan; CBC-04 selected verification and borrowed store transfer; CBC-05 sealed full-presence witness; CBC-06 one-time operation bind; CBC-07 grammar/public surface; CBC-08 1/100k resource facts; CBC-09 mutation-sensitive gates.

## Required review checks

Assess invariant ownership holes, public items that can mix facts from different valid owners, sufficiency of read-only pack/store controls, whether falsifiers kill a constant-body/bypass implementation, missing error/source and resource facts, and whether a simpler standard-library representation exists. Return ranked findings and cleared suspicions only; do not edit source or design a repair.

## Frozen command

```sh
RUSTFLAGS='--cfg canonical_byte_local_closure_red --check-cfg=cfg(canonical_byte_local_closure_red)' \
  cargo test -p nudox-operation --test canonical_byte_local_closure --locked --offline
```

Baseline red output is the four missing public symbols/methods: `plan_borrowed`, `BorrowedGenerationView`, `RootReadError`, `ValidatedRoot`, `Need::bind_borrowed`, and `LocalObjectProvider::bind_verified` (the root imports form one compiler diagnostic).
