# P5 C1 root integration review

Date: 2026-08-30

Reviewed candidate commits:

`3193cd09`, `f8ebf5b6`, `77ef0289`, `718b0c00`, `08ff2fd4`, `d1f856b6`,
`e61f34e9`, `6be0bd2c`, `70f60971`, and `9e713da9`.

## Disposition

No production or test commit is transplanted. The candidate is preserved byte-for-byte as evidence
and control material. Its useful mechanisms are preflight-before-write atomicity, immutable borrowed
lane views with exact cursors, scratch-atomic decoding, and typed local-reference bounds.

Promotion is blocked because the `0xc1` fragment only proves geometry and deliberately accepts every
payload mutation; it does not prove coordinate membership, ordering, uniqueness, or canonical form.
The separate `0xc2` type envelope is not bound to the fragment's `TypeId` lane. It accepts duplicate
structural nodes and cycles rather than interning and canonicalizing a type DAG. Both public formats
also freeze calibration-only two-item ceilings under production crate and schema names.

The nested workspace's 20 tests and strict Clippy pass, and no memory-safety or lifetime fault was
found. Those facts prove the small experiment, not a scalable IR boundary. The root workspace's
ordinary gate does not discover the candidate crate, and the return packet explicitly leaves type
products, atoms, pooled lists, external references, real frontend lowering, and C2-C6 untouched.

## Required next shape

The next C1 card must make one artifact authority own and validate every coordinate-bearing lane;
separate local and external reference types; define structural sort, interning, dedupe, and
permutation-stable bytes; derive coordinate widths from the 200-package corpus; and register the
shipping crate in the ordinary root quality graph. The retained tiny formats remain independent
controls for borrowing, atomic output, truncation, and input-dependence mutants.
