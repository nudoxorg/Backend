# P5 C1 type-DAG borrowed-read card

Status: REJECTED AS PRODUCT-COMBINED READ SLICE. The formatted decoder at `f9742596` was 278 lines
against 240; `78bb3b83` removes it. The active replacement is
`P5_C1_TYPE_DAG_REF_READ_CARD.md`. Ordered products move to their own edge-position card.

## Observable boundary

Close only canonical borrowed decoding: validate one padding-free `0xc2` type table once into caller
`TypeNode` scratch, then lend an exact fused cursor over that proof. The retained `0xc1` fragment and
all writer APIs remain byte-for-byte unchanged. Preparation and emission are a later card.

The public consumer validates `[0xc2, 1, 1, 2, 1, 0]`, observes a legal self-reference
`Reference(TypeId::new(0))`, proves both returned byte and node borrows lie in caller regions, and
exhausts/fuses the cursor. Replacing only `TypeId` with `EntityId` in the actual-rlib consumer must
produce E0308.

## Custody and paths

Baseline is clean commit `cda6a396`. Write only:

- `workspace2/domains/ir/crates/nudox-ir-format/src/lib.rs`: private module plus named reexports;
- `workspace2/domains/ir/crates/nudox-ir-format/src/type_dag.rs`: production read boundary;
- `workspace2/domains/ir/crates/nudox-ir-format/tests/type_dag.rs`: runtime and mutation proof;
- `workspace2/domains/ir/crates/nudox-ir-format/tests/fragment.rs`: actual-rlib fixture only;
- `workspace2/evidence/p5-c1/type-dag-read/**`: direct evidence and rejected mutants.

No manifest, lockfile, vocabulary, dependency, macro, unsafe, allocation, string owner, fragment,
writer, atom, list, external-ref, frontend, recipe, stage, scheduler, or publication edit.

## Wire, proof, and errors

Header is `magic=0xc2, schema=1, node_count 0..=2, body_bytes`; body records are primitive
`[0, code]` (`Bool=0`, `I32=1`), reference `[1, target]`, and ordered product
`[2, left, right]`. Dense ordinal is the node's `TypeId`. Back, forward, and self edges are legal;
only targets outside `0..node_count` fail. No cycle scan or interning exists.

Validation priority: short header; magic; schema; node count; declared total geometry; scratch
capacity; node tag/width; primitive code; first edge in node/left/right order; trailing body bytes.
Decode into a two-entry stack array, then copy the complete proof into caller scratch, so every error
leaves scratch unchanged. The cursor only splits that typed slice; it never reads wire bytes.

Use compact structured errors instead of a variant explosion:

```rust
pub enum TypeDagHeaderError { Magic { actual: u8 }, Schema { actual: u8 }, NodeCount { actual: u8 } }
pub enum TypeNodeFault {
    Truncated { required: usize, actual: usize }, Tag { actual: u8 }, Primitive { actual: u8 },
    Edge { slot: TypeEdgeSlot, target: TypeId, node_count: u8 },
}
pub enum TypeEdgeSlot { Reference, Left, Right }
pub enum TypeDagError {
    TruncatedEnvelope { actual: usize }, Header(TypeDagHeaderError),
    Geometry { expected: usize, actual: usize },
    ScratchTooSmall { required: usize, available: usize },
    Node { ordinal: TypeId, fault: TypeNodeFault },
    NodeBytes { expected: usize, actual: usize },
}
```

Public values are `PrimitiveType`, `TypeNode`, `TypeDagView<'bytes, 'scratch>`, and
`TypeNodeCursor<'scratch>` plus `TYPE_DAG_HEADER_BYTES`, `TYPE_DAG_MAGIC`, `TYPE_DAG_SCHEMA`, and
`MAX_TYPE_DAG_NODES`. Every value/error derives only the ordinary traits its tests consume. View
fields and cursor fields are private. `AsRef<[u8]>` returns the exact input.

## Falsifiers and caps

- Goldens: empty, Bool, I32, self reference, forward/back product; exact/fused cursor.
- Mutate every header cell, truncate every prefix, every tag/code, and each edge slot; assert exact
  structured operands and unchanged scratch.
- A body trailer after the declared node count is `NodeBytes`, not ignored.
- Actual exported rlibs prove E0308 and a one-token legal mutant.
- A constant-node decoder and an input-removal consumer each make a named test red.
- Record host sizes/alignment and pointer containment; wire bytes alone are portable.
- Scan production and consumer codegen honestly for allocation, copy, panic/unwind, indirect calls,
  and residual decode. Do not claim zero cost or panic freedom.

Formatted caps: `src/type_dag.rs` 240 lines; `src/lib.rs` net 20; `tests/type_dag.rs` 300;
`tests/fragment.rs` net 50; evidence 180. Any exceeded cap, new public item, second wire decode,
fragment change, or failed mutant stops this slice. Run focused tests, retained workspace tests,
format, warnings-denied Clippy, actual-rlib proof, two clean fresh-target gates, and status checks.

## Next decision

If direct evidence clears correctness and caps while fresh agent roles remain unavailable, retain the
slice as `EVIDENCE_BLOCKED` pending independent calibration/review. The next card may add prepared
emission; it may not broaden this decoder.
