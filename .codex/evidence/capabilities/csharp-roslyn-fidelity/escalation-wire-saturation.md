# AUTHORITY_FORK packet: wire-saturation gaps — generic variance + params modifier

Filed by: terra_csharp_fidelity. Targets: trunk shared surface
(compiler-ir + driver emission lane). Decision owner: Sol/trunk.

## The two facts the C# lane retains on the wire but drops in the lane
1. Declaration-site generic variance (`out T` / `in T`).
2. The `params` parameter modifier.

The v3 authority image carries both: TypeParameters row byte 10 (variance
0/1/2) and Parameters row byte 9 bit 0x1. The reader validates both into
closed values (`VarianceTag`, `is_params`). The image is NOT the gap.

## Exact drop-site evidence (line-pinned at baseline c4b6ed32e)
- Canonical record HAS the variance cell:
  compiler/ir/semantic.rs:1211-1226 —
  `pub struct TypeParameter { name, constraint, default, variance: Variance, is_const }`,
  `pub enum Variance { Invariant, Covariant, Contravariant, Bivariant }`.
- The emission lane has NO cell:
  compiler/ir/extension_pools.rs:17 —
  `pub struct ExtensionTypeParameter<'bytes> { name, constraint, default }`.
- The driver hard-codes the loss twice:
  compiler/driver/lower.rs:985 and :1005 —
  `variance: compiler_ir::Variance::Invariant, is_const: false`.
- The consumer documents the drop:
  compiler/driver/lower/csharp.rs:12-14 — "Two Roslyn facts the image
  retains have no lane cell and stay image-retained: declaration-site
  generic variance and the `params` modifier".
- Params: no cell exists on `CSharpFacts` (semantic.rs:1296-1304) nor on
  parameter carriers; `CSharpReferenceKind` collapses `ref readonly` onto
  `In` already (driver/lower/csharp.rs `lane_reference_kind`), the same
  lattice-pressure pattern.
- Precedent for a typed convention cell: `PythonParameterKind` with
  `VariadicPositional` (semantic.rs:1346-1352) solves Python variadics
  exactly the way a C# params cell would.

## Red falsifier (committed with this phase)
compiler/driver/lower/csharp.rs:
`wire_saturation_gaps_stay_image_retained_pending_trunk_cells` proves an
`out T` image cell decodes to `Variance::Invariant` and a `params`
parameter loses its modifier through admission+reopen. When trunk lands
the cells, flip the test to assert saturation; never delete it.

## Decision requested (pick one)
A. Add `variance: Variance` to `ExtensionTypeParameter`, thread it through
   `push_type_parameter` (new parameter; TypeScript passes Invariant
   explicitly — TS callers at driver/lower/typescript.rs:644 and Rust at
   driver/lower/rust.rs:1021 gain an explicit cell, no inference), and set
   it in build_ir from the lane value. Add `is_params: bool` to
   `CSharpFacts` (per-parameter carrier facts) with its wire bit on the
   reopened extension pools.
B. Keep both image-retained only, and document the lane as intentionally
   lossy for these two cells (the brief's law says a documented, typed
   decision is acceptable; the falsifier then pins it permanently).

Cost note for A: one shared-crate struct field + one driver function
signature + build_ir threading + reopened-pools wire bit; consumers are
enumerated (TS 2 call sites, Rust 1, Go/Java/Clang/Python none — they do
not push type parameters with variance).

This lane does NOT invent cells meanwhile (law 4 of the brief).
