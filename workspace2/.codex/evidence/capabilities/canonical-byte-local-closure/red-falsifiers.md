# Frozen red falsifiers

## Chief red journey reproduced before Phase 0

Command:

```sh
RUSTFLAGS='--cfg canonical_byte_local_closure_red --check-cfg=cfg(canonical_byte_local_closure_red)' \
  cargo test -p nudox-operation --test canonical_byte_local_closure --locked --offline
```

Raw baseline result: exit `101`.

```text
error[E0432]: no `nudox_hydration::plan_borrowed`
error[E0432]: no `nudox_root::{BorrowedGenerationView, RootReadError, ValidatedRoot}`
error[E0599]: `Need` has no `bind_borrowed`
error[E0599]: `LocalObjectProvider<ObjectDomain>` has no `bind_verified`
```

## Required attacks before a row can close

| Matrix | Attack | Expected exact effect |
| --- | --- | --- |
| CBC-01 | truncate valid root at every boundary; mutate header/count/row/parent/schema/object cells | `RootReadError` identifies first structural failure; no trusted witness/admission |
| CBC-02 | valid root paired with locality of another root; partial locality | exact locality/pairing error; no complete borrowed view |
| CBC-03 | remove descriptor forwarding or return constant requirement | plan ordering/cardinality and chief journey fail; no trusted-path fallback |
| CBC-04 | corrupt selected pack body/digest or remove `verify` before `insert_owned` | exact pack error, zero store admission; success keeps body pointer |
| CBC-05 | omit a required stored descriptor or stage partial projection | exact `VerificationError`, no `VerifiedGeneration` |
| CBC-06 | bind sealed witness to another root/key or delete single binding test | typed operation error / chief journey failure; success does not recheck later |
| CBC-08 | replace borrow with copied/native owner or add hidden scratch | pointer/allocation/high-water control fails at 1 or 100k rows |
| CBC-09 | input-removal and constant-body source mutants | focused or chief journey fails |
