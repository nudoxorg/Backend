# AUTHORITY_FORK packet: wire-saturation gaps — five image-retained fact classes

Filed by: terra_csharp_fidelity. Targets: trunk shared surface
(compiler-ir + driver emission lane). Decision owner: Sol/trunk.
Revised at candidate d2fed1732 after the independent review's M-1 finding:
the packet originally covered two fact classes; the review located three
more image-retained drops on the same lane surface. All five share one
decision shape, so the fork stays single.

## The five facts the C# lane retains on the wire but drops in the lane
1. Declaration-site generic variance (`out T` / `in T`).
2. The `params` parameter modifier.
3. Parameter default values (`has_default` bit + default spelling atom,
   image.rs Parameters row bytes 9/12; the consumer's
   `push_parameter_fact` never reads either).
4. Generic-constraint flag cells (`class`/`struct`/`notnull`/`unmanaged`/
   `new()`/`allows ref struct`, image.rs TypeParameters row byte 11 bits
   0x3f; the consumer's `csharp_facts` never reads the byte).
5. Second-and-later constraint types (the consumer keeps only the FIRST
   resolvable in-file nominal constraint per type parameter;
   driver/lower/csharp.rs `csharp_facts` `constraints.iter().find_map`).

The v3 authority image carries all five: the reader validates them into
closed values (`Parameter::decode` default cells, `GenericParameter::decode`
flag bytes, `ConstraintIter` full ranges). The image is NOT the gap.

## Exact drop-site evidence (line-pinned at candidate d2fed1732)
- Canonical record HAS the variance cell:
  compiler/ir/semantic.rs:1211-1226 —
  `pub struct TypeParameter { name, constraint, default, variance: Variance, is_const }`,
  `pub enum Variance { Invariant, Covariant, Contravariant, Bivariant }`.
  The canonical record also has `default: Option<TypeId>` — a default-value
  TYPE cell; parameter default SPELLINGS have no canonical cell at all.
- The emission lane has NO variance cell:
  compiler/ir/extension_pools.rs:17 —
  `pub struct ExtensionTypeParameter<'bytes> { name, constraint, default }`
  — one constraint slot, no flags.
- The driver hard-codes the loss twice:
  compiler/driver/lower.rs:987 and :1007 —
  `variance: compiler_ir::Variance::Invariant, is_const: false`.
- The consumer documents the gap and now pins it:
  compiler/driver/lower/csharp.rs:12-19 — "Two Roslyn facts..." (the doc
  sentence predates M-1; the pinned falsifier names the class set).
- Single-constraint collapse:
  compiler/driver/lower/csharp.rs:1213-1219 (`constraints.iter().find_map`
  keeps the first in-file nominal only).
- Parameter defaults: `push_parameter_fact`
  (compiler/driver/lower/csharp.rs:702-731) reads `parameter.ref_kind`
  only; `has_default`/`default` have no consumer.
- Params: no cell on `CSharpFacts` (semantic.rs:1296-1304) nor on
  parameter carriers; `CSharpReferenceKind` collapses `ref readonly` onto
  `In` already (driver/lower/csharp.rs `lane_reference_kind`), the same
  lattice-pressure pattern.
- Precedent for a typed convention cell: `PythonParameterKind` with
  `VariadicPositional` (semantic.rs:1346-1352) solves Python variadics
  exactly the way a C# params cell would.

## Red falsifier (committed with this phase)
compiler/driver/lower/csharp.rs:
`wire_saturation_gaps_stay_image_retained_pending_trunk_cells` pins the
variance and params drops from reopened pool bytes. The same falsifier
family is extended to pin classes 3-5 (default values, constraint flags,
second-and-later constraint types) so a trunk saturation that lands for
variance/params cannot silently pass while the rest stay dropped. When
trunk lands any cell, flip the matching assertion to assert saturation;
never delete a pin.

## Decision requested (pick one per class; A/B may mix)
A. Saturate the lane: thread variance through `ExtensionTypeParameter` +
   `push_type_parameter` (TypeScript callers at driver/lower/typescript.rs
   and Rust at driver/lower/rust.rs gain the explicit cell), add a params
   cell to `CSharpFacts`, add constraint-flag and multi-constraint rows,
   and add a parameter default-value cell (spelling or typed value).
B. Keep each class image-retained only, documented and pinned by the
   falsifier family (the brief's law allows a documented, typed decision;
   the pins then hold it permanently).

Cost note for A (variance thread): one shared-crate struct field + one
driver function signature + build_ir threading + reopened-pools wire bit;
consumers are enumerated (TS 2 call sites, Rust 1, Go/Java/Clang/Python
none — they do not push type parameters with variance).

This lane does NOT invent cells meanwhile (law 4 of the brief).
