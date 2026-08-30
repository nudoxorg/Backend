# Chief boundary: canonical-byte local closure

Capability: `canonical-byte-local-closure`

Public terminal: caller-owned canonical root bytes validate into a borrowed root witness; that
witness composes with canonical locality for complete-closure hydration; selected canonical object
pack bodies verify and transfer as borrowed owners into the bounded immutable store; the resulting
non-forgeable verified generation binds each pinned object operation at one identity-check
boundary, after which operation progress performs no generation or object equality recheck. The
verified-generation witness is reusable authority; it is not a consumable single-use token.

Prerequisites: typed identity authority, canonical root writer, canonical locality artifact,
complete-closure hydration proof, complete object-pack lookup and selected verification, immutable
memory-store admission.

Invariant owners and dependency direction:

- `nudox-root` owns canonical root grammar, validation, root/locality pairing, and borrowed
  selection;
- `nudox-hydration` owns demand binding, dependency-set identity, and complete-local-closure proof;
- `nudox-object-pack` owns directory/body pairing and selected content verification;
- `nudox-store-memory` owns bounded immutable payload transfer and rejection ownership;
- `nudox-operation` is the terminal consumer of verified generation authority and owns one-time
  request binding plus fused operation progress.

Chief red journey:
`crates/nudox-operation/tests/canonical_byte_local_closure.rs`.

Red command:

```sh
RUSTFLAGS='--cfg canonical_byte_local_closure_red --check-cfg=cfg(canonical_byte_local_closure_red)' \
  cargo test -p nudox-operation --test canonical_byte_local_closure
```

The dormant file-level `cfg` keeps ordinary baseline gates runnable until the manager closes the
missing public contracts. Closure removes that `cfg`; the same test must then run in the ordinary
workspace gate.

Negative space:

- no retained native row arena is required by the borrowed journey;
- no post-validation raw descriptor/schema reconstruction, panic, fallback, or omission;
- no provider/request generation or object equality recheck after successful binding;
- no body copy, per-object allocation, refcount, dynamic dispatch, test-only crate, async/runtime,
  transport, publication, cache, unsafe, new dependency, or new SIMD kernel;
- malformed root/pack bytes preserve their exact structured cause and never reach store admission;
- a partial projection or missing store object cannot produce `VerifiedGeneration`;
- a verified generation from another root cannot bind the operation.

Chief mutation sensitivity: deleting root/locality identity binding, selected-body verification,
store-presence verification, or verified-operation root binding must make the public journey fail.
Owning-crate fault tests and ordinary top-level cross-crate fault journeys must cover malformed
canonical root input, mismatched root/locality identity, malformed or substituted selected pack
bodies, partial or missing store presence, and stale verified-generation operation binding with
their exact typed errors. The implementation also owes isolated one-row and 100,000-row owner,
backing, peak-live, scratch, allocation, copy, scan/lookup/branch, and operation-work evidence.

Concurrent path ownership: none. The compiler controller remains authoritative for compiler/IR
paths; this capability owns only the five crates and layout evidence named above.

Unresolved product decisions: none. Representation choice, scratch layout, owner adapters, and
internal API factoring belong to the Terra capability cycle subject to the terminal and negative
space above.

## Authority fork: selected-body pointer oracle

The first source-isolated Terra pre-edit review found that the original chief journey compared the
stored leaf pointer with the input fixture pointer. `PreparedObjectPack::write` serializes the body
into caller-owned `pack_bytes`, so `ObjectPackView::lookup` necessarily returns a slice into that
canonical pack backing. The correct no-copy oracle compares the stored pointer with a freshly
selected and verified body from the same pack. Sol accepted that blocker and changed only this
pointer oracle; the public terminal and every negative-space rule above are unchanged. All Phase 0
packets and reviewer receipts predating this authority fork are stale and require a new digest and
fresh hostile review before implementation.

## Authority fork: sealed hydration witness dependency

The refreshed pre-edit review established that `LocalObjectProvider::bind_verified` has a current
production consumer for hydration's sealed `VerifiedGeneration` type. Sol authorizes promoting the
existing internal `nudox-operation -> nudox-hydration` test edge to a production dependency for
this one bind boundary. This implements the already-frozen `root -> hydration -> operation`
authority direction and is preferable to a new public trait, generic adapter, duplicated witness,
or relocation of invariant ownership. No external crate dependency or broader operation/hydration
coupling is authorized; removing the sealed witness from the bind must make its tests fail.

## Authority fork: transient linear-validation scratch

Canonical roots permit arbitrary parent-key order. A complete constant-memory cycle check can take
quadratic work on a long chain, which is not an acceptable interpretation of zero allocation at
100,000 rows. Sol authorizes one fallible transient `u32` lane, at most four bytes per declared row,
during `ValidatedRoot::try_from` so parent resolution and hierarchy validation remain linear after
key lookup. The lane is released before the borrowed witness returns; the witness still owns zero
root rows and zero backing bytes, and warmed view/planning/binding remains allocation-free. Evidence
must compare this safe scratch path with the constant-memory control at one and 100,000 rows,
including an adversarial chain, and retain the allocation source on reservation failure. A retained
lane, second row/descriptor arena, unbounded growth, or scratch without a measured work benefit is
not authorized.
