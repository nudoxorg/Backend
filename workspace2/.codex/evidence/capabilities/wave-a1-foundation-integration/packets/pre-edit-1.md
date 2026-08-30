# Independent review packet: Wave A.1 pre-edit

## Frozen source and scope

- Review source snapshot: Git archive of
  `5407851257dc99fd17bf08d7c47f0b413ae0f2da` only. It contains no Git history
  and no manager evidence journal.
- Chief public red test:
  `crates/nudox-operation/tests/wave_a1_foundation.rs`, SHA-256
  `4920e20c2b06e9271b614e6a2a342b968a28d8803f0908257b9801ba24f92775`.
- Intended affected production boundaries:
  `nudox-root`, `nudox-object-pack`, `nudox-object`, `nudox-store-memory`,
  `nudox-hydration`, and `nudox-operation`. No production edit exists yet.
- Required non-production evidence boundaries: this packet, frozen brief/matrix
  digest references below, and existing crate/top-level tests. No new test-only
  crate is permitted.

## Required terminal

An actual operation must start with canonical root bytes validated into a
borrowed root view and an authenticated sparse object-pack range sequence:
full-pack expected artifact identity plus an explicit proof authenticates the
header, header-plus-directory, and selected body range. The selected body then
matches the exact root descriptor and its content identity, is admitted
transactionally into the generic immutable store, and hydration replay yields a
non-forgeable verified generation capability that a real operation consumes.

The untrusted demand generation is checked exactly at binding to the validated
view. The verified operation may bind its `{generation,key}` request but may
not later reconstruct a generation/object equality check. Missing range is an
explicit typed result, never empty success. Every rejected authenticated body
retains its owner and exact causal error.

## Required ownership and dependency boundaries

- `GenerationRoot` remains native construction/control; planning uses a
  borrowed canonical `ValidatedRoot` plus `ValidatedLocality`.
- Full-pack authentication owns only physical sparse range authority;
  `ContentId` independently verifies body semantics.
- Pack must not become a store dependency. Store remains generic and may only
  accept a non-forgeable verified-owner capability.
- Hydration remains pure and narrowly feeds the operation capability. No
  publication, index, compiler, UI, cache, transport, or runtime policy is in
  this review scope.

## Required resource/error constraints

- Validation and trusted projection are zero-panic and do not duplicate a
  successful raw-to-closed semantic decode.
- At 100,000 rows, canonical root bytes are `8 + 63*N`; hierarchy scratch is
  reusable caller-owned `u32` storage (`4*N`), without retained native root
  sidecar after validation.
- Account for all new allocations, warmed allocations, peak live owners,
  copies, scans, and error-path owners. Do not introduce default
  `Box`/`Vec`/`Arc`/`dyn`, a cache, unsafe/SIMD, or a dependency without a
  current measured/type-level need.

## Review evidence to inspect

- Frozen capability brief SHA-256:
  `1a07801ede413c48947bfefb095c43b5853e3e888ec521881caadb6a9afd2c54`.
- Frozen proof matrix SHA-256:
  `50127116ca43ff5699e2992fbce2760a52d37f232df799d8e8b7fa92295acbc7`.
- `TESTING.md` SHA-256:
  `c29ae328a26117dd347c9b4952b24774c0cd7a5c6b8cc0b28ae2d7f22fda8e7b`.

Review the literal ABI skeleton and existing consumers adversarially before
editing: identify an invalid-state leak, unauthenticated parsing path, false
full-artifact authentication claim, duplicate invariant owner, lost error or
owner, material allocation/scan, dead marker, unearned public/dependency
surface, weak red test, or simpler standard-library representation. Return
ranked findings and cleared suspicions only; do not implement or propose an
unfrozen API.
